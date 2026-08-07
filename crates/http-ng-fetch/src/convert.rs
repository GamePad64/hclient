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
//!    fails loudly if anything we tried to set didn't make it, rather than
//!    letting a caller believe an unnamed forbidden header (`Sec-Foo`, say)
//!    went out when the browser quietly dropped it.
//!
//! 2. **No `RequestBody` variant silently becomes an empty body.**
//!    [`resolve_body`] handles `Empty`, `Full`, `Rewindable`, and
//!    `Streaming` on purpose — `Rewindable` by recursing into whatever its
//!    factory hands back, through the SAME match, not a partial one that
//!    only understands `Full` and drops everything else (that was the exact
//!    review-caught defect in vertical 2's native `body.rs`: a `Rewindable`
//!    wrapping anything but a non-empty `Full` collapsed to nothing sent).
//!    `Streaming` is the one variant this task genuinely cannot forward —
//!    see its doc comment — and that is a typed [`ErrorKind::Unsupported`],
//!    never a silent empty body.
//!
//! 3. **A capability that's probably right doesn't get trusted blindly.**
//!    `Capabilities::streaming_request_body` (Task 2's `supports_duplex`) is
//!    a presence check, not a behavioral one — whatwg/fetch#1470 records a
//!    browser could in principle expose the `duplex` getter without
//!    honoring it. This file sidesteps the question entirely rather than
//!    answering it wrong: `RequestBody::Streaming` is rejected
//!    UNCONDITIONALLY, regardless of what `caps.streaming_request_body`
//!    says, because building the `ReadableStream` a streaming fetch needs
//!    requires `wasm-streams`, which this crate does not yet depend on (see
//!    `Cargo.toml`). A wrong-in-the-optimistic-direction probe therefore
//!    can't turn into a silently truncated body: there is no code path here
//!    that ever attempts to send a streaming body at all.
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, UnsupportedCapability};
use wasm_bindgen::JsValue;

// ---------------------------------------------------------------------
// Typed failure causes. Each is a distinct type, not a formatted string,
// so `Error::source().downcast_ref` can recover the exact cause later —
// the same reasoning `http_ng_core::Error`'s own doc comment gives for why
// it wraps a `dyn Error` rather than stringifying up front.
// ---------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct ForbiddenHeader(pub(crate) String);
impl std::fmt::Display for ForbiddenHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "fetch forbids setting the `{}` header", self.0)
    }
}
impl std::error::Error for ForbiddenHeader {}

/// A header survived [`check_headers`] (it isn't one of the 14 names
/// [`crate::FORBIDDEN_HEADERS`] lists) but the browser dropped it anyway
/// when building the `Request` — almost certainly a `Sec-*`/`Proxy-*`
/// prefixed name, or one of the ten other forbidden names that fixed array
/// structurally can't carry (see its doc comment). Caught by
/// [`verify_headers_survived`], never allowed to pass as a quiet success.
#[derive(Debug)]
pub(crate) struct HeaderSilentlyDropped(pub(crate) String);
impl std::fmt::Display for HeaderSilentlyDropped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the `{}` header was accepted but the browser silently dropped it while building the \
             request (not one of the names FORBIDDEN_HEADERS checks ahead of time — most likely a \
             `Sec-`/`Proxy-`-prefixed name, which a fixed array cannot express)",
            self.0
        )
    }
}
impl std::error::Error for HeaderSilentlyDropped {}

/// A header value with bytes that aren't valid ASCII text. `http::HeaderMap`
/// allows opaque byte strings (`HeaderValue::from_bytes`); the Fetch API's
/// `Headers.append` only takes a JS string, so this is a real, nameable
/// limit of the backend, not an opaque `TypeError`.
#[derive(Debug)]
pub(crate) struct NonAsciiHeaderValue(pub(crate) String);
impl std::fmt::Display for NonAsciiHeaderValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the value of header `{}` is not valid ASCII text, which the Fetch API's `Headers` \
             interface requires",
            self.0
        )
    }
}
impl std::error::Error for NonAsciiHeaderValue {}

#[derive(Debug)]
pub(crate) struct BadUrl(pub(crate) String);
impl std::fmt::Display for BadUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for BadUrl {}

