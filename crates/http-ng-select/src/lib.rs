//! One transport that owns both protocol stacks and picks one per origin.
//!
//! ```no_run
//! # fn doc() -> Result<(), Box<dyn std::error::Error>> {
//! use http_ng_select::Selecting;
//! let rt = http_ng_rt_tokio::TokioHandle::current()?;
//! let dns = http_ng_dns_system::SystemDns::new(rt.clone());
//! let t = Selecting::new(
//!     http_ng_native::Native::new(rt.clone(), tls(), dns.clone()),
//!     http_ng_h3::H3::new(rt, tls(), dns.clone())?,
//!     dns,
//! )?;
//! let client = http_ng::Client::builder(t).build()?;
//! # Ok(()) }
//! # fn tls() -> http_ng_tls_rustls::Rustls { unimplemented!() }
//! ```
//!
//! # The gap this closes
//!
//! v0.3 W2 taught `http-ng-native` to fetch an origin's HTTPS record and
//! read its ALPN list, and wrote down in the same commit that there was
//! nowhere to act on it:
//!
//! > `SvcbEndpoint::alpn` containing `h3` is a fact this crate can read and
//! > cannot act on: `http-ng-h3` is a different crate with different bounds
//! > (`R: UdpBind + Spawn<..>`, `T: QuicTlsConnect`), `Native<R, T, D>` has
//! > neither, and `Client<T>` names exactly one transport type — there is
//! > nowhere in this codebase for "choose between two protocol stacks" to
//! > live.
//!
//! This is that place. It is a **new crate** and not a feature of either
//! member for the reason `http-ng-h3` is not a feature of
//! `http-ng-native`: Cargo's features are additive, so a
//! `http-ng-native/select` feature would put the whole QUIC stack, and the
//! `UdpBind + Spawn` bounds it needs from the runtime, into every build in
//! any graph that turned it on — including the ones that will only ever
//! speak HTTP/1.1. A crate is opt-in by being named.
//!
//! # Discovery has two tiers, and only the fast one is here
//!
//! `docs/v04-design.md` P12. Browsers do not race an unknown origin: they
//! *"only try QUIC if they know the server supports it"*, so first contact
//! is TCP unless something said otherwise before the connection was made.
//! There are two things that can say so, and they are not
//! interchangeable:
//!
//! - **The HTTPS record is the fast tier.** It arrives at resolution time,
//!   which is before the first connection, so QUIC can be used on the
//!   first request. This crate implements it and nothing else.
//! - **`Alt-Svc` is the slow tier.** It is a response header, so it can
//!   only help the *next* connection, and the first page load is never
//!   h3. It needs storage — a cache keyed by origin with the header's own
//!   `ma` as its lifetime, a negative half, and a scope — which is why
//!   `docs/v04-design.md` puts it second rather than first, and why it is
//!   **not in this crate**. What it would need is written down in
//!   `docs/v04-w1-acceptance.md` rather than half-built here.
//!
//! **Racing the two stacks is a third thing and is also not here.** It is a
//! hedge against a network that blocks UDP/443, applied *after* the choice
//! rather than in place of it, and v0.3 W2 recorded that the size of the
//! cost it pays is unverified. A measurement comes before a policy.
//!
//! # What the choice costs, in queries
//!
//! One `Resolve::lookup_svcb` per request, and it is **not** shared with
//! the one `http-ng-native` makes inside its own connector: that one is
//! `pub(crate)`, there is no way to hand a record across the `Transport`
//! seam, and this crate treats its members as read-only. So at an origin's
//! **default port**, where `http-ng-native` also does discovery, an
//! HTTP/1.1 or HTTP/2 request through this transport issues the type-65
//! query **twice**; an HTTP/3 one issues it once, because `http-ng-h3` does
//! no SVCB lookup at all. At a non-default port `http-ng-native` skips
//! discovery entirely and the count is one either way. Both counts are
//! measured rather than reasoned — `tests/dns_cost.rs` counts the calls a
//! resolver received.
//!
//! A `RequireVersion` demand costs **nothing**: it is answered before the
//! resolver is asked at all.
//!
//! # No cache, deliberately
//!
//! An answer remembered per origin would turn one query per request into
//! one per origin, and it is the thing this crate does not have — for the
//! reason `http-ng-native`'s discovery module gives about the other half of
//! the same problem: *"this origin has no HTTPS record" is a DNS answer
//! with a TTL of its own, which `SvcbEndpoint` does not carry, and
//! inventing a lifetime for someone else's answer is how a resolver's cache
//! and ours drift apart.* A resolver that caches (`http-ng-dns-hickory`
//! does, with the real TTLs) already removes the cost; one that does not
//! (`http-ng-dns-doh`, by an explicit decision of its own) would not have
//! it removed by a second cache here that no caller can turn off.
//!
//! # The capability answer, and the refusal
//!
//! `Transport::capabilities` returns a `&Capabilities`, so the answer is
//! **stored** rather than computed per call. What is stored is decided
//! field by field by one rule — *the value must be true whichever member
//! serves the request* — and where no value is, [`Selecting::new`] refuses
//! and names the field. See [`combine`].
#![forbid(unsafe_code)]

