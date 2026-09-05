// `Timeouts` is defined in `hclient-core`: transports read it from
// `http::Extensions`, and they don't depend on `hclient`.
use crate::error::InvalidBaseUrl;
pub use hclient_core::Timeouts;
use hclient_core::{
    Capabilities, Error, ErrorKind, RedirectSupport, RequireVersion, UnsupportedCapability,
};
use hclient_proto::redirect::RedirectPolicy;

/// A redirect policy as the client stores it.
///
/// `Arc<dyn ..>` rather than a type parameter, so `Client` keeps naming
/// none — and so a composed `Limit::new(10).and(SameOriginOnly)` is boxed
/// **once**, at `build()`, rather than costing anything per hop.
///
/// `Send + Sync` is amendment C12's shape: a value the caller owns
/// reaching `Client` by erasure, bound where they hand it over and
/// nowhere else.
/// A retry policy as the client stores it — see [`SharedRedirectPolicy`]
/// for why it is an `Arc<dyn ..>` and not a type parameter.
pub type SharedRetryPolicy = std::sync::Arc<dyn hclient_proto::retry::RetryPolicy + Send + Sync>; // send-bound-exception: amendment-C12

pub type SharedRedirectPolicy = std::sync::Arc<dyn RedirectPolicy + Send + Sync>; // send-bound-exception: amendment-C12

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {
    pub timeouts: Timeouts,
    /// A ceiling on the bytes a response body may yield, or `None` for
    /// none. Which bytes it counts is the outermost body wrapper's answer,
    /// and [`crate::body::ClientBody`] says why it is outermost: what the
    /// caller receives rather than what crossed the wire.
    ///
    /// `None` by default, and that is the decision rather than an
    /// omission: a default ceiling would fail a caller's legitimate large
    /// download on a number this crate picked, which is the shape
    /// `TcpOpts`' every-field-off default exists to avoid. `ureq` chooses
    /// the other way with 10 MB; the difference is that this client's
    /// callers include one streaming an arbitrary body to disk.
    pub response_limit: Option<u64>,
    /// Headers put on every request this client sends, including every
    /// redirect hop, unless the caller set that header on the request
    /// itself.
    ///
    /// **Empty by default, and no `User-Agent` is invented.** A default
    /// this crate chose would be a string every deployment of it announced
    /// to every server, changing what goes on the wire for a caller who
    /// asked for nothing — the rule `TcpOpts`' every-field-off default
    /// exists for. `reqwest` sends none either; `ureq` does.
    ///
    /// The caller's own header wins, for the reason `Host:` and
    /// `Content-Type` already follow one and two layers down: a header
    /// the caller wrote on the request is a decision about that request,
    /// and a default is a decision about the client.
    pub default_headers: http::HeaderMap,
    /// When to send a hop again, or `None` for never — the default.
    ///
    /// `None` rather than `Some(RetryPolicy::default())` because the
    /// default policy still retries *something*: a request that provably
    /// never reached a server. Opening sockets a caller did not ask for
    /// is a decision about their program's behaviour on a flaky network,
    /// so it is theirs to make — the same reason the QUIC race and the
    /// cookie jar are both off until asked for.
    pub retry: Option<SharedRetryPolicy>,
    /// `Option::None` here is "the caller never asked for a redirect
    /// policy" — distinct from `Some(Forbid)`, which is the
    /// caller explicitly asking not to follow and to be handed the 3xx.
    /// The distinction is load-bearing: the first is accepted by every
    /// backend, the second is refused by one that follows redirects
    /// internally, because it cannot be honoured there.
    ///
    /// A third thing again is `Some(Limit::new(0))` — follow
    /// zero hops, so the first 3xx is `ErrorKind::Redirect`.
    ///
    /// An `Option` rather than a bare `RedirectPolicy` because
    /// `check_supported` has to tell those two apart:
    /// `RedirectPolicy::default()` is `Limited(10)`, so a check that
    /// fired on any policy at all would reject every client built against
    /// a backend that follows redirects internally, including one whose
    /// author never mentioned redirects. Same idiom as `Timeouts`, whose
    /// fields are `Option<Duration>` for exactly the same reason.
    ///
    /// Every read site takes `unwrap_or_default()`, so an unconfigured
    /// client still follows up to `RedirectPolicy::default()`'s ten hops —
    /// this field's type changed, no behavior did.
    pub redirect: Option<SharedRedirectPolicy>,
    pub base_url: Option<http::Uri>,
    /// A bound on the **whole operation**, measured with the clock the
    /// client carries as its second type parameter.
    ///
    /// **Deliberately not a fourth field of [`Timeouts`]**, and the
    /// distinction is the difference between a capability that describes
    /// the transport and one that lies about it. `Timeouts` lives in
    /// `hclient-core` because TRANSPORTS read it out of
    /// `http::Extensions` and enforce it; no transport can enforce this
    /// one, because none of them owns the redirect loop that defines where
    /// the operation begins and ends. A `TimeoutSupport::total` next to
    /// `connect`/`first_byte`/`between_bytes` would therefore be a field
    /// describing the CLIENT sitting in a struct describing the backend —
    /// the shape this project has caught four times.
    ///
    /// It is consequently not checked against `Capabilities` at all (see
    /// `check_supported` below). What could be unhonourable here is the
    /// absence of a clock, and that is settled in the type system instead:
    /// see [`crate::NoClock`].
    ///
    /// Set by [`crate::ClientBuilder::total_timeout`] or
    /// [`crate::Client::total_timeout`]. There is deliberately no
    /// per-request override yet — see the v0.2 W4 report.
    pub total: Option<core::time::Duration>,
    /// The caller asked this client to keep a cookie jar of its own.
    ///
    /// A `bool` here and the jar itself in `Client`'s `Inner`, which is
    /// not an arrangement anyone would pick for its looks. `Config` is
    /// per-handle and `Clone`: [`crate::Client::total_timeout`] hands back
    /// a second handle over the same transport by cloning it. A jar in
    /// here would be *copied* by that call, and the two handles would then
    /// disagree about what the server had set — the jar is shared state,
    /// so it lives behind the same `Arc` the transport does. What has to
    /// be in `Config` is the one bit `check_supported` reads at
    /// `build()`, and this is that bit.
    ///
    /// Set only by [`crate::ClientBuilder::cookie_jar`], which exists only
    /// under the `cookies` feature. **The field is not `#[cfg]`-ed with
    /// it**, on purpose: `check_supported` destructures `Config` without a
    /// `..`-remainder precisely so that a new field cannot be forgotten,
    /// and a field that appears and disappears would take that check —
    /// and the refusal it performs — with it into half the builds.
    /// Without the feature nothing can set this, so it is `false` and
    /// `check_cookies_supported` is inert.
    pub cookies: bool,
    /// The caller asked this client to keep a response cache of its own.
    ///
    /// A `bool` here and the cache itself in `Client`'s `Inner`, for the
    /// reason spelled out on `cookies` one field up and for one more: the
    /// cache is shared not only by every clone of the client but by every
    /// *response body* it has handed out, since a recording body holds the
    /// same `Arc` and commits into it when it ends.
    ///
    /// Set only by [`crate::ClientBuilder::cache`], which exists only
    /// under the `cache` feature, and **not `#[cfg]`-ed with it** — the
    /// same argument the field above carries: `check_supported`
    /// destructures `Config` with no `..` remainder precisely so a new
    /// field cannot be forgotten, and a field that appears and disappears
    /// takes that check into half the builds with it.
    pub cache: bool,
}