/// Fetch throws a `TypeError` if a `GET`/`HEAD` request carries a body.
/// Checked ahead of time so the caller gets a typed, specific error instead
/// of `ErrorKind::Other` wrapping a browser-thrown `TypeError` string.
#[derive(Debug)]
pub(crate) struct BodyNotAllowedForMethod(pub(crate) http::Method);
impl std::fmt::Display for BodyNotAllowedForMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a `{}` request cannot carry a body — fetch forbids a body on GET and HEAD requests",
            self.0
        )
    }
}
impl std::error::Error for BodyNotAllowedForMethod {}

/// Upper bound on `RequestBody::Rewindable` nesting (a factory whose result
/// is itself another `Rewindable`). No legitimate factory needs this — one
/// factory calling through to a second, to a third, buys the caller
/// nothing — so past this depth [`resolve_body`] stops with a typed error
/// instead of silently giving up (an empty body) or recursing forever.
/// Same constant, same bound, same reasoning as
/// `http-ng-wasi/src/convert.rs`'s `MAX_REWIND_DEPTH`.
const MAX_REWIND_DEPTH: u8 = 16;

#[derive(Debug)]
pub(crate) struct RewindTooDeep;
impl std::fmt::Display for RewindTooDeep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RequestBody::Rewindable factory nested more than {MAX_REWIND_DEPTH} levels deep \
             (each factory call returned another Rewindable instead of a terminal body)"
        )
    }
}
impl std::error::Error for RewindTooDeep {}

#[derive(Debug)]
pub(crate) struct JsError(pub(crate) String);
impl std::fmt::Display for JsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "javascript error: {}", self.0)
    }
}
impl std::error::Error for JsError {}

/// Turns a rejected JS promise/thrown value into our `Error`. This is the
/// generic, honest fallback for a JS failure this file cannot classify any
/// further — every failure mode this file CAN name ahead of time
/// (forbidden header, bad URL, unsupported body) is caught earlier and
/// never reaches this function with a category to lose.
pub(crate) fn js_err(v: JsValue) -> Error {
    let msg = v
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(&v, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{v:?}"));
    Error::new(ErrorKind::Other, JsError(msg))
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
/// every header we tried to set is actually there. Closes the gap
/// `FORBIDDEN_HEADERS` structurally can't: a `Sec-*`/`Proxy-*`-prefixed
/// header, or one of the ten other forbidden names outside that fixed
/// array, sails past `check_headers` untouched and is then silently
/// dropped by the browser's own `Headers.append` (guard `"request"`, per
/// the Fetch Standard: forbidden names are skipped, not rejected with an
/// exception) — with no JS error to catch. Without this step, a caller who
/// pre-filtered against `Capabilities::forbidden_request_headers` and
/// still set, say, `Sec-Foo` would have it vanish with the transport
/// reporting success — exactly the silent no-op this project forbids.
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
/// nothing, a byte buffer, or "this was a stream and this task can't send
/// one" — see the module doc comment, point 2 and 3.
enum ResolvedBody {
    None,
    Full(js_sys::Uint8Array),
    Streaming,
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
/// Iterative, not recursive, and bounded by [`MAX_REWIND_DEPTH`]: a factory
/// that always returns another `Rewindable` would otherwise unwrap forever.
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
            // Not resolved further here — matched, not silently dropped;
            // the caller decides how to fail (module doc comment, point 3).
            RequestBody::Streaming(_) => return Ok(ResolvedBody::Streaming),
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
) -> Result<(web_sys::Request, Option<web_sys::AbortController>), Error> {
    let (parts, body) = req.into_parts();
    check_headers(&parts.headers, caps)?;
    let url = checked_url(&parts.uri)?;
    let resolved = resolve_body(body)?;

    if let ResolvedBody::Streaming = resolved {
        // Unconditional: see the module doc comment, point 3. Even a
        // browser whose `duplex` probe is a false positive can't cause a
        // truncated send here, because this arm never attempts one.
        return Err(Error::new(
            ErrorKind::Unsupported,
            UnsupportedCapability {
                what: "streaming_request_body",
                backend: "fetch",
            },
        ));
    }
    if matches!(resolved, ResolvedBody::Full(_))
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

    if let ResolvedBody::Full(arr) = &resolved {
        init.set_body_opt_u8_array(Some(arr));
    }

    let controller = web_sys::AbortController::new().ok();
    if let Some(c) = &controller {
        init.set_signal(Some(&c.signal()));
    }

    let request = web_sys::Request::new_with_str_and_init(&url, &init).map_err(js_err)?;

    verify_headers_survived(&parts.headers, &request)?;

    Ok((request, controller))
}
