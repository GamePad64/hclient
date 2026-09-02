//! [`Resolve`] over `hickory-resolver`: the backend that can actually
//! answer SVCB.
//!
//! `hclient-dns-system` goes through the platform resolver, which on Unix
//! means `res_query` and on Windows `DnsQuery_UTF8`. That works, and it is
//! the right default because it honours whatever the machine is configured
//! to do — `/etc/hosts`, split-horizon VPN DNS, mDNS. This backend exists
//! for the cases where that is not enough: a resolver that speaks DNS
//! itself can be pointed at specific upstreams, can cache with real TTLs,
//! and answers HTTPS/SVCB on every platform rather than on the two where a
//! system API happens to expose it.
//!
//! # What this costs, stated up front
//!
//! `tokio`. hickory's `tokio` feature is enabled here because its
//! `ConnectionProvider` needs an executor to drive sockets, and no
//! runtime-agnostic provider ships with it. That is a genuine departure
//! from the rest of this workspace, where the runtime seam exists precisely
//! so nothing below `hclient` names a runtime — `hclient-dns-system` needs
//! only `Blocking`, and works on smol. Choosing this resolver means
//! choosing tokio with it.
//!
//! The type is generic over `P: ConnectionProvider` rather than pinned to
//! `TokioResolver`, so a runtime-agnostic provider — hickory's or someone
//! else's — drops in without a change here. The feature is the constraint,
//! not the design.
//!
//! # Why the TTL is populated here and not by the system backend
//!
//! `Record::ttl` is `Option<Duration>` because `getaddrinfo` does not
//! expose one — the system backend returns `None` and is right to. hickory
//! parses records itself, so the TTL is present, and every address carries
//! its own rather than the RRset's minimum: a caller doing Happy Eyeballs
//! or its own caching wants the value the server actually sent.
#![forbid(unsafe_code)]

use futures_core::Stream;
use futures_util::StreamExt;
use hclient_core::{Error, ErrorKind};
use hclient_dns::{RData, Record, Resolve, SvcbEndpoint, rtype};
use hickory_resolver::ConnectionProvider;
use hickory_resolver::Resolver;
use hickory_resolver::net::{DnsError, NetError};
use hickory_resolver::proto::op::ResponseCode;
use hickory_resolver::proto::rr::rdata::svcb::{SVCB, SvcParamValue};
use hickory_resolver::proto::rr::{RData as Wire, RecordType};

use std::sync::Arc;
use std::time::Duration;

/// The one stream shape this crate hands back, named so the marker sits
/// on a line `cargo fmt` has no reason to reflow — the rule amendment C12
/// records about where a bound is written.
type SendRecords<'a> =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Record, Error>> + Send + 'a>>; // send-bound-exception: amendment-C15

/// The seam's type number as hickory's, or `None` for a type this crate
/// does not ask about — which is what [`Resolve::supports`] answers
/// `false` for, so the two cannot disagree.
fn wire_type(rtype: u16) -> Option<RecordType> {
    match rtype {
        rtype::A => Some(RecordType::A),
        rtype::AAAA => Some(RecordType::AAAA),
        rtype::HTTPS => Some(RecordType::HTTPS),
        _ => None,
    }
}

/// A [`Resolve`] backed by hickory.
///
/// Holds an `Arc` because the streams this trait returns must own their
/// resolver: `Resolve`'s methods take `&self`, but the stream they return
/// outlives the borrow.
#[derive(Debug)]
pub struct Hickory<P: ConnectionProvider> {
    inner: Arc<Resolver<P>>,
}

impl<P: ConnectionProvider> Clone for Hickory<P> {
    /// Hand-written: `#[derive(Clone)]` would require `P: Clone`, which is
    /// not needed — the `Arc` is what clones, and clones share one cache.
    /// Sharing the cache is the point; a per-clone cache would silently
    /// multiply upstream traffic.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<P: ConnectionProvider> Hickory<P> {
    /// Wrap an already-configured resolver.
    ///
    /// Taking a built `Resolver` rather than re-exporting hickory's builder
    /// keeps that crate's configuration surface — upstreams, DNSSEC,
    /// DoT/DoH, cache policy — out of this crate's API, where it would have
    /// to be version-tracked and would go stale.
    pub fn new(resolver: Resolver<P>) -> Self {
        Self {
            inner: Arc::new(resolver),
        }
    }

