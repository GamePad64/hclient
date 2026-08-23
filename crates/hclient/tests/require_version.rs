//! The gate: **which** transports may be handed a `RequireVersion` demand
//! at all.
//!
//! `hclient-native/tests/require_version.rs` and its `tests/http2.rs`
//! siblings check what a transport that *honours* demands does with one.
//! This file is the other half: a demand against a backend that cannot
//! honour one must be a typed `UnsupportedCapability`, not a mark quietly
//! going unread — the same arm a `RedirectPolicy` against
//! `RedirectSupport::Internal` takes, and the same one this project has
//! now used four times.
//!
//! # The pair, and why neither test means anything alone
//!
//! `version_select` is a `bool`, so a gate that fired on every marked
//! request and a gate that fired on none are each one character away from
//! the right one. Both directions are asserted below against the *same*
//! mock differing in that one field, so a mutation to the condition has
//! nowhere to hide.
//!
//! # And why it is checked per request rather than at `build()`
//!
//! Unlike a cookie jar or a client-level redirect policy, there is no
//! client-level version demand to check at construction — turning an ALPN
//! outcome into a request failure is right for a gRPC call and wrong for a
//! browser-shaped fetch, and only the caller of one request knows which it
//! is (`RequireVersion`'s own doc). So the check is in `Client::run`, and
//! `a_client_over_a_backend_that_cannot_select_still_builds` records that
//! `build()` is deliberately not where it lives: a client on
//! `hclient-fetch` must go on working for every caller who never mentions
//! a version.
//!
//! No target gate: everything here is `hclient-mock` and no socket, so it
//! builds and runs everywhere the crate does, including the browser job.
#![cfg(feature = "test-util")]

use hclient::caps::Capabilities;
use hclient::error::UnsupportedCapability;
use hclient::mock::MockTransport;
use hclient::{Client, ErrorKind, RequestBody, RequireVersion};

/// A backend differing from `Capabilities::default()` in exactly one field.
fn caps(version_select: bool) -> Capabilities {
    let mut c = Capabilities::default();
    c.version_select = version_select;
    c
}

fn ok() -> http::Response<Vec<bytes::Bytes>> {
    http::Response::builder().status(200).body(vec![]).unwrap()
}

fn demanding(v: http::Version) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .method("GET")
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    req.extensions_mut().insert(RequireVersion(v));
    req
}

fn send(c: &Client, req: http::Request<RequestBody>) -> Result<u16, hclient::Error> {
    futures_executor::block_on(c.execute(req)).map(|r| r.status().as_u16())
}

/// A demand against a backend that reports `version_select: false` is
/// refused — and refused **without reaching the transport**, which is the
/// half that makes it a gate rather than a nicety.
///
/// `hclient-fetch` and `hclient-wasi` are the real subjects: neither
/// selects the protocol version nor learns it, so neither has any moment
/// at which it could compare a demand against anything. Silently ignoring
/// the mark would hand a caller who requires HTTP/2 whatever the browser
/// or the host happened to use, and call it success.
#[test]
fn a_demand_against_a_backend_that_cannot_honour_one_is_unsupported() {
    let m = MockTransport::new().with_capabilities(caps(false));
    let c = Client::builder(m)
        .build()
        .expect("the client itself builds");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(ok());

    let err = send(&c, demanding(http::Version::HTTP_2)).expect_err("the demand must be refused");
    assert_eq!(*err.kind(), ErrorKind::Unsupported);
    let named = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<UnsupportedCapability>())
        .expect("the same typed refusal the other three capability gates raise");
    assert_eq!(named.what, "require_version");

    assert!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .is_empty(),
        "the refusal must come before the transport is asked to do anything — \
         a backend that cannot answer the demand must not be given a request \
         that carries one"
    );
}

