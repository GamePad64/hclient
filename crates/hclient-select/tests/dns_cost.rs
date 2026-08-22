//! What the first protocol choice costs, in DNS queries — counted rather
//! than reasoned about.
//!
//! The fast tier costs no new discovery, only the acting — and that is
//! true of the **mechanism**: the
//! HTTPS record was already being fetched and parsed before this crate
//! existed. It is not true of the query count, and the difference is worth
//! a file of its own.
//!
//! `hclient-native` fetches the record itself, inside its own connector,
//! for `https://` at the default port — so at the default port a naive
//! implementation makes the type-65 query **twice** for a request that
//! ends up on TCP: once here to choose the stack, once there to open the
//! connection. **The `Prefetch` seam removes the duplicate**: this
//! transport asks
//! the TCP member to do the lookup, reads the answer, and hands both the
//! answer and the request back, so the connector does not ask again. The
//! first three arms below say where the cost is now, and the first of them
//! is the one that changed:
//!
//! | request | queries |
//! |---|---|
//! | `https://origin/` chosen onto TCP | **1** — `Native::prepare`'s, and the connector reuses it |
//! | `https://origin/` chosen onto QUIC | **1** — `hclient-h3` does no SVCB lookup at all |
//! | `https://origin:port/` | **1** — the connector does no discovery away from the default port, so this transport asks for the prefixed record itself |
//!
//! The three are one number now, and that is the point: **one type-65
//! query per request that has a name to ask about, whichever stack
//! answers**. What decides the count is no longer which member serves the
//! request.
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
//! **1**, which is what the same request costs on TCP as well now that the
//! duplicate is gone (both measured, in the first two arms). It is not
//! measured directly for the reason the first two arms cannot connect at
//! all: an advertisement has to arrive in a response, and this process
//! cannot put a server on port 443 to send one.
//!
//! # Why two of these arms do not connect to anything
//!
//! The count is about the **default port**, and an unprivileged test
//! process cannot put a listener on 443 — the same wall
//! `hclient-native/tests/svcb.rs` hit and wrote down. So those two requests
//! fail, deliberately and quickly (a short `Timeouts::connect`), and the
//! failure is never the observation: the resolver's own log is.
#![cfg(not(target_family = "wasm"))]

mod fakedns;
mod servers;

use fakedns::{FakeDns, service_record};
use hclient_core::unversioned::Transport;
use hclient_core::{RequestBody, Timeouts};
use hclient_h3::H3;
use hclient_native::Native;
use hclient_rt_tokio::TokioHandle;
use hclient_select::Selecting;
use http_body_util::BodyExt;
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

type Selector = Selecting<TokioHandle, hclient_tls_rustls::Rustls, FakeDns>;

/// An empty trust store: nothing here completes a handshake, and the two
/// arms that reach a real server bring their own.
fn tls() -> hclient_tls_rustls::Rustls {
    let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    hclient_tls_rustls::Rustls::from_config(Arc::new(cfg))
}

fn selector(dns: FakeDns, tls: impl Fn() -> hclient_tls_rustls::Rustls) -> Selector {
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
        resolve: None,
        connect: Some(CONNECT),
        ..Timeouts::default()
    });
    req
}

/// The row this file was written to record, and the one that moved: the
/// record that chose the TCP stack is the record the TCP stack connects
/// under, so it is fetched **once**.
///
/// The name it is asked under is the assertion as much as the count is: an
/// answer that came back twice under the same name would be two lookups
/// however they were counted, and a single lookup under the wrong name
/// would be a record for another origin.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_chosen_onto_tcp_at_the_default_port_asks_for_the_record_once() {
    let dns = FakeDns::with_records(vec![service_record(1, &[b"http/1.1"])]);
    let t = selector(dns.clone(), tls);

    let _ = tokio::time::timeout(BOUND, t.execute(get(format!("https://{ORIGIN}/"))))
        .await
        .expect("the request finished inside the bound");

    assert_eq!(
        dns.svcb_names(),
        [ORIGIN],
        "the connector was asked to look, and does not look again for what it found"
    );
}

/// The QUIC arm pays once too, because `hclient-h3` reads no HTTPS record
/// at all — it resolves addresses and nothing else.
///
/// The contrast with the test above is what located the duplicate in
/// `hclient-native` (2 against 1). It is now the control for the other
/// direction:
/// with both at 1, a change that started asking twice on *either* path
/// moves one of these two rows.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_chosen_onto_quic_at_the_default_port_asks_once() {
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = selector(dns.clone(), tls);

    let _ = tokio::time::timeout(BOUND, t.execute(get(format!("https://{ORIGIN}/"))))
        .await
        .expect("the request finished inside the bound");

    assert_eq!(dns.svcb_names(), [ORIGIN]);
}

/// Away from the default port the connector does no discovery at all, so
/// there is nothing to share and this transport asks for itself — and the
/// one query it makes carries the `_port._https.` prefix RFC 9460 §2.3
/// puts a non-default service's record under.
///
/// **This is the fallback arm**, and it is what keeps "ask the connector
/// first" from being a rule about where discovery applies: the connector
/// answers `Discovered::NotConsulted`, which is not an answer, and this
/// transport goes and asks its own resolver under its own name. A single
/// query under the *prefixed* name is what says both halves happened —
/// the connector asked nothing (there is no bare-name query in the log)
/// and this transport asked once.
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
/// Two hops at a non-default port, where `hclient-native` does no
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