    /// The resolver underneath, for configuration this crate does not wrap.
    pub fn get_ref(&self) -> &Resolver<P> {
        &self.inner
    }

    /// Every type through one call, because the seam has one method and
    /// hickory's answer section is read the same way whatever was asked.
    ///
    /// A type this crate does not model — a `CNAME`, or anything else the
    /// answer section legitimately carries — is skipped rather than turned
    /// into an error, which is the same treatment `_ => None` gave the
    /// address path before there was one method.
    fn lookup_records(
        &self,
        name: &str,
        record_type: RecordType,
    ) -> impl Stream<Item = Result<Record, Error>> + use<P> {
        let resolver = Arc::clone(&self.inner);
        let owned = name.to_owned();
        futures_util::stream::once(async move { resolver.lookup(owned, record_type).await })
            .flat_map(|res| match res {
                Ok(lookup) => {
                    let items: Vec<_> = lookup
                        .answers()
                        .iter()
                        .filter_map(|rec| {
                            // The TTL is the **record's**, not the rdata's
                            // — hickory keeps it where DNS puts it, and so
                            // does `Record`.
                            let ttl = Some(Duration::from_secs(u64::from(rec.ttl)));
                            let rdata = match &rec.data {
                                Wire::A(a) => RData::A(a.0),
                                Wire::AAAA(a) => RData::Aaaa(a.0),
                                Wire::HTTPS(h) => RData::Https(to_endpoint(&h.0)),
                                _ => return None,
                            };
                            Some(Ok(Record::new(rdata).ttl(ttl)))
                        })
                        .collect();
                    futures_util::stream::iter(items)
                }
                // An empty result is a real answer — "asked, found none" —
                // and becomes an empty stream, the same shape as
                // `Resolve`'s default. `supports` is what keeps the two
                // distinguishable; that is the whole reason it exists.
                //
                // This rests on the `is_empty_answer` arm: hickory reports
                // NODATA as an error, so without it "this origin publishes
                // no HTTPS record" goes out as a resolver failure — on
                // nearly every origin. Do not remove the arm.
                Err(e) if is_empty_answer(&e) => futures_util::stream::iter(Vec::new()),
                Err(e) => futures_util::stream::iter(vec![Err(Error::new(ErrorKind::Resolve, e))]),
            })
    }
}

/// Whether a hickory error means "asked, and there is nothing of this type
/// here" rather than "the lookup failed".
///
/// NODATA is an ANSWER, not a failure. hickory does not hand back an empty
/// `Lookup` for it: "the name exists but has no record of this type" arrives
/// as `NoRecordsFound` with `response_code: NoError`. Reported as an error,
/// that made every host with no AAAA record — and every host with no HTTPS
/// record, which is very nearly every host — reach the caller as
/// `ErrorKind::Resolve`: a failure that never happened.
///
/// `hclient-native`'s `ResolveErrors` records failure per family, so a
/// v4-only host contributed a phantom v6 resolver failure to every request;
/// and when the v4 attempts then failed for their own reasons, the reported
/// cause blamed a resolver that had worked. An empty stream is the shape
/// `Resolve`'s own default uses, and the one `IpLiteralOnly` documents as
/// the rule: "erroring there would make every literal connection report a
/// failure it did not have."
///
/// # The response code check is load-bearing, not decoration
///
/// `NoRecordsFound` carries NXDOMAIN too. hickory-net 0.26's
/// `DnsError::from_response` folds `NXDomain | NoError if
/// !contains_answer() && !truncation` into the one variant, and
/// `NoRecords::response_code` is the only thing separating them — its own
/// doc says as much: "if `NXDOMAIN`, the domain does not exist (and no other
/// types). If `NoError`, then the domain exists but there exist either other
/// types at the same label, or subzones of that label."
///
/// So matching the variant alone would report "asked and found nothing" for
/// a domain that is not there at all — the mirror defect, the same lie in
/// the other direction, told exactly where a caller most needs the truth.
/// SERVFAIL, REFUSED and the rest never reach this function at all:
/// `from_response` turns them into `DnsError::ResponseCode`, a different
/// variant, and they stay errors without help from here.
fn is_empty_answer(e: &NetError) -> bool {
    matches!(
        e,
        NetError::Dns(DnsError::NoRecordsFound(no_records))
            if no_records.response_code == ResponseCode::NoError
    )
}

