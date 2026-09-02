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
//! [`Resolve::supports`]/the empty stream below: a capability that lies
//! about its own state is worse than a capability that's simply absent.
//!
//! So, as things stand today: each family's addresses go into
//! `Scheduler::offer_v4`/`offer_v6` in the SAME order the resolver handed
//! them out — neither `Resolve`, nor `Scheduler`, nor
//! `hclient_native::connect` (see its doc comment, the "RFC 6724 ... NOT
//! implemented here" section) sort them. This is a recorded, explicitly
//! named gap, not an oversight. Closing it would first require introducing a separate Source Address Selection
//! capability, which no trait has today.
//!
//! **Answering a type is a capability, not a fact.** [`Resolve::lookup`]
//! answers every type through one method, so a resolver that cannot ask
//! about one — `getaddrinfo` about HTTPS, `wasi:http` about anything —
//! hands back an empty stream rather than failing to compile. But an empty
//! stream is ambiguous on its own: it could mean *this resolver
//! cannot ask* or *it asked and got nothing back* — two different things
//! a caller is not obliged to conflate (the same principle that split
//! `RedirectSupport::None` and `Transparent` in `hclient-core`: a
//! capability that lies about its own absence or its own presence is worse
//! than a capability that is simply absent).
//!
//! [`Resolve::supports`] is where that distinction lives, and it is the
//! one place it can: a resolver that cannot answer a type leaves it at the
//! default `false`, and a resolver that can must answer `true` for exactly
//! the types its `lookup` will really ask about. Answering `true` for a
//! type and then always returning an empty stream conflates *cannot* with
//! *asked and found nothing* for anyone who reads only the capability.
//!
//! **One method, and that is what makes the seam additive.** A record type
//! this client learns to act on — TLSA for DANE, CAA before issuing —
//! arrives as an [`RData`] variant and changes this trait not at all;
//! under the shape this replaced it would have been a fourth associated
//! type, a fourth method and a fourth capability constant, which every
//! implementor outside this workspace would have had to grow.
#![forbid(unsafe_code)]

mod error;
mod overrides;
pub mod svcb;

pub use overrides::{Answer, Overrides};

use bytes::Bytes;
use futures_core::Stream;
use hclient_core::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// The two stream shapes this crate hands back, named so the marker sits
/// on a line `cargo fmt` has no reason to reflow — the rule amendment C12
/// records about where a bound is written.
type SendRecords<'a> =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Record, Error>> + Send + 'a>>; // send-bound-exception: amendment-C15

/// The RR type numbers this crate names, so a caller writes
/// `rtype::HTTPS` rather than `65`.
///
/// Numbers rather than an enum, and the reason is the registry: it gains
/// entries, and a resolver may be asked for one this crate has never
/// heard of. [`Resolve::supports`] takes the same `u16` for the same
/// reason.
pub mod rtype {
    /// RFC 1035 §3.4.1.
    pub const A: u16 = 1;
    /// RFC 3596 §2.1.
    pub const AAAA: u16 = 28;
    /// RFC 9460 §14.1.
    pub const HTTPS: u16 = 65;
}

/// One record a resolver reported.
///
/// **`#[non_exhaustive]`, so it is built through [`Record::new`]** — the
/// same shape [`SvcbEndpoint`] has, and for the reason that governs every
/// such decision here: this type is handed *back* and only read, so
/// exhaustiveness is not the mechanism and a field added later must not
/// be a breaking change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Record {
    /// The record's TTL as the resolver reported it.
    ///
    /// **`Option`, and the two absences are different instructions.**
    /// `None` is *the resolver did not say* — `getaddrinfo` does not, and
    /// neither does `fetch` — where `Some(ZERO)` is RFC 2181 §8's *do not
    /// cache this*. Collapsing them would invent a lifetime for an answer
    /// nobody gave.
    ///
    /// **It lives here and not on the rdata**, which is where an HTTPS
    /// record's used to be. A TTL belongs to the record rather than to
    /// what the record says — that is where DNS puts it, and where hickory
    /// keeps it — so one field answers for every type this seam learns,
    /// and no backend can fill one copy and forget the other.
    ///
    /// This is what makes a cache of HTTPS records honest, and it is why
    /// there was none: `hclient-native`'s discovery has no cache because
    /// inventing a lifetime for somebody else's answer is how a resolver's
    /// cache and ours drift apart — and the lifetime was always on the
    /// wire, simply not carried up here.
    pub ttl: Option<Duration>,
    /// What the record says.
    pub rdata: RData,
}

impl Record {
    /// A record with no TTL; add one with [`Record::ttl`].
    #[must_use]
    pub fn new(rdata: RData) -> Self {
        Self { ttl: None, rdata }
    }

    /// The TTL the resolver reported, if it reported one.
    #[must_use]
    pub fn ttl(mut self, ttl: Option<Duration>) -> Self {
        self.ttl = ttl;
        self
    }
}

/// The record types this crate models.
///
/// **`#[non_exhaustive]`, and that is what makes the seam additive.** A
/// record type this client learns to act on — TLSA for DANE, CAA before
/// issuing — arrives as a variant here and changes [`Resolve`] not at
/// all. Under the shape this replaced it would have been a fourth
/// associated type, a fourth method and a fourth capability flag: four
/// edits to the trait, which every implementor pays for.
///
/// The cost, stated because it is real: a resolver that can answer a type
/// this enum does not model has nowhere to put it. The extensibility is
/// this crate's rather than its implementors', which is the trade the
/// variant list buys — a typed answer at every call site instead of bytes
/// nobody but the caller can read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RData {
    /// An A record, RFC 1035 §3.4.1.
    A(Ipv4Addr),
    /// An AAAA record, RFC 3596 §2.2.
    Aaaa(Ipv6Addr),
    /// An HTTPS record, RFC 9460, already reduced to the client-facing
    /// form by whichever backend read it.
    Https(SvcbEndpoint),
}

