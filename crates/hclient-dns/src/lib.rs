//! Pluggable name resolution.
//!
//! Separate streams per address family, not a `Vec<SocketAddr>`: RFC 8305
//! requires starting to connect over AAAA without waiting for A —
//! `hclient-proto::happy_eyeballs::Scheduler` is fed results as
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
//! The tempting answer is the connector — `hclient-native::connect`, the
//! place where results actually reach `Scheduler::offer_v4`/`offer_v6`,
//! and `Scheduler` says the same thing from its own side of the seam:
//! "sorting is the caller's concern, before `offer_*`; it isn't done
//! here". That promise cannot be kept in the form stated: the full rule
//! requires
//! Source Address Selection (RFC 6724 Rule 1 onward) — knowledge of which
//! local address the OS would actually use to connect to a given
//! destination, i.e. access to the routing table, which NONE of this
//! vertical's traits (`Resolve`, `TcpConnect`, `Timer`) provide. A partial
//! implementation (only the rules that don't need Source Address
//! Selection) would be worse than none at all: it would look like RFC
//! 6724 compliance without being one — the same principle that split
//! `RedirectSupport::None`/`Transparent` in `hclient-core` and
//! `supports_svcb()`/the empty stream below: a capability that lies about
//! its own state is worse than a capability that's simply absent.
//!
//! So, as things stand today: each family's addresses go into
//! `Scheduler::offer_v4`/`offer_v6` in the SAME order the resolver handed
//! them out — neither `Resolve`, nor `Scheduler`, nor
//! `hclient_native::connect` (see its doc comment, the "RFC 6724 ... NOT
//! implemented here" section) sort them. This is a recorded, explicitly
//! named gap, not an oversight. Closing it would first require introducing a separate Source Address Selection
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
//! `hclient-core`: a capability that lies about its own absence or its own
//! presence is worse than a capability that's simply absent).
//! `supports_svcb()` is a separate entry point for that distinction: a
//! resolver that can't do SVCB leaves it at the default `false` and
//! inherits the default `lookup_svcb`; a resolver that can must override
//! BOTH methods together — overriding only `lookup_svcb` would conflate
//! "can't" with "can, and found nothing" all over again for anyone who
//! reads only `supports_svcb()`.
#![forbid(unsafe_code)]

mod error;
mod overrides;
pub mod svcb;

pub use overrides::{Answer, Overrides};

use bytes::Bytes;
use futures_core::Stream;
use hclient_core::Error;
use std::marker::PhantomData;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// The two stream shapes this crate hands back, named so the marker sits
/// on a line `cargo fmt` has no reason to reflow — the rule amendment C12
/// records about where a bound is written.
type SendAddrs<'a> =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, Error>> + Send + 'a>>; // send-bound-exception: amendment-C15

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
#[non_exhaustive]
pub struct SvcbEndpoint {
    pub priority: u16,
    pub target: String,
    pub alpn: Vec<Vec<u8>>,
    pub port: Option<u16>,
    pub ipv4hint: Vec<Ipv4Addr>,
    pub ipv6hint: Vec<Ipv6Addr>,
    pub ech_config_list: Option<Bytes>,
}

impl SvcbEndpoint {
    /// The two fields an HTTPS record cannot be without: RFC 9460's
    /// `SvcPriority` and `TargetName`. Everything else is a SvcParam,
    /// which is by definition optional — and a resolver that learns to
    /// parse a new one should not break every consumer, which is what the
    /// setters are for.
    #[must_use]
    pub fn new(priority: u16, target: String) -> Self {
        Self {
            priority,
            target,
            alpn: Vec::new(),
            port: None,
            ipv4hint: Vec::new(),
            ipv6hint: Vec::new(),
            ech_config_list: None,
        }
    }

    /// RFC 9460's `SvcPriority`. A setter as well as a `new` parameter,
    /// because a caller deriving one record from another overrides it —
    /// which is what `..base` used to express and what these replace.
    #[must_use]
    pub fn priority(mut self, priority: u16) -> Self {
        self.priority = priority;
        self
    }

    /// RFC 9460's `TargetName`, as the server sent it — trailing dot
    /// included, since normalising here would make a caller's comparison
    /// against a `Uri` host quietly disagree with the wire.
    #[must_use]
    pub fn target(mut self, target: String) -> Self {
        self.target = target;
        self
    }

