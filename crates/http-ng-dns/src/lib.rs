//! Pluggable name resolution.
//!
//! Separate streams per address family, not a `Vec<SocketAddr>`: RFC 8305
//! requires starting to connect over AAAA without waiting for A —
//! `http-ng-proto::happy_eyeballs::Scheduler` (Task 5) is fed results as
//! they arrive, not as one block once the resolver has finished both
//! families. This is the sole reason `Resolve` returns a `Stream` rather
//! than a `Future<Output = Vec<_>>`: nothing in the trait forces the
//! caller to wait for the stream to end or collect it into a `Vec` before
//! starting to connect to the first address.
//!
//! **Ordering guarantee within a stream: there isn't one.** `Resolve`
//! makes no promise that addresses of one family come in RFC 6724 §6
//! (Destination Address Selection) order — the resolver is free to hand
//! them out in DNS-response order, cache order, or any other order.
//!
//! **RFC 6724 §6 sorting is nobody's job today, not "the caller's job."**
//! The first version of this paragraph called it the connector's
//! responsibility (`http-ng-native::connect`, Task 11) — i.e. the place
//! where results actually reach `Scheduler::offer_v4`/`offer_v6`
//! (`Scheduler`, on its own side of the seam, says the same thing:
//! "sorting is the caller's concern, before `offer_*`; it isn't done
//! here"). Checking before Task 11 was implemented showed that this
//! promise can't be kept in the form stated: the full rule requires
//! Source Address Selection (RFC 6724 Rule 1 onward) — knowledge of which
//! local address the OS would actually use to connect to a given
//! destination, i.e. access to the routing table, which NONE of this
//! vertical's traits (`Resolve`, `TcpConnect`, `Timer`) provide. A partial
//! implementation (only the rules that don't need Source Address
//! Selection) would be worse than none at all: it would look like RFC
//! 6724 compliance without being one — the same principle that split
//! `RedirectSupport::None`/`Transparent` in `http-ng-core` and
//! `supports_svcb()`/the empty stream below: a capability that lies about
//! its own state is worse than a capability that's simply absent.
//!
//! So, as things stand today: each family's addresses go into
//! `Scheduler::offer_v4`/`offer_v6` in the SAME order the resolver handed
//! them out — neither `Resolve`, nor `Scheduler`, nor
//! `http_ng_native::connect` (see its doc comment, the "RFC 6724 ... NOT
//! implemented here" section) sort them. This is a recorded, explicitly
//! named gap, not an oversight — see the §9 "What we explicitly don't do"
//! table in `docs/superpowers/specs/2026-08-05-http-ng-design.md`. Closing
//! it would first require introducing a separate Source Address Selection
//! capability, which no trait has today.
//!
//! **SVCB is a capability, not a fact.** `lookup_svcb` carries a default
//! body that returns an empty stream — otherwise `getaddrinfo`,
//! `wasi:http`, and embedded resolvers couldn't implement the trait at
//! all, even though none of them perform a real SVCB/HTTPS query. But an
//! empty stream is ambiguous on its own: it could mean either "this
//! resolver can't do SVCB" or "the resolver asked and got zero records" —
//! two different things the caller isn't obligated to conflate (the same
//! principle that split `RedirectSupport::None` and `Transparent` in
//! `http-ng-core`: a capability that lies about its own absence or its own
//! presence is worse than a capability that's simply absent).
//! `supports_svcb()` is a separate entry point for that distinction: a
//! resolver that can't do SVCB leaves it at the default `false` and
//! inherits the default `lookup_svcb`; a resolver that can must override
//! BOTH methods together — overriding only `lookup_svcb` would conflate
//! "can't" with "can, and found nothing" all over again for anyone who
//! reads only `supports_svcb()`.
#![forbid(unsafe_code)]

use bytes::Bytes;
use futures_core::Stream;
use http_ng_core::Error;
use std::marker::PhantomData;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAddr {
    pub addr: IpAddr,
    pub ttl: Option<Duration>,
}

/// RFC 9460 HTTPS/SVCB. `alpn` provides h3 discovery without Alt-Svc,
/// `ech_config_list` feeds `rustls::EchConfig` directly.
///
/// Built in from day one: pinning the resolver to `SocketAddr` would close
/// off ECH and h3 discovery permanently, short of a breaking change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvcbEndpoint {
    pub priority: u16,
    pub target: String,
    pub alpn: Vec<Vec<u8>>,
    pub port: Option<u16>,
    pub ipv4hint: Vec<Ipv4Addr>,
    pub ipv6hint: Vec<Ipv6Addr>,
    pub ech_config_list: Option<Bytes>,
}

/// A stream that immediately says "nothing here."
///
/// Exists so `Resolve::lookup_svcb` can have a default body without
/// dragging `futures-util` into the library's dependencies: the only
/// thing needed from there was `stream::empty()`. `futures-core` supplies
/// one crate (the `Stream` trait itself); `futures-util` pulls in four
/// more (`futures-task`, `pin-project-lite`, `slab`, and itself) for the
/// sake of one function whose body is eight lines below. `futures-util`
/// is still needed by this crate's tests (`stream::iter`, `StreamExt`)
/// and lives in `[dev-dependencies]`, where it doesn't reach a
/// consumer's dependency graph.
struct EmptyStream<T>(PhantomData<T>);