mod body;
mod caps;

pub use body::SelectedBody;
pub use caps::{Disagreement, combine};

use futures_util::StreamExt;
use http_ng_core::{Capabilities, Error, RequestBody, RequireVersion, unversioned::Transport};
use http_ng_dns::Resolve;
use http_ng_h3::H3;
use http_ng_native::Native;
use http_ng_rt::{TcpConnect, Timer};
use http_ng_tls::TlsConnect;

/// The ALPN token HTTP/3 is identified by, RFC 9114 §3.2 — the one an
/// HTTPS record has to list for this transport to choose QUIC.
const ALPN_H3: &[u8] = b"h3";

/// The `https` scheme's default port, RFC 9110 §4.2.2. An origin at any
/// other port keeps its HTTPS record under a different name — see
/// [`Selecting::record_name`].
const HTTPS_DEFAULT_PORT: u16 = 443;

/// A transport that owns an HTTP/1.1+HTTP/2 stack and an HTTP/3 stack and
/// sends each request over one of them.
///
/// Named for what it does. An earlier draft of `docs/v04-design.md` called
/// it `Racing`, which put a policy in a type name before the policy was
/// decided — and the race is the hedge, not the chooser.
///
/// # The members are concrete, and that is the point
///
/// Not `Selecting<A: Transport, B: Transport>`. A generic pair would have
/// to be *told* which member speaks HTTP/3 — nothing in `Transport` or
/// `Capabilities` says so — and a caller could hand it two stacks that both
/// speak HTTP/1.1 and get a transport that chooses between them on the
/// strength of a record neither honours. With the two named, that is
/// unrepresentable, and the one thing generality would have bought (a third
/// member later, `http-ng-urlsession` for instance) is a different decision
/// with a different capability question attached — `docs/v04-design.md` §W3
/// expects `RedirectSupport::Internal` from it, which [`combine`] would
/// refuse against either member here.
///
/// # One `R`, one `T`, one `D` — measured, not assumed
///
/// `docs/v04-design.md` P1: one runtime type satisfies both bound sets, so
/// `Native<R, T, D>` and `H3<R, T, D>` can share all three parameters. P2
/// is the condition: `http-ng-rt-tokio` needs its `udp` feature on, or
/// `TokioHandle` does not implement `UdpAdoptStd` and this type cannot be
/// named. That is a Cargo feature a build asking for HTTP/3 turns on, and
/// an HTTP/1.1-only build does not pay for.
///
/// # Two bounds on the struct, which is not this crate's choice
///
/// `Native<R, T, D>` declares `R: TcpConnect + Timer` and `T: TlsConnect`
/// on the struct rather than on its impls, so a field of that type cannot
/// be written without them. They are repeated on every impl below for the
/// same reason and buy nothing else; the bounds that matter are on the
/// [`Transport`] impl.
pub struct Selecting<R, T, D>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    tcp: Native<R, T, D>,
    quic: H3<R, T, D>,
    /// The resolver this transport asks for HTTPS records — its own, and
    /// deliberately a third handle rather than one borrowed from a member:
    /// `Native` and `H3` own theirs and expose no accessor, and reaching
    /// into one of them would be this crate deciding which member's view of
    /// DNS is authoritative.
    dns: D,
    caps: Capabilities,
}

/// Hand-written rather than derived, for `H3`'s reason: a derive would
/// demand `Debug` from the runtime, the TLS backend and the resolver for
/// the benefit of a formatter.
impl<R, T, D> std::fmt::Debug for Selecting<R, T, D>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Selecting")
            .field("full_duplex", &self.caps.full_duplex)
            .field("timeouts", &self.caps.timeouts)
            .finish_non_exhaustive()
    }
}

/// Which stack a request goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stack {
    Tcp,
    Quic,
}

