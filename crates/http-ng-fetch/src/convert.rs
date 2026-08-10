//! Converts an `http::Request<RequestBody>` into a `web_sys::Request`, ready
//! to hand to the browser's `fetch()`.
//!
//! Three things this file is deliberately strict about, each traceable to a
//! defect an earlier task in this project actually shipped (see this task's
//! own brief for the pointers):
//!
//! 1. **A header fetch will refuse to send never disappears silently.**
//!    [`check_headers`] rejects any header in [`crate::FORBIDDEN_HEADERS`]
//!    (Task 2's verified-accurate, deliberately incomplete, subset of the
//!    Fetch Standard's forbidden-request-header predicate) before a single
//!    `web_sys` call is made. That list can't express fetch's `Sec-*`/
//!    `Proxy-*` prefix rule or the ten other named headers it doesn't carry
//!    (see `FORBIDDEN_HEADERS`'s own doc comment) — so
//!    [`verify_headers_survived`] closes the gap from the other side, after
//!    construction: it reads back the built `Request`'s own `Headers` and
//!    fails loudly if the NAME of anything we tried to set isn't there,
//!    rather than letting a caller believe an unnamed forbidden header
//!    (`Sec-Foo`, say) went out when the browser quietly dropped it. It
//!    checks presence, not byte-exact value fidelity — see
//!    [`verify_headers_survived`]'s own doc comment for the one documented
//!    gap that leaves open (harmless whitespace normalization, not a drop).
//!
//! 2. **No `RequestBody` variant silently becomes an empty body.**
//!    [`resolve_body`] handles `Empty`, `Full`, `Rewindable`, and
//!    `Streaming` on purpose — `Rewindable` by recursing into whatever its
//!    factory hands back, through the SAME match, not a partial one that
//!    only understands `Full` and drops everything else (that was the exact
//!    review-caught defect in vertical 2's native `body.rs`: a `Rewindable`
//!    wrapping anything but a non-empty `Full` collapsed to nothing sent).
//!    Since v0.2 W6 `Streaming` is forwarded rather than refused, where the
//!    browser will genuinely send it; where it will not, it is still a
//!    typed [`ErrorKind::Unsupported`], never a silent empty body and never
//!    a silently REPLACED one.
//!
//! 3. **The capability this file acts on is the one the browser was
//!    measured on.** Until v0.2 W6 `RequestBody::Streaming` was rejected
//!    unconditionally here, deliberately ignoring
//!    `caps.streaming_request_body`, because that field was fed by
//!    `caps::supports_duplex` — `'duplex' in Request.prototype`, a presence
//!    check whatwg/fetch#1470 records as insufficient. W6 replaced the
//!    deciding probe with #1470's behavioural one
//!    (`caps::supports_streaming_request_body`), measured against real
//!    servers in `docs/measurements/w6-request-streams/`, so this file now
//!    branches on `caps.streaming_request_body` instead of second-guessing
//!    it. That is not "trusting a capability blindly": it is the same
//!    stored answer, probed once in `Fetch::new()`, that
//!    `Transport::capabilities()` hands the caller — one fact with two
//!    readers rather than two claims that have to be kept in agreement.
//!
//!    The failure this guards against is not a rejection. Firefox 153
//!    accepts a `ReadableStream` body, answers `200`, and sends the 23-byte
//!    string `[object ReadableStream]` instead of the caller's bytes. There
//!    is no error to map and nothing inside the page notices, which is why
//!    the decision has to be made here, before `fetch` is called, and why
//!    "attempt it and report what went wrong" is not an option.
//!
//! 4. **What is lost once the stream is handed over, said plainly.** After
//!    `to_web_request` returns, the caller's `http_body::Body` is being
//!    polled by the browser through a `ReadableStream`, and the only
//!    channel back is `controller.error(value)`. A body that fails
//!    mid-stream, or that emits the request trailers this backend declares
//!    it cannot send (`Capabilities::request_trailers == false`), errors
//!    that stream with a message naming the cause — but the browser
//!    collapses ANY request-body stream failure into `fetch`'s own opaque
//!    `TypeError`, so what reaches the caller is
//!    [`crate::Fetch`]'s `ErrorKind::Connect`, not the typed cause. That is
//!    a real limit of the Fetch API rather than a shortcut taken here; the
//!    message survives where a human can read it (DevTools), and
//!    `tests/convert.rs` drains the built `Request`'s own stream to pin the
//!    policy at the layer that still has it.
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, UnsupportedCapability};
use wasm_bindgen::{JsCast, JsValue};

