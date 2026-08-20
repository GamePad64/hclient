//! The h1 upgrade — this crate's half of `docs/w4-upgrade-seam.md` §8 —
//! watched from the other side of a real socket.
//!
//! The framing that used to sit on top of this lives in
//! `hclient-tungstenite` now, and its tests moved with it. What is left
//! here is what stayed here: the request this transport puts on the wire,
//! the `101` recognised by **status**, and the bytes a server sent in the
//! same flight as it.
//!
//! Nothing here knows what WebSocket is, deliberately — the fixtures
//! answer with a bare `101` and no `Sec-WebSocket-Accept` at all, and the
//! upgrade succeeds, which is the honest statement of where the boundary
//! now is.
#![cfg(not(target_family = "wasm"))]

use hclient_core::Error;
use hclient_dns::IpLiteralOnly;
use hclient_native::{Native, NotSwitchingProtocols};
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::error::Error as _;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::Duration;

/// Ceiling for anything that must not hang. Never a threshold — every
/// failure it can produce is "this hung".
const BOUND: Duration = Duration::from_secs(30);

/// Binds a loopback port and runs `f` on one connection, on a thread of
/// its own.
fn serve<F>(f: F) -> SocketAddr
where
    F: FnOnce(std::net::TcpStream) + Send + 'static,
{
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    std::thread::spawn(move || {
        if let Ok((sock, _)) = listener.accept() {
            sock.set_read_timeout(Some(BOUND)).expect("read timeout");
            f(sock);
        }
    });
    addr
}

/// The request head, up to and including the blank line.
fn head(sock: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            return String::from_utf8_lossy(&buf[..i + 4]).into_owned();
        }
        match sock.read(&mut chunk) {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&buf).into_owned(),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

fn header<'h>(head: &'h str, name: &str) -> Option<&'h str> {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
}

fn native() -> Native<Tokio, Rustls, IpLiteralOnly> {
    Native::new(Tokio, Rustls::with_webpki_roots(), IpLiteralOnly)
}

fn open(uri: &str) -> http::Request<()> {
    http::Request::builder().uri(uri).body(()).unwrap()
}

/// A bare `101`: no `Upgrade:`, no `Connection:`, no
/// `Sec-WebSocket-Accept`. This crate asks for none of them.
const BARE_101: &[u8] = b"HTTP/1.1 101 Switching Protocols\r\n\r\n";

/// What this transport puts on the wire, and it is the same two things it
/// does to every other HTTP/1 request: origin-form, and `Host:` from the
/// authority when the caller did not set one.
///
/// The absolute URI goes in — `http://addr/chat` — and what must come out
/// is `GET /chat HTTP/1.1` with `Host: addr`. A transport that passed the
/// absolute form through would be sending a proxy-form request line to an
/// origin server.
#[tokio::test]
async fn the_request_goes_out_in_origin_form_with_a_host() {
    let (tx, rx) = mpsc::channel::<String>();
    let addr = serve(move |mut sock| {
        let h = head(&mut sock);
        let _ = tx.send(h);
        let _ = sock.write_all(BARE_101);
        // Hold the socket open so the client's upgrade completes.
        let mut sink = [0u8; 64];
        while matches!(sock.read(&mut sink), Ok(n) if n > 0) {}
    });

    let upgrading = tokio::time::timeout(
        BOUND,
        native().upgrade(open(&format!("http://{addr}/chat"))),
    )
    .await
    .expect("must not hang")
    .expect("a 101 is an upgrade");
    assert_eq!(
        upgrading.head().status,
        http::StatusCode::SWITCHING_PROTOCOLS
    );

    let sent = rx.recv_timeout(BOUND).expect("the server's report");
    assert!(
        sent.starts_with("GET /chat HTTP/1.1\r\n"),
        "an origin-form request line, and it was {:?}",
        sent.lines().next()
    );
    assert_eq!(header(&sent, "host"), Some(addr.to_string().as_str()));
}

/// A `Host:` the caller set is honoured rather than replaced — the same
/// rule `established::Rewritten::for_http1` follows for every other
/// request, and the reason `Host` is not on the framing crate's list of
/// headers it owns.
#[tokio::test]
async fn a_host_the_caller_set_is_left_alone() {
    let (tx, rx) = mpsc::channel::<String>();
    let addr = serve(move |mut sock| {
        let h = head(&mut sock);
        let _ = tx.send(h);
        let _ = sock.write_all(BARE_101);
        let mut sink = [0u8; 64];
        while matches!(sock.read(&mut sink), Ok(n) if n > 0) {}
    });

    let mut req = open(&format!("http://{addr}/"));
    req.headers_mut().insert(
        http::header::HOST,
        http::HeaderValue::from_static("chosen.example"),
    );
    let _up = tokio::time::timeout(BOUND, native().upgrade(req))
        .await
        .expect("must not hang")
        .expect("a 101 is an upgrade");

    let sent = rx.recv_timeout(BOUND).expect("the server's report");
    assert_eq!(header(&sent, "host"), Some("chosen.example"));
}

/// Rule 1: the `101` is recognised by its **status**.
///
/// hyper reports a finished ordinary exchange and a destroyed upgrade with
/// the same `Ready(Ok(()))`, so a transport that took the completion as
/// its signal would hand back a socket for any response at all. A `200`
/// must be [`NotSwitchingProtocols`], carrying the status the server
/// actually sent.
#[tokio::test]
async fn a_200_is_not_an_upgrade_and_says_which_status_it_was() {
    let addr = serve(|mut sock| {
        let _ = head(&mut sock);
        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let mut sink = [0u8; 64];
        while matches!(sock.read(&mut sink), Ok(n) if n > 0) {}
    });

    let outcome = tokio::time::timeout(BOUND, native().upgrade(open(&format!("http://{addr}/"))))
        .await
        .expect("must not hang");
    let err: Error = outcome.expect_err("a 200 is not an upgrade");
    let source = err
        .source()
        .and_then(|s| s.downcast_ref::<NotSwitchingProtocols>())
        .expect("the error must name the status, not just fail");
    assert_eq!(source.0, http::StatusCode::OK);
}

/// Rule 3: whatever the server sent in the same flight as the `101` comes
/// back from `finish`.
///
/// The fixture writes the `101` and the bytes in **one** `write_all` and
/// then never writes again, so a transport that dropped
/// `hyper::client::conn::http1::Parts::read_buf` has no second chance to
/// see them — and this test would be asserting on an empty buffer rather
/// than hanging, which is why it also holds the socket open: an empty
/// `read_buf` here means the bytes are gone for good.
#[tokio::test]
async fn the_bytes_that_arrived_with_the_101_are_handed_on() {
    let addr = serve(|mut sock| {
        let _ = head(&mut sock);
        let mut flight = BARE_101.to_vec();
        flight.extend_from_slice(b"in the same flight");
        let _ = sock.write_all(&flight);
        let mut sink = [0u8; 64];
        while matches!(sock.read(&mut sink), Ok(n) if n > 0) {}
    });

    let upgrading = tokio::time::timeout(BOUND, native().upgrade(open(&format!("http://{addr}/"))))
        .await
        .expect("must not hang")
        .expect("a 101 is an upgrade");
    let (_io, read_buf) = tokio::time::timeout(BOUND, upgrading.finish())
        .await
        .expect("must not hang")
        .expect("the connection must come apart");
    assert_eq!(&read_buf[..], b"in the same flight");
}
