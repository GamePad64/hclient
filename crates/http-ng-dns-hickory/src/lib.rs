//! [`Resolve`] over `hickory-resolver`: the backend that can actually
//! answer SVCB.
//!
//! `http-ng-dns-system` goes through the platform resolver, which on Unix
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
//! so nothing below `http-ng` names a runtime — `http-ng-dns-system` needs
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
//! `ResolvedAddr::ttl` is `Option<Duration>` because `getaddrinfo` does not
//! expose one — the system backend returns `None` and is right to. hickory
//! parses records itself, so the TTL is present, and every address carries
//! its own rather than the RRset's minimum: a caller doing Happy Eyeballs
//! or its own caching wants the value the server actually sent.
#![forbid(unsafe_code)]

use futures_core::Stream;
use futures_util::StreamExt;
use hickory_resolver::ConnectionProvider;
use hickory_resolver::Resolver;
use hickory_resolver::proto::rr::rdata::svcb::{SVCB, SvcParamValue};
use hickory_resolver::proto::rr::{RData, RecordType};
use http_ng_core::{Error, ErrorKind};
use http_ng_dns::{Resolve, ResolvedAddr, SvcbEndpoint};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

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

    fn lookup_ips(
        &self,
        name: &str,
        record_type: RecordType,
    ) -> impl Stream<Item = Result<ResolvedAddr, Error>> + use<P> {
        let resolver = Arc::clone(&self.inner);
        let owned = name.to_owned();
        futures_util::stream::once(async move { resolver.lookup(owned, record_type).await })
            .flat_map(|res| match res {
                Ok(lookup) => {
                    let items: Vec<_> = lookup
                        .answers()
                        .iter()
                        .filter_map(|rec| {
                            let ttl = Duration::from_secs(u64::from(rec.ttl));
                            let addr: IpAddr = match &rec.data {
                                RData::A(a) => IpAddr::V4(a.0),
                                RData::AAAA(a) => IpAddr::V6(a.0),
                                // A CNAME, or anything else the answer
                                // section legitimately carries, is not an
                                // address: skipped rather than turned into
                                // an error.
                                _ => return None,
                            };
                            Some(Ok(ResolvedAddr {
                                addr,
                                ttl: Some(ttl),
                            }))
                        })
                        .collect();
                    futures_util::stream::iter(items)
                }
                Err(e) => futures_util::stream::iter(vec![Err(Error::new(ErrorKind::Resolve, e))]),
            })
    }
}

/// Maps hickory's parsed record onto this project's shape.
///
/// Drops what `SvcbEndpoint` has no home for — `mandatory`,
/// `no-default-alpn`, private keys — rather than inventing fields. A
/// parameter this client cannot act on is not made visible just because it
/// was on the wire.
fn to_endpoint(svcb: &SVCB) -> SvcbEndpoint {
    let mut out = SvcbEndpoint {
        priority: svcb.svc_priority,
        // Trailing dot included: that is the name as the server sent it,
        // and normalising it here would make the caller's own comparison
        // against a `Uri` host quietly disagree with the wire.
        target: svcb.target_name.to_string(),
        alpn: Vec::new(),
        port: None,
        ipv4hint: Vec::new(),
        ipv6hint: Vec::new(),
        ech_config_list: None,
    };
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
    fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        self.lookup_ips(name, RecordType::A)
    }

    fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        self.lookup_ips(name, RecordType::AAAA)
    }

    /// `true`, unconditionally — and overridden together with
    /// `lookup_svcb` below, which is the only way `Resolve` permits either.
    /// Unlike the system backend, there is no target on which this can
    /// fail to work: hickory parses the record itself rather than asking a
    /// platform API that may or may not know the type.
    fn supports_svcb(&self) -> bool {
        true
    }

    fn lookup_svcb(&self, name: &str) -> impl Stream<Item = Result<SvcbEndpoint, Error>> {
        let resolver = Arc::clone(&self.inner);
        let owned = name.to_owned();
        futures_util::stream::once(async move { resolver.lookup(owned, RecordType::HTTPS).await })
            .flat_map(|res| match res {
                // An empty result is a real answer — "asked, found none" —
                // and becomes an empty stream, the same shape as `Resolve`'s
                // default. `supports_svcb()` above is what keeps the two
                // distinguishable; that is the whole reason it exists.
                Ok(lookup) => {
                    let items: Vec<_> = lookup
                        .answers()
                        .iter()
                        .filter_map(|rec| match &rec.data {
                            RData::HTTPS(h) => Some(Ok(to_endpoint(&h.0))),
                            _ => None,
                        })
                        .collect();
                    futures_util::stream::iter(items)
                }
                Err(e) => futures_util::stream::iter(vec![Err(Error::new(ErrorKind::Resolve, e))]),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_resolver::proto::rr::Name;
    use hickory_resolver::proto::rr::rdata::a::A;
    use hickory_resolver::proto::rr::rdata::aaaa::AAAA;
    use hickory_resolver::proto::rr::rdata::svcb::{
        Alpn, EchConfigList, IpHint, Mandatory, SvcParamKey, SvcParamValue,
    };
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::str::FromStr;

    fn svcb(params: Vec<(SvcParamKey, SvcParamValue)>) -> SVCB {
        SVCB::new(1, Name::from_str("example.com.").unwrap(), params)
    }

    /// Every parameter this project has a field for is carried across, and
    /// each is asserted on its VALUE rather than on its presence — a
    /// mapping that put the right key in the wrong field would satisfy a
    /// presence check.
    #[test]
    fn every_mapped_parameter_arrives_with_its_value() {
        let e = to_endpoint(&svcb(vec![
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
        let e = to_endpoint(&svcb(vec![]));
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
        assert_eq!(to_endpoint(&svcb(vec![])).target, "example.com.");
    }

    /// A parameter with no home in `SvcbEndpoint` is dropped, not
    /// smuggled into a field that nearly fits.
    #[test]
    fn an_unmapped_parameter_is_dropped_rather_than_misfiled() {
        let e = to_endpoint(&svcb(vec![(
            SvcParamKey::Mandatory,
            SvcParamValue::Mandatory(Mandatory(vec![SvcParamKey::Alpn])),
        )]));
        assert!(e.alpn.is_empty() && e.port.is_none() && e.ech_config_list.is_none());
    }
}