// ---------------------------------------------------------------------
// Typed failure causes. Each is a distinct type, not a formatted string,
// so `Error::source().downcast_ref` can recover the exact cause later —
// the same reasoning `http_ng_core::Error`'s own doc comment gives for why
// it wraps a `dyn Error` rather than stringifying up front.
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[error("fetch forbids setting the `{0}` header")]
pub(crate) struct ForbiddenHeader(pub(crate) String);

/// A header survived [`check_headers`] (it isn't one of the 14 names
/// [`crate::FORBIDDEN_HEADERS`] lists) but the browser dropped it anyway
/// when building the `Request` — almost certainly a `Sec-*`/`Proxy-*`
/// prefixed name, or one of the ten other forbidden names that fixed array
/// structurally can't carry (see its doc comment). Caught by
/// [`verify_headers_survived`], never allowed to pass as a quiet success.
#[derive(Debug, thiserror::Error)]
#[error(
    "the `{0}` header was accepted but the browser silently dropped it while building the \
     request (not one of the names FORBIDDEN_HEADERS checks ahead of time — most likely a \
     `Sec-`/`Proxy-`-prefixed name, which a fixed array cannot express)"
)]
pub(crate) struct HeaderSilentlyDropped(pub(crate) String);

/// A header value with bytes that aren't valid ASCII text. `http::HeaderMap`
/// allows opaque byte strings (`HeaderValue::from_bytes`); the Fetch API's
/// `Headers.append` only takes a JS string, so this is a real, nameable
/// limit of the backend, not an opaque `TypeError`.
#[derive(Debug, thiserror::Error)]
#[error(
    "the value of header `{0}` is not valid ASCII text, which the Fetch API's `Headers` \
     interface requires"
)]
pub(crate) struct NonAsciiHeaderValue(pub(crate) String);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct BadUrl(pub(crate) String);

/// Fetch throws a `TypeError` if a `GET`/`HEAD` request carries a body.
/// Checked ahead of time so the caller gets a typed, specific error instead
/// of `ErrorKind::Other` wrapping a browser-thrown `TypeError` string.
#[derive(Debug, thiserror::Error)]
#[error("a `{0}` request cannot carry a body — fetch forbids a body on GET and HEAD requests")]
pub(crate) struct BodyNotAllowedForMethod(pub(crate) http::Method);

/// Upper bound on `RequestBody::Rewindable` nesting (a factory whose result
/// is itself another `Rewindable`). No legitimate factory needs this — one
/// factory calling through to a second, to a third, buys the caller
/// nothing — so past this depth [`resolve_body`] stops with a typed error
/// instead of silently giving up (an empty body) or recursing forever.
///
/// **The practical ceiling is `MAX_REWIND_DEPTH - 1` (15), not
/// `MAX_REWIND_DEPTH` itself** — the constant names how many times
/// `resolve_body`'s loop inspects a body, not how many `Rewindable` layers
/// successfully unwrap; the last of those inspections is spent discovering
/// the budget is exhausted rather than unwrapping another layer. Confirmed
/// directly: a body nested 15 `Rewindable` layers deep resolves; nested 16
/// or deeper, it hits [`RewindTooDeep`] instead. Same constant, same
/// off-by-one, same reasoning as `http-ng-wasi/src/convert.rs`'s
/// `MAX_REWIND_DEPTH` — not a new discrepancy introduced here.
const MAX_REWIND_DEPTH: u8 = 16;

