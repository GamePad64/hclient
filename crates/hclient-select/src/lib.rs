//! One transport that owns both protocol stacks and picks one per origin.
//!
//! ```no_run
//! # fn doc() -> Result<(), Box<dyn std::error::Error>> {
//! use hclient_select::Selecting;
//! let rt = hclient_rt_tokio::TokioHandle::current()?;
//! let dns = hclient_dns_system::SystemDns::new(rt.clone());
//! let t = Selecting::new(
//!     rt.clone(),
//!     hclient_native::Native::new(rt.clone(), tls(), dns.clone()),
//!     hclient_h3::H3::new(rt, tls(), dns.clone())?,
//!     dns,
//! )?;
//! let client = hclient::Client::builder(t).build()?;
//! # Ok(()) }
//! # fn tls() -> hclient_tls_rustls::Rustls { unimplemented!() }
//! ```
//!
//! # The gap this closes
//!
//! v0.3 W2 taught `hclient-native` to fetch an origin's HTTPS record and
//! read its ALPN list, and wrote down in the same commit that there was
//! nowhere to act on it:
//!
//! > `SvcbEndpoint::alpn` containing `h3` is a fact this crate can read and
//! > cannot act on: `hclient-h3` is a different crate with different bounds
//! > (`R: UdpBind + Spawn<..>`, `T: QuicTlsConnect`), `Native<R, T, D>` has
//! > neither, and `Client<T>` names exactly one transport type — there is
//! > nowhere in this codebase for "choose between two protocol stacks" to
//! > live.
//!
//! Quoted as it stood then. `discovery.rs` has since been reworded twice —
//! once because this crate is that place, and once because `Client` no
//! longer names a transport type at all — so do not expect to find these
//! words there.
//!
//! This is that place. It is a **new crate** and not a feature of either
//! member for the reason `hclient-h3` is not a feature of
//! `hclient-native`: Cargo's features are additive, so a
//! `hclient-native/select` feature would put the whole QUIC stack, and the
//! `UdpBind + Spawn` bounds it needs from the runtime, into every build in
//! any graph that turned it on — including the ones that will only ever
//! speak HTTP/1.1. A crate is opt-in by being named.
//!
//! # Discovery has two tiers, and both are here
//!
//! `docs/v04-design.md` P12. Browsers do not race an unknown origin: they
//! *"only try QUIC if they know the server supports it"*, so first contact
//! is TCP unless something said otherwise before the connection was made.
//! There are two things that can say so, and they are not
//! interchangeable:
//!
//! - **The HTTPS record is the fast tier.** It arrives at resolution time,
//!   which is before the first connection, so QUIC can be used on the
//!   very first request to an origin (v0.4 W1, deliverable 3).
//! - **`Alt-Svc` is the slow tier** (deliverable 4, [`altsvc`]). It is a
//!   response **header**, so it can only ever help the *next* connection:
//!   the first request to an unknown origin goes over TCP no matter what
//!   the origin advertises, because the advertisement arrives in the
//!   answer to it. What it buys is every origin that publishes no HTTPS
//!   record, which is most of the web — the fast tier alone works for a
//!   minority of it.
//!
//! **Racing the two stacks is a third thing, it is here, and it is off
//! until it is asked for** ([`race`], v0.4). It is a hedge against a
//! network that blocks UDP/443, applied *after* the choice rather than in
//! place of it: [`Selecting::hedging`] starts a TCP connect beside an
//! outstanding QUIC one, and whichever produces a connection first carries
//! the request. **Neither arm sends anything until the race is over**,
//! which is what the staged connect changed and is the whole reason this
//! could be built — before it, a race made of two `Transport::execute`
//! calls delivered the losing arm's request to the origin as well.
//!
//! v0.3 W2 recorded that the size of the cost it pays was unverified, and
//! a measurement came before the policy: `docs/v04-w1-acceptance.md` §7
//! and `docs/v04-race.md`.
//!
//! # The record decides where there is one, and the header only where
//! there is not
//!
//! The order is a rule about *whose statement is fresher*, not a
//! preference between mechanisms. A record is fetched for this request; an
//! Alt-Svc entry was heard on an earlier one and may be up to its `ma`
//! old. So:
//!
//! 1. A first-ranked record listing `h3` chooses QUIC. **The Alt-Svc cache
//!    is not consulted, and its lock is not taken.**
//! 2. A first-ranked record *not* listing `h3` chooses TCP, and the cache
//!    is not consulted either — RFC 9460 §2.4.2 makes priority the
//!    operator's preference order, so an origin whose best endpoint is
//!    HTTP/2-only has asked for HTTP/2, and a header remembered from
//!    yesterday does not overrule it.
//! 3. Only where there is **no** record to read — the origin publishes
//!    none, the lookup failed, the resolver cannot ask, or the authority
//!    is an IP literal with no name to ask about — is the cache consulted.
//!
//! That ordering is also what keeps the slow tier from making the fast one
//! worse: on the fast path nothing new is read, locked or looked up.
//!
//! # What the choice costs, in queries
//!
//! **At most one `Resolve::lookup_svcb` per request, whichever stack
//! answers**, and it is the *same* one `hclient-native` makes inside its
//! own connector rather than a second like it. At an origin's default port
//! this transport does not make the query itself: it asks the TCP member
//! to make it — `hclient_native::Prefetch::prepare` — reads the answer,
//! and hands both the answer and the request back, so the connector does
//! not ask again. An HTTP/3 request costs the same one, because
//! `hclient-h3` does no SVCB lookup at all and the query has already been
//! made by then.
//!
//! It used to be **two** on the TCP path, which is what that seam was
//! built for; `docs/v04-w1-acceptance.md` §3.1 is the whole argument,
//! including why the record is fetched *by* the member rather than handed
//! *to* it. Both counts are measured rather than reasoned —
//! `tests/dns_cost.rs` counts the calls a resolver received, and
//! `tests/record_handover.rs` watches the connection, which is the half a
//! count cannot see.
//!
//! Away from the default port the member does no discovery at all — the
//! record lives under `_<port>._https.<host>`, a name only this transport
//! constructs — so it answers `Discovered::NotConsulted` and this
//! transport asks its own resolver, exactly as it always did. The rule is
//! *ask the member, because it was going to ask; where it did not look,
//! look for yourself*, and this crate keeps no copy of the member's rule
//! about where discovery applies.
//!
//! **Alt-Svc adds none of them.** A request chosen onto QUIC by a
//! remembered advertisement pays the same single lookup an origin with no
//! record already paid. Where the resolver cannot do SVCB at all the count
//! stays zero and the choice is still available.
//!
//! A `RequireVersion` demand costs **nothing**: it is answered before the
//! resolver and the cache are asked at all. So does `http://`, and so does
//! an IP literal — for a literal the member is not asked to prepare
//! either, because the query it would make on a *connection* would become
//! one per *request*.
//!
//! # No cache for the record, deliberately — and one for the header,
//! deliberately
//!
//! These are not in tension, and the difference is a fact about what each
//! answer carries. An HTTPS record remembered per origin would need a
//! lifetime this library would have to invent: *"this origin has no HTTPS
//! record" is a DNS answer with a TTL of its own, which `SvcbEndpoint`
//! does not carry, and inventing a lifetime for someone else's answer is
//! how a resolver's cache and ours drift apart* (`hclient-native`'s
//! `discovery`). A resolver that caches (`hclient-dns-hickory` does, with
//! the real TTLs) already removes that cost.
//!
//! An Alt-Svc advertisement carries its own max-age — RFC 7838 §3.1's `ma`
//! — given by the origin for exactly this purpose, so the cache in
//! [`altsvc`] invents nothing. It is also not optional in the way the
//! other would be: without it the header could not be acted on at all,
//! since by the time it arrives the connection it describes is the one
//! already in use.
//!
//! # The capability answer, and the refusal
//!
//! `Transport::capabilities` returns a `&Capabilities`, so the answer is
//! **stored** rather than computed per call. What is stored is decided
//! field by field by one rule — *the value must be true whichever member
//! serves the request* — and where no value is, [`Selecting::new`] refuses
//! and names the field. See [`combine`].
#![forbid(unsafe_code)]

