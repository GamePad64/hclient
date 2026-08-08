//! `SvcbEndpoint` — what a consumer can rely on after the record crosses
//! the seam.
//!
//! The struct is why the resolver is not pinned to `SocketAddr`: HTTPS/SVCB
//! is where ECH configuration and h3 discovery come from, and both are
//! carried as opaque octets that reach `rustls` and the ALPN negotiator
//! unchanged. The tests below are written from the consumer's side of the
//! trait — a record that only survives being constructed, and not being
//! streamed out of `lookup_svcb` and read, would be no use to anyone.

use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use http_ng_core::Error;
use http_ng_dns::{Resolve, ResolvedAddr, SvcbEndpoint};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A resolver that hands back exactly the records it was built with, so
/// what a test asserts on the far side is what it put in on this one.
#[derive(Debug)]
struct Canned(Vec<SvcbEndpoint>);

impl Resolve for Canned {
    fn lookup_ipv4(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        futures_util::stream::empty()
    }
    fn lookup_ipv6(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        futures_util::stream::empty()
    }
    fn supports_svcb(&self) -> bool {
        true
    }
    fn lookup_svcb(&self, _: &str) -> impl Stream<Item = Result<SvcbEndpoint, Error>> {
        futures_util::stream::iter(self.0.clone().into_iter().map(Ok).collect::<Vec<_>>())
    }
}

fn served(records: Vec<SvcbEndpoint>) -> Vec<SvcbEndpoint> {
    let resolver = Canned(records);
    futures_executor::block_on(resolver.lookup_svcb("example.com").collect::<Vec<_>>())
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("Canned never fails")
}

fn endpoint(target: &str) -> SvcbEndpoint {
    SvcbEndpoint {
        priority: 1,
        target: target.to_owned(),
        alpn: vec![],
        port: None,
        ipv4hint: vec![],
        ipv6hint: vec![],
        ech_config_list: None,
    }
}

/// A real ECHConfigList is a length-prefixed binary structure: it contains
/// NUL bytes, it is not valid UTF-8, and `rustls` parses the wire bytes
/// rather than a rendering of them. So the assertion is on the bytes
/// themselves, not on their length or on `is_some()` — a field that
/// arrived truncated, re-encoded, or lossily converted would still be
/// present and still be the right length under a weaker check.
#[test]
fn the_ech_config_list_reaches_the_consumer_byte_for_byte() {
    let wire = Bytes::copy_from_slice(&[
        0x00, 0x45, 0xfe, 0x0d, 0x00, 0x41, 0x00, 0x20, 0xff, 0xfe, 0x00, 0x80, 0x00, 0x00,
    ]);
    assert!(
        std::str::from_utf8(&wire).is_err(),
        "precondition: the fixture must be genuinely non-textual, or this test would pass \
         for a field that had been turned into a String"
    );

    let mut record = endpoint("svc.example");
    record.ech_config_list = Some(wire.clone());

    let got = served(vec![record]);
    let [only] = got.as_slice() else {
        panic!("one record in, one record out; got {got:?}")
    };
    assert_eq!(
        only.ech_config_list.as_ref(),
        Some(&wire),
        "the ECH configuration must arrive exactly as the resolver read it off the wire: \
         rustls::EchConfig parses these bytes, and a single altered or dropped one makes \
         the handshake fail rather than fall back"
    );
}

/// "The origin does not offer ECH" and "the origin offers an empty ECH
/// configuration list" are different facts, and `Option<Bytes>` exists so
/// the consumer is not forced to guess which one it has. Collapsing them —
/// storing a bare `Bytes` and letting the empty one mean "absent" — would
/// make an unusable ECH offer indistinguishable from no offer at all.
#[test]
fn no_ech_offered_is_not_the_same_record_as_an_empty_ech_list() {
    #[derive(Debug, PartialEq, Eq)]
    enum Ech {
        NotOffered,
        OfferedButEmpty,
        Config(Bytes),
    }

    fn state(record: &SvcbEndpoint) -> Ech {
        match &record.ech_config_list {
            None => Ech::NotOffered,
            Some(bytes) if bytes.is_empty() => Ech::OfferedButEmpty,
            Some(bytes) => Ech::Config(bytes.clone()),
        }
    }

    // The three differ in `ech_config_list` and in nothing else: were they to
    // differ in `target` or `priority` too, the inequality below would hold
    // for a reason that has nothing to do with ECH.
    let absent = endpoint("svc.example");
    let mut empty = endpoint("svc.example");
    empty.ech_config_list = Some(Bytes::new());
    let mut real = endpoint("svc.example");
    real.ech_config_list = Some(Bytes::from_static(b"\xfe\x0d"));

    let got = served(vec![absent, empty, real]);
    let states: Vec<_> = got.iter().map(state).collect();
    assert_eq!(
        states,
        vec![
            Ech::NotOffered,
            Ech::OfferedButEmpty,
            Ech::Config(Bytes::from_static(b"\xfe\x0d")),
        ],
        "three distinct answers must survive the seam as three; the first two collapsing \
         into one would report an origin that offers a broken ECH list as an origin that \
         offers none"
    );
    assert_ne!(
        got[0], got[1],
        "and the records themselves must not compare equal: they differ in this one field \
         and nothing else, so an equality that ignored it would let a cache serve the \
         ECH-less record for the ECH-offering one"
    );
}

