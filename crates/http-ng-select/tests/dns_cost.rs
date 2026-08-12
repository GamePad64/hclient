//! What the first protocol choice costs, in DNS queries — counted rather
//! than reasoned about.
//!
//! `docs/v04-design.md` §W1 says the fast tier *"costs no new discovery at
//! all — only the acting"*, and that is true of the **mechanism**: the
//! HTTPS record was already being fetched and parsed before this crate
//! existed. It is not true of the query count, and the difference is worth
//! a file of its own.
//!
//! `http-ng-native` fetches the record itself, inside its own connector,
//! for `https://` at the default port. That function is `pub(crate)`, no
//! record can cross the `Transport` seam, and this crate treats its members
//! as read-only — so at the default port the type-65 query is made twice
//! for a request that ends up on TCP. The first three arms below, and between them
//! they say exactly where the cost is:
//!
//! | request | queries |
//! |---|---|
//! | `https://origin/` chosen onto TCP | **2** — this transport's, then `http-ng-native`'s |
//! | `https://origin/` chosen onto QUIC | **1** — `http-ng-h3` does no SVCB lookup at all |
//! | `https://origin:port/` | **1** — `http-ng-native` skips discovery away from the default port |
//!
//! What would remove the duplicate is a way to hand an already-fetched
//! record to a member, which is a change to `http-ng-native` and therefore
//! a finding rather than an edit — see `docs/v04-w1-acceptance.md`.
//!
//! # The slow tier adds nothing to any of them (v0.4 W1 deliverable 4)
//!
//! `Alt-Svc` is a **response** header, so learning from it costs no query
//! at all, and acting on it costs whatever the request was already going
//! to cost. The last two arms measure that:
//!
//! | request | queries |
//! |---|---|
//! | one advertised onto QUIC, at a non-default port | **1** — the same one the hop before it paid, and no more |
//! | any request, where the resolver cannot do SVCB | **0** — including the one the advertisement puts on QUIC |
//!
//! The second is the row worth reading twice: an origin behind a resolver
//! with no SVCB support is unreachable by the fast tier at any price, and
//! the slow tier reaches it for nothing.
//!
//! One row is **not** measured here and is inferred from two that are: a
//! request the slow tier puts on QUIC at an origin's *default* port costs
//! **1** rather than the 2 the same request costs on TCP, because the
//! duplicate is `http-ng-native`'s and `http-ng-h3` makes no lookup (both
//! measured, in the first two arms). It is not measured directly for the
//! reason the first two arms cannot connect at all: an advertisement has
//! to arrive in a response, and this process cannot put a server on port
//! 443 to send one.
//!
//! # Why two of these arms do not connect to anything
//!
//! The count is about the **default port**, and an unprivileged test
//! process cannot put a listener on 443 — the same wall
//! `http-ng-native/tests/svcb.rs` hit and wrote down. So those two requests
//! fail, deliberately and quickly (a short `Timeouts::connect`), and the
//! failure is never the observation: the resolver's own log is.
#![cfg(not(target_family = "wasm"))]

mod fakedns;
mod servers;

use fakedns::{FakeDns, service_record};
use http_body_util::BodyExt;
use http_ng_core::unversioned::Transport;
use http_ng_core::{RequestBody, Timeouts};
use http_ng_h3::H3;
use http_ng_native::Native;
use http_ng_rt_tokio::TokioHandle;
use http_ng_select::Selecting;
use servers::ORIGIN;
use std::sync::Arc;
use std::time::Duration;

/// Short, because these two requests are expected to fail and the point is
/// only that everything that was going to ask DNS has asked by the time
/// they do.
const CONNECT: Duration = Duration::from_millis(300);

/// Generous, and never the assertion: it turns a mutation that hangs into a
/// red test rather than a stuck one.
const BOUND: Duration = Duration::from_secs(10);

type Selector = Selecting<TokioHandle, http_ng_tls_rustls::Rustls, FakeDns>;

/// An empty trust store: nothing here completes a handshake, and the two
/// arms that reach a real server bring their own.
fn tls() -> http_ng_tls_rustls::Rustls {
    let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    http_ng_tls_rustls::Rustls::from_config(Arc::new(cfg))
}

fn selector(dns: FakeDns, tls: impl Fn() -> http_ng_tls_rustls::Rustls) -> Selector {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    Selecting::new(
        rt.clone(),
        Native::new(rt.clone(), tls(), dns.clone()),
        H3::new(rt, tls(), dns.clone()).expect("H3::new does no I/O"),
        dns,
    )
    .expect("the two stacks agree")
}

fn get(uri: String) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    req.extensions_mut().insert(Timeouts {
        connect: Some(CONNECT),
        ..Timeouts::default()
    });
    req
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_chosen_onto_tcp_at_the_default_port_asks_for_the_record_twice() {
    let dns = FakeDns::with_records(vec![service_record(1, &[b"http/1.1"])]);
    let t = selector(dns.clone(), tls);

    let _ = tokio::time::timeout(BOUND, t.execute(get(format!("https://{ORIGIN}/"))))
        .await
        .expect("the request finished inside the bound");

    assert_eq!(
        dns.svcb_names(),
        [ORIGIN, ORIGIN],
        "this transport asked, and then `http-ng-native`'s own connector asked again"
    );
}

