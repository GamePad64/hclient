//! Runtime capability probing for the browser `fetch` backend.
//!
//! This is the one place in the project designed to let `Capabilities`
//! differ between processes running the exact same binary: Chrome sends a
//! `ReadableStream` request body and Firefox does not, and it's
//! `cfg!`-invisible because it's the same `wasm32-unknown-unknown` build in
//! every one of them. As of v0.2 W6 that difference is what
//! `Capabilities::streaming_request_body` actually reports —
//! [`supports_streaming_request_body`] decides it, `Fetch::caps` stores the
//! one answer, and BOTH readers (`Transport::capabilities()` and
//! `convert::to_web_request`, which now builds the stream rather than
//! refusing it) read that same stored value. One fact, one source: there is
//! no second place where "can this backend stream a request body" is
//! written down, so the declaration cannot drift from the behaviour.
//!
//! Everything else in this module is a fixed fact about the Fetch API
//! itself (no TLS knobs, browser-owned cookies, …) and could in principle
//! be a `const`, but lives next to the probe so `Capabilities` is assembled
//! in one place, not split between "facts" and "probes" for no reason a
//! caller can see.
//!
//! # Why the deciding probe is behavioural rather than a feature check
//!
//! [`supports_duplex`] — `'duplex' in Request.prototype` — is the cheap
//! check the ecosystem usually reaches for, and it is **not** the one that
//! decides anything here. It stays in this file as an observation, pinned
//! against the deciding probe by `tests/caps.rs`, because the day the two
//! disagree is the day the cheap one starts lying and this comment needs
//! rewriting.
//!
//! The reason the cheap check is not trusted is measured, not theoretical.
//! `docs/measurements/w6-request-streams/` drove Chrome 151 and Firefox 153
//! against three servers that recorded the bytes they actually received.
//! **Firefox does not refuse a `ReadableStream` request body — it replaces
//! it.** The `Request` constructor succeeds, `fetch` resolves, the server
//! answers `200`, and what arrives is the 23-byte ASCII string
//! `[object ReadableStream]` (`5b6f626a656374205265616461626c6553747265616d5d`)
//! with `Content-Type: text/plain;charset=UTF-8` — `USVString` conversion
//! applied to an object the implementation does not recognise as a body.
//! The caller's stream is never pulled; its data is discarded. Identical on
//! HTTP/1.1 and HTTP/2, because it happens during `Request` construction,
//! before a protocol is chosen.
//!
//! A silently corrupted body is worse than a refusal, and there is no error
//! to map, so "try it and turn the failure into a typed error" is not
//! available: this backend has to decide **before** it hands anything to
//! `fetch`. [`supports_streaming_request_body`] is that decision, and it is
//! whatwg/fetch#1470's own detection — see its doc comment.

use hclient_core::{
    CancelSupport, Capabilities, RedirectSupport, ReuseSupport, TimeoutSupport, TlsSupport,
};
use wasm_bindgen::{JsCast, JsValue};

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
/// **This is a presence check, not a behavioural one, and nothing in this
/// crate decides anything by it.** [`supports_streaming_request_body`] is
/// what `probe()` reads; this function survives as the cheap signal it
/// always was, kept so `tests/caps.rs` can pin the two against each other.
///
/// whatwg/fetch#1470 ("Feature detecting streaming requests") records Jake
/// Archibald raising exactly the scenario a presence check cannot catch: an
/// implementation could expose the `duplex` getter and still reject or
/// silently replace a streamed body when a `Request` is actually built. The
/// Fetch spec text prescribes no detection algorithm — it defines the IDL
/// attribute, full stop; #1470 is where the ecosystem worked out the
/// stronger check that [`supports_streaming_request_body`] implements.
///
/// Measured 2026-08-09 (`docs/measurements/w6-request-streams/`), the two
/// agree in both browsers this project's CI runs: Chrome 151 —
/// `prototypeHasDuplex: true`, behavioural `supported: true`; Firefox 153 —
/// `false` and `false`. So the presence check is not currently wrong; it is
/// merely not the one that would notice if it became wrong.
///
/// On any failure to even find `Request` or its prototype — which can't
/// happen in a real browser `fetch()` environment, since nothing in this
/// crate works at all without `Request` existing — the answer is `false`,
/// the same conservative floor `Capabilities::none()` uses everywhere else.
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

