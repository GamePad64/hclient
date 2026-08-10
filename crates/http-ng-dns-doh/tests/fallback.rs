//! Fail closed, or fail open — observed from outside, in both directions.
//!
//! The decision is the caller's, and §W3 requires it to be visible rather
//! than defaulted quietly. It is visible in the **type**: `Doh<C>` is
//! `Doh<C, NoFallback>`; `with_fallback` produces `Doh<C, F>`. These tests
//! check that the type is telling the truth, and — the part that is easy
//! to get wrong — that a fallback does **not** get consulted when DoH
//! answered.
//!
//! The fallback used here is a stub resolver holding one address, written
//! at the bottom of this file. It is deliberately not `SystemDns`: a test
//! whose expected answer comes from the machine it runs on is a test of
//! that machine.

mod support;

use futures_core::Stream;
use futures_util::StreamExt;
use http_ng_core::{Error, ErrorKind};
use http_ng_dns::{Resolve, ResolvedAddr};
use http_ng_dns_doh::Doh;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::NoTls;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use support::{FLAGS_SERVFAIL, Rr, Server, TYPE_A, message, noerror};

type Item = Result<ResolvedAddr, Error>;

/// The address the fallback hands back. Distinct from every address any
/// DoH fixture in this file returns, so which resolver answered is never
/// in doubt.
const FROM_FALLBACK: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 9);
/// The address the DoH server hands back.
const FROM_DOH: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);

fn transport() -> Native<Tokio, NoTls, http_ng_dns::IpLiteralOnly> {
    Native::new(Tokio, NoTls, http_ng_dns::IpLiteralOnly)
}

/// An endpoint nothing is listening on: `connect` fails immediately, which
/// is the "the DoH server is unreachable" case in its cheapest form.
fn dead_endpoint() -> http::Uri {
    "http://127.0.0.1:1/dns-query".parse().expect("a valid uri")
}

fn addrs(items: &[Item]) -> Vec<IpAddr> {
    items
        .iter()
        .map(|i| i.as_ref().expect("an address, not an error").addr)
        .collect()
}

/// The default. A DoH failure is a resolution failure, and nothing else
/// answers.
#[tokio::test]
async fn without_a_fallback_an_unreachable_doh_server_is_a_resolution_failure() {
    let doh = Doh::pinned(transport(), dead_endpoint()).expect("endpoint");
    let items: Vec<Item> = doh.lookup_ipv4("example.com").collect().await;
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].as_ref().expect_err("expected an error").kind(),
        &ErrorKind::Resolve
    );
}

/// The opt-in. Same dead endpoint, and now an address comes back — the
/// fallback's, which is the whole and only difference.
#[tokio::test]
async fn with_a_fallback_an_unreachable_doh_server_resolves_through_it() {
    let doh = Doh::pinned(transport(), dead_endpoint())
        .expect("endpoint")
        .with_fallback(Stub::new(FROM_FALLBACK));
    let items: Vec<Item> = doh.lookup_ipv4("example.com").collect().await;
    assert_eq!(addrs(&items), vec![IpAddr::V4(FROM_FALLBACK)]);
}

/// A DoH server that answers SERVFAIL has failed, not answered — so the
/// fallback applies to it too. This is a different code path from a
/// connect failure (the exchange completed and the message decoded), and
/// it is the one a resolver behind a broken upstream actually hits.
#[tokio::test]
async fn a_servfail_also_reaches_the_fallback() {
    let server = Server::answering(message("example.com", TYPE_A, FLAGS_SERVFAIL, &[]));
    let doh = Doh::pinned(transport(), server.endpoint())
        .expect("endpoint")
        .with_fallback(Stub::new(FROM_FALLBACK));
    let items: Vec<Item> = doh.lookup_ipv4("example.com").collect().await;
    assert_eq!(addrs(&items), vec![IpAddr::V4(FROM_FALLBACK)]);
}

