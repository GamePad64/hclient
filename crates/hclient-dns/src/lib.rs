#![no_std]
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

//! # `#![no_std]`, unconditionally
//!
//! The third crate on the device path, after `hclient-core` and
//! `hclient-proto` — [`IpLiteralOnly`] is what
//! `Native<Embassy, NoTls, IpLiteralOnly>` resolves with, so a bare-metal
//! build needs this crate and not the other resolvers.
//!
//! The attribute is a guard on **this crate's source** and nothing more:
//! `std` leaves the extern prelude, so a `std::` path here is a compile
//! error on an ordinary host `cargo check`. It is not a claim that the
//! crate links on a device — `http` still stands in the way, through
//! `hclient-core`. See `docs/no-std.md`.
//!
//! The `codec` feature is the one a device turns off: `dns-message-parser`
//! is `std`, and a build resolving through [`IpLiteralOnly`] never decodes
//! a DNS message.

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod svcb;

use alloc::string::String;
use alloc::vec::Vec;
use bytes::Bytes;
use core::marker::PhantomData;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;
use futures_core::Stream;
use hclient_core::Error;

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

/// The source behind [`IpLiteralOnly`]'s refusal.
///
/// It was `std::io::Error::other(format!(..))`, which is `io::Error` used
/// as a box for a string — there is no I/O here and nothing downstream ever
/// downcast to it. A named type says what happened, and it is what lets
/// this crate be `#![no_std]`: `Error::new` wants
/// `core::error::Error + Send + Sync`, which `io::Error` is not without
/// `std` and this is.
#[derive(Debug, thiserror::Error)]
#[error("this client was built without a resolver (IpLiteralOnly); `{0}` is not an IP literal")]
pub struct NotALiteral(pub alloc::string::String);

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
        Error::new(hclient_core::ErrorKind::Resolve, NotALiteral(name.into()))
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
    fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        OnceStream(match Self::literal(name) {
            Some(IpAddr::V4(a)) => Some(Ok(ResolvedAddr {
                addr: IpAddr::V4(a),
                ttl: None,
            })),
            // A v6 literal has no A record, and that is not an error.
            Some(IpAddr::V6(_)) => None,
            None => Some(Err(Self::not_a_literal(name))),
        })
    }

    fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        OnceStream(match Self::literal(name) {
            Some(IpAddr::V6(a)) => Some(Ok(ResolvedAddr {
                addr: IpAddr::V6(a),
                ttl: None,
            })),
            Some(IpAddr::V4(_)) => None,
            None => Some(Err(Self::not_a_literal(name))),
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