/// Maps hickory's parsed record onto this project's shape.
///
/// Drops what `SvcbEndpoint` has no home for — `mandatory`,
/// `no-default-alpn`, private keys — rather than inventing fields. A
/// parameter this client cannot act on is not made visible just because it
/// was on the wire.
fn to_endpoint(svcb: &SVCB) -> SvcbEndpoint {
    // The trailing dot on the target is included: that is the name as the
    // server sent it, and normalising it here would make the caller's own
    // comparison against a `Uri` host quietly disagree with the wire.
    let mut out = SvcbEndpoint::new(svcb.svc_priority, svcb.target_name.to_string());
    for (_, value) in &svcb.svc_params {
        match value {
            SvcParamValue::Alpn(alpn) => {
                out.alpn = alpn.0.iter().map(|s| s.as_bytes().to_vec()).collect();
            }
            SvcParamValue::Port(p) => out.port = Some(*p),
            SvcParamValue::Ipv4Hint(h) => out.ipv4hint = h.0.iter().map(|a| a.0).collect(),
            SvcParamValue::Ipv6Hint(h) => out.ipv6hint = h.0.iter().map(|a| a.0).collect(),
            SvcParamValue::EchConfigList(e) => {
                out.ech_config_list = Some(bytes::Bytes::from(e.0.clone()));
            }
            _ => {}
        }
    }
    out
}

impl<P: ConnectionProvider> Resolve for Hickory<P> {
    type Records<'a>
        = SendRecords<'a>
    // send-bound-exception: amendment-C15
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        wire_type(rtype).is_some()
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let Some(record_type) = wire_type(rtype) else {
            return Box::pin(futures_util::stream::empty());
        };
        Box::pin(self.lookup_records(name, record_type))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_resolver::proto::rr::Name;
    use hickory_resolver::proto::rr::rdata::a::A;
    use hickory_resolver::proto::rr::rdata::aaaa::AAAA;
    use hickory_resolver::proto::rr::rdata::svcb::{
        Alpn, EchConfigList, IpHint, Mandatory, SvcParamKey, SvcParamValue, Unknown,
    };
    use rstest::rstest;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::str::FromStr;

    fn svcb(params: Vec<(SvcParamKey, SvcParamValue)>) -> SVCB {
        SVCB::new(1, Name::from_str("example.com.").unwrap(), params)
    }

    /// The key a parameter is filed under on the wire.
    ///
    /// `to_endpoint` matches on the parameter's VALUE and ignores the key it
    /// was paired with, so a fixture that paired them wrongly would still
    /// pass while describing a record no server could send. Deriving the key
    /// from the value keeps every fixture honest. There is no public
    /// `SvcParamValue::key()` in hickory 0.26 to borrow.
    fn key_of(value: &SvcParamValue) -> SvcParamKey {
        match value {
            SvcParamValue::Mandatory(_) => SvcParamKey::Mandatory,
            SvcParamValue::Alpn(_) => SvcParamKey::Alpn,
            SvcParamValue::NoDefaultAlpn => SvcParamKey::NoDefaultAlpn,
            SvcParamValue::Port(_) => SvcParamKey::Port,
            SvcParamValue::Ipv4Hint(_) => SvcParamKey::Ipv4Hint,
            SvcParamValue::Ipv6Hint(_) => SvcParamKey::Ipv6Hint,
            SvcParamValue::EchConfigList(_) => SvcParamKey::EchConfigList,
            other => unreachable!("no wire key for {other:?}"),
        }
    }

    /// A record carrying exactly one parameter, filed under its own key.
    fn one(value: SvcParamValue) -> SVCB {
        svcb(vec![(key_of(&value), value)])
    }

    /// What every field of a parameterless record must look like. The cases
    /// below compare against this with `..bare()`, so each one asserts not
    /// only that its own field was filled but that no OTHER field was — which
    /// is what catches a mapping that puts the right value in the wrong home.
    /// The TTL is the record's and these fixtures build rdata, so every
    /// test that is not about the TTL passes `None` through one place.
    fn to_endpoint_no_ttl(svcb: &SVCB) -> SvcbEndpoint {
        to_endpoint(svcb)
    }

    fn bare() -> SvcbEndpoint {
        SvcbEndpoint::new(1, "example.com.".to_owned())
    }