pub mod altsvc;
mod body;
mod caps;
pub mod failures;
pub mod race;

pub use body::SelectedBody;
pub use caps::{Disagreement, combine};
pub use failures::{H3_FAILURE_TTL, H3Failures};
pub use race::DEFAULT_HEAD_START;

use altsvc::{AltSvcCache, Origin};
use futures_util::StreamExt;
use hclient_core::{
    Capabilities, Error, RequestBody, RequireVersion, Timeouts, unversioned::Transport,
};
use hclient_dns::Resolve;
use hclient_h3::{H3, StagedConnect};
use hclient_native::{Discovered, Native, Prefetch, Prepared, StagedConnect as TcpStagedConnect};
use hclient_rt::{TcpConnect, Timer};
use hclient_tls::TlsConnect;
use std::time::Duration;

/// The ALPN token HTTP/3 is identified by, RFC 9114 §3.2 — the one an
/// HTTPS record has to list, or an `Alt-Svc` field advertise, for this
/// transport to choose QUIC.
///
/// One constant for both tiers on purpose: they are two ways of learning
/// the same fact, and a second spelling of `h3` is a way for them to
/// disagree.
pub(crate) const ALPN_H3: &[u8] = b"h3";

/// The response header field the slow tier reads, RFC 7838 §3. Lowercase
/// because `HeaderMap` normalises, so this matches however it was sent.
const ALT_SVC: &str = "alt-svc";

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
/// member later, `hclient-urlsession` for instance) is a different decision
/// with a different capability question attached — `docs/v04-design.md` §W3
/// expects `RedirectSupport::Internal` from it, which [`combine`] would
/// refuse against either member here.
///
/// # One `R`, one `T`, one `D` — measured, not assumed
///
/// `docs/v04-design.md` P1: one runtime type satisfies both bound sets, so
/// `Native<R, T, D>` and `H3<R, T, D>` can share all three parameters. P2
/// is the condition: `hclient-rt-tokio` needs its `udp` feature on, or
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
    /// The clock, and the instant it is measured from.
    ///
    /// Held rather than borrowed from a member for the same reason `dns`
    /// is: `Native` and `H3` own theirs and expose no accessor. It is here
    /// for the slow tier alone — an `Alt-Svc` entry has a lifetime and
    /// something has to say when it has run out — and it is
    /// `hclient_rt::Timer` rather than `std::time::Instant::now()` for
    /// `hclient-native`'s reason: `Timer` is the one seam through which
    /// time reaches a transport here, so a caller testing under
    /// `tokio::time::pause()` sees what this transport sees.
    rt: R,
    /// What elapsed times below are measured from — this transport's
    /// construction, exactly as `Native`'s `epoch` is.
    epoch: R::Instant,
    alt_svc: AltSvcCache,
    /// The origins whose HTTP/3 has already cost a failed connect — the
    /// negative half of the slow tier, and see [`failures`] for why it is
    /// here rather than in either member.
    h3_failures: H3Failures,
    /// How long the QUIC arm runs alone before a TCP connect is started
    /// beside it, or `None` — which is the value a freshly constructed
    /// `Selecting` has — for a transport that does not race at all.
    ///
    /// **`None` is the default and that is a decision about what a plain
    /// client does**, not an omission: a transport that opened a UDP
    /// socket and a TCP one for the same request would be deciding, on
    /// every caller's behalf, what to spend on a network that blocks
    /// UDP/443. `docs/v04-design.md` §W1 says the same thing about
    /// `DefaultTransport`, which does not become this type either. See
    /// [`race`] and [`Selecting::hedging`].
    hedge: Option<Duration>,
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
            .field("alt_svc", &self.alt_svc)
            .field("h3_failures", &self.h3_failures)
            .field("hedge", &self.hedge)
            .finish_non_exhaustive()
    }
}