#[derive(Debug, thiserror::Error)]
#[error(
    "RequestBody::Rewindable factory nested {MAX_REWIND_DEPTH} levels deep or more \
     (each factory call returned another Rewindable instead of a terminal body)"
)]
pub(crate) struct RewindTooDeep;

#[derive(Debug, thiserror::Error)]
#[error("javascript error: {0}")]
pub(crate) struct JsError(pub(crate) String);

/// A `fetch` carrying a `ReadableStream` request body failed, and the
/// browser said only `TypeError: Failed to fetch`.
///
/// Wrapped around the underlying [`JsError`] rather than replacing it: the
/// browser's own text stays readable, and this adds the one cause the
/// browser structurally cannot report. Measured 2026-08-09 (Chrome 151,
/// `docs/measurements/w6-request-streams/`): a stream body over an HTTP/1.1
/// origin fails in ~3 ms with exactly that `TypeError`, before the stream is
/// pulled and with nothing reaching the server — and the same value is what
/// the Fetch Standard produces for a refused connection, with no `name`,
/// `message` or property separating the two. A caller who reads this cannot
/// tell which happened either; what changes is that the possibility is
/// named at all.
///
/// Only attached where it is relevant — a request whose body actually was a
/// stream (`Converted::streamed`). A failed `GET` does not get this text.
#[derive(Debug, thiserror::Error)]
#[error(
    "{0} — this request carried a streaming body, which a browser will only send over HTTP/2; \
     measured in Chrome, an HTTP/1.1 origin fails in milliseconds with exactly this error and \
     nothing on the wire, and the browser reports a genuine network failure identically"
)]
pub(crate) struct StreamingBodyFetchFailed(#[source] pub(crate) Error);

/// The caller's streaming request body produced a trailers frame.
///
/// `Capabilities::request_trailers` is `false` for this backend — fetch has
/// no request trailers in any form (whatwg/fetch#772 proposes removing the
/// response-side API too) — so there is nowhere to put them. Dropping them
/// and sending the rest is the silent no-op this project forbids; the
/// stream is errored instead, which fails the request.
///
/// Unlike every other cause in this file, this one cannot be raised before
/// `to_web_request` returns: trailer frames arrive after the last data
/// frame, by which point the browser owns the stream. The same shape, and
/// the same reasoning, as the trailers guard `http-ng-wasi` moved into its
/// `Body` (see that crate's notes on duplex request bodies).
#[derive(Debug, thiserror::Error)]
#[error(
    "the streaming request body produced trailers, which fetch cannot send (this backend \
     declares `request_trailers = false`)"
)]
pub(crate) struct RequestTrailersUnsupported;

/// Turns a rejected JS promise/thrown value into our `Error`. This is the
/// generic, honest fallback for a JS failure this file cannot classify any
/// further — every failure mode this file CAN name ahead of time
/// (forbidden header, bad URL, unsupported body) is caught earlier and
/// never reaches this function with a category to lose.
pub(crate) fn js_err(v: JsValue) -> Error {
    Error::new(ErrorKind::Other, JsError(js_message(&v)))
}

/// The three-step fallback (`as_string` → `.message` → `Debug`) on its own,
/// for the callers that have already named their own category and want the
/// text rather than [`js_err`]'s `ErrorKind::Other`.
///
/// Extracted out of [`js_err`] by `websocket.rs`, which needs the message
/// under `ErrorKind::Connect` and `ErrorKind::Body`. `body.rs` still keeps
/// a local copy for the reason its own module doc gives (it was written
/// beside a mid-edit `convert.rs`); that copy is not touched here, since
/// merging it is a change to a file this work has no other business in.
pub(crate) fn js_message(v: &JsValue) -> String {
    v.as_string()
        .or_else(|| {
            js_sys::Reflect::get(v, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{v:?}"))
}

/// Checks headers **before** touching a single `web_sys` type. Silently
/// dropping a forbidden header is a source of security bugs: the caller
/// thinks `Cookie` went out, but it never did. This is the fast, exact half
/// of the check; [`verify_headers_survived`] is the slow, honest-about-its-
/// own-incompleteness other half — see the module doc comment.
pub(crate) fn check_headers(h: &http::HeaderMap, caps: &Capabilities) -> Result<(), Error> {
    for name in h.keys() {
        if caps.forbidden_request_headers.contains(name) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                ForbiddenHeader(name.as_str().to_owned()),
            ));
        }
    }
    Ok(())
}

