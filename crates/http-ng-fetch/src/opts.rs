//! The `RequestInit` members a caller could not reach.
//!
//! Until this module, `to_web_request` set `method`, `headers`, `body`,
//! `signal` and `duplex` and nothing else — so a browser caller could not
//! send `credentials: "include"`, which is what a cross-origin
//! authenticated request needs, could not select `no-cors`, and could not
//! touch the cache mode or the referrer policy. Two independent browser
//! clients expose all of it (reqwest's wasm build and `gloo-net`), which
//! is what makes it an absence rather than a knob nobody wants.
//!
//! # Why on `Fetch` and not on `Transport` or in an extension
//!
//! Three of five backends have no such concept, so a `Transport` method
//! would be `Unsupported` for most of them — the rule that put
//! `WebSocketConnect` in its own trait and `StagedConnect` in its own
//! crate. `Native::multiplexed()` and `Native::expect_continue()` are the
//! shape this copies.
//!
//! A request extension is the tempting alternative and is wrong for the
//! reason `Prefetch::prepare` refuses to take an HTTPS record from a
//! caller: an extension is a channel that any code able to build a request
//! can write to, and `credentials: "include"` is a decision about which
//! origins get the user's cookies.
//!
//! # `redirect` is deliberately absent, and it is the interesting one
//!
//! `fetch`'s `redirect: "manual"` does not hand back a `3xx` a caller can
//! act on. For a cross-origin response it yields an **opaque-redirect**
//! filtered response: `status` reads `0`, the header list is empty, the
//! body is null, and `Location` is not readable. So
//! `Capabilities::redirects` could not honestly move from
//! [`RedirectSupport::Internal`](http_ng_core::RedirectSupport::Internal)
//! to `Transparent` — it would claim a policy `Client` could act on for
//! exactly the case where redirects matter and the browser gives nothing.
//! `http-ng-urlsession` is the backend that *can* report `Transparent`,
//! and the asymmetry between the two is stated where each of them is.
//!
//! `redirect: "error"` is a third thing — *fail rather than follow* — and
//! it is `RedirectPolicy::None` with the answer thrown away, which no
//! caller has asked for.

use web_sys::{ReferrerPolicy, RequestCache, RequestCredentials, RequestMode};

/// The `fetch` members this transport will set, where a caller asked for
/// something other than the browser's default.
///
/// Every field is `None` by default — *leave it to `fetch`* — which is
/// `TcpOpts`' and `H2Opts`' rule one crate over:
/// a value set here changes what the browser does, so a default of ours
/// would change behaviour for a caller who asked for nothing. That matters
/// more here than anywhere else in this workspace, because these members
/// govern **whose credentials go where**.
///
/// Not `#[non_exhaustive]`, for `TcpOpts`' reason: the whole use is
/// `FetchOpts { credentials: Some(..), ..Default::default() }`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FetchOpts {
    /// `mode` — the CORS mode. `no-cors` is the one a caller reaches for,
    /// and it is worth knowing what it buys and costs: the request may go
    /// cross-origin without a preflight, and the response is **opaque** —
    /// status `0`, no headers, an unreadable body. A caller who wants to
    /// read the answer wants `cors`.
    pub mode: Option<RequestMode>,
    /// `credentials` — whether cookies and HTTP auth travel.
    ///
    /// The browser's default is `same-origin`. `include` is what a
    /// cross-origin authenticated request needs, and it is the reason this
    /// whole module is a setter on the transport rather than a request
    /// extension: it decides which origins receive the user's cookies.
    pub credentials: Option<RequestCredentials>,
    /// `cache` — the browser's own HTTP cache mode.
    ///
    /// Note which cache this is. `Capabilities::owns_cache` is `true` for
    /// this backend precisely because the browser caches inside `fetch()`,
    /// and a client-side `HttpCache` is
    /// refused at `build()` for that reason. This field is how a caller
    /// reaches the cache that *is* in force.
    pub cache: Option<RequestCache>,
    /// `referrerPolicy` — what `Referer` the browser attaches.
    ///
    /// Not the header, which is on `forbidden_request_headers` because the
    /// browser owns it; this is the policy the browser applies when it
    /// writes one.
    pub referrer_policy: Option<ReferrerPolicy>,
}

/// Applies whatever the caller asked for, and nothing else.
///
/// Field by field rather than through a loop, because each `Some` reaches
/// a differently typed setter and there is nothing to share but the shape
/// — `http_ng_native::http2::handshake`'s answer to the same question.
pub(crate) fn apply(init: &web_sys::RequestInit, opts: &FetchOpts) {
    if let Some(v) = opts.mode {
        init.set_mode(v);
    }
    if let Some(v) = opts.credentials {
        init.set_credentials(v);
    }
    if let Some(v) = opts.cache {
        init.set_cache(v);
    }
    if let Some(v) = opts.referrer_policy {
        init.set_referrer_policy(v);
    }
}