/// **The half that is easy to get wrong.** A DoH answer is never
/// second-guessed: a resolver that asked the fallback on every lookup, or
/// that merged the two, would turn a working DoH deployment into one that
/// leaks every query to the plaintext resolver anyway.
///
/// Checked twice over — the address returned is the DoH one, and the stub
/// counts how many times it was asked.
#[tokio::test]
async fn a_successful_doh_answer_never_consults_the_fallback() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_A,
        &[Rr::a("example.com", 60, FROM_DOH.octets())],
    ));
    let stub = Stub::new(FROM_FALLBACK);
    let asked = stub.asked();
    let doh = Doh::pinned(transport(), server.endpoint())
        .expect("endpoint")
        .with_fallback(stub);

    let items: Vec<Item> = doh.lookup_ipv4("example.com").collect().await;

    assert_eq!(addrs(&items), vec![IpAddr::V4(FROM_DOH)]);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "the fallback was consulted"
    );
}

/// NXDOMAIN is an answer, and the fallback must not be asked for a second
/// opinion on it — that would be the downgrade the module doc describes,
/// happening on every non-existent name rather than only under attack.
#[tokio::test]
async fn nxdomain_is_an_answer_and_does_not_reach_the_fallback() {
    let server = Server::answering(message(
        "nope.example",
        TYPE_A,
        support::FLAGS_NXDOMAIN,
        &[],
    ));
    let stub = Stub::new(FROM_FALLBACK);
    let asked = stub.asked();
    let doh = Doh::pinned(transport(), server.endpoint())
        .expect("endpoint")
        .with_fallback(stub);

    let items: Vec<Item> = doh.lookup_ipv4("nope.example").collect().await;

    assert!(items.is_empty(), "expected an empty answer, got {items:?}");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "the fallback was consulted"
    );
}

/// The conflation `with_fallback`'s doc comment names as deliberate: DoH
/// failed, the fallback has nothing of this family, so the DoH error is
/// what surfaces. `Stub` answers only v4, so a v6 lookup exercises it.
#[tokio::test]
async fn a_fallback_with_nothing_to_say_leaves_the_doh_error_standing() {
    let doh = Doh::pinned(transport(), dead_endpoint())
        .expect("endpoint")
        .with_fallback(Stub::new(FROM_FALLBACK));
    let items: Vec<Item> = doh.lookup_ipv6("example.com").collect().await;
    assert_eq!(items.len(), 1);
    let e = items[0].as_ref().expect_err("expected the DoH error");
    assert!(
        e.to_string().contains("DoH request failed"),
        "the surviving error is not the DoH one: {e}"
    );
}

/// The fallback's own errors are its answer, and pass through — a fallback
/// that says "this name does not exist" must not be overwritten by "the
/// DoH server was unreachable", which is a true statement about a
/// different question.
#[tokio::test]
async fn a_fallback_that_errors_reports_its_own_error() {
    let doh = Doh::pinned(transport(), dead_endpoint())
        .expect("endpoint")
        // `IpLiteralOnly` errors for anything that is not a literal, which
        // is exactly a fallback with an opinion of its own.
        .with_fallback(http_ng_dns::IpLiteralOnly);
    let items: Vec<Item> = doh.lookup_ipv4("example.com").collect().await;
    assert_eq!(items.len(), 1);
    let e = items[0].as_ref().expect_err("expected an error");
    assert!(
        e.to_string().contains("IpLiteralOnly"),
        "the fallback's own error did not survive: {e}"
    );
}

// ── the stub ────────────────────────────────────────────────────────────

/// A resolver holding one v4 address, which counts how often it was asked.
///
/// v4 only: `lookup_ipv6` returns an empty stream, which is what
/// `a_fallback_with_nothing_to_say_leaves_the_doh_error_standing` needs.
#[derive(Debug, Clone)]
struct Stub {
    addr: Ipv4Addr,
    asked: Arc<AtomicUsize>,
}

impl Stub {
    fn new(addr: Ipv4Addr) -> Self {
        Self {
            addr,
            asked: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn asked(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.asked)
    }
}

impl Resolve for Stub {
    fn lookup_ipv4(&self, _name: &str) -> impl Stream<Item = Item> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        futures_util::stream::iter(vec![Ok(ResolvedAddr {
            addr: IpAddr::V4(self.addr),
            ttl: None,
        })])
    }
    fn lookup_ipv6(&self, _name: &str) -> impl Stream<Item = Item> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        futures_util::stream::iter(Vec::new())
    }
}