/// The URI the request will actually go out on: `url`, resolved against
/// `base` if a base is set.
///
/// The rule is RFC 3986 §5, the exact same one `redirect::decide` uses to
/// resolve `Location:`; the shared implementation is
/// `hclient_proto::uri::resolve_reference`. One client shouldn't understand
/// `/x` two different ways depending on whether the server sent it or the
/// caller did.
///
/// **Works on the string, not on `http::Uri`, and that's forced.**
/// `http::Uri` can't represent a path-relative reference at all:
/// `"v1/things"`, `"search?q=1"`, and `""` are all `InvalidUri` (measured).
/// And these are exactly the forms the base exists for: a reference with a
/// leading `/` REPLACES the base's path entirely per RFC, so if already-
/// parsed `Uri`s were resolved, the base's path could never affect
/// anything and the setting would amount to "set the origin", not "set the
/// base URL".
///
/// Without a base, it's an ordinary parse: there's nowhere to invent a
/// scheme and authority for the caller from, and a transport that needs
/// absolute-form will reject a relative URI itself, with its own type
/// (`WasiHttp` → `scheme_of`).
///
/// Before this round the function didn't exist at all, and `Config::base_url`
/// was written by a setter and read by nothing — the third "saved and
/// ignored" case in the project, and the second in this same struct after
/// `timeouts` (B1).
pub(crate) fn effective_uri(base: Option<&http::Uri>, url: &str) -> Result<http::Uri, Error> {
    let Some(base) = base else {
        // `hclient_proto::uri::parse`, NOT `url.parse::<http::Uri>()`.
        // That difference is the whole of the IDN inconsistency this
        // client used to have: `http::Uri` rejects a non-ASCII authority,
        // so `client.get("https://münchen.de/x")` failed here and
        // succeeded through the branch below, where `url::Url` punycoded
        // it. The conversion now lives at one boundary, in the sans-io
        // crate every backend shares.
        return hclient_proto::uri::parse(url).map_err(|e| Error::new(ErrorKind::Other, e));
    };
    hclient_proto::uri::resolve_reference(base, url).map_err(|e| match e {
        // "This base cannot be a base" is the setting's own problem, and
        // the only failure that can name both sides usefully.
        hclient_proto::uri::UriError::UnusableBase { .. } => Error::new(
            ErrorKind::Other,
            InvalidBaseUrl {
                base: base.clone(),
                requested: url.to_owned(),
            },
        ),
        // Everything else is about the reference, and `InvalidBaseUrl`'s
        // "a base URL must be absolute" would be a lie about it — a
        // non-ASCII host in a build without the `idn` feature most of all,
        // where the error is the only place the caller learns to send an
        // A-label.
        other => Error::new(ErrorKind::Other, other),
    })
}

