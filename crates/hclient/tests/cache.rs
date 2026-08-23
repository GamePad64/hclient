//! The RFC 9111 cache wired into `Client`, watched from the server's side.
//!
//! `hclient-cache` is tested on its own — 62 tests over the policy, the
//! directives, the store and a date corpus — and none of that says whether
//! `Client` ever consults it, consults it at the right moment, or stores
//! what came back. So nothing here re-tests the policy. **Every assertion
//! below is a count of requests a real server received**, because a cache
//! that decides perfectly and is never asked passes all 62 of its own
//! tests and fails the first one here.
//!
//! The refusal is the exception and has to be: "a client-side cache
//! against a cache-owning backend is rejected at `build()`" is a fact
//! about a type that never sends anything.
#![cfg(all(feature = "cache", feature = "test-util", not(target_family = "wasm")))]

use hclient::Client;
use hclient::cache::HttpCache;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("tokio runtime")
}

fn transport() -> Native<Tokio, Rustls, SystemDns<Tokio>> {
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
}

/// A server that records every request head it was sent and answers by a
/// script over `(nth request, the head)`.
///
/// Keep-alive is handled rather than closing per response, for the reason
/// `cookies.rs` gives one file over: `Native::new` pools, and a server
/// that hung up each time would put a pooled-socket retry into tests that
/// are about caching.
fn recording_server(
    respond: impl Fn(usize, &str) -> String + Send + 'static,
) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            loop {
                let mut buf = Vec::new();
                let mut b = [0u8; 1024];
                let complete = loop {
                    match s.read(&mut b) {
                        Ok(0) | Err(_) => break false,
                        Ok(n) => {
                            buf.extend_from_slice(&b[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break true;
                            }
                        }
                    }
                };
                if !complete {
                    break;
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let nth = {
                    let mut g = log.lock().expect("log");
                    g.push(head.clone());
                    g.len()
                };
                if s.write_all(respond(nth, &head).as_bytes()).is_err() {
                    break;
                }
                let _ = s.flush();
            }
        }
    });
    (addr, seen)
}

fn client(addr: std::net::SocketAddr) -> (Client, String) {
    let c = Client::builder(transport())
        .cache(HttpCache::new())
        .build()
        .expect("build");
    (c, format!("http://127.0.0.1:{}", addr.port()))
}

fn body(head: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\n{head}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// **The headline: a fresh entry is served without a second request.**
///
/// Counted at the server, which is the only place the claim is visible —
/// the caller gets the same bytes either way, which is the point of a
/// cache and also why a caller-side assertion would prove nothing.
#[test]
fn a_fresh_response_is_served_from_the_store_without_a_second_request() {
    let (addr, seen) = recording_server(|_, _| body("Cache-Control: max-age=60\r\n", "first"));
    rt().block_on(async move {
        let (c, base) = client(addr);
        for _ in 0..3 {
            let text = c
                .get(&format!("{base}/x"))
                .send()
                .await
                .expect("send")
                .collect()
                .await
                .expect("collect")
                .text()
                .expect("text");
            assert_eq!(text, "first", "the stored body, byte for byte");
        }
        assert_eq!(
            seen.lock().expect("log").len(),
            1,
            "three calls, one request: the other two were served from the store"
        );
    });
}

/// **A stale entry is revalidated, and a `304` serves the stored body.**
///
/// Two claims the fresh case cannot make: that the second request goes out
/// at all, that it carries `If-None-Match` built from the stored `ETag`,
/// and that a `304` — which has no body — still yields the body.
#[test]
fn a_stale_entry_is_revalidated_and_a_304_serves_the_stored_body() {
    let (addr, seen) = recording_server(|nth, head| {
        if nth == 1 {
            return body("Cache-Control: max-age=0\r\nETag: \"v1\"\r\n", "stored");
        }
        assert!(
            head.to_ascii_lowercase().contains("if-none-match: \"v1\""),
            "the revalidation must carry the stored validator:\n{head}"
        );
        "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nCache-Control: max-age=60\r\n\r\n".into()
    });
    rt().block_on(async move {
        let (c, base) = client(addr);
        for _ in 0..2 {
            let text = c
                .get(&format!("{base}/x"))
                .send()
                .await
                .expect("send")
                .collect()
                .await
                .expect("collect")
                .text()
                .expect("text");
            assert_eq!(text, "stored", "a 304 has no body; this one is the store's");
        }
        assert_eq!(
            seen.lock().expect("log").len(),
            2,
            "stale, so it asked — and asked conditionally"
        );
    });
}

/// **`no-store` is not stored**, which is the directive whose whole
/// content is a refusal.
#[test]
fn no_store_means_every_call_reaches_the_server() {
    let (addr, seen) = recording_server(|_, _| body("Cache-Control: no-store\r\n", "fresh"));
    rt().block_on(async move {
        let (c, base) = client(addr);
        for _ in 0..3 {
            let _ = c.get(&format!("{base}/x")).send().await.expect("send");
        }
        assert_eq!(seen.lock().expect("log").len(), 3);
    });
}

/// **A client-side cache against a transport that owns one is refused at
/// `build()`**, and this is the arm `Capabilities::owns_cache`'s doc
/// comment has promised since v0.1 with nothing to point at.
///
/// The same shape as the cookie jar's refusal, one field over:
/// `hclient-fetch` is the backend that triggers it, because the browser
/// caches on its own and a second cache there would store what the first
/// already holds while answering from neither.
#[test]
fn a_cache_against_a_transport_that_owns_one_is_refused_at_build() {
    let mut caps = hclient_core::Capabilities::default();
    caps.owns_cache = true;
    let err = Client::builder(hclient::mock::MockTransport::new().with_capabilities(caps))
        .cache(HttpCache::new())
        .build()
        .expect_err("a client-side cache cannot be honoured here");
    // The field, not the rendered message: a refusal that named the wrong
    // setting would read plausibly and be wrong, which is the failure the
    // cookie jar's twin of this test exists to catch one field over.
    assert_eq!(
        err.what, "cache",
        "the refusal must name the setting: {err}"
    );

    // The control: the same transport, saying it owns none, builds.
    let ok = Client::builder(hclient::mock::MockTransport::new())
        .cache(HttpCache::new())
        .build();
    assert!(ok.is_ok(), "a backend that owns no cache is not refused");
}
