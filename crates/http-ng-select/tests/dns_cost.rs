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
//! for a request that ends up on TCP. Three arms below, and between them
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