/// "Request-first, client-fallback", field by field.
///
/// reqwest can't do this (issue #2641 is unimplemented), which forces
/// `act-cli` to build a separate `reqwest::Client` for every component
/// call.
/// `#[doc(hidden)]`: the client's own request-over-client merge, called
/// once per hop inside `execute`. A transport is handed the result, never
/// this function; the one caller outside `src` is this crate's own
/// `tests/timeouts.rs`.
pub(crate) fn effective_timeouts(req: &http::Extensions, client: &Timeouts) -> Timeouts {
    match req.get::<Timeouts>() {
        None => *client,
        Some(o) => Timeouts {
            resolve: o.resolve.or(client.resolve),
            connect: o.connect.or(client.connect),
            first_byte: o.first_byte.or(client.first_byte),
            between_bytes: o.between_bytes.or(client.between_bytes),
        },
    }
}

/// The same "request-first, client-fallback" rule for the redirect policy.
///
/// Exists because the limit really does vary per request in the consumer
/// this library is being built for: `act`'s `http-client` component
/// computes `if args.follow_redirects { 10 } else { 0 }` on every call, and
/// `follow_redirects` is a per-request argument. Without a per-request
/// override that shape can only be expressed by building a new `Client` per
/// request — the exact cost `RequestBuilder::timeouts` already exists to
/// avoid (reqwest #2641).
///
/// **Whole-value, not field-by-field like `effective_timeouts`.**
/// `RedirectPolicy` is a plain value, not an `Option`, so
/// there is no "field the request left unset" to fall through to the
/// client; a request that set a policy replaces the client's entirely. If
/// `RedirectPolicy` ever grows a second field, this is the function that
/// has to decide whether that stays true.
///
/// The channel is `http::Extensions`, the same one `timeouts` travels in —
/// but the reader is `Client::execute` itself, not the transport: no
/// transport reads a `RedirectPolicy`, and the redirect stage is the only
/// consumer there has ever been.
///
/// `pub(crate)`, unlike its `effective_timeouts` sibling above: same
/// reasoning as `check_redirect_supported` — nothing outside this crate has
/// a use for it, and the facade's export list is already over-wide.
pub(crate) fn effective_redirect(
    req: &http::Extensions,
    client: &Option<SharedRedirectPolicy>,
) -> Option<SharedRedirectPolicy> {
    // `.cloned()` where this was `.copied()`: the per-request override is
    // an `Arc` now, so the clone is a refcount bump rather than a `u8`
    // copy. One allocation-free bump per hop, against a policy that can
    // hold a closure and a chain.
    req.get::<SharedRedirectPolicy>()
        .cloned()
        .or_else(|| client.clone())
}

/// Called from `ClientBuilder::build()`. Not a single silent no-op.
///
/// Destructures `cfg` without a `..`-remainder — the same recipe as
/// `Capabilities`'s `the_default_is_the_conservative_base` in `hclient-core`: a new
/// field on `Config` becomes a compile error naming it, instead of being
/// silently skipped (same for `Timeouts` fields, in
/// `check_timeouts_supported`). `redirect` and `base_url` are deliberately
/// not checked for support today — the `_` explicitly records that
/// decision rather than forgetting the field.
///
/// "Not checked **for support**" is about `Capabilities`, and only about
/// that. Both fields ARE applied: `base_url` in `effective_uri` on every
/// request, `redirect` by the redirect stage in `Client::execute`. This
/// note exists because "nobody handles these fields" is the reading to
/// avoid: a field saved and never applied is a silent no-op, which this
/// crate has shipped before.
///
/// `redirect` is not `_` here, because `hclient-fetch` reports
/// `redirects: RedirectSupport::Internal`: the browser follows the chain
/// itself, `Client`'s redirect stage never sees a single 3xx, and a
/// configured `RedirectPolicy` would be precisely the silent no-op this
/// module was built against — the module whose own first line says "Not a
/// single silent no-op" would have contained exactly one. So the field is
/// checked now, by `check_redirect_supported` below.
///
/// `base_url` remains deliberately unchecked **for support** — the `_`
/// records that decision rather than forgetting the field.
///
/// `cookies` arrived the same way `redirect` did, and the condition for it
/// was written down before the field existed: `Capabilities::
/// owns_cookie_jar`'s doc comment says a client-level cookie setting
/// "earns its refusal here the way `RedirectSupport::Internal` earned its
/// variant — the setting, the variant and the `check_supported` arm arrive
/// together". This is that arm; see `check_cookies_supported`.
///
/// `cache` arrived third and by the same route, with one difference worth
/// noticing: `owns_cache` did not need a variant or a field added for it —
/// it had been sitting in `Capabilities` since v0.1, set by one backend and
/// read by nothing. See `check_cache_supported`.
/// `#[doc(hidden)]`: this is `ClientBuilder::build`'s own gate, not a
/// caller's tool. It stays `pub` because `hclient-native`'s integration
/// tests call it — an integration test sees the public API only — and it
/// is off the page because a caller reaches it by calling `build()`.
#[doc(hidden)]
pub fn check_supported(
    cfg: &Config,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    let Config {
        timeouts,
        redirect,
        base_url: _,
        // Not checked for support, and this one can never be: no transport
        // enforces a whole-operation bound (see the field's doc comment),
        // so there is no capability to check it against. What it needs
        // instead — a clock — is guaranteed by the client's type, not by a
        // runtime check, so there is nothing for `build()` to refuse.
        total: _,
        // Nor this one, and for a related reason: the ceiling is enforced
        // by a body wrapper this client owns, `Limited`, which sits
        // outside everything a transport hands back. There is no
        // capability a backend could report about it, because there is
        // nothing a backend could do to honour or refuse it.
        response_limit: _,
        // Checked, and it has to be: a backend that forbids a header owns
        // it, and a default the transport would drop is a client-side
        // setting silently ignored — the shape this whole function exists
        // to refuse. `hclient-fetch` forbids `User-Agent` among others,
        // because the browser writes them.
        default_headers,
        // **Not checked, and this is a decision rather than an
        // omission.** A retry is this client sending the same request
        // again through the ordinary path, so every transport can honour
        // it by construction — there is no capability a backend could
        // report about it and nothing it could fail to do. The one
        // condition that can refuse a retry is the *body*, and that is
        // `RequestBody::retry_kind()`, which is a fact about the request
        // rather than about the transport, so it is answered per request
        // and not at `build()`.
        //
        // Contrast `redirect` two fields down, which is checked precisely
        // because a backend can follow the chain itself and then a
        // caller's policy would be a setting silently ignored.
        retry: _,
        // Checked, and by the same rule as `redirect` two fields up rather
        // than a new one: a predicate is a redirect decision, and a
        // backend that follows the chain internally never asks it.
        // Refused under its own name, because a caller who wrote
        // `redirect_predicate` should not be told `redirect_policy` was
        // the problem.
        cookies,
        cache,
    } = cfg;
    check_default_headers_supported(default_headers, caps, backend)?;
    check_timeouts_supported(timeouts, caps, backend)?;
    check_redirect_supported(redirect, caps, backend)?;
    check_cookies_supported(*cookies, caps, backend)?;
    check_cache_supported(*cache, caps, backend)
}

