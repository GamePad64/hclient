//! `RequireVersion` on a transport that speaks exactly one version
//! (v0.4 W2, `docs/v04-design.md` Appendix A).
//!
//! # The interesting question here is not "does it refuse"
//!
//! It is *which* demands it refuses. `hclient-h3` always speaks HTTP/3, so
//! there are two answers to get right and they pull in opposite
//! directions:
//!
//! - `RequireVersion(HTTP_3)` is satisfied **by construction** and must go
//!   through. This is why the crate reports `version_select: true`: with
//!   `false`, `Client`'s gate would refuse the one demand this transport
//!   meets without doing anything at all.
//! - Anything else can never be met, and must fail with the same typed
//!   `VersionNotAvailable` `hclient-native` raises, so a caller who swapped
//!   backends reads one type rather than two.
//!
//! # How "before the head" is established, and it is not by a clock
//!
//! `a_demand_this_transport_cannot_meet_is_refused_before_the_network` aims
//! at a **host `IpLiteralOnly` cannot resolve**. If the check sat anywhere
//! after connecting, that request could not produce `VersionNotAvailable`
//! at all — it would produce a resolver failure, because resolution is the
//! first thing `execute` does after the scheme check. So the *identity of
//! the error* is the witness, and it is causal: there is no ordering of
//! events in which a late check returns this error.
//!
//! The live-server test then adds the other half against a real QUIC
//! endpoint: `requests() == 0` and `accepted() == 0`, so the refusal cost
//! the server neither a stream nor a connection.
#![cfg(not(target_family = "wasm"))]

mod server;

use hclient_core::unversioned::Transport;
use hclient_core::{ErrorKind, RequestBody, RequireVersion, VersionNotAvailable};
use hclient_dns::IpLiteralOnly;
use hclient_h3::H3;
use hclient_rt_tokio::TokioHandle;
use server::Behaviour;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

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

fn get(url: &str, demand: Option<http::Version>) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .method("GET")
        .uri(url)
        .body(RequestBody::Empty)
        .unwrap();
    if let Some(v) = demand {
        req.extensions_mut().insert(RequireVersion(v));
    }
    req
}

fn refusal(e: &hclient_core::Error) -> VersionNotAvailable {
    assert_eq!(
        *e.kind(),
        ErrorKind::Unsupported,
        "the refusal is `Unsupported`, matching `hclient-native`'s: {e}"
    );
    *std::error::Error::source(e)
        .and_then(|s| s.downcast_ref::<VersionNotAvailable>())
        .unwrap_or_else(|| panic!("expected a typed VersionNotAvailable, got: {e}"))
}

/// **The refusal, positioned by the identity of the error rather than by a
/// clock.** `IpLiteralOnly` cannot resolve a name, so this URL's only other
/// possible outcome is a resolver failure — which is what a check placed
/// after `resolve` would necessarily produce.
#[tokio::test]
async fn a_demand_this_transport_cannot_meet_is_refused_before_the_network() {
    let t = h3(&server::identity().cert_der);
    let err = tokio::time::timeout(
        BOUND,
        t.execute(get(
            "https://a-name-no-resolver-here-can-answer.invalid/x",
            Some(http::Version::HTTP_2),
        )),
    )
    .await
    .expect("must not hang")
    .expect_err("HTTP/2 is not something this transport can ever speak");

    let named = refusal(&err);
    assert_eq!(named.required, http::Version::HTTP_2);
    assert_eq!(
        named.negotiated,
        http::Version::HTTP_3,
        "the refusal must name what this transport does speak"
    );
}

