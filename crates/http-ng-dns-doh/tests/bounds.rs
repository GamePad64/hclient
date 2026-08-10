//! The three things a DoH resolver must not let a server decide: how long
//! it waits, how many bytes it reads, and whether it asks at all.
//!
//! Each is checked against a server built to abuse it, and — for the
//! timeout — with a control that must hang without the bound, on the
//! pattern `http-ng-native/tests/timeouts.rs` established: a timeout test
//! showing only an error can be passed by an implementation that fails
//! everything.

mod support;

use futures_util::StreamExt;
use http_ng_core::{Error, ErrorKind, Timeouts};
use http_ng_dns::{IpLiteralOnly, Resolve, ResolvedAddr};
use http_ng_dns_doh::{Doh, MAX_RESPONSE_BYTES};
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::NoTls;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;
use support::{Reply, Rr, Server, TYPE_A, noerror};

type Item = Result<ResolvedAddr, Error>;

const BOUND: Duration = Duration::from_millis(250);
/// How long a test waits for something that must NOT happen.
const PATIENCE: Duration = Duration::from_millis(1500);

fn transport() -> Native<Tokio, NoTls, IpLiteralOnly> {
    Native::new(Tokio, NoTls, IpLiteralOnly)
}

fn doh(server: &Server) -> Doh<Native<Tokio, NoTls, IpLiteralOnly>> {
    Doh::pinned(transport(), server.endpoint()).expect("a loopback literal endpoint")
}

// ── the literal, which never becomes a query ────────────────────────────

/// **A DoH-backed client must still be able to reach `https://192.0.2.1/`.**
///
/// `http_ng_native::connect` hands `Uri::host()` to the resolver without
/// looking at it, so an IP-literal URL arrives here as the "name"
/// `192.0.2.1`. A resolver that queried for it would get NXDOMAIN from any
/// honest server and the connection would fail — so the literal is answered
/// from the string, and the assertion that matters is that **no request
/// reached the server at all**.
#[tokio::test]
async fn an_ip_literal_is_answered_without_a_query() {
    let server = Server::answering(noerror(
        "unused",
        TYPE_A,
        &[Rr::a("unused", 60, [198, 51, 100, 1])],
    ));
    let doh = doh(&server);

    let v4: Vec<Item> = doh.lookup_ipv4("192.0.2.1").collect().await;
    assert_eq!(
        v4.iter()
            .map(|i| i.as_ref().expect("an address").addr)
            .collect::<Vec<_>>(),
        vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))]
    );
    assert!(
        server.requests().is_empty(),
        "an IP literal was sent to the DoH server as a name"
    );
}

/// `Uri::host()` keeps the brackets on an IPv6 literal, and that is the
/// string this trait receives — the same trap `IpLiteralOnly::literal`
/// documents.
#[tokio::test]
async fn a_bracketed_ipv6_literal_is_answered_without_a_query() {
    let server = Server::answering(noerror("unused", TYPE_A, &[]));
    let doh = doh(&server);

    let v6: Vec<Item> = doh.lookup_ipv6("[2001:db8::1]").collect().await;
    assert_eq!(
        v6.iter()
            .map(|i| i.as_ref().expect("an address").addr)
            .collect::<Vec<_>>(),
        vec![IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().expect("v6"))]
    );
    assert!(server.requests().is_empty());
}

/// The other family gets an empty stream, not an error: "there is no AAAA
/// for this v4 literal" is a true and unremarkable answer, and erroring
/// would make every literal connection report a failure it did not have.
#[tokio::test]
async fn a_literal_of_the_wrong_family_is_empty_and_not_an_error() {
    let server = Server::answering(noerror("unused", TYPE_A, &[]));
    let doh = doh(&server);
    assert!(
        doh.lookup_ipv6("192.0.2.1")
            .collect::<Vec<Item>>()
            .await
            .is_empty()
    );
    assert!(
        doh.lookup_ipv4("[2001:db8::1]")
            .collect::<Vec<Item>>()
            .await
            .is_empty()
    );
    assert!(server.requests().is_empty());
}

/// An IP literal has no owner name to carry an HTTPS record, so there is
/// nothing to ask — and nothing goes over the wire.
#[tokio::test]
async fn an_ip_literal_asks_for_no_https_record_either() {
    let server = Server::answering(noerror("unused", support::TYPE_HTTPS, &[]));
    let doh = doh(&server);
    let found: Vec<Result<http_ng_dns::SvcbEndpoint, Error>> =
        doh.lookup_svcb("192.0.2.1").collect().await;
    assert!(found.is_empty());
    assert!(server.requests().is_empty());
}

// ── the clock ───────────────────────────────────────────────────────────

/// A server that accepts, reads the query, and never answers. Without a
/// `first_byte` bound this lookup does not end.
#[tokio::test]
async fn a_first_byte_bound_ends_a_lookup_a_silent_server_would_not() {
    let server = Server::spawn(|_| Reply::Silence);
    let doh = doh(&server).timeouts(Timeouts {
        connect: Some(BOUND),
        first_byte: Some(BOUND),
        between_bytes: Some(BOUND),
    });

    let items = tokio::time::timeout(PATIENCE, async {
        doh.lookup_ipv4("example.com").collect::<Vec<Item>>().await
    })
    .await
    .expect("the bound under test never fired");

    let e = items[0].as_ref().expect_err("expected a timeout");
    assert_eq!(e.kind(), &ErrorKind::Resolve, "{e}");
}

/// The control. The same silent server with every bound unset must hang —
/// otherwise the test above would pass against a transport that gave up on
/// its own, and would be measuring nothing.
#[tokio::test]
async fn the_same_silent_server_hangs_with_every_bound_unset() {
    let server = Server::spawn(|_| Reply::Silence);
    let doh = doh(&server).timeouts(Timeouts {
        connect: None,
        first_byte: None,
        between_bytes: None,
    });

    let outcome = tokio::time::timeout(PATIENCE, async {
        doh.lookup_ipv4("example.com").collect::<Vec<Item>>().await
    })
    .await;
    assert!(
        outcome.is_err(),
        "something other than our bound ended this lookup: {outcome:?}"
    );
}

/// And the default is not "no bound": a `Doh` nobody configured still
/// stops. The default `first_byte` is seconds, so this test only checks
/// that the bound exists and is finite, by bounding it from outside at a
/// value comfortably above it.
#[tokio::test]
async fn the_default_timeouts_are_not_none() {
    let server = Server::spawn(|_| Reply::Silence);
    let doh = doh(&server);

    let items = tokio::time::timeout(Duration::from_secs(20), async {
        doh.lookup_ipv4("example.com").collect::<Vec<Item>>().await
    })
    .await
    .expect("a Doh with default timeouts never stopped waiting");
    items[0].as_ref().expect_err("expected a timeout");
}

// ── the byte count ──────────────────────────────────────────────────────

/// The response body length is chosen by the server, and a DoH endpoint is
/// something a client talks to before it has decided to trust anything.
///
/// `MAX_RESPONSE_BYTES` is the largest a DNS message can be, so the cut can
/// only ever fall on a body that was never a legitimate answer.
#[tokio::test]
async fn a_response_larger_than_a_dns_message_can_be_is_refused() {
    let server = Server::spawn(|_| Reply::Dns(vec![0u8; MAX_RESPONSE_BYTES + 1]));
    let doh = doh(&server);
    let items: Vec<Item> = doh.lookup_ipv4("example.com").collect().await;
    let e = items[0].as_ref().expect_err("expected an error");
    assert!(
        e.to_string()
            .contains("could not read the DoH response body"),
        "the size limit did not fire: {e}"
    );
}
