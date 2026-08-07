//! Runtime capability probing for the browser `fetch` backend.
//!
//! This is the one place in the whole project where `Capabilities` actually
//! differs between processes running the exact same binary: Chrome ships
//! request-body streaming (`duplex: "half"`) and Firefox/Safari don't, and
//! it's `cfg!`-invisible because it's the same `wasm32-unknown-unknown`
//! build in every one of them. Everything else in this module is a fixed
//! fact about the Fetch API itself (no TLS knobs, browser-owned cookies,
//! …) and could in principle be a `const`, but lives next to the one
//! genuine probe so `Capabilities` is assembled in one place, not split
//! between "facts" and "probes" for no reason a caller can see.

use http_ng_core::{Capabilities, RedirectSupport, TimeoutSupport, TlsSupport, UpgradeSupport};

/// Headers that fetch forbids scripts from setting. We **declare** them
/// rather than silently dropping them on the floor: a caller who tries to
/// set `Cookie` or a `Proxy-*` header and has it vanish without a typed
/// error is a source of security bugs (a credential the caller believed
/// was attached, wasn't).
///
/// This list is a deliberately chosen, verified-accurate **subset** of the
/// Fetch Standard's "forbidden request-header name" algorithm
/// (<https://fetch.spec.whatwg.org/#forbidden-request-header>), not the
/// full set — the full algorithm also forbids `Accept-Charset`,
/// `Access-Control-Request-Headers`, `Access-Control-Request-Method`,
/// `Cookie2`, `DNT`, `Expect`, `Keep-Alive`, `Set-Cookie`, `Set-Cookie2`,
/// `Trailer`, and — structurally out of reach for a fixed array — every
/// header whose lowercased name starts with `proxy-` or `sec-`. A `[T; N]`
/// can list names, it cannot express a prefix rule; a caller that needs
/// the exact, complete, current predicate has to ask the browser itself
/// (`fetch()`'s own `TypeError` on a forbidden header) rather than trust
/// this list as exhaustive. Every name actually listed here **is**
/// currently forbidden (checked against the live spec text, not memory,
/// while writing this).
// NOTE (amendment-C4, reverified for this exact shape): `static FORBIDDEN:
// &[HeaderName] = &[..]` fails with E0492 for an inline array literal,
// because `HeaderName`'s `Custom` variant carries `Bytes`, which carries an
// `AtomicPtr`, and rustc's static-promotion check rejects by type
// regardless of which variant is actually live. A named `const` array,
// referenced afterward as `&FORBIDDEN_HEADERS`, does not hit that check —
// verified by compiling this exact form, not inherited from the note.
pub const FORBIDDEN_HEADERS: [http::HeaderName; 14] = [
    http::header::HOST,
    http::header::CONNECTION,
    http::header::CONTENT_LENGTH,
    http::header::COOKIE,
    http::header::ORIGIN,
    http::header::TRANSFER_ENCODING,
    http::header::TE,
    http::header::UPGRADE,
    http::header::REFERER,
    http::header::DATE,
    http::header::VIA,
    http::header::PROXY_AUTHENTICATE,
    http::header::PROXY_AUTHORIZATION,
    http::header::ACCEPT_ENCODING,
];

