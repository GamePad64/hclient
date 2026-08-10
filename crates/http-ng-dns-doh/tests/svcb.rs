//! `supports_svcb()` is `true`, and a record proves it by round-tripping.
//!
//! This is the claim the crate exists for: §W3 opens with *"it is the
//! answer for every platform whose system resolver cannot ask for an HTTPS
//! record, which is Windows 10, wasm, and anything behind a stub resolver
//! that drops type 65."* A capability that over-claims costs a caller a
//! wrong decision, and this codebase has caught that four times — so
//! `supports_svcb()` returning `true` is not asserted on its own anywhere
//! in this file. Every test that reads it also puts an HTTPS record on the
//! wire and checks what came out the other end.
//!
//! The record bytes are built by hand in `support` — RFC 9460 §2.2 RDATA:
//! a two-byte priority, a TargetName in label form, then key/length/value
//! triples in ascending key order.

mod support;

use futures_util::StreamExt;
use http_ng_dns::{Resolve, SvcbEndpoint};
use http_ng_dns_doh::Doh;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::NoTls;
use std::net::{Ipv4Addr, Ipv6Addr};
use support::{FLAGS_NXDOMAIN, Rr, Server, TYPE_HTTPS, message, noerror};

/// SvcParamKeys, RFC 9460 §14.3.2.
const KEY_MANDATORY: u16 = 0;
const KEY_ALPN: u16 = 1;
const KEY_PORT: u16 = 3;
const KEY_IPV4HINT: u16 = 4;
const KEY_ECH: u16 = 5;
const KEY_IPV6HINT: u16 = 6;
/// RFC 9461's `dohpath`: real, registered, and acted on by nothing here.
const KEY_DOHPATH: u16 = 7;

type Item = Result<SvcbEndpoint, http_ng_core::Error>;

fn doh(server: &Server) -> Doh<Native<Tokio, NoTls, http_ng_dns::IpLiteralOnly>> {
    Doh::pinned(
        Native::new(Tokio, NoTls, http_ng_dns::IpLiteralOnly),
        server.endpoint(),
    )
    .expect("a loopback literal endpoint")
}

async fn endpoints(server: &Server, name: &str) -> Vec<Item> {
    doh(server).lookup_svcb(name).collect().await
}

/// An `alpn` SvcParamValue: each id length-prefixed with one byte
/// (RFC 9460 §7.1).
fn alpn(ids: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for id in ids {
        out.push(u8::try_from(id.len()).expect("short id"));
        out.extend_from_slice(id.as_bytes());
    }
    out
}

/// An ECHConfigList as it appears in the SvcParamValue: the redundant
/// two-byte length prefix of RFC 9460 §7.3, then the payload.
fn ech(payload: &[u8]) -> Vec<u8> {
    let mut out = u16::try_from(payload.len())
        .expect("short list")
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(payload);
    out
}

/// The whole point, in one test: a ServiceMode record with the five
/// parameters `SvcbEndpoint` can hold, put on the wire by a server that
/// knows nothing about this crate, and read back field by field.
///
/// A resolver that answered `supports_svcb() == true` and returned nothing
/// fails here. So does one that returns endpoints with empty fields.
#[tokio::test]
async fn a_service_mode_record_round_trips_every_field_svcbendpoint_holds() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_HTTPS,
        &[Rr::https(
            "example.com",
            3600,
            1,
            "svc.example.net",
            &[
                (KEY_ALPN, alpn(&["h3", "h2"])),
                (KEY_PORT, 8443u16.to_be_bytes().to_vec()),
                (KEY_IPV4HINT, Ipv4Addr::new(192, 0, 2, 1).octets().to_vec()),
                (KEY_ECH, ech(&[0xab, 0xcd, 0xef])),
                (
                    KEY_IPV6HINT,
                    "2001:db8::1"
                        .parse::<Ipv6Addr>()
                        .expect("v6")
                        .octets()
                        .to_vec(),
                ),
            ],
        )],
    ));

    let found = endpoints(&server, "example.com").await;
    assert_eq!(found.len(), 1, "expected one endpoint, got {found:?}");
    let e = found[0].as_ref().expect("an endpoint, not an error");

    assert_eq!(e.priority, 1);
    assert_eq!(e.target, "svc.example.net");
    assert_eq!(e.alpn, vec![b"h3".to_vec(), b"h2".to_vec()]);
    assert_eq!(e.port, Some(8443));
    assert_eq!(e.ipv4hint, vec![Ipv4Addr::new(192, 0, 2, 1)]);
    assert_eq!(
        e.ipv6hint,
        vec!["2001:db8::1".parse::<Ipv6Addr>().expect("v6")]
    );
    // RFC 9460 §7.3's redundant length prefix survives, because that is
    // the form `rustls::EchConfig` parses.
    assert_eq!(
        e.ech_config_list.as_deref(),
        Some(&[0x00, 0x03, 0xab, 0xcd, 0xef][..])
    );

    // Read only after the record above proved the capability is real.
    assert!(doh(&server).supports_svcb());
}