/// After the `Request` exists, reads its own `Headers` back and confirms
/// the NAME of every header we tried to set is still there. Closes the gap
/// `FORBIDDEN_HEADERS` structurally can't: a `Sec-*`/`Proxy-*`-prefixed
/// header, or one of the ten other forbidden names outside that fixed
/// array, sails past `check_headers` untouched and is then silently
/// dropped by the browser's own `Headers.append` (guard `"request"`, per
/// the Fetch Standard: forbidden names are skipped, not rejected with an
/// exception) — with no JS error to catch. Without this step, a caller who
/// pre-filtered against `Capabilities::forbidden_request_headers` and
/// still set, say, `Sec-Foo` would have it vanish with the transport
/// reporting success — exactly the silent no-op this project forbids.
///
/// **What this does NOT check: value fidelity.** Only presence of the
/// NAME is compared, never the value. Confirmed directly (not assumed):
/// `web_sys::Headers::append` trims leading/trailing HTTP whitespace from
/// a value before storing it (the Fetch Standard's own "normalize a byte
/// sequence" step, RFC 7230 optional-whitespace stripping — not a Chrome
/// quirk) — see `header_value_whitespace_is_trimmed_and_not_flagged_as_a_
/// silent_drop` in `tests/convert.rs`. That is intentionally not treated
/// as a drop: the header survived, merely normalized the way any HTTP
/// implementation is allowed to. A caller relying on this function should
/// read it as "the name made it through", not "the exact bytes made it
/// through unchanged".
fn verify_headers_survived(
    sent: &http::HeaderMap,
    request: &web_sys::Request,
) -> Result<(), Error> {
    let seen = request.headers();
    for name in sent.keys() {
        if !seen.has(name.as_str()).map_err(js_err)? {
            return Err(Error::new(
                ErrorKind::Unsupported,
                HeaderSilentlyDropped(name.as_str().to_owned()),
            ));
        }
    }
    Ok(())
}

/// Builds the `web_sys::Headers` fetch will actually see, failing on the
/// first value that can't be represented as a JS string rather than
/// silently truncating or mangling it.
fn build_headers(h: &http::HeaderMap) -> Result<web_sys::Headers, Error> {
    let headers = web_sys::Headers::new().map_err(js_err)?;
    for (name, value) in h.iter() {
        let value = value.to_str().map_err(|_| {
            Error::new(
                ErrorKind::Unsupported,
                NonAsciiHeaderValue(name.as_str().to_owned()),
            )
        })?;
        headers.append(name.as_str(), value).map_err(js_err)?;
    }
    Ok(headers)
}

/// A `RequestBody`, resolved down to what fetch can actually be handed:
/// nothing, a byte buffer, or a single-pass body still to be wrapped in a
/// `ReadableStream` — see the module doc comment, points 2 and 3.
///
/// The `Streaming` arm carries the body rather than discarding it (which is
/// what it did before v0.2 W6, when the only thing left to do with it was
/// name it in an error). It is deliberately NOT converted to a
/// `ReadableStream` inside [`resolve_body`]: whether that conversion may
/// happen at all is `caps.streaming_request_body`'s decision, and
/// [`resolve_body`] has no `Capabilities`. Building the stream here would
/// also mean building it for a `GET`, only to throw it away one check
/// later.
enum ResolvedBody {
    None,
    Full(js_sys::Uint8Array),
    Streaming(Box<dyn http_body::Body<Data = bytes::Bytes, Error = Error> + Unpin + Send>), // send-bound-exception: amendment-C2
}

