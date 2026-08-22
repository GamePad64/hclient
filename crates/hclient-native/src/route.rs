//! Choosing between this transport's two stacks, and remembering what an
//! origin said about its own HTTP/3.
//!
//! Behind the `http3` feature, with the arm it routes to. Without it there
//! is one stack and nothing to choose between.
//!
//! # Two tiers, and the race is neither
//!
//! Browsers do not race an unknown origin: first contact is TCP unless
//! something said otherwise *before* the connection. An **HTTPS record**
//! says so at resolution time — the fast tier — and **`Alt-Svc`** is a
//! response header, so it can only help the *next* connection: the slow
//! tier, and the one that needs storage.
//!
//! The order between them is a rule rather than an accident: **the record
//! first, the cache only where there is no record.** So an origin that
//! publishes one never touches the cache, and the slow tier adds no query
//! and no lock to the fast tier's path.
//!
//! Racing the two stacks is a third thing, a hedge against a network that
//! blocks UDP/443, and it is off by default — see [`crate::Native::hedging`].
//!
//! # What the choice costs
//!
//! **One** type-65 query per request that has a name to ask about,
//! whichever stack answers. A [`RequireVersion`] demand, `http://` and an
//! IP literal cost none at all — and the record is fetched by the
//! connector's own lookup ([`crate::Prefetch::prepare`]), so a request
//! that ends up on TCP does not pay for a second one.

use crate::altsvc::{self, Origin};
use crate::connect::HTTPS_DEFAULT_PORT;
use crate::discovery::Discovered;
use crate::established::NativeBody as EstablishedBody;
use crate::{ALPN_H3, Native, Prefetch as _, Prepared, Protocol, spoken_version};
use futures_util::StreamExt as _;
use hclient_core::check_version;
use hclient_core::{Error, RequestBody, RequireVersion, Timeouts};
use hclient_dns::Resolve;
use hclient_rt::{TcpConnect, Timer};
use hclient_tls::TlsConnect;
use std::time::Duration;

/// The `Alt-Svc` header this transport reads on the way past.
const ALT_SVC: http::HeaderName = http::header::ALT_SVC;

