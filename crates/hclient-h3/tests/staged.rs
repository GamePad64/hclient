//! The staged connect over real QUIC, watched from the server's side.
//!
//! Three claims, and the difference from `hclient-native`'s file of the
//! same name is the third:
//!
//! - a staged connect completes a QUIC handshake and h3's SETTINGS and the
//!   origin hears **no request**;
//! - a staged connect **shares** the pooled connection rather than dialling
//!   a second one, which on this stack is what "found one" means — the
//!   `SendRequest` is cloned and the connection multiplexes;
//! - **a handle nobody spends needs no `Drop`.** `hclient_native::Staged`
//!   had to be given one, because it *owns* the connection it took out of
//!   the pool. Here `checkout` inserted the connection into the pool before
//!   its caller ever saw it (`docs/v04-w2-webtransport.md` §4b's first
//!   reason, from the other side), so a dropped handle leaves the pool
//!   exactly as it found it. The test is the same test — connect, drop,
//!   send — and the two crates reaching the same observable answer by
//!   different means is the point.
#![cfg(not(target_family = "wasm"))]

mod server;

use hclient_core::unversioned::Transport;
use hclient_core::{ErrorKind, RequestBody};
use hclient_dns::IpLiteralOnly;
use hclient_h3::{H3, StagedConnect};
use hclient_rt_tokio::TokioHandle;
use http_body_util::BodyExt;
use server::Behaviour;
use std::time::Duration;

/// Never an assertion — it turns a mutation that hangs into a red test
/// rather than an eternal one.
const BOUND: Duration = Duration::from_secs(10);

fn h3(
    cert: &rustls::pki_types::CertificateDer<'static>,
) -> H3<TokioHandle, hclient_tls_rustls::Rustls, IpLiteralOnly> {
    H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(cert),
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O")
}

fn get(addr: std::net::SocketAddr) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(format!("https://{addr}/hello"))
        .body(RequestBody::Empty)
        .expect("a well-formed request")
}

async fn body_of<B>(r: http::Response<B>) -> String
where
    B: http_body::Body<Data = bytes::Bytes>,
    B::Error: std::fmt::Debug,
{
    assert_eq!(r.status(), 200);
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Waits, bounded, for the server to have accepted `n` connections. The
/// bound is a failure rather than a `return`: a test whose premise never
/// arrived has not passed.
async fn accepted(s: &server::Server, n: usize) {
    tokio::time::timeout(BOUND, async {
        while s.accepted() < n {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the server never accepted {n} connection(s)"));
}

// --- the deliverable ----------------------------------------------------

/// **The whole point.** The handshake reaches the origin and the request
/// does not, until the handle is spent.
#[tokio::test(flavor = "multi_thread")]
async fn a_staged_connect_reaches_the_origin_and_sends_no_request() {
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    let staged = t.connect(get(s.addr)).await.expect("the server is up");
    accepted(&s, 1).await;
    assert_eq!(
        s.requests(),
        0,
        "a staged connect must not open a request stream"
    );

    let resp = t.exchange(staged).await.expect("the exchange succeeds");
    assert_eq!(body_of(resp).await, "hello over h3");
    assert_eq!(
        (s.accepted(), s.requests()),
        (1, 1),
        "one connection, one request, and the request came from the exchange"
    );
}

/// A staged connect at an origin this transport is already speaking to
/// costs no connection — `docs/connect-only-seam.md` §7's *"it must be
/// allowed to answer: I already had one"*, in the form this stack offers
/// it: the `SendRequest` is cloned and the QUIC connection multiplexes.
#[tokio::test(flavor = "multi_thread")]
async fn a_staged_connect_shares_the_pooled_connection() {
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    let resp = t.execute(get(s.addr)).await.expect("request 1");
    assert_eq!(body_of(resp).await, "hello over h3");

    let staged = t.connect(get(s.addr)).await.expect("request 2 connects");
    let resp = t.exchange(staged).await.expect("request 2 exchanges");
    assert_eq!(body_of(resp).await, "hello over h3");

    assert_eq!(
        (s.accepted(), s.requests()),
        (1, 2),
        "the staged connect must have shared the pooled connection"
    );
}

/// The answer to `docs/connect-only-seam.md` §9's open question about a
/// connection whose caller declined it — and it is a different mechanism
/// from `hclient-native`'s with the same observable end.
///
/// There is no `Drop` on this crate's handle: `checkout` had already put
/// the connection in the pool, and the spawned driver keeps it alive across
/// the gap. The cost is the other half of that sentence and is not free —
/// a declined connection goes on sending a `DEFAULT_KEEP_ALIVE` PING every
/// five seconds for as long as the transport lives.
#[tokio::test(flavor = "multi_thread")]
async fn a_handle_nobody_spends_leaves_the_connection_in_the_pool() {
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    let staged = t.connect(get(s.addr)).await.expect("the server is up");
    accepted(&s, 1).await;
    drop(staged);

    let resp = t.execute(get(s.addr)).await.expect("the next request");
    assert_eq!(body_of(resp).await, "hello over h3");
    assert_eq!(
        (s.accepted(), s.requests()),
        (1, 1),
        "the declined connection must be the one the next request used"
    );
}

/// A connect that is refused before a packet leaves hands the request back
/// whole, which is the property the fallback in `hclient-select` is built
/// on: nothing was sent, so sending it elsewhere is not a retry.
///
/// `http://` is the arm chosen because it is refused **causally** — in
/// `admit`, before resolution and before a datagram — where the failure
/// this seam exists for (a black hole) costs quinn's 30 s `max_idle_timeout`
/// and would make this a clock-driven test. What is under test here is the
/// hand-back, not the failure.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_connect_hands_the_request_back() {
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    let req = http::Request::builder()
        .uri(format!("http://{}/hello", s.addr))
        .header("x-mine", "1")
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    let refused = t
        .connect(req)
        .await
        .map(|_| ())
        .expect_err("HTTP/3 has no plaintext form");
    assert_eq!(*refused.error().kind(), ErrorKind::Connect);

    let (_, request) = refused.into_parts();
    assert_eq!(request.uri().scheme_str(), Some("http"));
    assert_eq!(request.headers()["x-mine"], "1");
    assert_eq!(
        s.accepted(),
        0,
        "the refusal must have cost the origin nothing"
    );
}

/// A version demand this transport cannot meet is refused in the same
/// place, and hands the request back the same way — so a caller staging a
/// connect on the QUIC arm of a pair can route a `RequireVersion(HTTP_2)`
/// request onward without having lost it.
#[tokio::test(flavor = "multi_thread")]
async fn a_version_demand_this_stack_cannot_meet_hands_the_request_back() {
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    let mut req = get(s.addr);
    req.extensions_mut()
        .insert(hclient_core::RequireVersion(http::Version::HTTP_2));
    let refused = t
        .connect(req)
        .await
        .map(|_| ())
        .expect_err("this transport speaks HTTP/3 alone");

    let (_, request) = refused.into_parts();
    assert!(
        request
            .extensions()
            .get::<hclient_core::RequireVersion>()
            .is_some(),
        "the request comes back as it went in, demand included"
    );
    assert_eq!(
        s.accepted(),
        0,
        "the demand is answered before a QUIC packet"
    );
}
