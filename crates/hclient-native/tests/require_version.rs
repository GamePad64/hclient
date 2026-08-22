//! `RequireVersion` on `hclient-native`, watched from the server's side of
//! the socket.
//!
//! # The assertion that matters is a negative one
//!
//! A demand the connection cannot meet must fail **before the head is
//! written**, and "before" is not a thing a client-side error can
//! demonstrate: an implementation that wrote the request, read nothing and
//! then noticed the version would produce the same `Err` to the caller.
//! So every refusal test here holds a real listener and asserts the server
//! read **zero bytes**.
//!
//! # Zero bytes, established causally rather than by waiting
//!
//! The fixture's reader loop ends on **EOF**, not on a timer: the client
//! drops the connection when it refuses, the socket closes, `read` returns
//! `Ok(0)`, and the accumulated bytes go down the channel. So the test
//! observes an event caused by the refusal rather than an absence measured
//! against a clock. Three timing-based assertions in this workspace turned
//! out to be flakes and one of them was hiding a real defect; a
//! `recv_timeout` that concluded "nothing arrived" from a quiet 200 ms
//! would be a fourth.
//!
//! The channel still carries a `BOUND` so a hang is a failure rather than
//! a hung test binary — but it is the ceiling on a green run, never the
//! thing being measured.
//!
//! # Plaintext, and where the HTTP/2 half lives
//!
//! `http://` means no ALPN, so `negotiated_protocol` answers `Http11` with
//! certainty and these tests measure the demand rather than a negotiation.
//! That covers three of the four cases — a demand met, a demand missed, no
//! demand at all — on a build with or without the `http2` feature.
//!
//! The fourth needs a TLS backend that reports an ALPN, and those tests are
//! in `tests/http2.rs` next to the `FakeTls` stub that exists for exactly
//! that (`a_demand_for_http2_is_served_by_a_connection_that_negotiated_it`
//! and `a_demand_for_http1_takes_h2_off_the_alpn_offer`), rather than
//! copied here with a second copy of the stub.

use hclient::error::VersionNotAvailable;
use hclient::{Client, RequireVersion};
use hclient_core::ErrorKind;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// Ceiling for anything that must not hang. Never the subject of an
/// assertion — see the module doc.
const BOUND: Duration = Duration::from_secs(30);

struct Fixture {
    addr: std::net::SocketAddr,
    /// One entry per accepted connection: everything that arrived on it
    /// before EOF.
    got: mpsc::Receiver<Vec<u8>>,
    /// TCP connections accepted. A refusal is expected to cost one of
    /// these and no bytes, and telling those two apart is the point.
    accepted: Arc<AtomicUsize>,
}

impl Fixture {
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

/// An HTTP/1.1 server that answers `200 ok` to anything that looks like a
/// request, and reports what it received on every connection — including
/// the connections on which it received nothing.
///
/// Reads until EOF rather than until a complete request, because the
/// interesting case is the one where no request ever comes: a loop that
/// waited for `\r\n\r\n` would hang exactly where this test needs an
/// answer.
///
/// # It is keep-alive, and the first version of it was not
///
/// This fixture answered and then `break`, closing the socket. Every test
/// here still passed — including
/// `a_pooled_connection_of_the_wrong_version_is_skipped_not_used`, whose
/// whole subject is a connection in the pool. It passed because there
/// never was one: the client's second request opened a fresh connection
/// either way, so `accepted == 2` held whether or not the pool filter
/// existed, and the mutation that deleted that filter survived.
///
/// The comment beside the `break` said "keep reading so the loop still
/// ends on the client's own close", describing something the code did not
/// do. That is the defect this project keeps finding, arriving in a test
/// rather than in a transport, and it was caught by running the mutation
/// rather than by reading the fixture.
///
/// So: no `break` after a response, `Content-Length` framing and no
/// `Connection: close`, and exactly one report per connection — the first
/// request if one arrived, otherwise what was there at EOF.
fn spawn_server() -> Fixture {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, got) = mpsc::channel();
    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_thread = Arc::clone(&accepted);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            accepted_thread.fetch_add(1, Ordering::SeqCst);
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut acc = Vec::new();
                let mut reported = false;
                let mut b = [0u8; 4096];
                while let Ok(n) = s.read(&mut b) {
                    if n == 0 {
                        break;
                    }
                    acc.extend_from_slice(&b[..n]);
                    // A head is a complete request here; nothing below
                    // sends a body.
                    if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                        // `Content-Length`-framed and no `Connection:
                        // close`, so the connection stays usable — and
                        // **the loop does not break**, which is the whole
                        // difference between a fixture the pool can reuse
                        // and one it cannot. See the note above.
                        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
                        if !reported {
                            let _ = tx.send(std::mem::take(&mut acc));
                            reported = true;
                        }
                        acc.clear();
                    }
                }
                // Exactly one report per connection: the first request if
                // one arrived, otherwise whatever was accumulated when the
                // client closed — which for a refusal is nothing, and is
                // the assertion.
                if !reported {
                    let _ = tx.send(acc);
                }
            });
        }
    });
    Fixture {
        addr,
        got,
        accepted,
    }
}