/// The QUIC arm pays once, because `http-ng-h3` reads no HTTPS record at
/// all — it resolves addresses and nothing else.
///
/// The contrast with the test above is the whole measurement: a suite that
/// only counted one of the two arms could not tell "the duplicate is
/// `http-ng-native`'s" from "this transport asks twice".
#[tokio::test(flavor = "multi_thread")]
async fn a_request_chosen_onto_quic_at_the_default_port_asks_once() {
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = selector(dns.clone(), tls);

    let _ = tokio::time::timeout(BOUND, t.execute(get(format!("https://{ORIGIN}/"))))
        .await
        .expect("the request finished inside the bound");

    assert_eq!(dns.svcb_names(), [ORIGIN]);
}

/// Away from the default port there is no duplicate, because
/// `http-ng-native` does no discovery there at all — and the one query
/// that is made carries the `_port._https.` prefix RFC 9460 §2.3 puts a
/// non-default service's record under.
///
/// This arm reaches a real server, so it also shows the count is not an
/// artefact of a request that failed early.
#[tokio::test(flavor = "multi_thread")]
async fn away_from_the_default_port_only_this_transport_asks() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![service_record(1, &[b"http/1.1"])]);
    let t = selector(dns.clone(), || servers::client_tls(&pair.cert_der));

    let resp = tokio::time::timeout(
        BOUND,
        t.execute(get(format!("https://{ORIGIN}:{}/hello", pair.port))),
    )
    .await
    .expect("inside the bound")
    .expect("the TCP server answered");
    assert_eq!(resp.status(), 200);
    assert_eq!(pair.tcp_answered(), 1);

    assert_eq!(
        dns.svcb_names(),
        [format!("_{}._https.{ORIGIN}", pair.port)]
    );
}

/// A request the **slow** tier puts on QUIC costs the same single query
/// the hop before it paid — Alt-Svc adds none of its own.
///
/// Two hops at a non-default port, where `http-ng-native` does no
/// discovery, so the only queries in the log are this transport's own: one
/// per request, before and after the origin advertised. The second hop is
/// on QUIC, which is what makes the count a fact about the slow tier
/// rather than about two identical TCP requests.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_advertised_onto_quic_asks_no_more_than_the_one_before_it() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let dns = FakeDns::new();
    let t = selector(dns.clone(), || servers::client_tls(&pair.cert_der));
    let uri = format!("https://{ORIGIN}:{}/hello", pair.port);

    for expected in ["h1", "h3"] {
        let resp = tokio::time::timeout(BOUND, t.execute(get(uri.clone())))
            .await
            .expect("inside the bound")
            .expect("one of the two servers answered");
        assert_eq!(resp.status(), 200);
        let body = resp
            .into_body()
            .collect()
            .await
            .expect("a complete body")
            .to_bytes();
        assert_eq!(body.as_ref(), expected.as_bytes());
    }

    let prefixed = format!("_{}._https.{ORIGIN}", pair.port);
    assert_eq!(
        dns.svcb_names(),
        [prefixed.clone(), prefixed],
        "one query per request, and the second one is the advertised hop"
    );
    assert_eq!(pair.tcp_answered(), 1);
    assert_eq!(pair.quic_answered(), 1);
}

/// The slow tier costs **zero** queries where the resolver cannot do SVCB
/// — and it is the only tier that can serve such an origin at all.
///
/// The fast tier stops at `Resolve::supports_svcb`, so an origin behind a
/// resolver with no SVCB support could never be chosen onto QUIC before
/// this tier existed. Now it can be, and the resolver is still not asked
/// anything.
#[tokio::test(flavor = "multi_thread")]
async fn a_resolver_that_cannot_ask_is_still_never_asked_and_still_reaches_quic() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let dns = FakeDns::cannot_ask_but_would_have_said(Vec::new());
    let t = selector(dns.clone(), || servers::client_tls(&pair.cert_der));
    let uri = format!("https://{ORIGIN}:{}/hello", pair.port);

    for _ in 0..2 {
        let resp = tokio::time::timeout(BOUND, t.execute(get(uri.clone())))
            .await
            .expect("inside the bound")
            .expect("one of the two servers answered");
        assert_eq!(resp.status(), 200);
        // Drained, so the exchange is finished before the next hop reads
        // the counters.
        let _ = resp.into_body().collect().await.expect("a complete body");
    }

    assert_eq!(dns.svcb_lookups(), 0);
    assert_eq!(pair.tcp_answered(), 1);
    assert_eq!(pair.quic_answered(), 1, "advertised onto QUIC with no DNS");
}
