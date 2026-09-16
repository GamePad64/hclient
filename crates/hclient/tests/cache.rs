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
                .get(format!("{base}/x"))
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
                .get(format!("{base}/x"))
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
            let _ = c.get(format!("{base}/x")).send().await.expect("send");
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
    let mut caps = hclient_core::caps::Capabilities::default();
    caps.owns_cache = true;
    let err = Client::builder(hclient::mock::MockTransport::new().with_capabilities(caps))
        .cache(HttpCache::new())
        .build()
        .expect_err("a client-side cache cannot be honoured here");
    // The field, not the rendered message: a refusal that named the wrong
    // setting would read plausibly and be wrong, which is the failure the
    // cookie jar's twin of this test exists to catch one field over.
    assert_eq!(
        err.unsupported()
            .expect("a setting the transport cannot honour, not a coding token")
            .what,
        "cache",
        "the refusal must name the setting: {err}"
    );

    // The control: the same transport, saying it owns none, builds.
    let ok = Client::builder(hclient::mock::MockTransport::new())
        .cache(HttpCache::new())
        .build();
    assert!(ok.is_ok(), "a backend that owns no cache is not refused");
}

/// **A body of exactly `max_body_bytes` is stored, and the byte after it
/// is not.**
///
/// The limit is a ceiling, and the check that enforces it on the streaming
/// path — `Recorder::push`, on the running total as frames arrive — reads
/// `>`. A mutation run found it survivable as `>=`, which turns the
/// ceiling into a value the cache refuses: every response sized exactly to
/// the caller's limit stops being cached, silently and with no error
/// anywhere, because falling out of the cache is not a failure.
///
/// **Counted at the server, which is the only place it shows.** A caller
/// gets the same bytes whether the entry was stored or not — that is what
/// a cache is — so the assertion has to be the request count. The pair is
/// the test: at the limit the second call must not reach the server, one
/// byte over it must.
///
/// `cache_rfc9111.rs` cannot see this. Its limit test drives
/// `HttpCache::store` directly, which is the other half of the bound —
/// `storing` against a declared `Content-Length` — and never reaches
/// `Recorder::push` at all. The first version of this test was written
/// there and passed under the mutation.
#[test]
fn a_body_of_exactly_the_limit_is_cached_and_one_byte_more_is_not() {
    use hclient::cache::Limits;

    fn client_with_limit(addr: std::net::SocketAddr, max: u64) -> (Client, String) {
        let c = Client::builder(transport())
            .cache(HttpCache::new().with_limits(Limits {
                max_body_bytes: max,
            }))
            .build()
            .expect("build");
        (c, format!("http://127.0.0.1:{}", addr.port()))
    }

    // Five bytes into a limit of five: stored, so the second call is served
    // from the store and the server sees one request.
    let (addr, seen) = recording_server(|_, _| body("Cache-Control: max-age=60\r\n", "12345"));
    rt().block_on(async move {
        let (c, base) = client_with_limit(addr, 5);
        for _ in 0..2 {
            let text = c
                .get(format!("{base}/x"))
                .send()
                .await
                .expect("send")
                .collect()
                .await
                .expect("collect")
                .text()
                .expect("text");
            assert_eq!(text, "12345");
        }
        assert_eq!(
            seen.lock().expect("log").len(),
            1,
            "a body of exactly the limit must be cached"
        );
    });

    // Six bytes into the same limit: refused, so both calls go out. Without
    // this half the test above would pass for a cache with no limit at all.
    let (addr, seen) = recording_server(|_, _| body("Cache-Control: max-age=60\r\n", "123456"));
    rt().block_on(async move {
        let (c, base) = client_with_limit(addr, 5);
        for _ in 0..2 {
            let text = c
                .get(format!("{base}/x"))
                .send()
                .await
                .expect("send")
                .collect()
                .await
                .expect("collect")
                .text()
                .expect("text");
            assert_eq!(text, "123456", "the caller gets the body either way");
        }
        assert_eq!(
            seen.lock().expect("log").len(),
            2,
            "one byte over the limit must not be cached"
        );
    });
}

/// **A caller who sends one conditional header owns it, and the cache
/// stands aside — either header alone is enough.**
///
/// `If-None-Match` from a caller is a question addressed to the *origin*.
/// A cache that answered it from a stored copy would answer a different
/// one, and a cache that added its own validator on top would send two.
/// `Client::run` reads the two headers with `||` for exactly that reason.
///
/// A mutation run found the `||` survivable as `&&`, which makes the
/// deference conditional on the caller sending *both*: the common case —
/// one header, usually `If-None-Match` — is then overwritten by the
/// cache's own validator, and the `304` that comes back answers the
/// cache's question rather than the caller's. Every test in this file and
/// in `cache_rfc9111.rs` stayed green.
///
/// So each header is exercised **alone**, which is the shape the mutation
/// needs: with `&&` both rows fail, and with the header pair sent together
/// neither would. Watched from the server's side, because what must be
/// true is about the bytes that left — the caller's own validator arriving
/// and no second one beside it.
#[test]
fn one_conditional_header_from_the_caller_is_enough_to_stand_the_cache_aside() {
    for (name, value) in [
        ("if-none-match", "\"caller-etag\""),
        ("if-modified-since", "Thu, 01 Jan 2026 00:00:00 GMT"),
    ] {
        let (addr, seen) = recording_server(|_, _| {
            body("Cache-Control: max-age=60\r\nETag: \"ours\"\r\n", "stored")
        });
        rt().block_on(async move {
            let (c, base) = client(addr);

            // Populate the store, so there is something the cache could
            // have answered from.
            let _ = c
                .get(format!("{base}/x"))
                .send()
                .await
                .expect("send")
                .collect()
                .await
                .expect("collect");
            assert_eq!(seen.lock().expect("log").len(), 1);

            // Now the caller asks the origin itself, with one header.
            let _ = c
                .get(format!("{base}/x"))
                .header(name, value)
                .send()
                .await
                .expect("send")
                .collect()
                .await
                .expect("collect");

            let log = seen.lock().expect("log");
            assert_eq!(
                log.len(),
                2,
                "{name} alone must reach the origin rather than the store"
            );
            let sent = log[1].to_ascii_lowercase();
            assert!(
                sent.contains(&format!("{name}: {}", value.to_ascii_lowercase())),
                "the caller's own {name} must go out: {sent}"
            );
            // And not ours beside it: two validators are two questions.
            assert!(
                !sent.contains("\"ours\""),
                "the cache added its own validator on top: {sent}"
            );
        });
    }
}
