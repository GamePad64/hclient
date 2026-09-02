//! What a caller gets back, against a server that answers on a socket.
//!
//! Nothing here reads a field of `Doh`. Every claim is: a real server put
//! these bytes on the wire, and this is what the `Resolve` stream then
//! yielded.
//!
//! The line these tests exist to defend is the one `hclient_dns`'s module
//! doc draws and re-draws: **"asked and found nothing" is not "could not
//! ask"**. An empty stream and an error are different answers, and every
//! condition below is placed on one side of it deliberately.

mod support;

use futures_util::StreamExt;
use hclient_core::ErrorKind;
use hclient_dns::{Record, Resolve, rtype};
use hclient_dns_doh::Doh;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;
use support::{
    FLAGS_NXDOMAIN, FLAGS_QUERY, FLAGS_SERVFAIL, FLAGS_TRUNCATED, Reply, Rr, Server, TYPE_A,
    TYPE_AAAA, message, noerror,
};

type Item = Result<Record, hclient_core::Error>;

fn transport() -> Native<Tokio, NoTls, hclient_dns::IpLiteralOnly> {
    Native::new(Tokio, NoTls, hclient_dns::IpLiteralOnly)
}

fn doh(server: &Server) -> Doh<Native<Tokio, NoTls, hclient_dns::IpLiteralOnly>> {
    Doh::pinned(transport(), server.endpoint()).expect("a loopback literal endpoint")
}

async fn v4(server: &Server, name: &str) -> Vec<Item> {
    doh(server).lookup(name, rtype::A).collect().await
}

async fn v6(server: &Server, name: &str) -> Vec<Item> {
    doh(server).lookup(name, rtype::AAAA).collect().await
}

fn addrs(items: &[Item]) -> Vec<IpAddr> {
    items
        .iter()
        .map(|i| {
            i.as_ref()
                .expect("an address, not an error")
                .rdata
                .addr()
                .expect("an address answer")
        })
        .collect()
}

fn one_error(items: Vec<Item>) -> hclient_core::Error {
    let mut items = items;
    assert_eq!(items.len(), 1, "expected exactly one item, got {items:?}");
    items
        .pop()
        .expect("one item")
        .expect_err("expected an error, got an address")
}

#[tokio::test]
async fn every_a_record_in_the_answer_reaches_the_caller() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_A,
        &[
            Rr::a("example.com", 60, [192, 0, 2, 1]),
            Rr::a("example.com", 60, [192, 0, 2, 2]),
        ],
    ));
    assert_eq!(
        addrs(&v4(&server, "example.com").await),
        vec![
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2)),
        ]
    );
}

#[tokio::test]
async fn an_aaaa_answer_reaches_the_v6_stream() {
    let addr: Ipv6Addr = "2001:db8::1".parse().expect("a v6 literal");
    let server = Server::answering(noerror(
        "example.com",
        TYPE_AAAA,
        &[Rr::aaaa("example.com", 60, addr)],
    ));
    assert_eq!(
        addrs(&v6(&server, "example.com").await),
        vec![IpAddr::V6(addr)]
    );
}

/// `Record::ttl` carries the record's own TTL, per record.
///
/// The two records deliberately carry **different** TTLs: a resolver that
/// took the RRset minimum, or the first record's value for both, would pass
/// a test with one record or with two equal ones.
#[tokio::test]
async fn each_address_carries_the_ttl_that_came_with_its_own_record() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_A,
        &[
            Rr::a("example.com", 300, [192, 0, 2, 1]),
            Rr::a("example.com", 45, [192, 0, 2, 2]),
        ],
    ));
    let items = v4(&server, "example.com").await;
    let ttls: Vec<Option<Duration>> = items
        .iter()
        .map(|i| i.as_ref().expect("an address").ttl)
        .collect();
    assert_eq!(
        ttls,
        vec![
            Some(Duration::from_secs(300)),
            Some(Duration::from_secs(45)),
        ],
        "the TTL the server sent is not the TTL the caller got"
    );
}

