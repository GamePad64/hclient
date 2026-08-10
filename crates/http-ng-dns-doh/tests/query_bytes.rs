//! What actually goes over the wire, byte for byte.
//!
//! `docs/v03-design.md` §W3 lists one premise about the codec as
//! **unverified**: *"`dns-message-parser` can encode a query, not only
//! decode a response — the decode path is what `http-ng-dns-system` uses;
//! the encode path is used by nothing here."* This file settles it, and
//! settles it the only way that is worth anything: the expected bytes are
//! written out here by hand from RFC 1035 §4.1, not produced by the same
//! library the test is checking.
//!
//! Everything asserted here is read off a socket by a server that knows
//! nothing about this crate.

mod support;

use futures_util::StreamExt;
use http_ng_dns::Resolve;
use http_ng_dns_doh::Doh;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::NoTls;
use support::{Rr, Server, TYPE_A, TYPE_AAAA, TYPE_HTTPS, noerror};

fn transport() -> Native<Tokio, NoTls, http_ng_dns::IpLiteralOnly> {
    Native::new(Tokio, NoTls, http_ng_dns::IpLiteralOnly)
}

/// The query RFC 1035 §4.1 describes for one question, with RFC 8484
/// §4.1's zero ID.
///
/// Header: ID 0; flags `0x0100` = `RD` and nothing else (no `QR`, opcode
/// QUERY, RCODE NOERROR); QDCOUNT 1; the other three counts 0. Then the
/// name in label form, QTYPE, QCLASS IN.
fn expected_query(name: &str, qtype: u16) -> Vec<u8> {
    let mut q = vec![
        0x00, 0x00, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    q.extend(support::name_wire(name));
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&1u16.to_be_bytes());
    q
}

#[tokio::test]
async fn an_a_lookup_posts_exactly_the_dns_query_rfc_1035_describes() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_A,
        &[Rr::a("example.com", 60, [192, 0, 2, 1])],
    ));
    let doh = Doh::pinned(transport(), server.endpoint()).expect("a loopback literal endpoint");

    let _ = doh.lookup_ipv4("example.com").collect::<Vec<_>>().await;

    let seen = server.requests();
    assert_eq!(seen.len(), 1, "one lookup is one request");
    assert_eq!(
        seen[0].body,
        expected_query("example.com", TYPE_A),
        "the query body is not the RFC 1035 encoding of one A question"
    );
}

#[tokio::test]
async fn the_question_carries_the_type_the_family_asked_for() {
    for (qtype, family) in [(TYPE_A, "v4"), (TYPE_AAAA, "v6"), (TYPE_HTTPS, "https")] {
        let server = Server::answering(noerror("example.com", qtype, &[]));
        let doh = Doh::pinned(transport(), server.endpoint()).expect("endpoint");
        match family {
            "v4" => {
                let _ = doh.lookup_ipv4("example.com").collect::<Vec<_>>().await;
            }
            "v6" => {
                let _ = doh.lookup_ipv6("example.com").collect::<Vec<_>>().await;
            }
            _ => {
                let _ = doh.lookup_svcb("example.com").collect::<Vec<_>>().await;
            }
        }
        let seen = server.requests();
        assert_eq!(
            seen[0].body,
            expected_query("example.com", qtype),
            "the {family} lookup did not ask for type {qtype}"
        );
    }
}

/// RFC 8484 §4.1 and §6: `POST` to the endpoint's own path, with both the
/// media type it sends and the one it will accept.
///
/// The path matters more than it looks: a resolver that dropped the URI's
/// path and posted to `/` would work against many public endpoints and
/// fail against `https://dns.example/some/prefix/dns-query`.
#[tokio::test]
async fn the_request_is_a_post_to_the_endpoints_own_path_with_both_media_types() {
    let server = Server::answering(noerror("example.com", TYPE_A, &[]));
    let doh = Doh::pinned(transport(), server.endpoint()).expect("endpoint");

    let _ = doh.lookup_ipv4("example.com").collect::<Vec<_>>().await;

    let seen = server.requests();
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/dns-query");
    assert_eq!(
        seen[0].header("content-type"),
        Some("application/dns-message")
    );
    assert_eq!(seen[0].header("accept"), Some("application/dns-message"));
}

/// RFC 8484 §4.1: "the DNS ID SHOULD be 0". Two identical queries must
/// therefore be byte-identical, which is what makes them cacheable and
/// which a randomised ID would silently take away.
#[tokio::test]
async fn two_identical_lookups_produce_byte_identical_queries() {
    let server = Server::answering(noerror("example.com", TYPE_A, &[]));
    let doh = Doh::pinned(transport(), server.endpoint()).expect("endpoint");

    let _ = doh.lookup_ipv4("example.com").collect::<Vec<_>>().await;
    let _ = doh.lookup_ipv4("example.com").collect::<Vec<_>>().await;

    let seen = server.requests();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].body, seen[1].body);
    assert_eq!(&seen[0].body[..2], &[0x00, 0x00], "the DNS ID is not zero");
}

/// A different name is a different query, so a test that only checked the
/// two above would also pass for an encoder that ignored its argument.
#[tokio::test]
async fn the_name_in_the_question_is_the_name_that_was_asked_for() {
    let server = Server::spawn(|r| {
        // Echo whatever question came in, so the resolver's own check
        // cannot be what fails here.
        let _ = r;
        support::Reply::Dns(noerror("other.example", TYPE_A, &[]))
    });
    let doh = Doh::pinned(transport(), server.endpoint()).expect("endpoint");

    let _ = doh.lookup_ipv4("other.example").collect::<Vec<_>>().await;

    let seen = server.requests();
    assert_eq!(seen[0].body, expected_query("other.example", TYPE_A));
}