/// The other direction, on the same mock with the one field flipped: a
/// backend that reports it honours demands gets the request, mark and all.
///
/// This is the test the "refuse every marked request" mutation dies on,
/// and it also pins that the mark **travels** — `Client` does not consume
/// it on the way past. A transport that never receives the demand cannot
/// enforce it.
#[test]
fn a_demand_against_a_backend_that_honours_one_reaches_it_unchanged() {
    let m = MockTransport::new().with_capabilities(caps(true));
    let c = Client::builder(m).build().expect("client");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(ok());

    assert_eq!(
        send(&c, demanding(http::Version::HTTP_2)).expect("no gate to trip"),
        200
    );

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].extensions.get::<RequireVersion>().copied(),
        Some(RequireVersion(http::Version::HTTP_2)),
        "the mark must arrive at the transport intact — the version included, \
         since the transport is what compares it"
    );
}

/// An **unmarked** request against the very backend that cannot honour a
/// demand is untouched. The gate reads the mark, not the capability alone.
///
/// Without this, a gate written as `if !caps.version_select` — no mark in
/// the condition at all — would pass both tests above and break every
/// browser client ever built. That is not a hypothetical mistake: it is
/// exactly the one `check_redirect_supported` documents at length, where
/// `RedirectPolicy::default()` made "asked for ten hops" and "never
/// mentioned redirects" indistinguishable.
#[test]
fn an_unmarked_request_is_unaffected_by_a_backend_that_cannot_select() {
    let m = MockTransport::new().with_capabilities(caps(false));
    let c = Client::builder(m).build().expect("client");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(ok());

    let req = http::Request::builder()
        .method("GET")
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    assert_eq!(send(&c, req).expect("no demand, no gate"), 200);
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1
    );
}

/// **The demand crosses an origin, where `AllowEarlyData` does not** — and
/// the asymmetry is a decision, not an oversight in `next_hop`'s strip
/// list.
///
/// `AllowEarlyData` says "replaying this is safe", which is a claim about
/// what a request does *at a server*: a caller who marked a request for
/// origin `a` never judged origin `b`, so the mark comes off with
/// `Authorization` and `Cookie`. `RequireVersion` is not that kind of
/// claim. It is a statement about the caller's own code — "the thing I am
/// about to do needs this protocol" — and it is exactly as true at hop 2
/// as at hop 1.
///
/// Dropping it would let a `302` to another origin deliver over HTTP/1.1
/// precisely the request that said it could not use HTTP/1.1, which is the
/// failure the mark exists to prevent, arriving through the one door left
/// open. The `false` in `vec![true, true]` is what that mistake looks
/// like.
#[test]
fn the_demand_survives_a_cross_origin_redirect() {
    let m = MockTransport::new().with_capabilities(caps(true));
    m.push_response_bytes(
        http::Response::builder()
            .status(302)
            .header("location", "https://b/second")
            .body(vec![])
            .unwrap(),
    );
    m.push_response_bytes(ok());

    let c = Client::builder(m).build().expect("client");
    let resp = futures_executor::block_on(c.execute(demanding(http::Version::HTTP_2)))
        .expect("both hops are served by a backend that honours demands");
    assert_eq!(resp.status(), 200);

    let carried: Vec<bool> = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests()
        .iter()
        .map(|r| r.extensions.get::<RequireVersion>().is_some())
        .collect();
    assert_eq!(
        carried,
        vec![true, true],
        "one bool per request handed to the transport, in order: whether it \
         carried the demand. The second must be `true` — a redirect is not a \
         reason to stop needing HTTP/2"
    );
}

/// `build()` is deliberately not where this lives, and this test is the
/// record of that decision rather than an assertion about a bug.
///
/// A cookie jar and a client-level redirect policy are *client* settings,
/// so they can be refused at construction. A version demand is not: there
/// is no client-level form of it, on purpose. If the check migrated to
/// `build()` it would have to reject the whole client, and a browser
/// client is exactly what most callers of `hclient-fetch` want.
#[test]
fn a_client_over_a_backend_that_cannot_select_still_builds() {
    let m = MockTransport::new().with_capabilities(caps(false));
    assert!(
        Client::builder(m).build().is_ok(),
        "a backend that cannot honour a version demand is still a perfectly \
         good backend for every caller who does not make one"
    );
}