fn client() -> Client {
    Client::builder(Native::new(
        Tokio,
        Rustls::with_webpki_roots(),
        SystemDns::new(Tokio),
    ))
    .build()
    .unwrap()
}

/// A `GET` carrying a demand.
///
/// Built as an `http::Request` and sent through `Client::execute` rather
/// than through `RequestBuilder`, because `RequestBuilder` has no
/// extension setter — the same route `tests/too_early.rs` takes to put an
/// `AllowEarlyData` on a request, and the same one a caller has today.
fn demanding(url: &str, v: http::Version) -> http::Request<hclient_core::RequestBody> {
    let mut req = http::Request::builder()
        .method("GET")
        .uri(url)
        .body(hclient_core::RequestBody::Empty)
        .unwrap();
    req.extensions_mut().insert(RequireVersion(v));
    req
}

/// The same, with no demand: the control's control.
fn plain(url: &str) -> http::Request<hclient_core::RequestBody> {
    http::Request::builder()
        .method("GET")
        .uri(url)
        .body(hclient_core::RequestBody::Empty)
        .unwrap()
}

/// Asserts an error is the typed refusal, and returns its two versions.
fn refusal(e: &hclient::Error) -> VersionNotAvailable {
    assert_eq!(
        *e.kind(),
        ErrorKind::Unsupported,
        "a demand this connection cannot meet is `Unsupported`, not a \
         connect or body failure: {e}"
    );
    *std::error::Error::source(e)
        .and_then(|s| s.downcast_ref::<VersionNotAvailable>())
        .unwrap_or_else(|| panic!("expected a typed VersionNotAvailable source, got: {e}"))
}

/// **The acceptance.** A caller demands HTTP/2 from a plaintext origin,
/// which can only ever be HTTP/1.1, and the server gets a connection and
/// **not one byte**.
///
/// The zero is the whole test. A check placed after
/// `established::exchange` — or no check at all, with the mismatch noticed
/// by the caller from `Response::version()` — produces the same `Err` for
/// the same request while the request itself has already been served.
#[tokio::test]
async fn a_demand_for_http2_is_refused_before_a_single_byte_is_written() {
    let server = spawn_server();
    let err = tokio::time::timeout(
        BOUND,
        client().execute(demanding(&server.url("/needs-h2"), http::Version::HTTP_2)),
    )
    .await
    .expect("must not hang")
    .expect_err("HTTP/2 is not reachable over plaintext, so the demand must fail");

    let named = refusal(&err);
    assert_eq!(named.required, http::Version::HTTP_2);
    assert_eq!(
        named.negotiated,
        http::Version::HTTP_11,
        "the refusal must name what this connection actually is, or a \
         caller cannot tell a plaintext origin from a server that declined h2"
    );

    let seen = server.got.recv_timeout(BOUND).expect(
        "the server must have accepted a connection and reached EOF on it — \
         if this times out the client is holding the socket open, which is a \
         different bug from the one under test",
    );
    assert!(
        seen.is_empty(),
        "the refusal must come before the head. The server received {} bytes: {:?}",
        seen.len(),
        String::from_utf8_lossy(&seen)
    );
    assert_eq!(
        server.accepted.load(Ordering::SeqCst),
        1,
        "one connection, refused — not zero (which would mean the demand was \
         answered before connecting and this test proves nothing about \
         ordering) and not two"
    );
}