/// Which stack a request goes to, carrying the request itself — and, on
/// the TCP arm, whatever the TCP stack has already been asked.
///
/// **A `Stack` and a request used to be two values**, and the request was
/// handed to the chosen member afterwards. They are one now because the
/// TCP arm may carry a [`Prepared`], which is a request *with* the HTTPS
/// record that was fetched for it — the pairing that makes the record
/// impossible to misdirect (see [`hclient_native::Prepared`]) is exactly
/// the pairing that stops them being separable here.
enum Route {
    /// The QUIC stack. Nothing prepared travels here: `hclient-h3` reads
    /// no HTTPS record at all, so there is nothing to hand it.
    Quic {
        req: http::Request<RequestBody>,
        /// Whether a QUIC connect that fails may send this request over
        /// TCP instead.
        ///
        /// `false` for exactly one shape of request, and it is not a
        /// policy of this crate's: a caller who wrote
        /// `RequireVersion(HTTP_3)` asked for HTTP/3, so falling back
        /// would be a silent downgrade — and would not even be silent,
        /// since `Native` refuses the demand and the caller would get
        /// `VersionNotAvailable` in place of the connect error that is the
        /// real answer.
        fallback: bool,
    },
    /// The TCP stack, with whatever [`Prefetch::prepare`] found for this
    /// request — including "nothing was looked up", for the requests this
    /// transport does not ask about (a `RequireVersion` demand, `http://`,
    /// an IP literal, a resolver that cannot ask). Those get
    /// [`Prepared::new`], which asserts nothing and leaves the connector's
    /// own discovery exactly as it was.
    Tcp(Prepared),
}