/// Which stack a request goes to, carrying the request itself — and, on
/// the TCP arm, whatever the TCP stack has already been asked.
///
/// **A `Stack` and a request are one value rather than two.** The TCP arm
/// may carry a [`Prepared`], which is a request *with* the HTTPS
/// record that was fetched for it — the pairing that makes the record
/// impossible to misdirect (see [`hclient_native::Prepared`]) is exactly
/// the pairing that stops them being separable here.
enum Route {
    /// The request cannot be served at all, and the reason was known
    /// before anything was resolved or dialled.
    ///
    /// A variant rather than an early `return` in [`Native::route`], so
    /// every exit from the decision is a `Route` and [`Native::routed`]
    /// has one `match` — an early return there would be the one path that
    /// skips the `Alt-Svc` read below it.
    Refuse(Error),
    /// The QUIC stack. Nothing prepared travels here: `hclient-h3` reads
    /// no HTTPS record at all, so there is nothing to hand it.
    Quic {
        req: http::Request<RequestBody>,
        /// Whether a QUIC connect that fails may send this request over
        /// TCP instead.
        ///
        /// `false` for exactly one shape of request, and it is not a
        /// policy of this transport's: a caller who wrote
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
/// attempt the bound has no room for — refusing rather than silently
/// degrading, and never worse than the behaviour it replaces, where a QUIC
/// origin that could not be reached simply failed the request.
///
/// A free function so it can be read without the six `where` clauses the
/// method above carries.
pub(crate) fn spend_connect_budget(req: &mut http::Request<RequestBody>, spent: Duration) -> bool {
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

impl<R, T, D, H, P> Native<R, T, D, H, P>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
    H: hclient_core::unversioned::Hooks + Clone + Unpin,
    P: crate::proxy::ProxyProtocol,
{
    /// [`Transport::execute`], with the choice in front of it.
    ///
    /// The chosen path gets the request the caller built, extensions
    /// included, and reads `Timeouts`, `AllowEarlyData` and
    /// `RequireVersion` off it exactly as it does when there is nothing to
    /// choose between — which is what keeps each path's behaviour a fact
    /// about that path rather than about the routing.
    ///
    /// The response is handed back unchanged: the `Alt-Svc` field is
    /// *read* on the way past and is not removed, because a caller
    /// inspecting its own response is entitled to see what the origin
    /// sent. That read is the whole of the slow tier's cost on a response
    /// carrying no such field — one `HeaderMap` lookup, no clock and no
    /// lock.
    pub(crate) async fn routed(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<crate::NativeBody<R, T, H>>, Error> {
        // Before the request is moved into `route`. A redirect chain
        // re-enters `execute` per hop with that hop's own URI, so each
        // origin in it hears its own advertisement and no other's.
        let origin = Self::alt_svc_origin(req.uri());
        let resp = match self.route(req).await {
            Route::Refuse(e) => Err(e),
            Route::Tcp(prepared) => self.run(prepared).await,
            // One call site, hedged or not — see [`crate::race::Raced`] for
            // why that is load-bearing rather than tidy.
            Route::Quic { req, fallback } => self.serve_quic(req, fallback, origin.as_ref()).await,
        };
        if let (Ok(r), Some(origin)) = (&resp, &origin) {
            self.note_alt_svc(origin, r.headers());
        }
        resp
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
    /// Three things follow, and none of them is a policy of this transport's:
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
                // A demand this transport has no arm for is refused with
                // the type a connection that negotiated the wrong protocol
                // raises, rather than a second spelling of it — and
                // refused here, before a resolver or a socket is touched,
                // because the answer is already known.
                if self.h3.is_none() || !self.versions.h3 {
                    return Route::Refuse(
                        check_version(req.extensions(), spoken_version(Some(Protocol::Http11)))
                            .expect_err("HTTP/3 was demanded, which is not HTTP/1.1"),
                    );
                }
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
        // for a request that never met this transport — in particular an IP
        // literal keeps paying its record lookup once per *connection*
        // inside the connector, rather than once per request out here.
        if is_ip_literal(&host) || !self.dns.supports_svcb() {
            return self.by_advertisement(Prepared::new(req), &host, port);
        }

        let prepared = self.prepare(req).await;
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
        // **No arm is not a refusal here, unlike a demand.** A record or
        // an advertisement is the origin's hint about what it can do; the
        // caller asked for nothing, so a transport that cannot take the
        // hint serves the request over TCP and says nothing. Only
        // `RequireVersion(HTTP_3)` — where the caller *did* ask — becomes
        // a `VersionNotAvailable`.
        if self.h3.is_none() || !self.versions.h3 {
            return Route::Tcp(prepared);
        }
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

    /// Ask the QUIC stack to connect; spend the connection if it answered,
    /// and route the request over TCP if it did not.
    ///
    /// # This is the staged connect's first customer, and the race is not
    ///
    /// A failure memory without a fallback degrades the caller rather
    /// than protecting them, and the obvious fallback would be a
    /// request-level retry with a `RequestBody::retry_kind()` condition on
    /// it. This is not that, because
    /// the request is never handed to the QUIC stack: `connect` takes it,
    /// fails, and gives it back untouched. Nothing was sent, so there is
    /// nothing to decide about idempotency — [`crate::h3::Refused`] is the
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
    /// lookup, exactly as it does for a request that never met this transport.
    /// **It is paid by the request that discovers the failure and by no
    /// other**, which is what the memory is for.
    pub(crate) async fn over_quic(
        &self,
        req: http::Request<RequestBody>,
        fallback: bool,
        origin: Option<&Origin>,
    ) -> Result<http::Response<crate::NativeBody<R, T, H>>, Error> {
        let began = self.now();
        // Read before the request moves into the arm: `hclient-h3`
        // declares `between_bytes: false` and this transport declares
        // `true`, so a body from the QUIC arm that escaped `bound_body`
        // would be the silent hole in a declared capability that helper
        // exists against. The arm is erased; the promise is this
        // transport's.
        let every = req
            .extensions()
            .get::<Timeouts>()
            .copied()
            .unwrap_or_default()
            .between_bytes;
        // Through the erased pair rather than a concrete `H3`, which is
        // what keeps the QUIC bounds off this transport's own signature —
        // see `crate::h3_arm`. `connect_boxed` hands back a handle
        // borrowed from the arm, so it cannot outlive this call.
        let Some(arm) = self.h3.as_ref().filter(|_| self.versions.h3) else {
            return Err(Error::new(hclient_core::ErrorKind::Unsupported, NoQuicArm));
        };
        let refused = match arm.connect_boxed(req).await {
            Ok(staged) => {
                return staged
                    .exchange_boxed()
                    .await
                    .map(|r| self.bound_body(r.map(EstablishedBody::from_h3), every));
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
    /// spellings of the budget rule is exactly the drift this transport keeps
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
    ) -> Result<http::Response<crate::NativeBody<R, T, H>>, Error> {
        if !spend_connect_budget(&mut req, spent) {
            return Err(error);
        }
        self.run(Prepared::new(req)).await
    }
}

/// Routing chose the QUIC arm on a transport that has none.
///
/// Unreachable through [`Native::route`], which only ever chooses QUIC
/// after finding an arm — it exists because `over_quic` is also reached
/// from the hedge, and an `unreachable!` in a transport is a panic in a
/// caller's process for a mistake that is ours.
#[derive(Debug, thiserror::Error)]
#[error("this transport has no QUIC arm; see `Native::http3`")]
pub struct NoQuicArm;