/// The other half of the pair, and it is not decoration: a check that
/// refused everything would pass the test above. A demand this connection
/// **does** meet must be served, all the way to a response.
#[tokio::test]
async fn a_demand_the_connection_meets_is_served_normally() {
    let server = spawn_server();
    let resp = tokio::time::timeout(
        BOUND,
        client().execute(demanding(&server.url("/needs-h1"), http::Version::HTTP_11)),
    )
    .await
    .expect("must not hang")
    .expect("HTTP/1.1 is exactly what a plaintext connection speaks");
    assert_eq!(resp.status(), 200);

    let seen = server
        .got
        .recv_timeout(BOUND)
        .expect("server saw the connection");
    let text = String::from_utf8_lossy(&seen);
    assert!(
        text.starts_with("GET /needs-h1 HTTP/1.1"),
        "a satisfied demand must change nothing about what goes out: {text:?}"
    );
}

/// The control, and the one that stops the demand from becoming a cost
/// everyone pays. An unmarked request is untouched — same request line,
/// same response, no refusal on any path.
#[tokio::test]
async fn an_unmarked_request_is_unaffected() {
    let server = spawn_server();
    let resp = tokio::time::timeout(BOUND, client().execute(plain(&server.url("/plain"))))
        .await
        .expect("must not hang")
        .expect("no demand, no new failure mode");
    assert_eq!(resp.status(), 200);

    let seen = server
        .got
        .recv_timeout(BOUND)
        .expect("server saw the connection");
    assert!(
        String::from_utf8_lossy(&seen).starts_with("GET /plain HTTP/1.1"),
        "{:?}",
        String::from_utf8_lossy(&seen)
    );
}

/// **The pooled path, which is the one a missing check actually leaks
/// through.** On a fresh connection the transport at least has to decide
/// something; on a pooled one there is a live socket sitting under a key
/// that already says which protocol it speaks, and `established::exchange`
/// will write the head onto it without asking.
///
/// So: one ordinary request first, to leave a live HTTP/1.1 connection in
/// the pool (`Native::new` pools by default), then a second request
/// demanding HTTP/2. The pooled connection must be **skipped rather than
/// used**, and the second connection the client then opens must carry no
/// bytes either.
///
/// Two servers' worth of evidence in one: `accepted == 2` says the pooled
/// entry was not reused, and the second capture being empty says nothing
/// was written on the replacement.
#[tokio::test]
async fn a_pooled_connection_of_the_wrong_version_is_skipped_not_used() {
    let server = spawn_server();
    let c = client();

    let first = tokio::time::timeout(BOUND, c.execute(plain(&server.url("/warm"))))
        .await
        .expect("must not hang")
        .expect("an ordinary request, to put a connection in the pool");
    assert_eq!(first.status(), 200);
    // Drain the body so the connection is checked back in rather than
    // dropped — a pool that never received the connection would make the
    // rest of this test vacuous.
    assert_eq!(
        http_body_util::BodyExt::collect(first.into_body())
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        b"ok"
    );

    let err = tokio::time::timeout(
        BOUND,
        c.execute(demanding(&server.url("/needs-h2"), http::Version::HTTP_2)),
    )
    .await
    .expect("must not hang")
    .expect_err("still plaintext, still HTTP/1.1");
    assert_eq!(refusal(&err).required, http::Version::HTTP_2);

    let first_seen = server.got.recv_timeout(BOUND).expect("first connection");
    assert!(
        String::from_utf8_lossy(&first_seen).starts_with("GET /warm HTTP/1.1"),
        "{:?}",
        String::from_utf8_lossy(&first_seen)
    );
    let second_seen = server.got.recv_timeout(BOUND).expect(
        "a second connection must have been opened and closed — if the pooled \
         one had been reused there would be no second capture at all",
    );
    assert!(
        second_seen.is_empty(),
        "the demand must be answered before the head on the pooled path too. \
         Got: {:?}",
        String::from_utf8_lossy(&second_seen)
    );
    assert_eq!(
        server.accepted.load(Ordering::SeqCst),
        2,
        "the pooled HTTP/1.1 connection must be skipped, not served from — a \
         reuse would leave this at 1 and put `/needs-h2` on the wire"
    );
}