impl From<IpAddr> for RData {
    /// An address answer, sorted into the variant its family names.
    ///
    /// A caller that already knows the family writes the variant; this is
    /// for one that holds an [`IpAddr`] and does not, which is every
    /// resolver whose backend hands back both families through one call.
    ///
    /// There is deliberately no `From<SvcbEndpoint>` beside it: a caller
    /// holding one of those always knows it is an HTTPS record, so the
    /// conversion would convert nothing and `RData::Https(e)` is no
    /// longer to write.
    fn from(addr: IpAddr) -> Self {
        match addr {
            IpAddr::V4(v4) => Self::A(v4),
            IpAddr::V6(v6) => Self::Aaaa(v6),
        }
    }
}

impl RData {
    /// The RR type this answer is, for a caller checking that the answer
    /// matches the question.
    #[must_use]
    pub const fn rtype(&self) -> u16 {
        match self {
            Self::A(_) => rtype::A,
            Self::Aaaa(_) => rtype::AAAA,
            Self::Https(_) => rtype::HTTPS,
        }
    }

    /// The service binding, for the one variant that is one.
    ///
    /// The counterpart of [`RData::addr`], and it exists for the same
    /// reason: a caller that asked type 65 still has to write the match,
    /// because one method answers every type and nothing in the type
    /// system says which one came back.
    #[must_use]
    pub const fn https(&self) -> Option<&SvcbEndpoint> {
        match self {
            Self::Https(endpoint) => Some(endpoint),
            _ => None,
        }
    }

    /// The address, for the two variants that are one.
    ///
    /// A convenience with a purpose: the connect path filters a stream by
    /// family, and `filter_map(RData::addr)` reads as a filter where a
    /// `match` with an unreachable arm would read as a case nobody
    /// believes in.
    #[must_use]
    pub const fn addr(&self) -> Option<IpAddr> {
        match self {
            Self::A(a) => Some(IpAddr::V4(*a)),
            Self::Aaaa(a) => Some(IpAddr::V6(*a)),
            _ => None,
        }
    }
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

/// Name resolution.
///
/// One question — a name and an RR type — and one stream of answers.
///
/// **The type number is the parameter, and that is what makes the seam
/// additive.** A record type this client learns to act on arrives as an
/// [`RData`] variant; the trait, its one associated type and its two
/// methods do not move, so nothing outside this workspace has to grow a
/// method to keep compiling.
pub trait Resolve {
    /// The answers to one question.
    type Records<'a>: Stream<Item = Result<Record, Error>>
    where
        Self: 'a;

    /// Whether this resolver can be asked for `rtype` at all.
    ///
    /// **The distinction an empty stream cannot carry.** A resolver that
    /// asked and found nothing and one that cannot ask both yield no
    /// items; only this says which. A caller that acts on the difference
    /// — the connector deciding whether to spend a query on an HTTPS
    /// record — must ask here rather than infer it from emptiness.
    ///
    /// **It takes the type number rather than answering one question per
    /// type**, which is the shape `system_resolver::Support::allows` next
    /// door already had: `hclient-dns-system`'s answer was literally that
    /// function with `HTTPS` written in, and the generality was being
    /// discarded at this seam.
    ///
    /// The default is `false` for everything: a resolver says what it can
    /// do, and one that says nothing is taken at its word rather than
    /// asked to prove a negative. That is the understating direction, and
    /// the one a wrong answer is cheap in.
    fn supports(&self, _rtype: u16) -> bool {
        false
    }

    /// The records of type `rtype` for `name`.
    ///
    /// An empty stream is *no records*, which for a resolver that cannot
    /// ask about `rtype` at all is also what [`supports`](Self::supports)
    /// is for. It has no default: a resolver that answers nothing at all
    /// resolves nothing, and this is the one method every implementor owes.
    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a>;
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
/// [`Resolve::supports`] answers `false` for HTTPS, so ECH and h3
/// discovery correctly read as unavailable.
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
    type Records<'a>
        = SendRecords<'a>
    // send-bound-exception: amendment-C15
    where
        Self: 'a;

    /// A literal is an address and nothing else, so this resolver answers
    /// `A` and `AAAA` and refuses everything else — including `HTTPS`,
    /// which it could not have asked about even before the seam took a
    /// type number.
    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        Box::pin(OnceStream(match (Self::literal(name), rtype) {
            (Some(IpAddr::V4(a)), rtype::A) => Some(Ok(Record::new(RData::A(a)))),
            (Some(IpAddr::V6(a)), rtype::AAAA) => Some(Ok(Record::new(RData::Aaaa(a)))),
            // A literal of the other family has no record of this type,
            // and that is not an error — the same answer the two
            // methods this replaced gave by returning nothing.
            (Some(_), rtype::A | rtype::AAAA) => None,
            // Any other type: this resolver has no records at all, which
            // `supports` says in advance.
            (Some(_), _) => None,
            (None, _) => Some(Err(Self::not_a_literal(name))),
        }))
    }
}

// Tests live in `tests/`, not here: `ip_literal_only.rs` (the two
// directions `IpLiteralOnly` has to keep apart, as a case table),
// `svcb_capability.rs` (`supports`/`lookup` and the distinction
// they carry between them), `svcb_endpoint.rs` (what a consumer can rely on
// once a record crosses the seam) and `resolve_streams.rs` (what returning
// a `Stream` per family promises). Every one of them reaches this crate
// only through its public API, so there is nothing left for a `#[cfg(test)]`
// module inside `src` to see that an integration test cannot.