impl<R, T, D> Selecting<R, T, D>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
    Native<R, T, D>: Transport,
    H3<R, T, D>: Transport,
    D: Resolve,
{
    /// Both stacks, and the resolver this transport asks about origins.
    ///
    /// Fails with a [`Disagreement`] when the two report a capability that
    /// cannot be made into one honest answer — see [`combine`] for the rule
    /// and for the one disagreement reachable in this workspace today
    /// (`Native::without_pool()` against `H3`'s shared connections).
    ///
    /// `dns` is asked for HTTPS records only. Addresses are still each
    /// member's own business, resolved through the resolver that member was
    /// built with; handing a different one here does not redirect their
    /// connections, it only changes what this transport believes about an
    /// origin's protocols.
    pub fn new(tcp: Native<R, T, D>, quic: H3<R, T, D>, dns: D) -> Result<Self, Box<Disagreement>> {
        let caps = combine(tcp.capabilities(), quic.capabilities()).map_err(Box::new)?;
        Ok(Self {
            tcp,
            quic,
            dns,
            caps,
        })
    }

    /// The name an origin's HTTPS record lives under, RFC 9460 §2.3.
    ///
    /// The scheme's default port takes the origin name itself; any other
    /// port takes the `_<port>._https.` prefix, and the record fetched
    /// under it is the one that describes *that* service.
    ///
    /// # `http-ng-native` deliberately does not do this, and the reason
    /// does not reach here
    ///
    /// Its `discovery` module refuses to construct the prefixed name
    /// because *"it would then have to decide what `lookup_ipv4`/
    /// `lookup_ipv6` are asked for (the prefixed name has no addresses),
    /// and that is a resolver-facing question the `Resolve` seam does not
    /// answer today"* — and so it applies discovery at the default port
    /// only. This transport reads **one bit** off the record, the presence
    /// of `h3` in its ALPN list, and resolves no addresses from it at all:
    /// whichever member is chosen resolves the *origin* name itself, as it
    /// always did. So the question that stopped the connector never comes
    /// up, and the alternative — using the default-port record's ALPN for a
    /// service on another port — is the thing that rule exists to prevent.
    fn record_name(host: &str, port: u16) -> String {
        if port == HTTPS_DEFAULT_PORT {
            host.to_owned()
        } else {
            format!("_{port}._https.{host}")
        }
    }

    /// Which stack this request goes to, and everything that goes into the
    /// answer.
    ///
    /// # A `RequireVersion` demand outranks the record, and asks nobody
    ///
    /// Both members report `version_select: true` — each honours a demand
    /// for the version it speaks and refuses every other one before writing
    /// a byte — so the conjunction this transport stores is `true`, and a
    /// transport reporting `true` owes an answer. Routing by the record and
    /// leaving the demand to whichever member won would fail
    /// `RequireVersion(HTTP_3)` at any origin without a record, with
    /// `VersionNotAvailable` from a transport that owns an HTTP/3 stack —
    /// an under-claim of exactly the kind `Capabilities::version_select`'s
    /// own doc warns about.
    ///
    /// A demand for anything else goes to the TCP stack, including
    /// `HTTP_09` and `HTTP_10`, which it refuses. That refusal is the
    /// member's and reads the same as it does without this transport;
    /// producing a second one here would be a second implementation of a
    /// rule that already exists.
    ///
    /// # Then: `https` only, a name only, and a resolver that can answer
    ///
    /// `http://` never chooses QUIC — HTTP/3 has no cleartext form, and an
    /// HTTPS record against a cleartext origin means something this
    /// transport will not do on the caller's behalf (RFC 9460 §9.5 makes it
    /// an instruction to upgrade the scheme, which is a redirect-shaped
    /// decision belonging to whoever owns the request). An IP literal is
    /// skipped because it has no name to look up, and asking a real
    /// resolver for `_443._https.127.0.0.1` is a query with no answer that
    /// every request would pay for. And `Resolve::supports_svcb()` is
    /// **asked** rather than inferred from an empty stream, which is the
    /// distinction that method exists to carry.
    async fn choose(&self, req: &http::Request<RequestBody>) -> Stack {
        if let Some(RequireVersion(v)) = req.extensions().get::<RequireVersion>() {
            return if *v == http::Version::HTTP_3 {
                Stack::Quic
            } else {
                Stack::Tcp
            };
        }
        if req.uri().scheme() != Some(&http::uri::Scheme::HTTPS) {
            return Stack::Tcp;
        }
        let Some(host) = req.uri().host() else {
            return Stack::Tcp;
        };
        if is_ip_literal(host) {
            return Stack::Tcp;
        }
        if !self.dns.supports_svcb() {
            return Stack::Tcp;
        }
        let name = Self::record_name(host, req.uri().port_u16().unwrap_or(HTTPS_DEFAULT_PORT));
        if self.origin_offers_h3(&name).await {
            Stack::Quic
        } else {
            Stack::Tcp
        }
    }

    /// Whether the highest-preference ServiceMode record under `name` lists
    /// `h3`.
    ///
    /// **The lowest priority wins and the rest are not consulted**, which
    /// is the origin's own ranking being honoured rather than a shortcut:
    /// RFC 9460 §2.4.2 makes priority the operator's preference order, so
    /// an origin whose first-ranked endpoint is HTTP/2-only and whose
    /// second offers `h3` has asked for HTTP/2. Reading `h3` off *any*
    /// record would override that, and would also let one endpoint in an
    /// attacker-influenced answer decide the protocol for the whole origin.
    ///
    /// **AliasMode records are skipped**, and the skip is load-bearing for
    /// the same reason `http-ng-native`'s selection gives: they arrive with
    /// `priority: 0` and every other field empty (RFC 9460 §2.4.1 — a
    /// recipient MUST ignore an AliasMode record's SvcParams), and 0 is
    /// numerically below every ServiceMode priority, so a selection that
    /// did not skip them would reliably pick the one record whose ALPN list
    /// is empty and never choose QUIC at all.
    ///
    /// **A lookup error is not fatal**, and that is a decision rather than
    /// a discarded `Result`: type 65 is answered with SERVFAIL by a long
    /// tail of middleboxes, and a client that failed the request there
    /// would be unable to reach origins it reaches perfectly well over
    /// TCP. What a failed lookup produces is the answer this transport
    /// would have given without any discovery at all.
    async fn origin_offers_h3(&self, name: &str) -> bool {
        let mut best: Option<http_ng_dns::SvcbEndpoint> = None;
        let mut records = std::pin::pin!(self.dns.lookup_svcb(name));
        while let Some(record) = records.next().await {
            let Ok(record) = record else { continue };
            if record.priority == 0 {
                continue;
            }
            if best.as_ref().is_none_or(|b| record.priority < b.priority) {
                best = Some(record);
            }
        }
        best.is_some_and(|e| e.alpn.iter().any(|a| a.as_slice() == ALPN_H3))
    }
}