/// A CNAME in front of the addresses is the ordinary shape of an aliased
/// host, and the addresses are legitimately owned by a different name.
///
/// This test is what stops the owner-name check that would look like
/// hardening and would break every CDN-hosted origin.
#[tokio::test]
async fn addresses_behind_a_cname_are_returned_and_the_cname_is_stepped_over() {
    let server = Server::answering(noerror(
        "www.example.com",
        TYPE_A,
        &[
            Rr::cname("www.example.com", 60, "cdn.example.net"),
            Rr::a("cdn.example.net", 60, [192, 0, 2, 7]),
        ],
    ));
    assert_eq!(
        addrs(&v4(&server, "www.example.com").await),
        vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7))]
    );
}

/// NOERROR with nothing of the type asked for. An answer, so an empty
/// stream — and specifically **not** an error, because a host with an A
/// record and no AAAA is the commonest thing on the Internet and RFC 8305
/// asks both questions of every name.
#[tokio::test]
async fn no_records_of_the_type_is_an_empty_stream_and_not_an_error() {
    let server = Server::answering(noerror("example.com", TYPE_AAAA, &[]));
    assert!(v6(&server, "example.com").await.is_empty());
}

/// NXDOMAIN is an authority saying the name does not exist. Also an
/// answer.
#[tokio::test]
async fn nxdomain_is_an_empty_stream_and_not_an_error() {
    let server = Server::answering(message("nope.example", TYPE_A, FLAGS_NXDOMAIN, &[]));
    assert!(v4(&server, "nope.example").await.is_empty());
}

/// SERVFAIL is the resolver saying it could not answer. **Not** an empty
/// stream: nobody established that the name has no addresses.
#[tokio::test]
async fn servfail_is_an_error_and_not_an_empty_stream() {
    let server = Server::answering(message("example.com", TYPE_A, FLAGS_SERVFAIL, &[]));
    let e = one_error(v4(&server, "example.com").await);
    assert_eq!(e.kind(), &ErrorKind::Resolve);
    assert!(
        e.to_string().contains("RCODE 2"),
        "the RCODE is not in the message: {e}"
    );
}

/// A DoH response is carried over TCP-or-better, so `TC` means the
/// server's own upstream answer was cut and it passed that on. Not a
/// partial RRSet.
#[tokio::test]
async fn a_truncated_answer_is_an_error() {
    let server = Server::answering(message(
        "example.com",
        TYPE_A,
        FLAGS_TRUNCATED,
        &[Rr::a("example.com", 60, [192, 0, 2, 1])],
    ));
    let e = one_error(v4(&server, "example.com").await);
    assert!(e.to_string().contains("truncated"), "{e}");
}

/// `QR` clear: what came back is a query. Nothing answered.
#[tokio::test]
async fn a_response_without_the_qr_bit_is_an_error() {
    let server = Server::answering(message(
        "example.com",
        TYPE_A,
        FLAGS_QUERY,
        &[Rr::a("example.com", 60, [192, 0, 2, 1])],
    ));
    let e = one_error(v4(&server, "example.com").await);
    assert!(e.to_string().contains("rather than answering"), "{e}");
}

/// A server answering a question nobody asked. Over plain DNS this is a
/// cache-poisoning primitive; here it would be a silently wrong address,
/// which is worse than an error.
#[tokio::test]
async fn an_answer_to_a_different_question_is_refused() {
    let server = Server::answering(noerror(
        "attacker.example",
        TYPE_A,
        &[Rr::a("attacker.example", 60, [198, 51, 100, 1])],
    ));
    let e = one_error(v4(&server, "example.com").await);
    assert!(e.to_string().contains("different question"), "{e}");
}

/// The same, for the type: an A answer to an AAAA question.
#[tokio::test]
async fn an_answer_of_a_different_type_is_refused() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_A,
        &[Rr::a("example.com", 60, [192, 0, 2, 1])],
    ));
    let e = one_error(v6(&server, "example.com").await);
    assert!(e.to_string().contains("different question"), "{e}");
}

/// The same, for the CLASS. `IN` is the only class a web client has any
/// business with, and a `CH` answer to an `IN` question is as much a
/// different question as a different name would be.
///
/// **This test exists because a mutation survived without it.** Deleting
/// the `q_class` clause from `check_question` passed the whole suite: every
/// fixture sent `IN`, so nothing could tell whether the field was read.
#[tokio::test]
async fn an_answer_in_a_different_class_is_refused() {
    let server = Server::answering(support::message_in_class(
        "example.com",
        TYPE_A,
        support::CLASS_CH,
        support::FLAGS_NOERROR,
        &[Rr::a("example.com", 60, [192, 0, 2, 1])],
    ));
    let e = one_error(v4(&server, "example.com").await);
    assert!(e.to_string().contains("different question"), "{e}");
}