    /// The `alpn` SvcParam — the protocols the origin says it speaks.
    #[must_use]
    pub fn alpn(mut self, alpn: Vec<Vec<u8>>) -> Self {
        self.alpn = alpn;
        self
    }

    /// The `port` SvcParam.
    #[must_use]
    pub fn port(mut self, port: Option<u16>) -> Self {
        self.port = port;
        self
    }

    /// The `ipv4hint` SvcParam.
    #[must_use]
    pub fn ipv4hint(mut self, hints: Vec<Ipv4Addr>) -> Self {
        self.ipv4hint = hints;
        self
    }

    /// The `ipv6hint` SvcParam.
    #[must_use]
    pub fn ipv6hint(mut self, hints: Vec<Ipv6Addr>) -> Self {
        self.ipv6hint = hints;
        self
    }

    /// The `ech` SvcParam, carried verbatim. A connector passes it on only
    /// to a TLS backend that says it applies one — see
    /// `TlsConnect::applies_ech`, and the measurement behind that gate.
    #[must_use]
    pub fn ech_config_list(mut self, ech: Option<Bytes>) -> Self {
        self.ech_config_list = ech;
        self
    }
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

/// What a resolver with no SVCB answer names as its
/// [`Resolve::Svcb`].
///
/// It exists because associated type defaults are unstable: the method
/// used to be defaulted to an empty stream and now the type has to be
/// named, so this is the name. `Resolve`'s own doc explains why the
/// emptiness is indistinguishable from *asked and found nothing* on
/// purpose, and why [`Resolve::supports_svcb`] is the distinction.
pub struct NoSvcb(EmptyStream<SvcbEndpoint>);

impl NoSvcb {
    /// The only way to make one.
    #[must_use]
    pub fn new() -> Self {
        Self(EmptyStream::new())
    }
}

impl Default for NoSvcb {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for NoSvcb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NoSvcb")
    }
}

impl Stream for NoSvcb {
    type Item = Result<SvcbEndpoint, Error>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(None)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(0))
    }
}

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
    /// **Associated types, not RPITITs**, for the reason
    /// `hclient_rt::TcpConnect::Connecting` gives at length: a consumer
    /// that must prove its own future `Send` — `hclient-native`, so that
    /// `hclient::Client`'s request future can be — has to *name* these,
    /// and `impl Stream` has no name. Naming is not requiring: each
    /// resolver still says for itself, and one whose answers cannot cross
    /// a thread writes a box without the `Send` and is a `Resolve` like
    /// any other.
    ///
    /// The lifetime is `&self`'s alone, so `name` is **not** borrowed by
    /// the stream — every implementor here already owned it.
    type Ipv4<'a>: Stream<Item = Result<ResolvedAddr, Error>>
    where
        Self: 'a;
    /// [`Ipv4`](Self::Ipv4)'s counterpart; see [`lookup_ipv6`](Self::lookup_ipv6)
    /// for why the two are separate streams rather than one.
    type Ipv6<'a>: Stream<Item = Result<ResolvedAddr, Error>>
    where
        Self: 'a;
    /// **Not defaulted, unlike the method it used to accompany.**
    /// Associated type defaults are unstable, so a resolver that has no
    /// SVCB answer names [`NoSvcb`] here and forwards to it — two lines,
    /// and the pair still has to be written together, which is what
    /// [`supports_svcb`](Self::supports_svcb)'s own doc already asked for.
    type Svcb<'a>: Stream<Item = Result<SvcbEndpoint, Error>>
    where
        Self: 'a;

    fn lookup_ipv4<'a>(&'a self, name: &str) -> Self::Ipv4<'a>;
    /// AAAA records. A separate stream from `lookup_ipv4`, not a variant
    /// of one enum and not a shared `Vec` — RFC 8305 §3/§4 requires
    /// starting IPv6 attempts without waiting for the IPv4 answer, and
    /// separate streams are the only shape that allows this without extra
    /// parsing on the caller's side.
    fn lookup_ipv6<'a>(&'a self, name: &str) -> Self::Ipv6<'a>;

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
    fn lookup_svcb<'a>(&'a self, name: &str) -> Self::Svcb<'a>;
}

