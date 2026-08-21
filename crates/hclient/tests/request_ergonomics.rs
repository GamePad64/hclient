//! `query`, `form`, `basic_auth`, `bearer_auth` — watched on the wire.
//!
//! Every one of these produces bytes, and the bytes are the whole of what
//! they are for: a query built with the wrong escape set reaches a form
//! parser as different data, and an `Authorization` header that never went
//! out is a request that quietly failed authentication somewhere else. So
//! the observer here is a server recording the request head and body, not
//! the builder's own view of itself.
#![cfg(all(feature = "test-util", not(target_family = "wasm")))]

use hclient::Client;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("tokio runtime")
}

/// Records whole requests — head and body alike, since `form` puts its
/// answer in the second.
fn recording_server() -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut buf = Vec::new();
            let mut b = [0u8; 1024];
            loop {
                match s.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&b[..n]);
                        // Head complete, and the body — if any — is
                        // whatever `Content-Length` said. These requests
                        // are small enough to arrive in one or two reads.
                        let text = String::from_utf8_lossy(&buf);
                        if let Some(h) = text.find("\r\n\r\n") {
                            let want: usize = text
                                .to_ascii_lowercase()
                                .split("content-length:")
                                .nth(1)
                                .and_then(|r| r.split("\r\n").next())
                                .and_then(|v| v.trim().parse().ok())
                                .unwrap_or(0);
                            if buf.len() >= h + 4 + want {
                                break;
                            }
                        }
                    }
                }
            }
            log.lock()
                .expect("log")
                .push(String::from_utf8_lossy(&buf).into_owned());
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = s.flush();
        }
    });
    (addr, seen)
}

fn client() -> Client {
    Client::builder(Native::new(
        Tokio,
        Rustls::with_webpki_roots(),
        SystemDns::new(Tokio),
    ))
    .build()
    .expect("build")
}

/// **A query already in the URL survives, and two calls are one query.**
///
/// The failure a replacing setter would cause is invisible from the call
/// site: the caller's own `?tenant=acme` would simply be gone.
#[test]
fn query_appends_and_never_replaces() {
    let (addr, seen) = recording_server();
    rt().block_on(async move {
        let c = client();
        let url = format!("http://127.0.0.1:{}/search?tenant=acme", addr.port());
        let _ = c
            .get(&url)
            .query([("q", "one two")])
            .query([("page", "2")])
            .send()
            .await
            .expect("send");
    });
    let head = seen.lock().expect("log")[0].clone();
    assert!(
        head.starts_with("GET /search?tenant=acme&q=one+two&page=2 HTTP/1.1\r\n"),
        "the caller's own query first, then each call in order: {:?}",
        head.lines().next()
    );
}

/// **The form serialiser, not RFC 3986's**, which is the difference that
/// silently corrupts data rather than failing: a `+` sent as itself reads
/// back as a space.
#[test]
fn a_form_body_uses_the_form_escape_set_and_declares_its_type() {
    let (addr, seen) = recording_server();
    rt().block_on(async move {
        let c = client();
        let url = format!("http://127.0.0.1:{}/submit", addr.port());
        let _ = c
            .post(&url)
            .form([("note", "a+b c"), ("é", "日")])
            .send()
            .await
            .expect("send");
    });
    let req = seen.lock().expect("log")[0].clone();
    assert!(
        req.to_ascii_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"),
        "declared: {req}"
    );
    assert!(
        req.ends_with("note=a%2Bb+c&%C3%A9=%E6%97%A5"),
        "the body is the form set, byte for byte: {req:?}"
    );
}

/// A caller who set `Content-Type` meant it — the same rule `Host:`
/// follows one layer down.
#[test]
fn form_leaves_a_caller_set_content_type_alone() {
    let (addr, seen) = recording_server();
    rt().block_on(async move {
        let c = client();
        let url = format!("http://127.0.0.1:{}/submit", addr.port());
        let _ = c
            .post(&url)
            .header("content-type", "application/vnd.example+form")
            .form([("a", "1")])
            .send()
            .await
            .expect("send");
    });
    let req = seen.lock().expect("log")[0].clone();
    assert!(req.contains("application/vnd.example+form"), "{req}");
    assert!(
        req.ends_with("a=1"),
        "and the body is still encoded: {req:?}"
    );
}