/// ALPN protocol IDs are opaque octet sequences on the wire (RFC 7301), not
/// text, which is why the field is `Vec<Vec<u8>>` and not `Vec<String>`.
/// A consumer matches them byte-for-byte against `b"h3"`; a resolver that
/// had to go through UTF-8 to deliver them could not carry an unknown
/// protocol ID at all.
#[test]
fn alpn_ids_stay_opaque_octets_and_keep_the_order_the_resolver_gave_them() {
    let mut record = endpoint("svc.example");
    record.alpn = vec![b"h3".to_vec(), b"h2".to_vec(), vec![0xff, 0x00, 0x01]];

    let got = served(vec![record]);
    let [only] = got.as_slice() else {
        panic!("one record in, one record out; got {got:?}")
    };
    assert_eq!(
        only.alpn,
        vec![b"h3".to_vec(), b"h2".to_vec(), vec![0xff, 0x00, 0x01]],
        "every ID, in the order given, including one that is not valid UTF-8"
    );
    assert!(
        only.alpn.iter().any(|id| id == b"h3"),
        "h3 discovery works by matching this ID exactly — the whole reason the record is \
         read before a connection is made"
    );
}

/// Hints and port are what let a consumer skip a second round trip: an
/// endpoint whose `target` it would otherwise have to resolve. All three
/// must survive the seam together — a record that arrives with its hints
/// dropped looks exactly like a record that never had any.
#[test]
fn port_and_address_hints_survive_so_the_target_need_not_be_resolved_again() {
    let mut record = endpoint("svc.example");
    record.port = Some(8443);
    record.ipv4hint = vec![Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(192, 0, 2, 2)];
    record.ipv6hint = vec!["2001:db8::1".parse::<Ipv6Addr>().unwrap()];

    let got = served(vec![record.clone()]);
    let [only] = got.as_slice() else {
        panic!("one record in, one record out; got {got:?}")
    };
    // Both forms on purpose. The whole-record comparison catches a field
    // this test forgot to name; the field-by-field ones catch the opposite
    // failure, an equality that quietly ignores a field and so reports two
    // different records as the same one.
    assert_eq!(only, &record, "the record must cross the seam unchanged");
    assert_eq!(
        only.port,
        Some(8443),
        "an explicit port must not be lost — dropping it silently sends the request to 443"
    );
    assert_eq!(only.ipv4hint.len(), 2, "both hints, not just the first");
    assert_eq!(
        only.ipv6hint,
        vec!["2001:db8::1".parse::<Ipv6Addr>().unwrap()],
        "the v6 hint must not be folded into the v4 list or dropped"
    );
}

/// A resolution result is produced on one task and used on another — a
/// connector spawned per address family, a cache filled in the background.
/// Both types therefore have to move across a thread boundary and be
/// duplicated for the several attempts Happy Eyeballs starts. Moving a real
/// value through `spawn` and comparing it on the far side exercises
/// `Send + 'static`, `Clone` and `PartialEq` at once, rather than asserting
/// them at compile time and observing nothing.
#[test]
fn a_resolved_address_and_an_endpoint_can_be_moved_to_another_thread_and_compared() {
    let addr = ResolvedAddr {
        addr: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        ttl: Some(std::time::Duration::from_secs(30)),
    };
    let mut record = endpoint("svc.example");
    record.ech_config_list = Some(Bytes::from_static(b"\xfe\x0d"));

    let (addr_here, record_here) = (addr.clone(), record.clone());
    let sent_back = std::thread::spawn(move || (addr, record))
        .join()
        .expect("the worker thread must not panic");

    assert_eq!(
        sent_back,
        (addr_here, record_here),
        "what one task resolved is what another task connects to"
    );
}
