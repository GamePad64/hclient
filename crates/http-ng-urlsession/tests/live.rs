//! `URLSession` against a real server, on a real Apple platform.
//!
//! Gated to Apple targets, so a Linux or wasm run of the workspace skips
//! the file rather than pretending. What that costs is stated where it is
//! felt: **these tests do not run on the Linux or Windows arms of this
//! project's CI matrix**, and the macOS arm is the only place they are
//! evidence of anything.
#![cfg(target_vendor = "apple")]

use http_ng_core::RequestBody;
use http_ng_core::unversioned::Transport;
use http_ng_urlsession::UrlSession;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;

/// A server that records the request head it was sent and answers by a
/// script — the shape every wire-level test in this workspace uses,
/// because the claim is always about what actually went out.
fn server(respond: &'static str) -> (SocketAddr, mpsc::Receiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match s.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => head.push(b[0]),
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&head).into_owned());
            let _ = s.write_all(respond.as_bytes());
            let _ = s.flush();
        }
    });
    (addr, rx)
}

async fn collect(body: impl http_body::Body<Data = bytes::Bytes>) -> String {
    use http_body_util::BodyExt as _;
    let mut body = std::pin::pin!(body);
    let mut out = Vec::new();
    while let Some(frame) = body.as_mut().frame().await {
        let Ok(frame) = frame else { break };
        if let Ok(d) = frame.into_data() {
            out.extend_from_slice(&d);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The whole exchange: the request reaches a real server, and the head and
/// the streamed body come back.
#[test]
fn a_request_reaches_a_real_server_and_the_body_comes_back() {
    let (addr, seen) = server(
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Probe: yes\r\nConnection: close\r\n\r\nhello",
    );
    let t = UrlSession::new();
    let url = format!("http://127.0.0.1:{}/x", addr.port());
    let resp = futures_executor::block_on(
        t.execute(
            http::Request::builder()
                .uri(&url)
                .header("x-sent", "1")
                .body(RequestBody::Empty)
                .expect("request"),
        ),
    )
    .expect("the exchange completes");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("x-probe").map(|v| v.to_str().unwrap()),
        Some("yes"),
        "the response headers come through"
    );
    let body = futures_executor::block_on(collect(resp.into_body()));
    assert_eq!(body, "hello");

    let head = seen.recv().expect("the server saw it");
    assert!(head.starts_with("GET /x HTTP/1.1\r\n"), "{head}");
    assert!(
        head.to_ascii_lowercase().contains("x-sent: 1"),
        "the caller's header reaches the wire: {head}"
    );
}

/// **A redirect is NOT followed**, which is the capability claim that
/// separates this backend from `http-ng-fetch`.
///
/// The delegate answers `willPerformHTTPRedirection` with `nil`, so the
/// `302` is handed back as an ordinary response and `Client`'s redirect
/// policy is what decides. A browser gives no such choice — which is why
/// that backend reports `RedirectSupport::Internal` and this one reports
/// `Transparent`. This server would answer a second request, so a backend
/// that followed would return `200` where this asserts `302`.
#[test]
fn a_redirect_is_handed_back_rather_than_followed() {
    let (addr, seen) = server(
        "HTTP/1.1 302 Found\r\nLocation: /elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let t = UrlSession::new();
    let url = format!("http://127.0.0.1:{}/start", addr.port());
    let resp = futures_executor::block_on(
        t.execute(
            http::Request::builder()
                .uri(&url)
                .body(RequestBody::Empty)
                .expect("request"),
        ),
    )
    .expect("the exchange completes");

    assert_eq!(resp.status(), 302, "the 3xx is the answer, not a hop");
    assert_eq!(
        resp.headers().get("location").map(|v| v.to_str().unwrap()),
        Some("/elsewhere"),
        "and its Location reaches the caller, who is the one deciding"
    );
    let first = seen.recv().expect("one request");
    assert!(first.starts_with("GET /start "), "{first}");
    assert!(
        seen.recv_timeout(std::time::Duration::from_millis(500))
            .is_err(),
        "exactly one request: a followed redirect would be a second"
    );
}

/// A `POST` with a body, because a request body is a different code path
/// from a header and the two fail separately.
#[test]
fn a_post_body_reaches_the_server() {
    let (addr, seen) = server("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
    let t = UrlSession::new();
    let url = format!("http://127.0.0.1:{}/submit", addr.port());
    let resp = futures_executor::block_on(
        t.execute(
            http::Request::builder()
                .method("POST")
                .uri(&url)
                .body(RequestBody::Full(bytes::Bytes::from_static(b"PAYLOAD")))
                .expect("request"),
        ),
    )
    .expect("the exchange completes");
    assert_eq!(resp.status(), 204);
    let head = seen.recv().expect("the server saw it");
    assert!(head.starts_with("POST /submit "), "{head}");
    assert!(
        head.to_ascii_lowercase().contains("content-length: 7"),
        "the body's length is declared: {head}"
    );
}

/// **The capabilities are what the code does**, not what the platform
/// could do — asserted rather than only written down, because this is the
/// claim a `Client` acts on at `build()`.
#[test]
fn the_capabilities_match_the_configuration() {
    let t = UrlSession::new();
    let c = t.capabilities();
    assert!(
        !c.owns_cookie_jar,
        "the session's cookie storage is nil, so http-ng's own jar is in force"
    );
    assert!(!c.owns_cache, "and its cache likewise");
    assert_eq!(
        c.redirects,
        http_ng_core::RedirectSupport::Transparent,
        "the delegate refuses them, so Client's policy decides"
    );
}
