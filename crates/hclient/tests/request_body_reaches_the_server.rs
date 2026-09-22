//! A request body must arrive at the server.
//!
//! Found from a consumer: an OCI registry rejected every blob upload with
//! `400`, its log naming the digest of *empty* content against the digest the
//! uploader declared.
//!
//! The bytes were reaching the wire; what was missing was `content-length`.
//! hyper writes it on the HTTP/1 path, h2 does not — it frames the body and
//! does not need it — and a server that sizes the body from the header then
//! reads nothing. `curl` fails the same way when the header is suppressed, so
//! this was never about this client.
//! **Native-only, and the line is needed for a reason no test run here
//! shows.** This stands up a real TCP server and drives it through
//! `hclient-native` over `hclient-rt-tokio`, none of which is built for
//! wasm — and `wasm-pack test`, which runs the browser suites, builds
//! **every** test target of the crate whatever it was told to run: the
//! command it issues is `cargo build --tests --test wasm_default`, where
//! `--tests` wins. So this file failed the browser job, and the failure
//! read as *browser tests failed* rather than as a file that was never
//! meant for that target. CI was red from 2026-09-15 for a week on it.
#![cfg(not(target_family = "wasm"))]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// A server that reads one request whole and records how many body bytes it
/// actually received.
fn echoing_server() -> (std::net::SocketAddr, Arc<Mutex<Vec<usize>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // Read until the headers are complete, then until Content-Length
            // bytes of body have arrived.
            let body_len = loop {
                let n = match s.read(&mut chunk) {
                    Ok(0) | Err(_) => break 0,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                let Some(end) = find_headers_end(&buf) else {
                    continue;
                };
                let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                let want: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if buf.len() - end >= want {
                    break buf.len() - end;
                }
            };
            recorder.lock().expect("seen").push(body_len);
            let _ = s.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n");
            let _ = s.flush();
        }
    });
    (addr, seen)
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// **The bytes handed to `body` are the bytes the server reads.**
///
/// Asserted on the server's side of the connection rather than on the request
/// value, because a body that is `Some` in Rust and absent on the wire is
/// exactly the failure this is for.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn a_full_request_body_arrives_whole() {
    rt().block_on(async { body_arrives().await });
}

async fn body_arrives() {
    let (addr, seen) = echoing_server();
    let payload = vec![b'x'; 10_000];

    let transport = hclient_native::Native::new(
        hclient_rt_tokio::Tokio,
        hclient_tls_rustls::Rustls::with_webpki_roots(),
        hclient_dns_system::SystemDns::new(hclient_rt_tokio::Tokio),
    );
    let client = hclient::Client::builder(transport).build().expect("client");

    let resp = client
        .put(format!("http://{addr}/upload"))
        .header("content-type", "application/octet-stream")
        .body(hclient::RequestBody::Full(payload.clone().into()))
        .send()
        .await
        .expect("send");
    assert!(resp.status().is_success(), "status {}", resp.status());

    let got = seen.lock().expect("seen").first().copied().unwrap_or(0);
    assert_eq!(
        got,
        payload.len(),
        "the server read {got} body bytes, not the {} that were sent",
        payload.len()
    );
}

/// **`content-length` is declared for a body of known size.**
///
/// **This test does not prove the fix.** It runs over HTTP/1.1 — what a raw
/// `TcpListener` speaks — where hyper writes the header itself, so it passes
/// with the h2 fix removed. It is kept because it pins the HTTP/1 half against
/// a future change that stops declaring the length there too.
///
/// The h2 half has no unit test here: this transport speaks h2 only over TLS
/// (no prior-knowledge h2c), so reproducing it needs a TLS server, which this
/// file deliberately does not stand up. It was verified against a live zot
/// registry instead — `400` without the header, `201` with it, same request,
/// same connection — and that is the evidence for the fix.
#[test]
fn a_sized_body_declares_its_length() {
    let (addr, seen) = header_recording_server();
    rt().block_on(async {
        let transport = hclient_native::Native::new(
            hclient_rt_tokio::Tokio,
            hclient_tls_rustls::Rustls::with_webpki_roots(),
            hclient_dns_system::SystemDns::new(hclient_rt_tokio::Tokio),
        );
        let client = hclient::Client::builder(transport).build().expect("client");
        let _ = client
            .put(format!("http://{addr}/upload"))
            .body(hclient::RequestBody::Full(vec![b'y'; 1234].into()))
            .send()
            .await;
    });
    let head = seen
        .lock()
        .expect("seen")
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(
        head.to_ascii_lowercase().contains("content-length: 1234"),
        "the request declared no content-length:\n{head}"
    );
}

/// Records the request head verbatim.
fn header_recording_server() -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            recorder
                .lock()
                .expect("seen")
                .push(String::from_utf8_lossy(&buf[..n]).into_owned());
            let _ = s.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n");
            let _ = s.flush();
        }
    });
    (addr, seen)
}