/// RFC 9460 §2.5: a ServiceMode TargetName of `.` means the record's own
/// owner name. Substituted here so no consumer has to know the convention.
#[tokio::test]
async fn a_service_mode_record_with_a_root_target_takes_its_owner_name() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_HTTPS,
        &[Rr::https("example.com", 60, 1, "", &[])],
    ));
    let found = endpoints(&server, "example.com").await;
    assert_eq!(
        found[0].as_ref().expect("an endpoint").target,
        "example.com"
    );
}

/// RFC 9460 §8: a record whose `mandatory` list names a key this client
/// does not act on must be ignored — one record, not the RRSet. `dohpath`
/// (key 7) is the registered key `http_ng_dns::svcb::RECOGNISED_KEYS`
/// deliberately excludes, and DoH is exactly where someone would expect it
/// to be honoured.
#[tokio::test]
async fn a_record_making_dohpath_mandatory_is_ignored_and_the_usable_one_is_kept() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_HTTPS,
        &[
            Rr::https(
                "example.com",
                60,
                1,
                "unusable.example",
                &[
                    (KEY_MANDATORY, KEY_DOHPATH.to_be_bytes().to_vec()),
                    (KEY_DOHPATH, b"/dns-query{?dns}".to_vec()),
                ],
            ),
            Rr::https("example.com", 60, 2, "usable.example", &[]),
        ],
    ));
    let found = endpoints(&server, "example.com").await;
    assert_eq!(found.len(), 1, "expected the unusable record to be dropped");
    assert_eq!(
        found[0].as_ref().expect("an endpoint").target,
        "usable.example"
    );
}

/// RFC 9460 §8, the other half: a `mandatory` list naming a key the record
/// does not carry is malformed, and malformed means the whole RRSet goes.
#[tokio::test]
async fn a_mandatory_key_the_record_does_not_carry_is_an_error() {
    let server = Server::answering(noerror(
        "example.com",
        TYPE_HTTPS,
        &[Rr::https(
            "example.com",
            60,
            1,
            "svc.example",
            &[(KEY_MANDATORY, KEY_PORT.to_be_bytes().to_vec())],
        )],
    ));
    let found = endpoints(&server, "example.com").await;
    let e = found[0].as_ref().expect_err("expected an error");
    assert!(e.to_string().contains("mandatory"), "{e}");
}

/// No HTTPS record for a name that exists. An answer, so an empty stream —
/// and with `supports_svcb() == true` that empty stream unambiguously
/// means "asked, found none", which is the whole reason the two methods
/// have to be overridden together.
#[tokio::test]
async fn a_name_with_no_https_record_is_an_empty_stream_not_an_error() {
    let server = Server::answering(noerror("example.com", TYPE_HTTPS, &[]));
    assert!(endpoints(&server, "example.com").await.is_empty());
    assert!(doh(&server).supports_svcb());
}

#[tokio::test]
async fn nxdomain_for_an_https_query_is_also_an_empty_stream() {
    let server = Server::answering(message("nope.example", TYPE_HTTPS, FLAGS_NXDOMAIN, &[]));
    assert!(endpoints(&server, "nope.example").await.is_empty());
}

/// A DoH failure on an SVCB lookup is an error, never an empty stream —
/// and specifically never routed to a fallback resolver, because a
/// fallback that reports `supports_svcb() == false` would answer "there
/// are none" to a question it never asked.
#[tokio::test]
async fn a_failed_svcb_lookup_is_an_error_even_with_a_fallback_configured() {
    let doh = Doh::pinned(
        Native::new(Tokio, NoTls, http_ng_dns::IpLiteralOnly),
        "http://127.0.0.1:1/dns-query".parse().expect("uri"),
    )
    .expect("endpoint")
    .with_fallback(http_ng_dns::IpLiteralOnly);

    let found: Vec<Item> = doh.lookup_svcb("example.com").collect().await;
    assert_eq!(found.len(), 1);
    found[0].as_ref().expect_err("expected an error");
}