/// Adapts the `http_body::Body` inside a [`RequestBody::Streaming`] to the
/// `Stream<Item = Result<JsValue, JsValue>>` `wasm_streams::ReadableStream::
/// from_stream` consumes — the request-direction mirror of `body.rs`, which
/// runs the same bridge the other way for responses.
///
/// The two ends of the bridge are not symmetric, and the difference is who
/// pulls. `body.rs` is polled by this crate; this type is polled by the
/// browser, through the underlying source `wasm-streams` installs, at
/// whatever rate the connection drains. `from_stream` sets the queuing
/// strategy's high-water mark to 0, so nothing is buffered ahead: the
/// caller's body is polled when the browser asks for a chunk and not
/// before. That is what makes this a stream rather than a copy, and
/// `tests/convert.rs`'s `a_streaming_body_is_not_drained_when_the_request_is_built`
/// pins the "and not before" half — the half a `Content-Length` or a
/// green build would not notice.
struct BodyStream(Box<dyn http_body::Body<Data = bytes::Bytes, Error = Error> + Unpin + Send>); // send-bound-exception: amendment-C2

/// A JS `Error` carrying `text`, for `controller.error()`. Not
/// `JsValue::from_str`: a thrown string loses its message in some console
/// formatters and cannot carry a stack, and this value is the only thing a
/// human debugging a failed streamed upload will have to go on (see the
/// module doc comment, point 4).
fn js_error_value(text: &str) -> JsValue {
    js_sys::Error::new(text).into()
}

impl futures_core::Stream for BodyStream {
    type Item = Result<JsValue, JsValue>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use http_body::Body as _;
        use std::task::Poll;
        // `Box<dyn ..>` is `Unpin` whatever it holds, and the trait object
        // is additionally bounded `+ Unpin` by `RequestBody::Streaming`
        // itself, so this needs no projection machinery.
        let body = &mut self.get_mut().0;
        match std::pin::Pin::new(body).poll_frame(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            // The caller's own error. It cannot be handed across as a typed
            // Rust value — the far side of this bridge is JavaScript — so it
            // crosses as its `Display` text and is lost as a category; see
            // the module doc comment, point 4.
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(js_error_value(&e.to_string())))),
            Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                // Zero-length data frames are forwarded rather than skipped.
                // Skipping would need a loop, and a body that yields empty
                // frames forever would then spin inside a single `poll_next`
                // instead of merely making no progress; an empty chunk is a
                // legal `BufferSource` and costs the stream nothing.
                Ok(data) => Poll::Ready(Some(Ok(js_sys::Uint8Array::from(&data[..]).into()))),
                // `into_data` fails only for a trailers frame — see
                // [`RequestTrailersUnsupported`] for why this is an error
                // and not a silent drop.
                Err(_trailers) => Poll::Ready(Some(Err(js_error_value(
                    &RequestTrailersUnsupported.to_string(),
                )))),
            },
        }
    }
}