/// A response cache of the client's own, against a transport that already
/// keeps one, is an error — not a setting that quietly does the work
/// twice.
///
/// **The counterpart `owns_cache` never had.** The field has been set to
/// `true` by exactly one backend since v0.1 and read by nobody, which is
/// honest — there was no client-side cache for it to refuse — and is the
/// state `check_cookies_supported` was in before `ClientBuilder::
/// cookie_jar` existed. This is the arm that ends it.
///
/// **`hclient-fetch` is the case this exists for, and what "twice" costs
/// there is not symmetrical with the jar's.** The browser has its own HTTP
/// cache and applies it inside `fetch()`, so a second cache in front of it
/// would hold entries the browser has already evicted, would answer from
/// them without the browser ever being asked, and — because `Cache-Control`
/// on a *request* is one of the fields a `fetch`-shaped transport cannot
/// reliably put on the wire — could not even be told to revalidate the way
/// the RFC provides for. Two caches disagreeing about one resource is
/// worse than either alone, and the client's is the one with less
/// information.
///
/// **Only the cache-owning direction is rejected**, exactly as for the
/// jar. A backend reporting `Capabilities::default()` says "keeps no cache",
/// which is the truth for a transport that has never thought about
/// caching, and is precisely where the client's own belongs.
///
/// **The gate is `owns_cache` and nothing else** — not
/// `response_decompression == Internal`, and not
/// `forbidden_request_headers` containing `Cache-Control`, even though
/// `hclient-fetch` happens to be the backend for all three. They are
/// different claims that coincide there by accident, the trap
/// `decompress::negotiate` spells out for `Accept-Encoding` and
/// `check_cookies_supported` repeats for `Cookie`. A transport that
/// decoded bodies internally while keeping no cache must still be
/// cacheable in front of.
pub(crate) fn check_cache_supported(
    cache: bool,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    if cache && caps.owns_cache {
        return Err(UnsupportedCapability {
            what: "cache",
            backend,
        });
    }
    Ok(())
}