/// Whether `Request.prototype.duplex` exists as a readable property.
///
/// The Fetch Standard's Request interface declares `duplex` as a getter
/// (`dom-request-duplex`); browsers that implement it install that getter
/// on `Request.prototype`, so checking for the property's existence there
/// — without invoking it, and without constructing a `Request` — is
/// side-effect-free and needs nothing beyond the constructor itself.
///
/// **This is a presence check, not a behavioral one, and that distinction
/// is real, not theoretical.** whatwg/fetch#1470 ("Feature detecting
/// streaming requests") records Jake Archibald raising exactly the
/// scenario this task was asked to rule out: an implementation could in
/// principle expose the `duplex` getter and still reject or silently
/// buffer a streamed body when `fetch()` is actually called, which a pure
/// presence check cannot catch. The Fetch spec text itself does not
/// prescribe a detection algorithm — it defines the IDL attribute, full
/// stop; #1470 is where the ecosystem worked out a stronger, behavioral
/// check (construct a `Request` with a `ReadableStream` body and a
/// `duplex` accessor-getter, then confirm the getter was actually read
/// *and* the stream wasn't eagerly drained into a `Content-Type:
/// text/plain` guess). That check needs a throwaway `Request` per call,
/// which conflicts with this probe being called once, cheaply, with no
/// object construction (see `probe()`'s doc comment). Checked against
/// current Browser Compat Data for `api.Request.duplex` while writing
/// this (Chrome/Edge/WebView 131+: yes; Firefox, Safari, Safari iOS: no) —
/// today, no shipping browser is known to expose the getter without also
/// honoring `duplex: "half"`, so presence tracks reality exactly. If that
/// ever stops being true, this function starts lying, silently, in the
/// optimistic direction; there's no way to close that gap without paying
/// for a real request construction on every probe.
///
/// On any failure to even find `Request` or its prototype — this function
/// can't happen in a real browser `fetch()` environment, since nothing in
/// this crate works at all without `Request` existing — the answer is
/// `false`, the same conservative floor `Capabilities::none()` uses
/// everywhere else. It is never `true` on an inconclusive read: a caller
/// that branches on `true` may skip buffering the whole body up front, so
/// a wrong `true` here is the dangerous direction, not the safe one.
pub(crate) fn supports_duplex() -> bool {
    let Ok(ctor) = js_sys::Reflect::get(
        &js_sys::global(),
        &wasm_bindgen::JsValue::from_str("Request"),
    ) else {
        return false;
    };
    let Ok(proto) = js_sys::Reflect::get(&ctor, &wasm_bindgen::JsValue::from_str("prototype"))
    else {
        return false;
    };
    js_sys::Reflect::has(&proto, &wasm_bindgen::JsValue::from_str("duplex")).unwrap_or(false)
}

/// Builds this process's `Capabilities` by asking the running browser.
///
/// Called exactly once, from `Fetch::new()`, and the result is stored in
/// `Fetch::caps` — not recomputed per request. Every read here
/// (`js_sys::Reflect::get`/`has`) only inspects `Request`'s prototype; none
/// of them construct a `Request`, so there is nothing here observable to
/// the page, DevTools' network panel, or a server. The whole crate runs on
/// wasm32 without `target_feature = "atomics"` (the same precondition
/// `promise.rs`'s `SingleThreaded` documents), so there is exactly one JS
/// thread in the process — "what happens across threads" is "nothing
/// happens across threads", by construction, not by locking.
pub(crate) fn probe() -> Capabilities {
    let duplex = supports_duplex();
    let mut c = Capabilities::none();
    // Streaming request bodies in fetch only exist together with
    // `duplex: "half"` — the same browser feature under two names.
    c.streaming_request_body = duplex;
    c.full_duplex = duplex;
    // The browser owns redirects: a policy can be set
    // (`redirect: "follow"/"error"/"manual"`), but `"manual"` gives back an
    // opaque redirect (status 0, no headers) rather than letting us
    // observe the hop, so this is `Configurable`, not `Inspectable`.
    c.redirects = RedirectSupport::Configurable;
    c.owns_cookie_jar = true;
    c.owns_cache = true;
    // `AbortSignal` is one deadline for the whole exchange; none of the
    // three phase timeouts (`connect`/`first_byte`/`between_bytes`) can be
    // expressed through it individually. Declaring any of the three would
    // be a capability that lies.
    c.timeouts = TimeoutSupport {
        connect: false,
        first_byte: false,
        between_bytes: false,
    };
    c.tls_config = TlsSupport::None;
    // `WebSocket` in the browser is a wholly separate global, unreachable
    // from a `fetch`-shaped `Transport`.
    c.upgrade = UpgradeSupport::None;
    c.forbidden_request_headers = &FORBIDDEN_HEADERS;
    c
}
