//! One transport that owns both protocol stacks and picks one per origin.
//!
//! ```no_run
//! # fn doc() -> Result<(), Box<dyn std::error::Error>> {
//! use http_ng_select::Selecting;
//! let rt = http_ng_rt_tokio::TokioHandle::current()?;
//! let dns = http_ng_dns_system::SystemDns::new(rt.clone());
//! let t = Selecting::new(
//!     rt.clone(),
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
//! **Racing the two stacks is a third thing and is not here.** It is a
//! hedge against a network that blocks UDP/443, applied *after* the choice
//! rather than in place of it, and v0.3 W2 recorded that the size of the
//! cost it pays is unverified. A measurement comes before a policy.
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
//! At most one `Resolve::lookup_svcb` per request, and it is **not** shared
//! with the one `http-ng-native` makes inside its own connector: that one
//! is `pub(crate)`, there is no way to hand a record across the
//! `Transport` seam, and this crate treats its members as read-only. So at
//! an origin's **default port**, where `http-ng-native` also does
//! discovery, an HTTP/1.1 or HTTP/2 request through this transport issues
//! the type-65 query **twice**; an HTTP/3 one issues it once, because
//! `http-ng-h3` does no SVCB lookup at all. At a non-default port
//! `http-ng-native` skips discovery entirely and the count is one either
//! way. Both counts are measured rather than reasoned —
//! `tests/dns_cost.rs` counts the calls a resolver received.
//!
//! **Alt-Svc adds none of them.** A request chosen onto QUIC by a
//! remembered advertisement pays the same single lookup an origin with no
//! record already paid — and one *fewer* than the same request would have
//! paid on TCP, because `http-ng-native`'s duplicate goes with it. Where
//! the resolver cannot do SVCB at all the count stays zero and the choice
//! is still available.
//!
//! A `RequireVersion` demand costs **nothing**: it is answered before the
//! resolver and the cache are asked at all.
//!
//! # No cache for the record, deliberately — and one for the header,
//! deliberately
//!
//! These are not in tension, and the difference is a fact about what each
//! answer carries. An HTTPS record remembered per origin would need a
//! lifetime this library would have to invent: *"this origin has no HTTPS
//! record" is a DNS answer with a TTL of its own, which `SvcbEndpoint`
//! does not carry, and inventing a lifetime for someone else's answer is
//! how a resolver's cache and ours drift apart* (`http-ng-native`'s
//! `discovery`). A resolver that caches (`http-ng-dns-hickory` does, with
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

pub use body::SelectedBody;
pub use caps::{Disagreement, combine};

use altsvc::{AltSvcCache, Origin};
use futures_util::StreamExt;
use http_ng_core::{Capabilities, Error, RequestBody, RequireVersion, unversioned::Transport};
use http_ng_dns::Resolve;
use http_ng_h3::H3;
use http_ng_native::{Discovered, Native, Prefetch, Prepared};
use http_ng_rt::{TcpConnect, Timer};
use http_ng_tls::TlsConnect;
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
    /// The clock, and the instant it is measured from.
    ///
    /// Held rather than borrowed from a member for the same reason `dns`
    /// is: `Native` and `H3` own theirs and expose no accessor. It is here
    /// for the slow tier alone — an `Alt-Svc` entry has a lifetime and
    /// something has to say when it has run out — and it is
    /// `http_ng_rt::Timer` rather than `std::time::Instant::now()` for
    /// `http-ng-native`'s reason: `Timer` is the one seam through which
    /// time reaches a transport here, so a caller testing under
    /// `tokio::time::pause()` sees what this transport sees.
    rt: R,
    /// What elapsed times below are measured from — this transport's
    /// construction, exactly as `Native`'s `epoch` is.
    epoch: R::Instant,
    alt_svc: AltSvcCache,
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
/// impossible to misdirect (see [`http_ng_native::Prepared`]) is exactly
/// the pairing that stops them being separable here.
enum Route {
    /// The QUIC stack. Nothing prepared travels here: `http-ng-h3` reads
    /// no HTTPS record at all, so there is nothing to hand it.
    Quic(http::Request<RequestBody>),
    /// The TCP stack, with whatever [`Native::prepare`] found for this
    /// request — including "nothing was looked up", for the requests this
    /// transport does not ask about (a `RequireVersion` demand, `http://`,
    /// an IP literal, a resolver that cannot ask). Those get
    /// [`Prepared::new`], which asserts nothing and leaves the connector's
    /// own discovery exactly as it was.
    Tcp(Prepared),
}

/// `R: Clone` is `Native`'s and not this crate's — it is the bound
/// `Native`'s own exchange impl declares (its response body outlives
/// `execute` and needs a clock of its own), and [`Native::prepare`] and
/// [`Native::execute_prepared`] are inherent methods on that impl, so
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
    /// not: nothing in `http-ng-rt` reports an interface coming up, a
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
    /// The failure it can cause is a slow one rather than a wrong one: an
    /// unreachable alternative costs a connect that fails, after which the
    /// request fails. What this crate does *not* have is the memory of
    /// that failure — see [`altsvc`] and `docs/v04-w1-acceptance.md` §9.
    pub fn network_changed(&self) {
        self.alt_svc.network_changed();
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
    /// [`Native::prepare`] does the lookup this transport needs *and* the
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
                Route::Quic(req)
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
            Some(true) => Route::Quic(prepared.into_request()),
            Some(false) => Route::Tcp(prepared),
            None => self.by_advertisement(prepared, &host, port),
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
            Route::Quic(prepared.into_request())
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
    async fn origin_offers_h3(&self, name: &str) -> Option<bool> {
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

impl<R, T, D> Transport for Selecting<R, T, D>
where
    // `Clone` for the reason given above the inherent impl: it is
    // `Native`'s bound, and this impl calls `Native`'s inherent methods.
    R: TcpConnect + Timer + Clone,
    T: TlsConnect,
    Native<R, T, D>: Prefetch + Transport<Error = Error>,
    H3<R, T, D>: Transport<Error = Error>,
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
    /// The TCP arm goes through `Native::execute_prepared` rather than
    /// `Transport::execute`, which is the same exchange with one thing
    /// added: the HTTPS record the choice was made from, so the connector
    /// does not fetch it a second time. Nothing about the request itself
    /// changes — the record travels *with* it, in a `Prepared` that
    /// [`Native::prepare`] built out of this very request.
    ///
    /// The response is likewise handed back unchanged — the `Alt-Svc`
    /// field is *read* on the way past and is not removed, because a
    /// caller inspecting its own response is entitled to see what the
    /// origin sent. That read is the whole of the slow tier's cost on a
    /// response that carries no such field: one `HeaderMap` lookup, no
    /// clock and no lock (see [`Selecting::note_alt_svc`]).
    ///
    /// A failed exchange teaches nothing: there is no response head to
    /// read, and this transport keeps no memory of failures — see
    /// [`altsvc`] for whose that would be.
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
            Route::Quic(req) => self
                .quic
                .execute(req)
                .await
                .map(|r| r.map(SelectedBody::Quic)),
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