/// A cookie jar of the client's own, against a transport that already
/// keeps one, is an error — not a setting that quietly does the work
/// twice.
///
/// **Twice is the point, and it is worse than useless.** `hclient-fetch`
/// is the case this exists for: the browser attaches `Cookie` itself and
/// processes `Set-Cookie` itself, and `Cookie` is on that backend's
/// `forbidden_request_headers`. A client-side jar there would store every
/// `Set-Cookie` a second time — including the ones the browser refused, on
/// its own rules — while the header it produced was dropped on the way
/// out. The result is not "redundant"; it is a jar whose contents differ
/// from the ones actually being sent, which is the worst of the three
/// possible outcomes.
///
/// **Only the jar-owning direction is rejected.** A backend that does not
/// keep a jar is exactly where the client's own belongs, and that includes
/// every backend reporting `Capabilities::default()` — silence there means
/// "keeps no jar", which is the truth for a transport that has never
/// thought about cookies.
///
/// **The gate is `owns_cookie_jar` and nothing else** — in particular NOT
/// `forbidden_request_headers` containing `Cookie`, even though the one
/// transport here that owns a jar also forbids the header. That is the
/// same trap `decompress::negotiate` spells out for `Accept-Encoding`: the
/// two claims coincide in `hclient-fetch` by accident, and they are
/// different claims. A transport that forbade `Cookie` while keeping no
/// jar would be a real hole — our header dropped, the jar filling up
/// anyway — and it is deliberately left as one rather than papered over
/// here, because it does not exist: no backend in this workspace takes
/// that shape, and a check with no case to catch is a check nobody can
/// test. The place to add it, if such a backend ever arrives, is this
/// function.
///
/// `pub(crate)`, like its two siblings and for the same reason: the
/// `hclient` facade already exports more plumbing than it should.
pub(crate) fn check_cookies_supported(
    cookies: bool,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    if cookies && caps.owns_cookie_jar {
        return Err(UnsupportedCapability {
            what: "cookie_jar",
            backend,
        });
    }
    Ok(())
}

/// A `RedirectPolicy` the caller actually asked for, against a backend
/// that follows redirects itself, is an error — not a setting that quietly
/// does nothing.
///
/// **Only `Internal` is rejected**, and that is the whole point of the
/// variant existing separately from `None`. Under `Transparent` (what
/// `wasi:http` does) the 3xx reaches us and `Client`'s own stage honours
/// the policy in full; under `None` the backend has simply said nothing
/// about redirects, which `Capabilities::default()` returns for every field it
/// hasn't been told about — neither is a reason to reject a policy the
/// stage can carry out.
///
/// **Only `Some` is rejected**, and that is why `Config::redirect` is an
/// `Option`. `RedirectPolicy::default()` is `Limited(10)`; a check
/// reading a bare `RedirectPolicy` could not distinguish "follow up to ten
/// hops, and I mean it" from "I never mentioned redirects", so it would
/// reject every browser client ever built — including `Client::new()`'s
/// own, whose `.expect(...)` in `client.rs` rests on this exact line not
/// firing. That is not a fix, it is an unusable backend. The idiom is
/// `check_timeouts_supported`'s `connect.is_some()`, one function down:
/// "the caller asked for this".
///
/// `pub(crate)`, like `check_timeouts_supported` and for the same reason:
/// `Client::execute` needs it over a merged value rather than over a whole
/// `Config`, and the `hclient` facade already exports more plumbing than
/// it should.
pub(crate) fn check_redirect_supported(
    redirect: &Option<SharedRedirectPolicy>,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    if redirect.is_some() && caps.redirects == RedirectSupport::Internal {
        return Err(UnsupportedCapability {
            what: "redirect_policy",
            backend,
        });
    }
    Ok(())
}

/// A [`RequireVersion`] demand against a backend that cannot honour one is
/// an error — not a mark that quietly goes unread.
///
/// **The whole shape is `check_redirect_supported`'s**, one function up,
/// deliberately and down to the `what` string: a caller who asked for
/// something the transport cannot do gets an
/// [`UnsupportedCapability`] naming the setting and the backend, at the
/// same point in `Client::run` as the timeouts and the redirect policy. A
/// second refusal path — a different error kind, a different moment, a
/// `Result` shaped some other way — would be a second thing to learn for
/// no gain.
///
/// # There is no client-level half, and that is the decision
///
/// `Timeouts` and `RedirectPolicy` are each merged from a client setting
/// and a per-request one before being checked here. This one has no client
/// setting to merge. Turning an ALPN outcome into a request failure is
/// right for a gRPC call and wrong for a browser-shaped fetch, and a
/// client-wide switch would apply one answer to both — the same argument
/// that keeps `AllowEarlyData` per request. So the demand is read from the
/// request's extensions and nowhere else, and `Config` gains no field.
///
/// # What `version_select` has to mean for this to be right
///
/// `true` is "honours a demand", not "chooses a version" — so
/// `hclient-h3`, which speaks HTTP/3 and nothing else, reports `true` and
/// answers `RequireVersion(HTTP_3)` by proceeding. If it reported `false`
/// this function would refuse that request, which is the failure mode this
/// doc exists to prevent someone reintroducing: the gate is about whether
/// the backend can *answer*, and the answer itself belongs to the
/// transport.
///
/// The backends that report `false` are `hclient-fetch` and
/// `hclient-wasi`, and for them the refusal is the only honest outcome:
/// neither selects the protocol version nor learns it — both also report
/// `version_reported: false` — so there is no moment at which either could
/// compare a demand against anything. Silently ignoring the mark would
/// hand a caller who requires HTTP/2 a response over whatever the browser
/// or the host happened to use.
///
/// `pub(crate)`, like its siblings and for the same reason: the `hclient`
/// facade already exports more plumbing than it should.
pub(crate) fn check_version_demand_supported(
    extensions: &http::Extensions,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    if extensions.get::<RequireVersion>().is_some() && !caps.version_select {
        return Err(UnsupportedCapability {
            what: "require_version",
            backend,
        });
    }
    Ok(())
}