/// The same, against a real QUIC server, so the negative is observed from
/// the far end as well: a refused demand costs the server neither a
/// connection nor a stream.
#[tokio::test]
async fn a_refused_demand_reaches_the_server_as_nothing_at_all() {
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    let err = tokio::time::timeout(
        BOUND,
        t.execute(get(
            &format!("https://{}/x", s.addr),
            Some(http::Version::HTTP_11),
        )),
    )
    .await
    .expect("must not hang")
    .expect_err("HTTP/1.1 has no QUIC form");
    assert_eq!(refusal(&err).required, http::Version::HTTP_11);

    assert_eq!(s.requests(), 0, "no stream was opened");
    assert_eq!(
        s.accepted(),
        0,
        "and no connection either — this transport answers the demand before \
         it resolves anything, so nothing was risked at all"
    );
}

/// **The arm the refusal must not take.** A demand for HTTP/3 against a
/// transport that speaks HTTP/3 is served, end to end, over real QUIC.
///
/// Without this, "refuse every marked request" would pass both tests above
/// and would be exactly the wrong implementation — the one that makes
/// `version_select: true` a lie.
#[tokio::test]
async fn a_demand_for_http3_is_served() {
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    let resp = tokio::time::timeout(
        BOUND,
        t.execute(get(
            &format!("https://{}/x", s.addr),
            Some(http::Version::HTTP_3),
        )),
    )
    .await
    .expect("must not hang")
    .expect("HTTP/3 is what this transport speaks; the demand must be met");

    assert_eq!(resp.status(), 200);
    assert_eq!(s.requests(), 1, "the request really did go out");
}

/// The control: an unmarked request over the same transport and server is
/// untouched, so nothing above is describing a transport that broke.
#[tokio::test]
async fn an_unmarked_request_is_unaffected() {
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    let resp = tokio::time::timeout(
        BOUND,
        t.execute(get(&format!("https://{}/x", s.addr), None)),
    )
    .await
    .expect("must not hang")
    .expect("no demand, no new failure mode");
    assert_eq!(resp.status(), 200);
    assert_eq!(s.requests(), 1);
}

/// **Through a real `Client`, which is where declaring `false` would
/// actually cost something.**
///
/// Everything above calls `Transport::execute` directly, so `Client`'s
/// `UnsupportedCapability` gate never runs and a wrong `version_select`
/// would go unwitnessed by any of them. Measured: flipping this crate's
/// declaration to `false` kills only the read-back below — a test that
/// asserts the field and nothing about what it does.
///
/// This one is the behaviour. With `version_select: false` the gate in
/// `Client::run` refuses `RequireVersion(HTTP_3)` before the transport is
/// asked anything, and a caller who wanted HTTP/3 from the HTTP/3
/// transport gets `UnsupportedCapability`. That is the exact sentence the
/// crate's doc comments make, so it needs a test rather than a claim.
#[tokio::test]
async fn a_client_over_this_transport_does_not_refuse_a_demand_for_http3() {
    let s = server::start(Behaviour::Echo);
    let c = hclient::Client::builder(h3(&s.cert_der))
        .build()
        .expect("client");

    let resp = tokio::time::timeout(
        BOUND,
        c.execute(get(
            &format!("https://{}/x", s.addr),
            Some(http::Version::HTTP_3),
        )),
    )
    .await
    .expect("must not hang")
    .expect("the gate must let through the one demand this backend meets");

    assert_eq!(resp.status(), 200);
    assert_eq!(s.requests(), 1, "the request reached the server");
}

/// The declaration, next to the behaviour that earns it — v0.2 W4's rule.
///
/// `true` here is not a claim to *choose* a version. It is the claim that
/// a demand is read and answered, which the four tests above are the
/// evidence for. See `Capabilities::version_select`'s doc in
/// `hclient-core` for why a one-version transport reporting `false` would
/// be the under-claim rather than the safe answer.
#[tokio::test]
async fn version_select_is_declared_and_the_tests_above_are_why() {
    let t = h3(&server::identity().cert_der);
    let c = t.capabilities();
    assert!(c.version_select);
    assert!(
        c.version_reported,
        "unchanged — a caller can still read the version off the response"
    );
}