/// **Both schemes reach the wire**, asserted together because a builder
/// that produced one and dropped the other would pass either alone.
#[test]
fn basic_and_bearer_both_reach_the_wire() {
    let (addr, seen) = recording_server();
    rt().block_on(async move {
        let c = client();
        let url = format!("http://127.0.0.1:{}/x", addr.port());
        let _ = c
            .get(&url)
            .basic_auth("Aladdin", "open sesame")
            .send()
            .await
            .expect("send");
        let _ = c.get(&url).bearer_auth("t0k3n").send().await.expect("send");
    });
    let log = seen.lock().expect("log").clone();
    assert!(
        log[0].contains("authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==")
            || log[0].contains("Authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
        "{}",
        log[0]
    );
    assert!(
        log[1]
            .to_ascii_lowercase()
            .contains("authorization: bearer t0k3n"),
        "{}",
        log[1]
    );
}

/// **A colon in the username is refused rather than encoded**, because
/// RFC 7617 §2 makes it the separator: `("a:b", "")` and `("a", "b")`
/// would otherwise produce identical bytes and one of the two callers
/// would be silently wrong. The control is the pair above, which sends.
#[test]
fn a_colon_in_the_username_is_a_build_error_and_nothing_is_sent() {
    let (addr, seen) = recording_server();
    rt().block_on(async move {
        let c = client();
        let url = format!("http://127.0.0.1:{}/x", addr.port());
        let err = c
            .get(&url)
            .basic_auth("a:b", "p")
            .send()
            .await
            .expect_err("a username with a colon is not representable");
        assert!(
            std::error::Error::source(&err)
                .and_then(|s| s.downcast_ref::<hclient::error::ColonInUsername>())
                .is_some(),
            "and it must be the typed refusal: {err:?}"
        );
    });
    assert!(
        seen.lock().expect("log").is_empty(),
        "a build error must not reach the network"
    );
}

/// **The credential does not survive a `Debug`**, which is the one thing
/// `set_sensitive` does and the only place it can be seen — not the wire,
/// where the header goes out in full either way.
///
/// Written because the mutation that removes the marking survived
/// everything above: an observable property with no observer is a gap,
/// not a control.
#[test]
fn an_authorization_credential_is_not_printed_by_debug() {
    let (addr, _seen) = recording_server();
    rt().block_on(async move {
        let c = client();
        let url = format!("http://127.0.0.1:{}/x", addr.port());
        for (b, needle) in [
            (
                c.get(&url).basic_auth("Aladdin", "open sesame"),
                "QWxhZGRpbjpvcGVuIHNlc2FtZQ==",
            ),
            (c.get(&url).bearer_auth("t0k3n"), "t0k3n"),
        ] {
            let shown = format!("{b:?}");
            assert!(
                !shown.contains(needle),
                "the credential must not be printable: {shown}"
            );
        }
    });
}

/// **A JSON body reaches the wire with its header**, and the value that
/// cannot be serialised is the builder's error rather than a surprise
/// after a connection was opened.
///
/// The failing half uses a map with a non-string key, which is
/// `serde_json`'s own documented refusal — the smallest value that cannot
/// be JSON at all rather than a contrived one.
#[cfg(feature = "json")]
#[test]
fn a_json_body_reaches_the_wire_and_an_unserialisable_value_never_does() {
    let (addr, seen) = recording_server();
    rt().block_on(async move {
        let c = client();
        let url = format!("http://127.0.0.1:{}/x", addr.port());
        let _ = c
            .post(&url)
            .json(&serde_json::json!({"a": 1, "b": "two"}))
            .send()
            .await
            .expect("send");

        let bad: std::collections::BTreeMap<(i32, i32), i32> = [((1, 2), 3)].into_iter().collect();
        let err = c
            .post(&url)
            .json(&bad)
            .send()
            .await
            .expect_err("a map with a non-string key is not JSON");
        assert_eq!(*err.kind(), hclient_core::ErrorKind::Other, "{err:?}");
    });
    let log = seen.lock().expect("log").clone();
    assert_eq!(log.len(), 1, "only the serialisable one was sent");
    assert!(
        log[0]
            .to_ascii_lowercase()
            .contains("content-type: application/json"),
        "{}",
        log[0]
    );
    assert!(
        log[0].ends_with(r#"{"a":1,"b":"two"}"#),
        "the body is the serialisation, byte for byte: {:?}",
        log[0]
    );
}