/// `R: Clone` is `Native`'s and not this crate's — it is the bound
/// `Native`'s own exchange impl declares (its response body outlives
/// `execute` and needs a clock of its own), and [`Prefetch::prepare`] and
/// [`Prefetch::execute_prepared`] are inherent methods on that impl, so
/// calling them means repeating it. Every runtime in this workspace is
/// already `Clone`, and `Selecting` could not be constructed by a caller
/// whose `Native` did not satisfy it in any case.
impl<R, T, D> Selecting<R, T, D>
where
    R: TcpConnect + Timer + Clone,
    T: TlsConnect,
    Native<R, T, D>: Prefetch,
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
    ///
    /// **And it is not asked at all where the TCP member is asked
    /// instead**, which is at an origin's default port — see the crate
    /// doc's cost section. That is the one place a caller who hands three
    /// different resolvers in can see the difference, and it is the
    /// honest direction: the record that chooses the stack is then the
    /// record the connection is made under, where two independent lookups
    /// could disagree. `dns` is still what answers away from the default
    /// port, where the member does not look.
    ///
    /// `rt` is a third handle for the same reason `dns` is, and it is used
    /// for one thing: the slow tier's clock. Both members take one of
    /// their own at construction and neither lends it out.
    pub fn new(
        rt: R,
        tcp: Native<R, T, D>,
        quic: H3<R, T, D>,
        dns: D,
    ) -> Result<Self, Box<Disagreement>> {
        let caps = combine(tcp.capabilities(), quic.capabilities()).map_err(Box::new)?;
        let epoch = rt.now();
        Ok(Self {
            tcp,
            quic,
            dns,
            caps,
            rt,
            epoch,
            alt_svc: AltSvcCache::default(),
            h3_failures: H3Failures::default(),
            hedge: None,
        })
    }

    /// The caller has seen a network configuration change: forget every
    /// `Alt-Svc` advertisement that did not carry `persist=1`.
    ///
    /// **This exists because the transport cannot see the event itself,
    /// and saying so at the type is the honest alternative to pretending
    /// the cache is safe.** RFC 7838 §2.2 asks a client to clear
    /// non-persistent alternatives on a network change *"when information
    /// about network state is available"* — and to a `Transport` it is
    /// not: nothing in `hclient-rt` reports an interface coming up, a
    /// default route changing or a VPN connecting. An application usually
    /// can see it, so the duty is handed to the caller in the one shape
    /// that lets them discharge it.
    ///
    /// Until it is called, every entry behaves as though it carried
    /// `persist=1`. That is the unsafe direction — a laptop that moved
    /// networks is advertising an alt-authority that was reachable
    /// somewhere else — and it is bounded by two things rather than
    /// argued away: nothing is written to disk, and the cache is a field
    /// of this transport, so dropping the client does the whole job.
    ///
    /// **Both memories are cleared, and not by the same rule** — see
    /// [`failures`]. The advertisement cache keeps `persist=1` entries,
    /// because that flag is the origin's own claim that what it advertised
    /// is a property of the origin rather than of the path. The failure
    /// memory keeps nothing: *"UDP/443 did not get through"* is a fact
    /// about the network alone, no peer ever asked us to carry it, and it
    /// is exactly the entry a network change makes certainly wrong.
    pub fn network_changed(&self) {
        self.alt_svc.network_changed();
        self.h3_failures.network_changed();
    }

    /// Hedge every request this transport sends to QUIC with a TCP connect
    /// started `head_start` later — the race, and it is off until this is
    /// called.
    ///
    /// **It is a hedge and not a chooser** (`docs/v04-design.md` §W1 P12).
    /// The record and the advertisement decide which stack an origin
    /// speaks; this decides nothing, and only stops a request waiting out
    /// quinn's 30 s `max_idle_timeout` at an origin whose UDP does not get
    /// through. A request the two tiers sent to TCP is not raced, and
    /// neither is a `RequireVersion(HTTP_3)` demand — a TCP connection
    /// opened for a request that can never be sent over TCP is a
    /// connection opened for nothing.
    ///
    /// # What the head start is, and what to pass
    ///
    /// [`DEFAULT_HEAD_START`] where there is no reason to pass anything
    /// else: 250 ms, RFC 8305 §5's Connection Attempt Delay, which is
    /// already this workspace's answer to the same question one layer
    /// down. [`race`] has what the staged connect changed about that
    /// number's justification — and about its floor, which is now
    /// [`Duration::ZERO`]: with no head start both stacks connect at once,
    /// **and the losing arm still sends nothing**, which is the property
    /// that made this worth building.
    ///
    /// Bigger is not free and smaller is not unsafe. A head start below
    /// one QUIC handshake costs a TCP connect and TLS handshake the
    /// request will probably not use — checked into the pool warm rather
    /// than thrown away. A head start above it is paid, in full, by the
    /// first request to an origin whose HTTP/3 cannot be reached, and by
    /// no other one for [`H3_FAILURE_TTL`] afterwards, because a QUIC arm
    /// that loses the race teaches [`failures`].
    ///
    /// # And it is spent inside `Timeouts::connect`, not beside it
    ///
    /// The QUIC arm carries the caller's whole `connect` bound and the
    /// hedge carries what is left after the head start, so the pair costs
    /// one bound rather than two. A caller whose bound has no room for two
    /// connects in it — `head_start` at or above `Timeouts::connect` —
    /// gets the sequential fallback instead, unchanged, rather than a
    /// refusal or a doubled bound.
    #[must_use]
    pub fn hedging(mut self, head_start: Duration) -> Self {
        self.hedge = Some(head_start);
        self
    }

    /// Elapsed on the runtime's own clock, from this transport's epoch.
    ///
    /// Read at the two places the slow tier needs a time — when an entry
    /// is looked up and when one is stored — and nowhere else. A request
    /// whose choice is settled by a record or by a `RequireVersion` demand
    /// does not read the clock at all.
    fn now(&self) -> Duration {
        self.rt.elapsed_since(self.epoch)
    }

    /// The name an origin's HTTPS record lives under, RFC 9460 §2.3.
    ///
    /// The scheme's default port takes the origin name itself; any other
    /// port takes the `_<port>._https.` prefix, and the record fetched
    /// under it is the one that describes *that* service.
    ///
    /// # `hclient-native` deliberately does not do this, and the reason
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
    /// # Then: `https` only
    ///
    /// `http://` never chooses QUIC — HTTP/3 has no cleartext form, and an
    /// HTTPS record against a cleartext origin means something this
    /// transport will not do on the caller's behalf (RFC 9460 §9.5 makes it
    /// an instruction to upgrade the scheme, which is a redirect-shaped
    /// decision belonging to whoever owns the request). A cleartext origin
    /// reaches neither tier: no record is fetched for it and no `Alt-Svc`
    /// field of its is remembered, because a remembered one could never be
    /// acted on.
    ///
    /// # Then the record — the fast tier — where there is a name to ask
    /// about and a resolver that can answer
    ///
    /// An IP literal is skipped because it has no name to look up, and
    /// asking a real resolver for `_443._https.127.0.0.1` is a query with
    /// no answer that every request would pay for. And
    /// `Resolve::supports_svcb()` is **asked** rather than inferred from an
    /// empty stream, which is the distinction that method exists to carry.
    ///
    /// A record that answers — either way — is the end of it. It was
    /// fetched for *this* request, so it is the freshest thing available,
    /// and its ranking is the origin's own: an origin whose first-ranked
    /// endpoint is HTTP/2-only has asked for HTTP/2, and an `Alt-Svc`
    /// header heard on an earlier request does not overrule that.
    ///
    /// # The connector is asked first, because it was going to ask anyway
    ///
    /// [`Prefetch::prepare`] does the lookup this transport needs *and* the
    /// one the TCP stack was about to make inside its own connector, and
    /// hands back both the answer and the request. Before it, the same
    /// record was fetched twice for one request chosen onto TCP at an
    /// origin's default port — counted, in `tests/dns_cost.rs`.
    ///
    /// Three things follow, and none of them is a policy of this crate's:
    ///
    /// - **Where the connector does not look, this transport looks for
    ///   itself.** [`Discovered::NotConsulted`] is not an answer; it is
    ///   the connector saying discovery does not apply to this request (a
    ///   port other than 443 — where the record lives under a prefixed
    ///   name only *this* transport constructs — or an origin its own
    ///   negative cache is holding off). The fallback is the lookup this
    ///   crate has always made, and it costs exactly what it did before.
    /// - **Where the connector does look, its answer is the answer.** One
    ///   query per request, and the record that chose the stack is the
    ///   record the connection is made under, which two independent
    ///   lookups could not promise.
    /// - **The rule about where discovery applies is not copied here.**
    ///   This transport asks and is told. A copy would be a second place
    ///   for that rule to live, and the two would drift into asking twice
    ///   again or into never asking at all.
    ///
    /// # And only then the cache — the slow tier
    ///
    /// Reached exactly when there is no record to read: none published,
    /// the lookup failed, the resolver cannot ask, or the authority is a
    /// literal. Those are also all the cases in which the fast tier gives
    /// no answer, so nothing the fast tier decides pays for this — no
    /// extra query, and the cache's lock is not taken on a path a record
    /// settled.
    async fn route(&self, req: http::Request<RequestBody>) -> Route {
        if let Some(RequireVersion(v)) = req.extensions().get::<RequireVersion>() {
            return if *v == http::Version::HTTP_3 {
                // Not filtered by the failure memory, and that is the same
                // rule as the line above it: a demand is answered before
                // the resolver and the cache are asked, so it is answered
                // before this transport's memory of failures too. A caller
                // who demanded HTTP/3 has said what they want done about
                // an origin that may not be reachable over it.
                Route::Quic {
                    req,
                    fallback: false,
                }
            } else {
                Route::Tcp(Prepared::new(req))
            };
        }
        if req.uri().scheme() != Some(&http::uri::Scheme::HTTPS) {
            return Route::Tcp(Prepared::new(req));
        }
        let Some(host) = req.uri().host() else {
            return Route::Tcp(Prepared::new(req));
        };
        let (host, port) = (
            host.to_owned(),
            req.uri().port_u16().unwrap_or(HTTPS_DEFAULT_PORT),
        );

        // The two requests this transport asks nothing about. Neither is
        // prepared, so the TCP stack behaves for them exactly as it does
        // for a request that never met this crate — in particular an IP
        // literal keeps paying its record lookup once per *connection*
        // inside the connector, rather than once per request out here.
        if is_ip_literal(&host) || !self.dns.supports_svcb() {
            return self.by_advertisement(Prepared::new(req), &host, port);
        }

        let prepared = self.tcp.prepare(req).await;
        let offers_h3 = match prepared.discovered() {
            Discovered::Record { alpn } => Some(alpn.iter().any(|a| a.as_slice() == ALPN_H3)),
            // An answer, and the reason `Discovered` has three variants:
            // the connector looked and there is no record, so this
            // transport must not look again — and the slow tier is what
            // this case is for, which is the common case on today's web.
            Discovered::NoRecord => None,
            // Not an answer. The connector does not do discovery here, so
            // nobody has asked yet, and the question is still this
            // transport's to ask.
            Discovered::NotConsulted => {
                self.origin_offers_h3(&Self::record_name(&host, port)).await
            }
        };
        match offers_h3 {
            Some(true) => self.quic_unless_it_failed(prepared, &host, port),
            Some(false) => Route::Tcp(prepared),
            None => self.by_advertisement(prepared, &host, port),
        }
    }

    /// The last word on every request the two tiers send to QUIC: **what
    /// this transport managed to reach**, which is a different question
    /// from what the origin offers.
    ///
    /// It is a veto rather than a fourth tier, and the order is the whole
    /// of it. The record and the cache answer *"does this origin speak
    /// HTTP/3"*; only the origin can answer that, and a failed connect of
    /// ours is not evidence against it — a network that blocks UDP/443
    /// says nothing about the server behind it. So the memory does not
    /// overrule the record and does not remove the advertisement; it stops
    /// this transport from spending a connect on an answer it already has,
    /// and stops nothing else. When the window closes the very next
    /// request tries QUIC again, with the record and the cache exactly as
    /// they were.
    ///
    /// Both tiers go through here, and that is the point of it being one
    /// function: an origin that publishes a record listing `h3` on a
    /// network that blocks UDP/443 is precisely the case this exists for,
    /// and a veto that only covered the advertisement would leave it.
    fn quic_unless_it_failed(&self, prepared: Prepared, host: &str, port: u16) -> Route {
        if self
            .h3_failures
            .suppressed(&Origin::new(host, port), self.now())
        {
            return Route::Tcp(prepared);
        }
        Route::Quic {
            req: prepared.into_request(),
            fallback: true,
        }
    }

    /// The slow tier's answer for a request the fast tier could not
    /// settle, with the prepared request carried through either way.
    ///
    /// Split out of [`Self::route`] because it is reached from two places
    /// — a request with no record, and a request this transport asks no
    /// record for — and one of them is the IP literal, which is served by
    /// this tier and not by the fast one (`altsvc`'s doc says why that is
    /// not an exception).
    fn by_advertisement(&self, prepared: Prepared, host: &str, port: u16) -> Route {
        if self
            .alt_svc
            .advertises_h3(&Origin::new(host, port), self.now())
        {
            self.quic_unless_it_failed(prepared, host, port)
        } else {
            Route::Tcp(prepared)
        }
    }

    /// The origin an `Alt-Svc` advertisement is remembered against, or
    /// `None` for a request that has no business in the slow tier at all.
    ///
    /// `https` and a host, and nothing else: those are exactly the
    /// requests that could be chosen onto QUIC, so they are exactly the
    /// ones whose advertisements can be acted on. An IP literal *is* one —
    /// it has an origin even though it has no name, and the fast tier
    /// skips it for a reason that is about DNS rather than about QUIC.
    fn alt_svc_origin(uri: &http::Uri) -> Option<Origin> {
        if uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
            return None;
        }
        Some(Origin::new(
            uri.host()?,
            uri.port_u16().unwrap_or(HTTPS_DEFAULT_PORT),
        ))
    }

    /// Take in whatever this response said about the origin it came from.
    ///
    /// # Absence is not an instruction, and it costs nothing
    ///
    /// A response with no `Alt-Svc` field leaves here before the clock is
    /// read or the cache's lock is taken — which is every response from
    /// every origin that has never heard of the header, and is why the
    /// slow tier does not make anything slower. RFC 7838 §3's *"invalidates
    /// and replaces"* is about a field that is **present**; a missing one
    /// says nothing and changes nothing.
    ///
    /// # Both stacks are read, not only the TCP one
    ///
    /// The header is how an origin keeps its advertisement fresh and how
    /// it withdraws one, and it can do either over HTTP/3 as easily as
    /// over HTTP/1.1. Reading it only on the TCP path would leave a
    /// `clear` sent over QUIC unheard, and an origin unable to take back
    /// what it advertised without first being dropped back to TCP by
    /// something else.
    ///
    /// # Repeated field lines
    ///
    /// RFC 9110 §5.3 makes several `Alt-Svc` lines one comma-joined list,
    /// so they are parsed one at a time and their alternatives run
    /// together — with a `clear` in any of them winning outright, which is
    /// §3's own rule for a reply carrying both.
    fn note_alt_svc(&self, origin: &Origin, headers: &http::HeaderMap) {
        let mut values = headers.get_all(ALT_SVC).into_iter().peekable();
        if values.peek().is_none() {
            return;
        }
        let mut alternatives = Vec::new();
        for value in values {
            match altsvc::parse(value.as_bytes()) {
                altsvc::FieldValue::Clear => {
                    self.alt_svc
                        .note(origin, &altsvc::FieldValue::Clear, self.now());
                    return;
                }
                altsvc::FieldValue::Alternatives(a) => alternatives.extend(a),
            }
        }
        self.alt_svc.note(
            origin,
            &altsvc::FieldValue::Alternatives(alternatives),
            self.now(),
        );
    }

    /// Whether the highest-preference ServiceMode record under `name` lists
    /// `h3` — and `None` when there is no such record at all.
    ///
    /// **Three states rather than two, and the third is what makes the two
    /// tiers compose.** "This origin publishes a record and it does not
    /// offer `h3`" and "this origin publishes no record" are different
    /// facts: the first is the origin's own ranking, which the slow tier
    /// must not overrule, and the second is the silence that leaves the
    /// slow tier the only thing there is to go on. Collapsing them to
    /// `false` would let a stale header beat a fresh record.
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
    /// the same reason `hclient-native`'s selection gives: they arrive with
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
    async fn origin_offers_h3(&self, name: &str) -> Option<bool> {
        let mut best: Option<hclient_dns::SvcbEndpoint> = None;
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
        best.map(|e| e.alpn.iter().any(|a| a.as_slice() == ALPN_H3))
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

/// The QUIC arm, which is the one arm this transport does not simply hand
/// a request to.
///
/// The bounds are the [`Transport`] impl's below, repeated because an
/// inherent method cannot inherit them.
impl<R, T, D> Selecting<R, T, D>
where
    R: TcpConnect + Timer + Clone,
    T: TlsConnect,
    Native<R, T, D>: Prefetch + Transport<Error = Error> + TcpStagedConnect<Error = Error>,
    H3<R, T, D>: StagedConnect<Error = Error>,
    <Native<R, T, D> as Transport>::Body:
        http_body::Body<Data = bytes::Bytes, Error = Error> + Unpin,
    <H3<R, T, D> as Transport>::Body: http_body::Body<Data = bytes::Bytes, Error = Error> + Unpin,
    D: Resolve,
{
    /// Ask the QUIC stack to connect; spend the connection if it answered,
    /// and route the request over TCP if it did not.
    ///
    /// # This is the staged connect's first customer, and the race is not
    ///
    /// `docs/v04-w1-acceptance.md` §9.3's first blocker was that a failure
    /// memory *"degrades the caller rather than protecting them"* without a
    /// fallback, and that the fallback would be *"request-level retry with
    /// a `RequestBody::retry_kind()` condition on it"*. It is not, because
    /// the request is never handed to the QUIC stack: `connect` takes it,
    /// fails, and gives it back untouched. Nothing was sent, so there is
    /// nothing to decide about idempotency — [`hclient_h3::Refused`] is the
    /// type that makes that a fact about the code rather than a promise.
    ///
    /// # The budget is spent once, which took work
    ///
    /// `Timeouts::connect` is one bound for *this request*, and a
    /// sequential fallback is the plainest way to double it: the QUIC arm
    /// spends it, and the TCP arm reads the same field off the same request
    /// and spends it again. *"A bound a server can double by answering
    /// `425` is not a bound"* — `hclient::Client`'s rule, and the same
    /// arithmetic. So the request handed to TCP carries what is **left**,
    /// and where nothing is left the QUIC failure stands as the answer.
    ///
    /// That is a rewrite of the caller's extension, which this transport
    /// otherwise never does, and the value written is not a policy of ours:
    /// it is the caller's own bound minus what has been spent against it.
    ///
    /// # What it costs, and who pays
    ///
    /// One extra type-65 query, on the fallback path only. The record this
    /// request's `Prepared` carried was consumed when the QUIC arm took the
    /// request, and there is deliberately no way to pair a record with a
    /// request it was not fetched for — so the TCP member does its own
    /// lookup, exactly as it does for a request that never met this crate.
    /// **It is paid by the request that discovers the failure and by no
    /// other**, which is what the memory is for.
    async fn over_quic(
        &self,
        req: http::Request<RequestBody>,
        fallback: bool,
        origin: Option<&Origin>,
    ) -> Result<http::Response<SelectedBody<NativeBodyOf<R, T, D>, QuicBodyOf<R, T, D>>>, Error>
    {
        let began = self.now();
        let refused = match self.quic.connect(req).await {
            Ok(staged) => {
                return self
                    .quic
                    .exchange(staged)
                    .await
                    .map(|r| r.map(SelectedBody::Quic));
            }
            Err(refused) => refused,
        };
        // Read once, after the connect and before anything else: the two
        // uses below — how much of the bound is gone, and when this failure
        // expires — are one instant, and two reads would be two facts where
        // there is one.
        let now = self.now();
        let (error, req) = refused.into_parts();
        if let Some(origin) = origin {
            self.h3_failures.note(origin, now);
        }
        if !fallback {
            return Err(error);
        }
        self.after_quic_failed(req, error, now.saturating_sub(began))
            .await
    }

    /// The tail of [`Self::over_quic`], from *"the QUIC connect failed and
    /// this request may go over TCP"* onwards.
    ///
    /// A function of its own because [`race`] reaches the same place by a
    /// different road — a race whose QUIC arm failed outright is the
    /// sequential fallback's own case arriving through the race — and two
    /// spellings of the budget rule is exactly the drift this crate keeps
    /// out of `route`.
    ///
    /// `spent` is what has already gone against `Timeouts::connect`: the
    /// connect, or the whole race. The failure memory is **not** written
    /// here, because the two callers know different things about which
    /// origin (and whether) to blame.
    async fn after_quic_failed(
        &self,
        mut req: http::Request<RequestBody>,
        error: Error,
        spent: Duration,
    ) -> Result<http::Response<SelectedBody<NativeBodyOf<R, T, D>, QuicBodyOf<R, T, D>>>, Error>
    {
        if !spend_connect_budget(&mut req, spent) {
            return Err(error);
        }
        self.tcp
            .execute_prepared(Prepared::new(req))
            .await
            .map(|r| r.map(SelectedBody::Tcp))
    }
}

/// The two member bodies, named once each so that `Selecting::over_quic`
/// and the [`Transport`] impl cannot spell them differently.
pub(crate) type NativeBodyOf<R, T, D> = <Native<R, T, D> as Transport>::Body;
pub(crate) type QuicBodyOf<R, T, D> = <H3<R, T, D> as Transport>::Body;

/// Take `spent` off the request's `Timeouts::connect`, and say whether
/// there is anything left to connect with.
///
/// `true` where no bound was set at all — an unbounded arm followed by an
/// unbounded arm is what a caller who set no bound asked for, and it is
/// what a QUIC black hole costs today (quinn's 30 s `max_idle_timeout`)
/// with or without this function.
///
/// `false` means the caller's whole connect budget went on the first arm,
/// and the honest answer is then the first arm's error rather than a second
/// attempt the bound has no room for. That is `docs/v04-w1-acceptance.md`
/// §7.5's precondition met by refusing rather than by silently degrading —
/// and it is never worse than the behaviour it replaces, where a QUIC
/// origin that could not be reached simply failed the request.
///
/// A free function so it can be read without the six `where` clauses the
/// method above carries.
fn spend_connect_budget(req: &mut http::Request<RequestBody>, spent: Duration) -> bool {
    let timeouts = req
        .extensions()
        .get::<Timeouts>()
        .copied()
        .unwrap_or_default();
    let Some(connect) = timeouts.connect else {
        return true;
    };
    let left = connect.saturating_sub(spent);
    // Written before the answer, not after it, because the two callers
    // want different things from `false`. The sequential fallback stops
    // and never sends this request, so the write it does not read is
    // harmless; the race's *winner* has a connection already and needs
    // what is left — including nothing at all, which is a bound the
    // member will answer with its own `Timeout(Connect)` one instant
    // before it would have anyway.
    req.extensions_mut().insert(Timeouts {
        resolve: None,
        connect: Some(left),
        ..timeouts
    });
    !left.is_zero()
}

impl<R, T, D> Transport for Selecting<R, T, D>
where
    // `Clone` for the reason given above the inherent impl: it is
    // `Native`'s bound, and this impl calls `Native`'s inherent methods.
    R: TcpConnect + Timer + Clone,
    T: TlsConnect,
    // `StagedConnect` on **both** members since the race (deliverable 5):
    // the QUIC arm has gone through the staged pair since the failure
    // memory landed, and the hedge is the TCP one's first consumer in this
    // workspace. See [`race`].
    Native<R, T, D>: Prefetch + Transport<Error = Error> + TcpStagedConnect<Error = Error>,
    // `StagedConnect` rather than `Transport` alone — it is a supertrait of
    // it, so this is the same bound plus the staged pair the QUIC arm goes
    // through. See `Selecting::over_quic`.
    H3<R, T, D>: StagedConnect<Error = Error>,
    <Native<R, T, D> as Transport>::Body:
        http_body::Body<Data = bytes::Bytes, Error = Error> + Unpin,
    <H3<R, T, D> as Transport>::Body: http_body::Body<Data = bytes::Bytes, Error = Error> + Unpin,
    D: Resolve,
{
    type Body =
        SelectedBody<<Native<R, T, D> as Transport>::Body, <H3<R, T, D> as Transport>::Body>;
    type Error = Error;

    /// Choose, hand the request over whole, and read what the answer said
    /// about the next one.
    ///
    /// Nothing is rewritten on the way: the chosen member gets the request
    /// the caller built, extensions included, and reads `Timeouts`,
    /// `AllowEarlyData` and `RequireVersion` off it exactly as it does when
    /// it is the only transport. This is what keeps the two members'
    /// behaviour a fact about them rather than a fact about this crate.
    ///
    /// The TCP arm goes through `Prefetch::execute_prepared` rather than
    /// `Transport::execute`, which is the same exchange with one thing
    /// added: the HTTPS record the choice was made from, so the connector
    /// does not fetch it a second time. Nothing about the request itself
    /// changes — the record travels *with* it, in a `Prepared` that
    /// [`Prefetch::prepare`] built out of this very request.
    ///
    /// The response is likewise handed back unchanged — the `Alt-Svc`
    /// field is *read* on the way past and is not removed, because a
    /// caller inspecting its own response is entitled to see what the
    /// origin sent. That read is the whole of the slow tier's cost on a
    /// response that carries no such field: one `HeaderMap` lookup, no
    /// clock and no lock (see `Selecting::note_alt_svc`).
    ///
    /// The QUIC arm is **staged** — `connect`, then `exchange` — and is
    /// the one place in this crate where the request is not simply handed
    /// over. See `Selecting::over_quic` for the two things that buys and
    /// the one thing it costs.
    ///
    /// **And where [`Selecting::hedging`] was called, it is raced**: a TCP
    /// connect is started `head_start` later and whichever arm produces a
    /// connection first carries the request, which is still handed to
    /// neither of them until the race is over. [`race`] is the whole of
    /// that, and it changes nothing about the two arms below it —
    /// including the one place this crate rewrites an extension, which is
    /// still `Timeouts::connect` and is still spent once.
    ///
    /// A failed *exchange* still teaches nothing: there is no response
    /// head to read, and a failure after the connect is not a fact about
    /// the origin's HTTP/3. A failed **connect** is, and is remembered —
    /// see [`failures`], which the race widened by one case and no more.
    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Error> {
        // Before the request is moved into `route`. A redirect chain
        // re-enters `execute` per hop with that hop's own URI, so each
        // origin in it hears its own advertisement and no other's.
        let origin = Self::alt_svc_origin(req.uri());
        let resp = match self.route(req).await {
            Route::Tcp(prepared) => self
                .tcp
                .execute_prepared(prepared)
                .await
                .map(|r| r.map(SelectedBody::Tcp)),
            // One call site, hedged or not — see [`race::Raced`] for why
            // that is load-bearing rather than tidy.
            Route::Quic { req, fallback } => self.serve_quic(req, fallback, origin.as_ref()).await,
        };
        if let (Ok(r), Some(origin)) = (&resp, &origin) {
            self.note_alt_svc(origin, r.headers());
        }
        resp
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