/// Whether this browser will actually **send** a `ReadableStream` handed to
/// it as a request body — as opposed to accepting it, reporting success,
/// and putting entirely different bytes on the wire.
///
/// This is the single fact `Capabilities::streaming_request_body` reports
/// and the single fact `convert::to_web_request` acts on. It is
/// whatwg/fetch#1470's behavioural detection, and it asks two questions of
/// one throwaway `Request`:
///
/// 1. **Was `duplex` read?** The `duplex` member is supplied as an
///    accessor rather than a value, so the getter running is direct
///    evidence that the `Request` constructor took the streaming-body path
///    at all (that path is where the Fetch Standard requires `duplex` to be
///    `"half"`).
/// 2. **Did the browser invent a `Content-Type`?** A browser that does not
///    recognise a `ReadableStream` as a body type falls back to `USVString`
///    conversion — it stringifies the object to `[object ReadableStream]`
///    and, because that is now a string body, stamps
///    `Content-Type: text/plain;charset=UTF-8` on the request. So an
///    invented `Content-Type` is the fingerprint of the corruption itself,
///    read one level below the network and before a single byte is sent.
///
/// Both must hold, and the construction must not throw. Measured
/// 2026-08-09 (`docs/measurements/w6-request-streams/results/`): Chrome 151
/// — `duplexAccessed: true`, `hasContentType: false`; Firefox 153 —
/// `duplexAccessed: false`, `hasContentType: true`, no throw anywhere. The
/// same reports confirm what each browser then does on the wire, which is
/// why this two-question form is the one implemented rather than the
/// cheaper [`supports_duplex`].
///
/// **Nothing observable happens.** No request is sent, no page-observable
/// effect, no entry in DevTools' network panel: a `Request` is a value, and
/// only `fetch()` sends one. The stream handed in is a bare, empty
/// `new ReadableStream()` that is never enqueued to, never locked and never
/// read — in a browser that supports streams it is merely attached to a
/// `Request` that is then dropped, and in one that does not it is
/// stringified.
///
/// Called once, from `Fetch::new()`. The cost is one `Request`
/// construction per process, which is what buys the accuracy the presence
/// check cannot give; `probe()`'s own doc comment used to argue that cost
/// was not worth paying, written when nothing in this crate could act on
/// the answer either way.
///
/// Every failure answers `false` — the conservative direction. A wrong
/// `true` here is what would let a corrupt body onto the wire; a wrong
/// `false` costs a caller a typed `Unsupported` error it can see and
/// handle.
pub(crate) fn supports_streaming_request_body() -> bool {
    use std::cell::Cell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;

    let Ok(stream) = web_sys::ReadableStream::new() else {
        return false;
    };

    // Built as a plain `Object` rather than a `web_sys::RequestInit`
    // because `duplex` has to be an ACCESSOR, and because web-sys 0.3.103
    // has no `duplex` binding on `RequestInit` at all (checked in that
    // version's `gen_RequestInit.rs`: 25 setters, none of them `duplex`) —
    // `convert::to_web_request` sets it through `Reflect` for the same
    // reason. `RequestInit` is declared `extends = js_sys::Object`, so the
    // cast below is a type-level rename of something the browser was always
    // going to read as an ordinary dictionary.
    let init = js_sys::Object::new();
    if js_sys::Reflect::set(
        &init,
        &JsValue::from_str("method"),
        &JsValue::from_str("POST"),
    )
    .is_err()
    {
        return false;
    }
    if js_sys::Reflect::set(&init, &JsValue::from_str("body"), stream.as_ref()).is_err() {
        return false;
    }

    let accessed = Rc::new(Cell::new(false));
    let flag = Rc::clone(&accessed);
    let getter = Closure::wrap(Box::new(move || {
        flag.set(true);
        JsValue::from_str("half")
    }) as Box<dyn FnMut() -> JsValue>);
    let descriptor = js_sys::Object::new();
    if js_sys::Reflect::set(
        &descriptor,
        &JsValue::from_str("get"),
        getter.as_ref().unchecked_ref(),
    )
    .is_err()
    {
        return false;
    }
    js_sys::Object::define_property(&init, &JsValue::from_str("duplex"), &descriptor);

    // The same URL the measurement harness used, and it is never fetched.
    let Ok(request) = web_sys::Request::new_with_str_and_init(
        "https://example.com/",
        init.unchecked_ref::<web_sys::RequestInit>(),
    ) else {
        // No measured browser throws here — Firefox stringifies instead —
        // but a browser that DID throw would be one that cannot send a
        // stream body, so `false` is the right answer rather than a
        // surprise.
        return false;
    };

    // `unwrap_or(true)`, not `unwrap_or(false)`: `has()` failing means we
    // could not find out whether a `Content-Type` was invented, and "we
    // could not find out" has to resolve to the answer that keeps a
    // possibly-corrupt body off the wire.
    let invented_content_type = request.headers().has("Content-Type").unwrap_or(true);

    // Read the flag while `getter` is still alive; dropping the `Closure`
    // invalidates the JS function it installed, and `descriptor`/`init` go
    // with it at the end of this scope. The browser copied what it needed
    // out of the dictionary during construction.
    accessed.get() && !invented_content_type
}