/// A fully-qualified name — `https://example.com./` is a legal URL, and
/// `Uri::host()` hands the trailing dot straight through to this trait.
/// The root label is not part of the name for the purpose of comparing
/// what the server echoed, and the lookup must succeed.
///
/// **Also written because a mutation survived without it**: dropping the
/// `trim_end_matches('.')` on the asked-for name left every existing test
/// green, and turned every fully-qualified host into a
/// `QuestionMismatch`.
#[tokio::test]
async fn a_fully_qualified_name_with_a_trailing_dot_resolves() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_A,
        &[Rr::a("example.com", 60, [192, 0, 2, 1])],
    ));
    assert_eq!(
        addrs(&v4(&server, "example.com.").await),
        vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))]
    );
}

/// DNS 0x20: a server may echo the question in a different case, and that
/// is the same question.
#[tokio::test]
async fn a_question_echoed_in_a_different_case_is_the_same_question() {
    let server = Server::answering(noerror(
        "ExAmPlE.CoM",
        TYPE_A,
        &[Rr::a("ExAmPlE.CoM", 60, [192, 0, 2, 1])],
    ));
    assert_eq!(
        addrs(&v4(&server, "example.com").await),
        vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))]
    );
}

#[tokio::test]
async fn a_non_200_is_an_error_naming_the_status() {
    let server = Server::spawn(|_| Reply::Status(502, noerror("example.com", TYPE_A, &[])));
    let e = one_error(v4(&server, "example.com").await);
    assert_eq!(e.kind(), &ErrorKind::Resolve);
    assert!(e.to_string().contains("502"), "{e}");
}

/// A captive portal answering 200 with a login page. Refused on the
/// content-type, before the bytes are handed to a DNS decoder.
#[tokio::test]
async fn a_200_that_is_not_a_dns_message_is_refused_on_its_content_type() {
    let server = Server::spawn(|_| Reply::Typed("text/html", b"<html>sign in</html>".to_vec()));
    let e = one_error(v4(&server, "example.com").await);
    assert!(e.to_string().contains("text/html"), "{e}");
}

/// The media type may carry parameters (`;charset=`), and RFC 8484 §6
/// names only the type. A server that adds one is not a captive portal.
#[tokio::test]
async fn a_content_type_with_parameters_is_still_a_dns_message() {
    let body = noerror(
        "example.com",
        TYPE_A,
        &[Rr::a("example.com", 60, [192, 0, 2, 1])],
    );
    let server = Server::spawn(move |_| {
        Reply::Typed("Application/DNS-Message; charset=binary", body.clone())
    });
    assert_eq!(
        addrs(&v4(&server, "example.com").await),
        vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))]
    );
}

/// Bytes that are not a DNS message at all, under the right content-type.
#[tokio::test]
async fn a_body_that_is_not_a_dns_message_is_an_error() {
    let server = Server::answering(vec![0xff; 7]);
    let e = one_error(v4(&server, "example.com").await);
    assert!(e.to_string().contains("not a valid DNS message"), "{e}");
}

/// The category is `Resolve` whatever failed underneath, because that is
/// the operation the caller asked for. `hclient-native`'s connector reads
/// `kind()` to decide which failure to surface, and a DoH server's own
/// connect failure reported as `Connect` would send anyone reading it to
/// the wrong host entirely.
#[tokio::test]
async fn a_dead_endpoint_is_a_resolve_error_not_a_connect_one() {
    // Bound to nothing: port 1 on loopback refuses immediately.
    let doh = Doh::pinned(
        transport(),
        "http://127.0.0.1:1/dns-query".parse().expect("uri"),
    )
    .expect("endpoint");
    let items: Vec<Item> = doh.lookup("example.com", rtype::A).collect().await;
    let e = one_error(items);
    assert_eq!(e.kind(), &ErrorKind::Resolve);
    assert!(
        e.to_string().contains("DoH request failed"),
        "the transport's own failure is not in the chain: {e}"
    );
}