/// The same check, but over a single `Timeouts` rather than the whole
/// `Config` — because `Client::execute` checks the **merged** result of
/// `effective_timeouts`, not the client's configuration — checking the
/// latter leaves per-request timeouts unchecked and client-level ones
/// checked but never reaching the transport. One shared body, not a second
/// copy of the `checks` array:
/// these two checks must not drift apart, and two phase lists will drift
/// the moment a new phase shows up.
///
/// `pub(crate)`, unlike `check_supported`: the `hclient` facade already
/// exports more plumbing than it should (finding §6.7 of the same review),
/// and there's no reason to grow that debt with a new name.
pub(crate) fn check_timeouts_supported(
    t: &Timeouts,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    let Timeouts {
        resolve,
        connect,
        first_byte,
        between_bytes,
    } = t;
    let checks = [
        (resolve.is_some(), caps.timeouts.resolve, "resolve_timeout"),
        (connect.is_some(), caps.timeouts.connect, "connect_timeout"),
        (
            first_byte.is_some(),
            caps.timeouts.first_byte,
            "first_byte_timeout",
        ),
        (
            between_bytes.is_some(),
            caps.timeouts.between_bytes,
            "between_bytes_timeout",
        ),
    ];
    for (requested, supported, what) in checks {
        if requested && !supported {
            return Err(UnsupportedCapability { what, backend });
        }
    }
    Ok(())
}