/// Builds this process's `Capabilities` by asking the running browser.
///
/// Called exactly once, from `Fetch::new()`, and the result is stored in
/// `Fetch::caps` — not recomputed per request. The one read that is not a
/// prototype inspection is [`supports_streaming_request_body`], which
/// constructs a single throwaway `Request`; that is still nothing a page,
/// DevTools' network panel or a server can see, because constructing a
/// `Request` sends nothing (see that function's own doc comment). The whole
/// crate runs on wasm32 without `target_feature = "atomics"` (the same
/// precondition `promise.rs`'s `SingleThreaded` documents), so there is
/// exactly one JS thread in the process — "what happens across threads" is
/// "nothing happens across threads", by construction, not by locking.
///
/// **`Capabilities` describes this transport, not this browser — and those
/// stopped being the same question the moment `Transport::capabilities()`
/// became a public method (Task 5).** Two fields below are the reason this
/// doc comment exists: see each one's own comment for the specific claim
/// that changed and why.
pub(crate) fn probe() -> Capabilities {
    let mut c = Capabilities::none();
    // `streaming_request_body` — derived, since v0.2 W6, from the one
    // function that decides it, in the shape `response_decompression`
    // (W5) and `connection_reuse` (W2) below already use: the value a
    // caller reads off `Capabilities` and the value `convert::to_web_request`
    // branches on are not two claims that happen to agree, they are one
    // stored answer with two readers.
    //
    // It is a genuine, browser-varying runtime fact and this is the field
    // `Capabilities`'s own doc comment was written for. It is honest about
    // THIS CRATE and not merely about the browser — the property the
    // earlier hardcoded `false` was protecting — because as of W6 the two
    // coincide: `convert::to_web_request` builds the `ReadableStream` and
    // sets `duplex: "half"` exactly when this is `true`, and returns a
    // typed `UnsupportedCapability` exactly when it is `false`.
    //
    // **What `true` does not promise, and cannot.** Measured on the same
    // day as the probe itself: Chrome sends a stream body only over
    // HTTP/2. Over an HTTP/1.1 origin — cleartext, and TLS with ALPN
    // restricted to `http/1.1`, so this is about the protocol and not
    // about the secure context — `fetch` fails in ~3 ms with a bare
    // `TypeError: Failed to fetch`, before the stream is pulled and with
    // nothing reaching the server. That is a fact about an ORIGIN, and
    // `Capabilities` has no per-origin dimension to hold it; nor can the
    // answer be predicted per request, because the browser exposes the
    // negotiated protocol nowhere a caller can read before committing
    // (`PerformanceResourceTiming.nextHopProtocol` arrives only after a
    // first response to that origin, and cross-origin only with
    // `Timing-Allow-Origin`).
    //
    // Reporting the floor — `false` everywhere, W3's rule for
    // `full_duplex` — is deliberately NOT what happens here, and the
    // difference is what the two mistakes cost. Over-claiming
    // `full_duplex` costs a caller a deadlock: it waits for a response
    // that cannot arrive. Over-claiming `streaming_request_body` on an
    // HTTP/1.1 origin costs a caller a loud, immediate, typed error with
    // no bytes sent — `convert::StreamingBodyNeedsHttp2` names the cause,
    // which the browser's own `TypeError` does not. Under-claiming it
    // costs every HTTP/2 origin in Chrome the feature entirely. The
    // ordering of those costs is the whole argument, and it is the reason
    // the floor rule is cited rather than followed.
    c.streaming_request_body = supports_streaming_request_body();
    // `full_duplex` stays `false`, and NOT merely because it is unprobed.
    // `duplex: "half"` is half duplex by name and by measurement: in the
    // Chrome/h2 run the three body chunks were written at t = 294/594/894
    // ms and the `fetch` promise resolved at 1206 ms — the response was
    // not readable until the request body had finished. Streaming a
    // request is not duplex, and W3's floor rule applies to this field
    // unchanged.
    c.full_duplex = false;
    // `redirects`: `Internal`, not `Configurable`. `to_web_request`
    // (`convert.rs`) never calls `RequestInit::set_redirect` — fetch's
    // default, `redirect: "follow"`, governs every request this crate
    // sends, unconditionally. That default is resolved INSIDE the browser:
    // the JS code (and everything built on top of it, including `Client`'s
    // own redirect stage) only ever sees the final response, never an
    // intermediate 3xx. `RedirectSupport::Configurable` claimed "we set
    // the policy" — a specific, false claim: nothing here reads a
    // `RedirectPolicy` or translates it into any fetch option. That
    // variant was deleted in v0.4 W1, once it turned out to have no
    // truthful carrier in the workspace at all. This is
    // exactly the case `RedirectSupport::Internal`'s own doc comment in
    // `hclient-core` names by example — "a browser's `fetch()` with
    // `redirect: "follow"` (the default) follows the redirect inside the
    // browser, and the JS code sees only the final response" — written
    // before this crate existed, describing this crate.
    c.redirects = RedirectSupport::Internal;
    // The browser owns the connection, and `AbortController` is how it is
    // asked to drop it. `crate::AbortOnDrop` arms that controller across
    // the one `.await` in `execute` that can be interrupted, so a dropped
    // future aborts the signal the browser is actually fetching with — and
    // the Fetch Standard's "abort a fetch" algorithm terminates the ongoing
    // fetch from there.
    //
    // Not taken on the spec's word: `tests/transport.rs`'s
    // `dropping_the_execute_future_makes_the_browser_report_an_aborted_fetch`
    // wraps `globalThis.fetch` (where `execute` looks it up), lets a real
    // request go out to a real URL, drops the future, and then reads the
    // browser's own verdict on the promise it kept — a `DOMException` named
    // `AbortError`, which is a value nothing on this side can produce.
    c.cancel_on_drop = CancelSupport::Supported;
    // The browser keeps its own connections alive across `fetch()` calls,
    // per origin, and has done since HTTP/1.1 — a caller batching work
    // against one origin is not paying for a handshake per request here.
    // `ReuseSupport::None` would be a lie in the other direction, and the
    // rule this project applies is that a default must never be stronger
    // than the truth, not that it must always be weaker.
    //
    // **The one declaration in this whole set with no external observer,
    // and it is named as such rather than left to look like the others.**
    // Every other capability here is either measured from outside (see
    // `tests/transport.rs`) or is a fact about code in this crate. This one
    // is neither: from inside the sandbox there is no way to see whether
    // two `fetch()` calls shared a socket — no API exposes it, a server
    // counting accepted connections cannot be reached from a headless
    // browser test the way it can from a native one, and nothing in CI
    // checks it. It rests on the Fetch Standard and on how every engine
    // implements it, which is good evidence and is not a measurement.
    c.connection_reuse = ReuseSupport::Supported;
    // NOT derived from `FORBIDDEN_HEADERS` containing `Accept-Encoding`
    // three fields down, even though both are true of this backend: "the
    // header cannot be sent" and "the body reaching you is already decoded"
    // are different claims that coincide here by accident (see
    // `DecompressionSupport`'s own doc comment, which says so at the seam).
    // It is read from `body::RESPONSE_DECOMPRESSION`, the same constant
    // `body::content_length_hint` consults to decide it may not trust
    // `Content-Length` — one fact about the browser, read twice, in the
    // shape `hclient-native`'s `reuse_of` established for `ReuseSupport`.
    c.response_decompression = crate::body::RESPONSE_DECOMPRESSION;
    c.owns_cookie_jar = true;
    c.owns_cache = true;
    // `AbortSignal` is one deadline for the whole exchange; none of the
    // three phase timeouts (`connect`/`first_byte`/`between_bytes`) can be
    // expressed through it individually. Declaring any of the three would
    // be a capability that lies.
    c.timeouts = TimeoutSupport {
        resolve: false,
        connect: false,
        first_byte: false,
        between_bytes: false,
    };
    c.tls_config = TlsSupport::None;
    // There is no `upgrade` field to set any more, and the sentence that
    // used to be here is why: *"`WebSocket` in the browser is a wholly
    // separate global, unreachable from a `fetch`-shaped `Transport`"*. It
    // is still true, and `src/websocket.rs` is the conclusion drawn from
    // it — this crate reaches that global through
    // `hclient_core::unversioned::WebSocketConnect`, which a transport
    // says it can do by implementing it rather than by declaring anything
    // here.
    c.forbidden_request_headers = &FORBIDDEN_HEADERS;
    c
}