/// Whether `host` — as `http::Uri::host` gives it, which keeps an IPv6
/// literal's brackets — is an address rather than a name.
///
/// The brackets come off first, for `IpLiteralOnly`'s reason: without that
/// every IPv6 literal would read as a name and be looked up.
fn is_ip_literal(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<std::net::IpAddr>().is_ok()
}

impl<R, T, D> Transport for Selecting<R, T, D>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
    Native<R, T, D>: Transport<Error = Error>,
    H3<R, T, D>: Transport<Error = Error>,
    <Native<R, T, D> as Transport>::Body:
        http_body::Body<Data = bytes::Bytes, Error = Error> + Unpin,
    <H3<R, T, D> as Transport>::Body: http_body::Body<Data = bytes::Bytes, Error = Error> + Unpin,
    D: Resolve,
{
    type Body =
        SelectedBody<<Native<R, T, D> as Transport>::Body, <H3<R, T, D> as Transport>::Body>;
    type Error = Error;

    /// Choose, then hand the request over whole.
    ///
    /// Nothing is rewritten on the way: the chosen member gets the request
    /// the caller built, extensions included, and reads `Timeouts`,
    /// `AllowEarlyData` and `RequireVersion` off it exactly as it does when
    /// it is the only transport. This is what keeps the two members'
    /// behaviour a fact about them rather than a fact about this crate.
    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Error> {
        match self.choose(&req).await {
            Stack::Tcp => self
                .tcp
                .execute(req)
                .await
                .map(|r| r.map(SelectedBody::Tcp)),
            Stack::Quic => self
                .quic
                .execute(req)
                .await
                .map(|r| r.map(SelectedBody::Quic)),
        }
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// The identity, for the reason both members give for their own: the
    /// category was set where the failure happened, by the member that had
    /// it, and the hook's default would do the same. The line states the
    /// intent where it is read and survives the default changing.
    fn to_error(&self, e: Error) -> Error {
        e
    }
}