/// Resolves `RequestBody` to [`ResolvedBody`]. `Rewindable` is unwrapped by
/// calling its factory and feeding the result back through this SAME match
/// — not a partial match that only recognizes `Full` and silently treats
/// everything else (including a `Rewindable` wrapping `Streaming`, or
/// another `Rewindable`) as an empty body. That partial-match shape is the
/// exact defect this task's brief calls out from vertical 2's native
/// `body.rs`; the mutation check in `tests/convert.rs` reverts to it and
/// confirms it's caught.
///
/// Iterative, not recursive, and bounded by [`MAX_REWIND_DEPTH`] (see its
/// own doc comment for the exact, off-by-one-from-the-constant ceiling): a
/// factory that always returns another `Rewindable` would otherwise unwrap
/// forever.
fn resolve_body(body: RequestBody) -> Result<ResolvedBody, Error> {
    let mut body = body;
    for _ in 0..MAX_REWIND_DEPTH {
        match body {
            RequestBody::Empty => return Ok(ResolvedBody::None),
            RequestBody::Full(b) if b.is_empty() => return Ok(ResolvedBody::None),
            RequestBody::Full(b) => {
                return Ok(ResolvedBody::Full(js_sys::Uint8Array::from(&b[..])));
            }
            RequestBody::Rewindable(f) => body = f(),
            // Carried out whole — matched, not silently dropped; the caller
            // decides whether it can be sent (module doc comment, point 3).
            // A `Rewindable` wrapping a `Streaming` reaches this arm through
            // the loop above, exactly like any other terminal variant.
            RequestBody::Streaming(b) => return Ok(ResolvedBody::Streaming(b)),
        }
    }
    Err(Error::new(ErrorKind::Other, RewindTooDeep))
}

/// The URL string fetch will parse, or a typed error if `uri` isn't one
/// fetch can use at all — an absolute `http`/`https` URL. Checked here,
/// ahead of time, rather than left to `web_sys::Request::new_with_str_and_init`'s
/// own `TypeError` (which `js_err` can only wrap as `ErrorKind::Other`):
/// the same reasoning `http-ng-native`'s `wants_tls` and `http-ng-wasi`'s
/// `scheme_of` already apply to their own URI checks — a rejected scheme is
/// `ErrorKind::Unsupported`, not an opaque backend error.
///
/// Only checks the scheme, not the authority separately: `http::Uri`
/// itself refuses to represent a `Some("http")`/`Some("https")` scheme
/// without an authority — `Uri::builder().scheme("http").path_and_query(
/// "/x").build()` fails with "authority missing", and every string that
/// would otherwise parse to that shape (`"http://"`, `"http:///"`, ...)
/// fails to parse at all (verified directly, not assumed — an earlier
/// version of this function had a second check for exactly that case, and
/// mutation-testing it found the check dead: no `http::Uri` reachable
/// through this crate's own public construction API can ever hit it, so
/// there was no way to write a test that exercised it rather than the
/// scheme check above). A relative-form URI (no scheme at all, e.g.
/// `"/relative"`) is caught by the `None` arm below, before authority ever
/// comes into it.
fn checked_url(uri: &http::Uri) -> Result<String, Error> {
    match uri.scheme_str() {
        Some("http") | Some("https") => Ok(uri.to_string()),
        Some(other) => Err(Error::new(
            ErrorKind::Unsupported,
            BadUrl(format!(
                "URI scheme `{other}` is not `http` or `https`: `{uri}`"
            )),
        )),
        None => Err(Error::new(
            ErrorKind::Unsupported,
            BadUrl(format!(
                "URI has no scheme, fetch needs an absolute URL: `{uri}`"
            )),
        )),
    }
}