    /// One parameter in, exactly one field out — and every other field left
    /// as a bare record leaves it.
    ///
    /// Comparing whole `SvcbEndpoint` values is safe here, and that was
    /// checked rather than assumed: every hickory type these fixtures build
    /// (`SVCB`, `SvcParamKey`, `SvcParamValue`, `Alpn`, `IpHint`,
    /// `EchConfigList`, `Mandatory`) uses a plain `#[derive(PartialEq)]` in
    /// hickory-proto 0.26, and `SvcbEndpoint` itself is derived over std
    /// types. The sibling crate `hclient-dns-system` cannot do this:
    /// `dns-message-parser`'s `ServiceParameter` compares only the key
    /// number, so `ALPN{["h2"]} == ALPN{["h3"]}` there.
    #[rstest]
    #[case::alpn_keeps_the_order_the_server_sent(
        // Neither sorted nor reverse-sorted, so a mapping that ordered the
        // list rather than preserving it cannot match by luck. The order is
        // the server's preference, and h3-before-h2 is the whole point.
        SvcParamValue::Alpn(Alpn(vec!["h3".to_owned(), "http/1.1".to_owned(), "h2".to_owned()])),
        bare().alpn(vec![b"h3".to_vec(), b"http/1.1".to_vec(), b"h2".to_vec()]),
    )]
    #[case::port(SvcParamValue::Port(8443), bare().port(Some(8443)))]
    #[case::port_zero_is_a_port_that_was_sent_not_an_absence(
        SvcParamValue::Port(0),
        bare().port(Some(0)),
    )]
    #[case::ipv4hint_keeps_every_address_in_order(
        SvcParamValue::Ipv4Hint(IpHint(vec![
            A(Ipv4Addr::new(192, 0, 2, 1)),
            A(Ipv4Addr::new(198, 51, 100, 2)),
        ])),
        bare().ipv4hint(vec![Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(198, 51, 100, 2)]),
    )]
    #[case::ipv6hint_keeps_every_address_in_order(
        SvcParamValue::Ipv6Hint(IpHint(vec![
            AAAA(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            AAAA(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2)),
        ])),
        bare().ipv6hint(vec![
                Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
                Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2),
            ]),
    )]
    #[case::ech_passes_through_byte_for_byte(
        // Leading and trailing zero bytes: this is an opaque blob handed
        // straight to rustls, and anything that trimmed or truncated it
        // would produce a config that fails only at handshake time.
        SvcParamValue::EchConfigList(EchConfigList(vec![0x00, 0x45, 0xFE, 0x00, 0x00])),
        bare().ech_config_list(Some(bytes::Bytes::from_static(&[0x00, 0x45, 0xFE, 0x00, 0x00]))),
    )]
    #[case::no_default_alpn_has_no_field_and_disturbs_none(SvcParamValue::NoDefaultAlpn, bare())]
    #[case::mandatory_has_no_field_and_disturbs_none(
        SvcParamValue::Mandatory(Mandatory(vec![SvcParamKey::Alpn])),
        bare(),
    )]
    fn one_parameter_fills_exactly_the_field_it_belongs_in(
        #[case] value: SvcParamValue,
        #[case] expected: SvcbEndpoint,
    ) {
        assert_eq!(to_endpoint(&one(value)), expected);
    }

    /// AliasMode is SvcPriority 0 (RFC 9460 §2.4.2). It has to arrive as 0:
    /// a mapping that treated 0 as "unset" would present every alias record
    /// as a ServiceMode one, and the caller would try to connect to a target
    /// that is only meant to be followed.
    #[test]
    fn priority_zero_arrives_as_zero_rather_than_as_a_default() {
        let alias = SVCB::new(
            0,
            Name::from_str("example.net.").unwrap(),
            vec![(SvcParamKey::Port, SvcParamValue::Port(8443))],
        );
        let e = to_endpoint(&alias);
        assert_eq!(e.priority, 0, "0 is AliasMode, not a missing priority");
        assert_eq!(e.target, "example.net.");
    }

    /// RFC 9460 §2.5.2: a ServiceMode target of `.` means "use the owner
    /// name". Resolving that is the caller's business — this crate hands
    /// over what the server sent, and `.` must survive as `.` rather than
    /// collapsing to an empty string that no comparison would match.
    #[test]
    fn a_root_target_survives_as_the_root() {
        assert_eq!(
            to_endpoint(&SVCB::new(1, Name::root(), Vec::new())).target,
            "."
        );
    }

    /// A key from the private-use range carries an `Unknown` value this crate
    /// has no branch for. It must be stepped over — and, more than that, the
    /// parameters AFTER it must still be read. A `_ => return out` would pass
    /// any test that put the unknown key last.
    #[test]
    fn an_unknown_parameter_is_stepped_over_and_the_rest_of_the_record_still_maps() {
        let e = to_endpoint_no_ttl(&svcb(vec![
            (
                SvcParamKey::Alpn,
                SvcParamValue::Alpn(Alpn(vec!["h2".to_owned()])),
            ),
            (
                SvcParamKey::Key(65280),
                SvcParamValue::Unknown(Unknown(vec![0x09, 0x09])),
            ),
            (SvcParamKey::Port, SvcParamValue::Port(8443)),
        ]));

        assert_eq!(e.alpn, vec![b"h2".to_vec()]);
        assert_eq!(
            e.port,
            Some(8443),
            "the parameter after the unknown one is still read — an unknown key \
             narrows the record, it does not truncate it"
        );
    }

    /// Every parameter this project has a field for is carried across, and
    /// each is asserted on its VALUE rather than on its presence — a
    /// mapping that put the right key in the wrong field would satisfy a
    /// presence check.
    #[test]
    fn every_mapped_parameter_arrives_with_its_value() {
        let e = to_endpoint_no_ttl(&svcb(vec![
            (
                SvcParamKey::Alpn,
                SvcParamValue::Alpn(Alpn(vec!["h3".to_owned(), "h2".to_owned()])),
            ),
            (SvcParamKey::Port, SvcParamValue::Port(8443)),
            (
                SvcParamKey::Ipv4Hint,
                SvcParamValue::Ipv4Hint(IpHint(vec![A(Ipv4Addr::new(192, 0, 2, 1))])),
            ),
            (
                SvcParamKey::Ipv6Hint,
                SvcParamValue::Ipv6Hint(IpHint(vec![AAAA(Ipv6Addr::new(
                    0x2001, 0xdb8, 0, 0, 0, 0, 0, 1,
                ))])),
            ),
            (
                SvcParamKey::EchConfigList,
                SvcParamValue::EchConfigList(EchConfigList(vec![0xAB, 0xCD])),
            ),
        ]));

        assert_eq!(e.priority, 1);
        assert_eq!(e.target, "example.com.");
        assert_eq!(
            e.alpn,
            vec![b"h3".to_vec(), b"h2".to_vec()],
            "ALPN order is the server's and must not be reordered — it is a preference list"
        );
        assert_eq!(e.port, Some(8443));
        assert_eq!(e.ipv4hint, vec![Ipv4Addr::new(192, 0, 2, 1)]);
        assert_eq!(
            e.ipv6hint,
            vec![Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)]
        );
        assert_eq!(
            e.ech_config_list.as_deref(),
            Some(&[0xAB, 0xCD][..]),
            "the ECH config feeds rustls directly; a truncated or empty one is worse than none"
        );
    }

    /// A record with no parameters is a valid record, not a failure — and
    /// every optional field must come back empty rather than defaulted to
    /// something plausible.
    #[test]
    fn a_bare_record_maps_to_empty_fields_not_to_guesses() {
        let e = to_endpoint_no_ttl(&svcb(vec![]));
        assert_eq!(e.priority, 1);
        assert!(e.alpn.is_empty());
        assert_eq!(e.port, None, "no port means no port, not 443");
        assert!(e.ipv4hint.is_empty());
        assert!(e.ipv6hint.is_empty());
        assert_eq!(e.ech_config_list, None);
    }

    /// The trailing dot is kept deliberately: it is what the server sent,
    /// and stripping it here would make a caller's comparison against a
    /// `Uri` host disagree with the wire in one direction only.
    #[test]
    fn the_target_keeps_the_form_the_server_sent() {
        assert_eq!(to_endpoint_no_ttl(&svcb(vec![])).target, "example.com.");
    }

    /// A parameter with no home in `SvcbEndpoint` is dropped, not
    /// smuggled into a field that nearly fits.
    #[test]
    fn an_unmapped_parameter_is_dropped_rather_than_misfiled() {
        let e = to_endpoint_no_ttl(&svcb(vec![(
            SvcParamKey::Mandatory,
            SvcParamValue::Mandatory(Mandatory(vec![SvcParamKey::Alpn])),
        )]));
        assert!(e.alpn.is_empty() && e.port.is_none() && e.ech_config_list.is_none());
    }
}