/// A [`Resolve`] that performs no name resolution: it accepts IP literals
/// and refuses everything else.
///
/// For constrained targets that have `std` but no room for a resolver — or
/// no permission to make DNS queries. `http://192.0.2.1:8080/` and
/// `http://[2001:db8::1]/` work; `http://example.com/` fails with a typed
/// error naming what is missing, rather than hanging or silently resolving
/// to nothing.
///
/// It is honest in both directions, which is the point. A name that is not
/// a literal produces an **error**, never an empty stream: an empty stream
/// from a resolver means "asked, found nothing", and this resolver never
/// asked. And a literal of the wrong family produces an empty stream rather
/// than an error, because `4` and `6` are queried in parallel per RFC 8305
/// and "there is no AAAA for this v4 literal" is a true, unremarkable
/// answer — erroring there would make every literal connection report a
/// failure it did not have.
///
/// `supports_svcb()` stays `false` by inheriting the trait default, so ECH
/// and h3 discovery correctly read as unavailable.
#[derive(Debug, Clone, Copy, Default)]
pub struct IpLiteralOnly;

impl IpLiteralOnly {
    /// `http::Uri::host()` returns an IPv6 literal WITH its brackets
    /// (`[::1]`), and that is the string this trait receives, so the
    /// brackets are stripped here. Without this, every IPv6 literal would
    /// be rejected as "not a literal" — the failure would look like a
    /// resolver limitation rather than a parsing bug.
    fn literal(name: &str) -> Option<IpAddr> {
        let bare = name
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(name);
        bare.parse::<IpAddr>().ok()
    }

    fn not_a_literal(name: &str) -> Error {
        Error::new(
            hclient_core::ErrorKind::Resolve,
            std::io::Error::other(format!(
                "this client was built without a resolver (IpLiteralOnly); `{name}` is not an IP literal"
            )),
        )
    }
}

/// Yields at most one item, then ends. Enough for a resolver that answers
/// from the name itself.
#[derive(Debug)]
struct OnceStream<T>(Option<T>);

impl<T: Unpin> Stream for OnceStream<T> {
    type Item = T;
    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<T>> {
        Poll::Ready(self.0.take())
    }
}

impl Resolve for IpLiteralOnly {
    type Svcb<'a>
        = crate::NoSvcb
    where
        Self: 'a;

    fn lookup_svcb<'a>(&'a self, _name: &str) -> Self::Svcb<'a> {
        crate::NoSvcb::new()
    }

    type Ipv4<'a>
        = SendAddrs<'a>
    // send-bound-exception: amendment-C15
    where
        Self: 'a;

    fn lookup_ipv4<'a>(&'a self, name: &str) -> Self::Ipv4<'a> {
        Box::pin({
            OnceStream(match Self::literal(name) {
                Some(IpAddr::V4(a)) => Some(Ok(ResolvedAddr {
                    addr: IpAddr::V4(a),
                    ttl: None,
                })),
                // A v6 literal has no A record, and that is not an error.
                Some(IpAddr::V6(_)) => None,
                None => Some(Err(Self::not_a_literal(name))),
            })
        })
    }

    type Ipv6<'a>
        = SendAddrs<'a>
    // send-bound-exception: amendment-C15
    where
        Self: 'a;

    fn lookup_ipv6<'a>(&'a self, name: &str) -> Self::Ipv6<'a> {
        Box::pin({
            OnceStream(match Self::literal(name) {
                Some(IpAddr::V6(a)) => Some(Ok(ResolvedAddr {
                    addr: IpAddr::V6(a),
                    ttl: None,
                })),
                Some(IpAddr::V4(_)) => None,
                None => Some(Err(Self::not_a_literal(name))),
            })
        })
    }
}

// Tests live in `tests/`, not here: `ip_literal_only.rs` (the two
// directions `IpLiteralOnly` has to keep apart, as a case table),
// `svcb_capability.rs` (`supports_svcb`/`lookup_svcb` and the distinction
// they carry between them), `svcb_endpoint.rs` (what a consumer can rely on
// once a record crosses the seam) and `resolve_streams.rs` (what returning
// a `Stream` per family promises). Every one of them reaches this crate
// only through its public API, so there is nothing left for a `#[cfg(test)]`
// module inside `src` to see that an integration test cannot.