impl<T> EmptyStream<T> {
    const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> Stream for EmptyStream<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(None)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(0))
    }
}

pub trait Resolve {
    /// A records. Each stream item is independent: an error on one is not
    /// required to stop the rest (for example, a resolver with multiple
    /// upstreams may report a partial failure and keep going).
    fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>>;
    /// AAAA records. A separate stream from `lookup_ipv4`, not a variant
    /// of one enum and not a shared `Vec` — RFC 8305 §3/§4 requires
    /// starting IPv6 attempts without waiting for the IPv4 answer, and
    /// separate streams are the only shape that allows this without extra
    /// parsing on the caller's side.
    fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>>;

    /// Whether the resolver can do SVCB/HTTPS queries at all.
    ///
    /// The `false` default pairs with the default `lookup_svcb` below:
    /// together they say "this capability is absent," not "present, but
    /// found zero records." A resolver that gives a real answer for SVCB
    /// must override both methods together.
    fn supports_svcb(&self) -> bool {
        false
    }

    /// SVCB/HTTPS records (RFC 9460). The default is an empty stream:
    /// without it, a `getaddrinfo` wrapper, `wasi:http`, and embedded
    /// resolvers that have no access to raw DNS records couldn't
    /// implement the trait at all. The empty stream from the default and
    /// an empty stream from a resolver that genuinely asked for SVCB and
    /// found nothing are indistinguishable at this level on purpose — the
    /// distinction is carried by `supports_svcb()` above; a caller that
    /// cares about the difference must ask it, rather than inferring an
    /// answer from the stream's emptiness.
    fn lookup_svcb(&self, _name: &str) -> impl Stream<Item = Result<SvcbEndpoint, Error>> {
        EmptyStream::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    struct Static;
    impl Resolve for Static {
        fn lookup_ipv4(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(vec![Ok(ResolvedAddr {
                addr: "127.0.0.1".parse().unwrap(),
                ttl: None,
            })])
        }
        fn lookup_ipv6(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::empty()
        }
        // lookup_svcb / supports_svcb deliberately not implemented — the
        // defaults must work without them.
    }

    #[test]
    fn svcb_has_a_default_returning_empty() {
        let got: Vec<_> = futures_executor::block_on(Static.lookup_svcb("x").collect());
        assert!(
            got.is_empty(),
            "otherwise getaddrinfo, wasi, and embedded couldn't implement the trait"
        );
    }

    #[test]
    fn svcb_default_capability_is_false() {
        // The `lookup_svcb` default (empty) and the `supports_svcb`
        // default (false) must agree in meaning: an empty stream without
        // this method would be a lie by default, not an absent answer.
        assert!(
            !Static.supports_svcb(),
            "the default must explicitly say \"can't do this\", not stay silent about it"
        );
    }

    #[test]
    fn families_are_separate_streams() {
        let v4: Vec<_> = futures_executor::block_on(Static.lookup_ipv4("x").collect());
        let v6: Vec<_> = futures_executor::block_on(Static.lookup_ipv6("x").collect());
        assert_eq!(v4.len(), 1);
        assert_eq!(
            v6.len(),
            0,
            "must be able to connect over AAAA without waiting for A"
        );
    }

    struct Two;
    impl Resolve for Two {
        fn lookup_ipv4(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(vec![
                Ok(ResolvedAddr {
                    addr: "10.0.0.1".parse().unwrap(),
                    ttl: None,
                }),
                Ok(ResolvedAddr {
                    addr: "10.0.0.2".parse().unwrap(),
                    ttl: None,
                }),
            ])
        }
        fn lookup_ipv6(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::empty()
        }
    }

    #[test]
    fn items_are_consumable_one_at_a_time_without_collecting() {
        // A stream, not a Vec: the caller can take the first address and
        // (in a real connector) start connecting without waiting for the
        // second one and without calling `.collect()` on the whole stream.
        let mut s = std::pin::pin!(Two.lookup_ipv4("x"));
        let first = futures_executor::block_on(s.next()).unwrap().unwrap();
        assert_eq!(first.addr, "10.0.0.1".parse::<IpAddr>().unwrap());
        // The second item is still sitting in the stream, untouched by the
        // first `.next()` — proof that taking the first didn't consume the
        // rest.
        let second = futures_executor::block_on(s.next()).unwrap().unwrap();
        assert_eq!(second.addr, "10.0.0.2".parse::<IpAddr>().unwrap());
    }

    struct WithSvcb;
    impl Resolve for WithSvcb {
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
            futures_util::stream::iter(vec![Ok(SvcbEndpoint {
                priority: 1,
                target: "svc.example".into(),
                alpn: vec![b"h3".to_vec()],
                port: Some(443),
                ipv4hint: vec![],
                ipv6hint: vec![],
                ech_config_list: None,
            })])
        }
    }

    #[test]
    fn a_resolver_implementing_svcb_reports_the_capability_and_the_data_together() {
        // The distinction from `supports_svcb`'s doc comment works both
        // ways: a resolver that can do SVCB must declare it via
        // `supports_svcb()` AND return real records via `lookup_svcb`.
        assert!(WithSvcb.supports_svcb());
        let got: Vec<_> = futures_executor::block_on(WithSvcb.lookup_svcb("x").collect());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_ref().unwrap().target, "svc.example");
    }
}
