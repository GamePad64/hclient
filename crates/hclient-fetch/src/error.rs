//! The twenty causes this backend names because the browser will not.
//!
//! **What they have in common is a `TypeError`.** A browser reports a
//! failed `fetch` as one string with no `name`, no `code` and no property
//! separating a refused connection from a header it silently dropped; a
//! failed WebSocket handshake is a close code and nothing else, because
//! there is deliberately no reason on `onerror`. So almost every type here
//! exists to say the one thing the platform structurally cannot — which is
//! why they read better as a list than as three lists in the three files
//! that raise them: read together they are an inventory of where the Fetch
//! and WebSocket APIs stop reporting, and that inventory is the honest
//! description of this transport.
//!
//! They divide by *when* rather than by which file they came from, and the
//! division is load-bearing. Eight are refusals made **before** anything
//! reaches the browser ([`ForbiddenHeader`], [`NonAsciiHeaderValue`],
//! [`BadUrl`], [`BodyNotAllowedForMethod`], [`HeaderNotSendable`],
//! [`BadSubprotocol`], [`UnsupportedScheme`], [`NoAuthority`]) — a typed
//! error at the line the caller wrote, instead of a thrown `TypeError`
//! later. The rest are the browser's own answers, translated: a value it
//! took and dropped ([`HeaderSilentlyDropped`]), a promise it rejected
//! ([`JsError`], [`StreamRead`], [`ConstructorRefused`], [`SendRefused`]),
//! a stream whose chunk was the wrong shape ([`NotAByteChunk`],
//! [`NotAMessage`]), and a socket that ended without saying why
//! ([`HandshakeFailed`], [`ConnectionLost`], [`NotOpen`]).
//!
//! Two are neither, and they are the ones worth finding here.
//! [`StreamingBodyFetchFailed`] **wraps** a [`JsError`] rather than
//! replacing it, because the browser's text is right and merely
//! incomplete. [`RequestTrailersUnsupported`] cannot be raised before the
//! request is handed over at all — trailers arrive after the last data
//! frame — so it is the one refusal here that a caller meets late.
//!
//! Every one of them is private and reaches a caller only through
//! `Error::source`, and the `ErrorKind` each is filed under stays at the
//! site that raises it: the same cause is a `Decode` in one place and a
//! `Body` in another, and that choice belongs to the code making it.

use hclient_core::Error;

// ---------------------------------------------------------------------
// Building a request the browser will accept — `convert.rs`.
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[error("fetch forbids setting the `{0}` header")]
pub(crate) struct ForbiddenHeader(pub(crate) String);

/// A header survived [`crate::convert::check_headers`] (it isn't one of
/// the 14 names [`crate::FORBIDDEN_HEADERS`] lists) but the browser
/// dropped it anyway when building the `Request` — almost certainly a
/// `Sec-*`/`Proxy-*` prefixed name, or one of the ten other forbidden
/// names that fixed array structurally can't carry (see its doc comment).
/// Caught by `convert::verify_headers_survived`, never allowed to pass as
/// a quiet success.
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
/// Unlike every other cause raised in `convert.rs`, this one cannot be
/// raised before `to_web_request` returns: trailer frames arrive after the
/// last data frame, by which point the browser owns the stream. The same
/// shape, and the same reasoning, as the trailers guard `hclient-wasi`
/// moved into its `Body` (see that crate's notes on duplex request
/// bodies).
#[derive(Debug, thiserror::Error)]
#[error(
    "the streaming request body produced trailers, which fetch cannot send (this backend \
     declares `request_trailers = false`)"
)]
pub(crate) struct RequestTrailersUnsupported;

// ---------------------------------------------------------------------
// Reading a response body — `body.rs`. Two, not one, and which
// `ErrorKind` each carries is that module's doc comment.
// ---------------------------------------------------------------------

/// The underlying `ReadableStream` rejected a `read()` — the browser's own
/// signal that the exchange didn't finish cleanly (a network failure, the
/// connection closing before the promised length, an explicit upstream
/// `controller.error()`). A transport failure, not a decode problem:
/// `ErrorKind::Body`.
#[derive(Debug, thiserror::Error)]
#[error("reading the response body stream failed: {0}")]
pub(crate) struct StreamRead(pub(crate) String);

/// A `ReadableStream` chunk that isn't a `Uint8Array`. Every chunk a real
/// `fetch()` response body produces IS one (`Response.body` is specified as
/// a byte stream) — this is a defensive check against a stream that
/// violates that (see `tests/body.rs`'s construction of one), not something
/// ordinary network traffic can trigger. The READ succeeded — no rejection,
/// nothing wrong with the transport — it's the SHAPE of what came back
/// that's wrong: `ErrorKind::Decode`, the same category
/// `hclient`'s `Response::text`/`Response::json` use for "these bytes don't
/// parse as promised".
#[derive(Debug, thiserror::Error)]
#[error("a ReadableStream chunk from the response body was not a Uint8Array")]
pub(crate) struct NotAByteChunk;

// ---------------------------------------------------------------------
// The browser's own WebSocket — `websocket.rs`.
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[error(
    "the browser's WebSocket cannot send a `{0}` header: `new WebSocket(url, protocols)` takes a \
     URL and a subprotocol list, and nothing else reaches the handshake"
)]
pub(crate) struct HeaderNotSendable(pub(crate) http::HeaderName);

#[derive(Debug, thiserror::Error)]
#[error("a `{0}` value that is not visible ASCII cannot become a subprotocol")]
pub(crate) struct BadSubprotocol(pub(crate) http::HeaderName);

#[derive(Debug, thiserror::Error)]
#[error("unsupported URI scheme for a WebSocket: {0:?} (expected ws, wss, http or https)")]
pub(crate) struct UnsupportedScheme(pub(crate) String);

#[derive(Debug, thiserror::Error)]
#[error("a WebSocket URL needs a host: `{0}`")]
pub(crate) struct NoAuthority(pub(crate) String);

#[derive(Debug, thiserror::Error)]
#[error("the browser refused to construct a WebSocket: {0}")]
pub(crate) struct ConstructorRefused(pub(crate) String);

/// The browser tells us a close code and nothing else — see
/// [`crate::websocket`]'s module doc on why there is no `onerror` handler
/// to ask.
#[derive(Debug, thiserror::Error)]
#[error(
    "the WebSocket handshake failed (close code {0}); a browser reports no reason for a failed \
     WebSocket handshake, deliberately"
)]
pub(crate) struct HandshakeFailed(pub(crate) u16);

#[derive(Debug, thiserror::Error)]
#[error("the WebSocket closed without a close handshake (code {0})")]
pub(crate) struct ConnectionLost(pub(crate) u16);

#[derive(Debug, thiserror::Error)]
#[error("a WebSocket message was neither a string nor an ArrayBuffer")]
pub(crate) struct NotAMessage;

#[derive(Debug, thiserror::Error)]
#[error(
    "the WebSocket is not open (readyState {0}), and the browser would have discarded this \
     message without reporting anything"
)]
pub(crate) struct NotOpen(pub(crate) u16);

#[derive(Debug, thiserror::Error)]
#[error("the browser refused to send on the WebSocket: {0}")]
pub(crate) struct SendRefused(pub(crate) String);