/// Converts one `http::Request<RequestBody>` into a `web_sys::Request`,
/// ready for `fetch()`.
///
/// Returns the `AbortController` alongside the request, not folded into
/// it: fetch's only way to cancel an in-flight exchange is calling
/// `AbortController::abort()` on the controller whose signal was attached
/// at construction time, and a later task (the `Transport` impl, driving
/// deadlines and cancellation) needs to hold onto it after this function
/// returns. `Option`, not a bare `AbortController`: `AbortController::new()`
/// has no documented failure mode in a real browser, but the constructor
/// still returns `Result` on the JS side, and a request without cancellation
/// support is a strictly smaller, still-honest capability — unlike a
/// forbidden header or an unbuildable body, there's no lie in sending a
/// request that merely can't be aborted early.
pub(crate) fn to_web_request(
    req: http::Request<RequestBody>,
    caps: &Capabilities,
) -> Result<Converted, Error> {
    let (parts, body) = req.into_parts();
    check_headers(&parts.headers, caps)?;
    let url = checked_url(&parts.uri)?;
    let resolved = resolve_body(body)?;

    if matches!(resolved, ResolvedBody::Streaming(_)) && !caps.streaming_request_body {
        // The probe behind this field is behavioural and measured — see the
        // module doc comment, point 3, and `caps::supports_streaming_request_body`.
        // A browser that would stringify the stream instead of sending it
        // never reaches the construction below.
        return Err(Error::new(
            ErrorKind::Unsupported,
            UnsupportedCapability {
                what: "streaming_request_body",
                backend: "fetch",
            },
        ));
    }
    // Both body-carrying arms, not just `Full`. Fetch throws on a body with
    // `GET`/`HEAD` regardless of how that body is expressed, and a
    // `RequestBody::Streaming` that happens to yield no frames is still a
    // body as far as the `Request` constructor is concerned — the length is
    // unknown until the stream ends, which is long after construction.
    if !matches!(resolved, ResolvedBody::None)
        && (parts.method == http::Method::GET || parts.method == http::Method::HEAD)
    {
        return Err(Error::new(
            ErrorKind::Unsupported,
            BodyNotAllowedForMethod(parts.method.clone()),
        ));
    }

    let init = web_sys::RequestInit::new();
    init.set_method(parts.method.as_str());

    let headers = build_headers(&parts.headers)?;
    init.set_headers(&headers);

    let streamed = matches!(resolved, ResolvedBody::Streaming(_));
    match resolved {
        ResolvedBody::None => {}
        ResolvedBody::Full(arr) => init.set_body_opt_u8_array(Some(&arr)),
        ResolvedBody::Streaming(body) => {
            let stream = wasm_streams::ReadableStream::from_stream(BodyStream(body));
            init.set_body_opt_readable_stream(Some(&stream.into_raw()));
            // `duplex: "half"` — required, and required BEFORE the fact.
            // Chrome refuses to construct a `Request` with a stream body
            // without it ("Failed to construct 'Request': The `duplex`
            // member must be specified for a request with a streaming
            // body", measured), so this is not a hint the browser may
            // ignore. Set through `Reflect` because web-sys 0.3.103's
            // `RequestInit` has no `duplex` binding at all — the same
            // reason `caps::supports_streaming_request_body` builds its
            // probe dictionary by hand.
            //
            // "half", never "full": the response is not readable until the
            // request body has finished, which is why
            // `Capabilities::full_duplex` stays `false` (see `caps::probe`).
            js_sys::Reflect::set(
                init.unchecked_ref::<js_sys::Object>(),
                &JsValue::from_str("duplex"),
                &JsValue::from_str("half"),
            )
            .map_err(js_err)?;
        }
    }

    let controller = web_sys::AbortController::new().ok();
    if let Some(c) = &controller {
        init.set_signal(Some(&c.signal()));
    }

    let request = web_sys::Request::new_with_str_and_init(&url, &init).map_err(js_err)?;

    verify_headers_survived(&parts.headers, &request)?;

    Ok(Converted {
        request,
        abort: controller,
        streamed,
    })
}

/// What [`to_web_request`] hands back.
///
/// A struct rather than the `(Request, Option<AbortController>)` tuple it
/// used to be, for one field: `streamed`. `execute` needs it to name the
/// cause the browser hides (see [`StreamingBodyFetchFailed`]), and it
/// cannot recompute it from the original `http::Request` — that has been
/// consumed, and a `RequestBody::Rewindable` whose factory returns a
/// `Streaming` looks like neither from the outside. Reading it back off the
/// built `web_sys::Request` does not work either: a `Full` body is exposed
/// as a `ReadableStream` there too.
pub(crate) struct Converted {
    pub(crate) request: web_sys::Request,
    pub(crate) abort: Option<web_sys::AbortController>,
    /// Whether the body handed to the browser is a `ReadableStream` built
    /// from a [`RequestBody::Streaming`] — not merely whether the request
    /// has a body.
    pub(crate) streamed: bool,
}