/// A default header a backend forbids is an error at `build()`, never a
/// header quietly dropped on the way out.
///
/// The asymmetry with `RequestBuilder::header` is deliberate and is about
/// **when the caller finds out**: a per-request header is written next to
/// the request that carries it, where a default is written once and
/// applies to traffic the author may never look at again. A client whose
/// `User-Agent` silently vanished on one backend would be a client whose
/// author had no reason to look.
fn check_default_headers_supported(
    headers: &http::HeaderMap,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    // `what` names the setting rather than the header, which is the
    // convention every other arm here follows — and the header is not
    // `&'static str`, so carrying it would mean widening a core type for
    // one call site. The caller wrote the map; the transport's own
    // `forbidden_request_headers` is what says which of theirs it was.
    for name in headers.keys() {
        if caps.forbidden_request_headers.contains(name) {
            return Err(UnsupportedCapability {
                what: "default_headers",
                backend,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hclient_core::{Capabilities, TimeoutSupport};
    use std::time::Duration;

    fn secs(n: u64) -> Option<Duration> {
        Some(Duration::from_secs(n))
    }

    /// `Capabilities::default()` with only `redirects` changed — everything
    /// else stays off, so nothing but the field under test can make
    /// `check_supported` return `Err` and pass one of these tests for the
    /// wrong reason.
    fn caps_with_redirects(r: RedirectSupport) -> Capabilities {
        let mut c = Capabilities::default();
        c.redirects = r;
        c
    }

    #[test]
    fn request_overrides_client_field_by_field() {
        let client = Timeouts {
            resolve: None,
            connect: secs(1),
            first_byte: secs(2),
            between_bytes: secs(3),
        };
        let mut ext = http::Extensions::new();
        ext.insert(Timeouts {
            resolve: None,
            connect: secs(9),
            ..Default::default()
        });
        let eff = effective_timeouts(&ext, &client);
        assert_eq!(eff.connect, secs(9), "request overrides");
        assert_eq!(eff.first_byte, secs(2), "the rest falls back to the client");
        assert_eq!(eff.between_bytes, secs(3));
    }

    #[test]
    fn client_config_used_when_request_says_nothing() {
        let client = Timeouts {
            resolve: None,
            connect: secs(1),
            ..Default::default()
        };
        let eff = effective_timeouts(&http::Extensions::new(), &client);
        assert_eq!(eff.connect, secs(1));
    }

    #[test]
    fn unsupported_timeout_is_an_error_not_a_silent_noop() {
        let cfg = Config {
            timeouts: Timeouts {
                resolve: None,
                between_bytes: secs(5),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut caps = Capabilities::default();
        caps.timeouts = TimeoutSupport {
            resolve: true,
            connect: true,
            first_byte: true,
            between_bytes: false,
        };
        let err = check_supported(&cfg, &caps, "wasi:http").unwrap_err();
        assert_eq!(err.what, "between_bytes_timeout");
        assert_eq!(err.backend, "wasi:http");
    }

    #[test]
    fn supported_config_passes() {
        let cfg = Config {
            timeouts: Timeouts {
                resolve: None,
                connect: secs(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut caps = Capabilities::default();
        caps.timeouts = TimeoutSupport {
            resolve: true,
            connect: true,
            first_byte: false,
            between_bytes: false,
        };
        assert!(check_supported(&cfg, &caps, "wasi:http").is_ok());
    }

    // ── extra checks: field-by-field, not all-or-nothing ─────────────
    //
    // `request_overrides_client_field_by_field` overrides `connect` and
    // checks that `first_byte`/`between_bytes` fall back to the client —
    // that already tells "field by field" apart from "all or nothing" (a
    // naive "if extensions has a Timeouts — take it whole" implementation
    // would return `None` here, not `client.first_byte`). The test below
    // hits a different field (`first_byte`), so the same property isn't
    // an artifact of `connect` being the struct's first field.
    #[test]
    fn request_overrides_first_byte_only_leaves_others_from_client() {
        let client = Timeouts {
            resolve: None,
            connect: secs(1),
            first_byte: secs(2),
            between_bytes: secs(3),
        };
        let mut ext = http::Extensions::new();
        ext.insert(Timeouts {
            resolve: None,
            first_byte: secs(9),
            ..Default::default()
        });
        let eff = effective_timeouts(&ext, &client);
        assert_eq!(
            eff.connect,
            secs(1),
            "not overridden by request — take the client's"
        );
        assert_eq!(eff.first_byte, secs(9), "request overrides");
        assert_eq!(
            eff.between_bytes,
            secs(3),
            "not overridden by request — take the client's"
        );
    }

    // ── extra checks: check_supported names the RIGHT phase ──────────
    //
    // `unsupported_timeout_is_an_error_not_a_silent_noop` covers only
    // `between_bytes`. Since `checks` in `check_supported` is an array of
    // three independent triples, a small refactor slip (say, a copy-pasted
    // index) could return the right error for one phase and a wrong one
    // (wrong field in `what`, or the wrong phase triggering the error) for
    // the other two, unnoticed. Check all three phases separately: each
    // one requests only that field, and only that same field is
    // unsupported in `Capabilities`.
    #[test]
    fn unsupported_connect_is_named_connect_not_another_phase() {
        let cfg = Config {
            timeouts: Timeouts {
                resolve: None,
                connect: secs(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut caps = Capabilities::default();
        caps.timeouts = TimeoutSupport {
            resolve: false,
            connect: false,
            first_byte: true,
            between_bytes: true,
        };
        let err = check_supported(&cfg, &caps, "wasi:http").unwrap_err();
        assert_eq!(err.what, "connect_timeout");
        assert_eq!(err.backend, "wasi:http");
    }

    #[test]
    fn unsupported_first_byte_is_named_first_byte_not_another_phase() {
        let cfg = Config {
            timeouts: Timeouts {
                resolve: None,
                first_byte: secs(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut caps = Capabilities::default();
        caps.timeouts = TimeoutSupport {
            resolve: true,
            connect: true,
            first_byte: false,
            between_bytes: true,
        };
        let err = check_supported(&cfg, &caps, "wasi:http").unwrap_err();
        assert_eq!(err.what, "first_byte_timeout");
        assert_eq!(err.backend, "wasi:http");
    }

    #[test]
    fn unsupported_between_bytes_is_named_between_bytes_not_another_phase() {
        let cfg = Config {
            timeouts: Timeouts {
                resolve: None,
                between_bytes: secs(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut caps = Capabilities::default();
        caps.timeouts = TimeoutSupport {
            resolve: false,
            connect: true,
            first_byte: true,
            between_bytes: false,
        };
        let err = check_supported(&cfg, &caps, "wasi:http").unwrap_err();
        assert_eq!(err.what, "between_bytes_timeout");
        assert_eq!(err.backend, "wasi:http");
    }

    // ── the redirect check: `Internal` backends, and only them ───────
    //
    // The prediction the old `redirect: _` in `check_supported` was
    // written against came true when `hclient-fetch` started reporting
    // `RedirectSupport::Internal`. These four tests are the check that
    // replaced it, and the middle two matter as much as the first: a check
    // that rejected any configured policy, or one that rejected under
    // anything but a single blessed variant, would break the browser
    // backend or the WASI one respectively while still passing the first
    // test. That second wording used to name `Configurable` as the blessed
    // one, which v0.4 W1 deleted — with three variants left, the mutant it
    // now describes is "reject unless `Transparent`", and that one is
    // caught, but by neither of the four tests below: it was run against
    // the whole workspace (1156 tests) and killed by exactly two, both in
    // `crates/hclient/tests/redirect.rs` — `enforces_the_hop_limit` and
    // `redirect_limit_of_zero_sends_only_the_original_request`. They catch
    // it because `MockTransport` starts from `Capabilities::default()`, whose
    // `redirects` is `None`, so a check keyed on `Transparent` refuses a
    // policy against every plain mock in the suite. The four tests here
    // cover `Internal` and `Transparent` and deliberately not `None`; the
    // guard against that arm is those two integration tests, which is where
    // to look before narrowing this condition.

    #[test]
    fn configured_redirect_policy_against_an_internal_backend_is_an_error() {
        let cfg = Config {
            redirect: Some(std::sync::Arc::new(hclient_proto::redirect::Limit::new(5))),
            ..Default::default()
        };
        let err = check_supported(
            &cfg,
            &caps_with_redirects(RedirectSupport::Internal),
            "fetch",
        )
        .unwrap_err();
        assert_eq!(err.what, "redirect_policy");
        assert_eq!(err.backend, "fetch");
    }

    /// The test that stops the fix from bricking the browser backend.
    ///
    /// `Config::default()` is what `Client::new()` builds on
    /// `wasm32-unknown-unknown`, and `Fetch` is an `Internal` backend — if
    /// "a policy is present" were read off a bare `RedirectPolicy` rather
    /// than an `Option`, `RedirectPolicy::default()` would make this `Err`
    /// and every browser client would fail to build, including one whose
    /// author never mentioned redirects.
    #[test]
    fn an_unconfigured_redirect_against_an_internal_backend_is_fine() {
        let cfg = Config::default();
        assert!(cfg.redirect.is_none(), "the premise of the whole check");
        assert!(
            check_supported(
                &cfg,
                &caps_with_redirects(RedirectSupport::Internal),
                "fetch"
            )
            .is_ok()
        );
    }

    /// `Transparent` specifically, not `None`: the two are different claims
    /// (`Capabilities::default()` returns `None` for a backend that said
    /// nothing, while `Transparent` is `wasi:http` — and, since v0.4 W1,
    /// `hclient-native` and `hclient-h3` — positively stating that the 3xx
    /// arrives as-is), and a check written as "reject unless `Configurable`"
    /// would pass a `None`-only test while breaking every non-browser
    /// backend that follows redirects through `Client`'s own stage.
    ///
    /// That sentence outlived its variant: `Configurable` was deleted in
    /// v0.4 W1, and the mutant it now describes is "reject unless
    /// `Transparent`". **This test does not catch that one** — it is the
    /// arm the mutant keeps. What catches it, measured across the whole
    /// workspace, is the pair named in the block comment above these four
    /// tests, both in `crates/hclient/tests/redirect.rs` and both by way of
    /// `MockTransport`'s `Capabilities::default()`.
    ///
    /// The direction this test *does* guard is the opposite one — a check
    /// that fired **on** `Transparent`, which would refuse a policy against
    /// `wasi:http`, `hclient-h3` and the native transport alike. Measured
    /// too, and it is the loud mutant of the three: 22 failures across the
    /// workspace, this test among them, since `portable_example.rs` and
    /// `hclient-tower` both build mocks that positively declare
    /// `Transparent` rather than leaving `Capabilities::default()`.
    #[test]
    fn configured_redirect_policy_against_a_transparent_backend_is_fine() {
        let cfg = Config {
            redirect: Some(std::sync::Arc::new(hclient_proto::redirect::Limit::new(5))),
            ..Default::default()
        };
        assert!(
            check_supported(
                &cfg,
                &caps_with_redirects(RedirectSupport::Transparent),
                "wasi:http"
            )
            .is_ok()
        );
    }

    /// The merge is whole-value, and the request wins.
    ///
    /// Not "the request's limit is used" alone — that would also pass if
    /// the function ignored its `client` argument entirely — so both
    /// directions are checked here, and the client-only case below.
    #[test]
    fn request_redirect_policy_replaces_the_clients() {
        use hclient_proto::redirect::Limit;

        // **Compared by behaviour, not by value.** The policy is an
        // `Arc<dyn ..>` now, so there is no `PartialEq` to assert on —
        // and asking the returned policy what it does is the better
        // assertion anyway: it would fail for a merge that returned the
        // right *type* configured wrongly.
        fn probe(hops: u8) -> hclient_proto::redirect::ProposedRedirect<'static> {
            static FROM: std::sync::LazyLock<http::Uri> =
                std::sync::LazyLock::new(|| "https://a/".parse().unwrap());
            static TO: std::sync::LazyLock<http::Uri> =
                std::sync::LazyLock::new(|| "https://a/next".parse().unwrap());
            hclient_proto::redirect::ProposedRedirect::new(
                &FROM,
                &TO,
                http::StatusCode::FOUND,
                &http::Method::GET,
                false,
                hops,
                &[],
            )
        }

        fn limit_in_force(p: &SharedRedirectPolicy) -> u8 {
            (0u8..=20)
                .find(|&h| {
                    matches!(
                        p.follow(&probe(h)),
                        hclient_proto::redirect::RedirectVerdict::Refuse(_)
                    )
                })
                .expect("some hop count is refused")
        }

        let client: Option<SharedRedirectPolicy> = Some(std::sync::Arc::new(Limit::new(3)));
        let mut ext = http::Extensions::new();
        let on_request: SharedRedirectPolicy = std::sync::Arc::new(Limit::new(7));
        ext.insert(on_request);

        assert_eq!(
            limit_in_force(&effective_redirect(&ext, &client).unwrap()),
            7,
            "the request's policy wins"
        );
        assert_eq!(
            limit_in_force(&effective_redirect(&http::Extensions::new(), &client).unwrap()),
            3,
            "with nothing on the request, the client's stands"
        );
        assert!(
            effective_redirect(&http::Extensions::new(), &None).is_none(),
            "and with neither, nobody asked"
        );
    }

    #[test]
    fn a_request_only_redirect_policy_is_still_checked_against_internal() {
        let mut ext = http::Extensions::new();
        ext.insert::<SharedRedirectPolicy>(std::sync::Arc::new(
            hclient_proto::redirect::Limit::new(0),
        ));
        let merged = effective_redirect(&ext, &None);
        let err = check_redirect_supported(
            &merged,
            &caps_with_redirects(RedirectSupport::Internal),
            "fetch",
        )
        .unwrap_err();
        assert_eq!(err.what, "redirect_policy");
    }
}
