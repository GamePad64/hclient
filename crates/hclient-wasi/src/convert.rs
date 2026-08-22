//! Conversion between hclient <-> `wasi:http` 0.3, and honoring the host's
//! setters.
//!
//! `wasi:http` 0.3's setters return `result<_, ...>` for exactly this
//! reason: so the host can say "unsupported" or "immutable". This crate's
//! predecessor, `wasi-fetch`, discarded seven such `Result`s via `let _ =`
//! (`set_connect_timeout`, `set_first_byte_timeout`, `set_between_bytes_timeout`,
//! `set_method`, `set_scheme`, `set_authority`, `set_path_with_query`). Here
//! every such rejection becomes a typed `Error` instead of silently
//! vanishing — CI checks this structurally, without relying on review
//! discipline: the `no-discarded-wasi-setter-result` ast-grep rule, and
//! the corpus it was accepted against, in `scripts/ast-grep`.

use bytes::Bytes;
use hclient_core::{Error, ErrorKind, RequestBody};
use http_body::{Body as HttpBody, Frame};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use wasip3::http::types::{
    ErrorCode, HeaderError, Method as WM, RequestOptions, RequestOptionsError, Scheme,
};

pub(crate) fn to_wasi_method(m: &http::Method) -> WM {
    match *m {
        http::Method::GET => WM::Get,
        http::Method::POST => WM::Post,
        http::Method::PUT => WM::Put,
        http::Method::DELETE => WM::Delete,
        http::Method::PATCH => WM::Patch,
        http::Method::HEAD => WM::Head,
        http::Method::OPTIONS => WM::Options,
        _ => WM::Other(m.to_string()),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("URI scheme must be http or https")]
pub(crate) struct BadScheme;

/// `BadScheme` is the same class of
/// failure as `Rejected` (below) — "the backend just doesn't take this
/// particular value" — and it used to be the only one of them flattened
/// into `ErrorKind::Other`, even though it's just as useful for the caller
/// here to be able to tell "the backend doesn't support this" from other
/// errors via `is_unsupported()`.
pub(crate) fn scheme_of(uri: &http::Uri) -> Result<Scheme, Error> {
    match uri.scheme_str() {
        Some("https") => Ok(Scheme::Https),
        Some("http") => Ok(Scheme::Http),
        _ => Err(Error::new(ErrorKind::Unsupported, BadScheme)),
    }
}

/// Applies timeouts, **without swallowing host rejections**.
///
/// `wasi:http` 0.3's setters return
/// `result<_, request-options-error{not-supported, immutable, other}>` for
/// exactly this reason: so the host can say "unsupported". `wasi-fetch`
/// discarded seven such `Result`s via `let _ =`; here every rejection
/// becomes an error — see `unsupported_timeout` for exactly what gets
/// carried into it from `RequestOptionsError`.
pub(crate) fn apply_timeouts(
    opts: &RequestOptions,
    connect: Option<u64>,
    first_byte: Option<u64>,
    between_bytes: Option<u64>,
) -> Result<(), Error> {
    if let Some(ns) = connect {
        opts.set_connect_timeout(Some(ns))
            .map_err(|e| unsupported_timeout("connect_timeout", e))?;
    }
    if let Some(ns) = first_byte {
        opts.set_first_byte_timeout(Some(ns))
            .map_err(|e| unsupported_timeout("first_byte_timeout", e))?;
    }
    if let Some(ns) = between_bytes {
        opts.set_between_bytes_timeout(Some(ns))
            .map_err(|e| unsupported_timeout("between_bytes_timeout", e))?;
    }
    Ok(())
}

fn unsupported_timeout(what: &'static str, source: RequestOptionsError) -> Error {
    Error::new(ErrorKind::Unsupported, TimeoutRejected { what, source })
}

/// The host's rejection of applying a request timeout option.
///
/// Unlike `Rejected` (below), this rejection has a substantive reason —
/// `RequestOptionsError::{NotSupported,Immutable,Other}` — and it isn't
/// flattened into a string: `Display` reports both WHAT was rejected and
/// WHY, and the `RequestOptionsError` itself stays fully reachable through
/// `Error::source()`. The same principle as `wasi_err` for `ErrorCode`:
/// the category (`ErrorKind::Unsupported`) is kept separate from the
/// reason, and the reason isn't a string.
///
/// The field is named `source` on purpose, not by accident of naming:
/// that is what makes it the `Error::source()` of this type, without an
/// attribute — and it must stay so, because the reason is exactly what a
/// caller comes down the chain for.
#[derive(Debug, thiserror::Error)]
#[error("backend `wasi:http` does not support `{what}`: {source}")]
pub(crate) struct TimeoutRejected {
    what: &'static str,
    source: RequestOptionsError,
}

/// The host's rejection of applying `method`/`scheme`/`authority`/
/// `path_with_query`.
///
/// Unlike `TimeoutRejected`, these `wasi:http` setters return a bare
/// `Result<(), ()>` — the host never sends back any reason at all,
/// there's nothing to wrap in `source`. `what` is all there is to report.
#[derive(Debug, thiserror::Error)]
#[error("wasi:http host rejected setting `{0}`")]
pub(crate) struct Rejected(&'static str);
pub(crate) fn rejected(what: &'static str) -> Error {
    Error::new(ErrorKind::Unsupported, Rejected(what))
}

/// `Fields::from_list` failed: the header is syntactically invalid,
/// forbidden, or exceeds a host limit.
///
/// `#[source]`, not `#[from]`: the category is chosen per variant by
/// `fields_error` below, so there is no single `HeaderError -> Error`
/// conversion to hand out.
#[derive(Debug, thiserror::Error)]
#[error("invalid headers: {0}")]
pub(crate) struct FieldsError(#[source] HeaderError);
/// `HeaderError::Forbidden`/`::Immutable`
/// — the host refuses to accept a specific header — structurally the same
/// class of failure as `Rejected`/`TimeoutRejected` (`ErrorKind::Unsupported`),
/// not a generic error. `InvalidSyntax`/`SizeExceeded`/`Other` are not a
/// capability rejection but a defect/limit on the caller's side, and stay
/// `Other`.
pub(crate) fn fields_error(e: HeaderError) -> Error {
    let kind = match &e {
        HeaderError::Forbidden | HeaderError::Immutable => ErrorKind::Unsupported,
        _ => ErrorKind::Other,
    };
    Error::new(kind, FieldsError(e))
}

/// `ErrorCode` goes into `Error::new` as-is, unwrapped — the same choice
/// as in `body.rs` for response-body read errors: `ErrorCode` already
/// implements `Debug`/`Display`/`core::error::Error` by hand, a wrapper
/// wouldn't add substance, and `Error::new` erases the concrete type into
/// `Arc<dyn Error + Send + Sync>` anyway.
///
/// The category is preserved, not flattened into a single string:
/// `wasi-fetch` collapsed everything into `Error::Transport(format!("{e:?}"))`,
/// and the host side, `act`, then reconstructed the category by
/// substring-matching down the `source()` chain. Variant names checked
/// against `wasip3-0.7.0+wasi-0.3.0/src/service.rs:161-206`.
pub(crate) fn wasi_err(e: ErrorCode) -> Error {
    use hclient_core::Phase;
    use wasip3::http::types::ErrorCode as EC;
    let kind = match &e {
        EC::DnsTimeout | EC::DnsError(_) => ErrorKind::Resolve,
        EC::DestinationNotFound
        | EC::DestinationUnavailable
        | EC::DestinationIpProhibited
        | EC::DestinationIpUnroutable
        | EC::ConnectionRefused
        | EC::ConnectionTerminated
        | EC::ConnectionLimitReached => ErrorKind::Connect,
        EC::ConnectionTimeout => ErrorKind::Timeout(Phase::Connect),
        EC::ConnectionReadTimeout | EC::HttpResponseTimeout => ErrorKind::Timeout(Phase::FirstByte),
        EC::ConnectionWriteTimeout => ErrorKind::Timeout(Phase::BetweenBytes),
        EC::TlsProtocolError | EC::TlsCertificateError | EC::TlsAlertReceived(_) => ErrorKind::Tls,
        EC::HttpRequestDenied => ErrorKind::Status,
        EC::LoopDetected => ErrorKind::Redirect,
        EC::HttpUpgradeFailed | EC::ConfigurationError => ErrorKind::Unsupported,
        EC::HttpResponseIncomplete
        | EC::HttpResponseBodySize(_)
        | EC::HttpResponseTransferCoding(_)
        | EC::HttpResponseContentCoding(_)
        // the same request/trailer
        // families that already give `ErrorKind::Body` for the response
        // above — 14 of the 39 `ErrorCode` variants used to fall into
        // `_ => Other` without a second look; here only the ones that
        // obviously belong to the already-existing `Body` category (body
        // or trailer size/presence) are explicitly folded in, rather than
        // inventing a new category for the rest —
        // `HttpRequestMethodInvalid`/`HttpRequestUriInvalid`/
        // `HttpProtocolError` and so on honestly stay `Other`: there's no
        // obvious category for them here.
        | EC::HttpRequestLengthRequired
        | EC::HttpRequestBodySize(_)
        | EC::HttpRequestTrailerSectionSize(_)
        | EC::HttpRequestTrailerSize(_)
        | EC::HttpResponseTrailerSectionSize(_)
        | EC::HttpResponseTrailerSize(_) => ErrorKind::Body,
        _ => ErrorKind::Other,
    };
    Error::new(kind, e)
}

/// Writing the request body (`BodyWriter::send_http_body`) failed.
///
/// Wraps the whole `wasip3::http_compat::Error`, not just its `Display`:
/// the reason (`HttpBody` — our own frame source failed;
/// `StreamReaderClosed` — the host closed the reading end early;
/// `ResultReaderClosed` / `InvalidTrailers` — a failure at the tail of the
/// transfer) stays reachable through `Error::source()` instead of being
/// flattened into a string. See `resolve_send` for exactly when this
/// error wins and when it yields to the `send` error.
#[derive(Debug, thiserror::Error)]
#[error("failed to write request body: {0}")]
pub(crate) struct BodyWriteFailed(#[source] wasip3::http_compat::Error);
fn body_write_failed(e: wasip3::http_compat::Error) -> Error {
    Error::new(ErrorKind::Body, BodyWriteFailed(e))
}

/// Folds the outcome of two concurrent actions (`client::send` and
/// `BodyWriter::send_http_body`) into a single `Result`. Neither of the
/// two input `Result`s is discarded — this is exactly the point where the
/// task's draft proposed `let (resp, _written) = join!(..)`.
///
/// **On the third discarded `Result` — review resolution, finding B-3,
/// revisited with evidence, wording refined in fix round 2 (finding 5: an
/// earlier version of this comment overstated what had been measured —
/// see below).** `Request::new`'s second return value — a
/// `FutureReader<Result<(), ErrorCode>>`, documented upstream as
/// "resolves to result of transmission of this request" — is also
/// discarded (`Transport::execute` drops it explicitly, with a comment at
/// the drop site). The review's plan — fold it in here as a third input —
/// was implemented and **rolled back**: measured on a live host (wasmtime
/// 47, `wasip3` 0.7.0, twice — once through this crate's full path, once
/// through bare `wasip3` calls bypassing `hclient-wasi` entirely, to rule
/// out a bug in our own race logic) that this future is **not
/// guaranteed** to resolve before the response body has been fully
/// drained — NOT "never resolves earlier": separately re-checked (a
/// 12-byte `Content-Length` response, `resp` dropped immediately, body
/// never touched) — `transmitted` resolves to `Ok(())` without a single
/// `poll_frame` call. But for `chunked`, trailer-bearing responses, and
/// any body of appreciable size, it measurably does not resolve until
/// `Body` is drained by hand — `send()` returned `Ok` immediately, the
/// transmission future stayed `Pending` until the entire `Body` had been
/// drained, and only resolved then. `execute()` hands `Body` to the
/// caller BEFORE they decide whether to read it — normally later,
/// partially, or never. Waiting on this future here UNCONDITIONALLY would
/// mean either: `execute()` doesn't return for a typical response with a
/// body until it has drained it entirely on the caller's behalf
/// (destroying the streaming `Body` exists for), or it hangs for the
/// ordinary case of "the body is read after getting the `Response`" —
/// that is, after `execute()` was already obligated to have returned.
///
/// Neither option is compatible with THIS shape of the seam — but a third
/// option is compatible, and out of scope for this fix round: carry this
/// future into the returned `Body` and await it at the end of the
/// stream, surfacing a transmission failure as a terminal body error
/// instead of a clean `None`. Exactly what's measured here is what
/// justifies it — the future resolves at the moment the body finishes
/// draining, and that's precisely where `Body` lives. A candidate for
/// v0.2, not a rejected dead end. Details and the drop site —
/// `Transport::execute` in `lib.rs`.
///
/// **Policy on a mismatch between outcomes.**
/// - `send` returned `Err` → it takes priority regardless of the write's
///   outcome: `ErrorCode` carries a substantive taxonomy (DNS/connect/
///   TLS/…), and a body-write failure in this situation is almost always
///   a symptom of it (the host closed the connection — so it won't
///   finish reading the body either).
/// - `send` returned `Ok`, but the body write returned `Err` → the result
///   is **not** treated as a success, even with a response in hand.
///   `wasi:http` could have returned anything in that case, including
///   something based on data it never received in full — the response
///   can't be trusted as an answer to the specific request the caller
///   asked for. An early, deliberate host rejection (the server read the
///   headers, returned a 4xx, and closed the request stream without
///   reading the body to the end) is deliberately not carved out into a
///   success path in v0.1: this is a conscious narrowing, not a
///   forgotten case — telling apart "the host refused to read further"
///   from "our own frame source broke" from `wasip3::http_compat::Error`
///   alone is unreliable (both surface as similar variants), and
///   widening `Capabilities` for this case is out of this task's scope.
pub(crate) fn resolve_send<T>(
    resp: Result<T, ErrorCode>,
    written: Result<u64, wasip3::http_compat::Error>,
) -> Result<T, Error> {
    match (resp, written) {
        (Ok(r), Ok(_)) => Ok(r),
        (Err(e), _) => Err(wasi_err(e)),
        (Ok(_), Err(e)) => Err(body_write_failed(e)),
    }
}

/// Races `client::send` against the body write instead of waiting for
/// both via `join!`. `join!` used to
/// hold `execute` until BOTH arms finished; a host rejection known at
/// t≈0 (e.g. `ConnectionRefused`) would be held back until the body write
/// finished — for an unbounded streaming body, forever, since it would
/// never finish writing on its own. `resolve_send` already discards the
/// write's outcome when `send` fails (see its doc comment) — the
/// short-circuit here only changes the latency, not the policy: if `send`
/// resolves to `Err` first, this function returns it immediately,
/// dropping the still-unfinished `write_fut` (safe — the component model
/// supports cancelling an unfinished subtask via `drop`, see
/// `TaskCancelOnDrop` in the generated bindings). If `send` resolves
/// successfully first (or second), the write is still awaited —
/// `resolve_send` can't trust the response without knowing its outcome.
///
/// **This wait, and only this wait, is what makes `full_duplex = false`.**
/// The `Either::Left((Ok(r), write_fut))` arm holds a ready response in
/// hand and blocks on the write; for an
/// infinite `Streaming` body, forever. The shape of `Transport::execute`
/// has nothing to do with it: `write_fut` can be carried into the
/// returned `Body` (this crate's type) and polled further from
/// `poll_frame` — exactly the trick the `resolve_send` doc comment
/// already proposes for `transmitted`. The review implemented this and
/// measured it: 0.094s to the response head versus hanging. Full
/// justification and the three costs it's deferred for — in
/// `WasiHttp::new` (`lib.rs`).
pub(crate) async fn race_send_with_body<T>(
    send_fut: impl Future<Output = Result<T, ErrorCode>>,
    write_fut: impl Future<Output = Result<u64, wasip3::http_compat::Error>>,
) -> Result<T, Error> {
    use futures::future::Either;
    let send_fut = std::pin::pin!(send_fut);
    let write_fut = std::pin::pin!(write_fut);
    match futures::future::select(send_fut, write_fut).await {
        Either::Left((Err(e), _write_fut)) => Err(wasi_err(e)),
        Either::Left((Ok(r), write_fut)) => resolve_send(Ok(r), write_fut.await),
        Either::Right((written, send_fut)) => resolve_send(send_fut.await, written),
    }
}

/// Parses the request's `Trailer:` header(s) into a set of declared
/// trailer field names. `Trailer:` is a comma-separated list (RFC 9110
/// §6.5.1), possibly repeated across several headers; both forms fold
/// into one set. `HeaderName` already compares case-insensitively (`http`
/// stores the name in canonical form), so `X-Checksum` and `x-checksum`
/// are the same name.
pub(crate) fn declared_trailer_names(
    headers: &http::HeaderMap,
) -> std::collections::HashSet<http::HeaderName> {
    headers
        .get_all(http::header::TRAILER)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| http::HeaderName::from_bytes(s.trim().as_bytes()).ok())
        .collect()
}

/// The request body emitted trailer field(s) that `Trailer:` didn't name.
///
/// `wasi:http` honestly accepts trailers from `BodyWriter` (they go into
/// `result_writer` regardless of headers — see
/// `wasip3::http_compat::body_writer::send_http_body`), but it's measured
/// on a live host that the host's
/// HTTP/1.1 encoder silently drops them on the wire if the specific field
/// name wasn't declared in advance via `Trailer:` (RFC 9110 §6.5.1: a
/// receiver that chooses not to buffer the body is required to ignore
/// undeclared trailers) — review resolution, fix round 2 finding 2: what
/// needs comparing is the field NAMES themselves, not just whether the
/// `Trailer:` header is present. A `Trailer: X-Other` header, declaring a
/// different field than the one actually emitted (`x-checksum`), loses
/// data exactly as if the header were absent entirely — measured:
/// `execute` returned `Ok(200)`, while the wire showed `0\r\n\r\n` with no
/// trailer.
///
/// **The error arrives after the fact** (review resolution, fix round 2
/// finding 4): by the time the caller sees this error, the request has
/// already reached the server and gotten a response — the guard only
/// fires AFTER `race_send_with_body` has already succeeded, because
/// trailer field names, generally speaking, are only known once the body
/// has ended, and there's no predicting that before the headers. Don't
/// take this as a sign the request can be blindly retried: for a
/// non-idempotent request, that's a double send.
#[derive(Debug, thiserror::Error)]
#[error(
    "streaming request body emitted trailer field(s) [{}] that the request's \
     `Trailer:` header did not declare — wasi:http's HTTP/1.1 encoder drops undeclared \
     trailer fields silently. This error arrives after the request already reached the \
     server and was answered (race_send_with_body already succeeded) — do not retry \
     blindly, a non-idempotent request may already have taken effect.",
    .0.iter().map(http::HeaderName::as_str).collect::<Vec<_>>().join(", ")
)]
pub(crate) struct UndeclaredTrailers(Vec<http::HeaderName>);
pub(crate) fn undeclared_trailers(names: Vec<http::HeaderName>) -> Error {
    Error::new(ErrorKind::Body, UndeclaredTrailers(names))
}

/// An `http_body::Body` wrapper that leaves the frames passing through
/// untouched, and only collects field names from trailer frames that
/// actually pass through the wrapped body — needed by `Transport::execute`
/// so that, after a successful send, it can check them against what
/// `Trailer:` declared — the field names, not just whether the header is
/// present — right at the point
/// where a frame actually arrives, without predicting it ahead of time.
pub(crate) struct TrailerWatch<B> {
    inner: B,
    seen: Arc<Mutex<Vec<http::HeaderName>>>,
}

impl<B> TrailerWatch<B> {
    /// Wraps `inner` and returns the list of field names seen in trailer
    /// frames — grows as it's polled. `Arc<Mutex<_>>`, not
    /// `Rc<RefCell<_>>`: `Payload::Streaming` already carries `+ Send`
    /// (amendment-C2), and the wrapper shouldn't narrow that bond for a
    /// future `Transport::execute` (`tests/shape.rs` checks this property
    /// from outside the crate). `Mutex`, not something more exotic — the
    /// guest is single-threaded, there's never any contention for the
    /// lock, `Mutex` here is purely a formality for `Sync`.
    pub(crate) fn new(inner: B) -> (Self, Arc<Mutex<Vec<http::HeaderName>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                inner,
                seen: seen.clone(),
            },
            seen,
        )
    }
}

impl<B> HttpBody for TrailerWatch<B>
where
    B: HttpBody<Data = Bytes, Error = Error> + Unpin,
{
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let poll = Pin::new(&mut self.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(f))) = &poll {
            // `trailers_ref`
            // returns `Some(&HeaderMap)` even for an EMPTY trailers frame
            // (`Frame::trailers(HeaderMap::new())`) — such a frame loses
            // nothing on the wire (there's nothing to lose), so we only
            // register non-empty maps, not the mere fact "this is a
            // trailers frame".
            if let Some(map) = f.trailers_ref()
                && !map.is_empty()
            {
                let mut seen = self
                    .seen
                    .lock()
                    .expect("single-threaded guest, never poisoned");
                seen.extend(map.keys().cloned());
            }
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// What actually needs writing to the request body, after unwrapping
/// `RequestBody`. `Bytes` is a whole buffer (`RequestBody::Full`, already
/// non-empty); `Streaming` is the frame stream as-is, unbuffered.
///
/// The `+ Send` on `Streaming` is the same amendment-C2, as on
/// `RequestBody::Streaming` (`hclient-core/src/body.rs`), which is
/// unwrapped into this (see `resolve_payload`): `Box<T>: Send` only
/// requires `T: Send`, `Sync` isn't needed. Not a new bond, just carrying
/// forward an existing one — `RequestBody::Streaming` was already `Send`
/// before it got here.
pub(crate) enum Payload {
    Bytes(Bytes),
    Streaming(Box<dyn http_body::Body<Data = Bytes, Error = Error> + Unpin + Send>), // send-bound-exception: amendment-C2
}

// Manual `Debug`, not `#[derive]`: `Streaming` carries `Box<dyn
// http_body::Body>` — a trait object with no `Debug` bound, derive
// wouldn't have compiled. The same trick as `body::Inner` in `body.rs` —
// prints only the variant name.
impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Payload::Bytes(b) => f.debug_tuple("Bytes").field(b).finish(),
            Payload::Streaming(_) => f.write_str("Streaming(..)"),
        }
    }
}

/// Upper bound on the nesting of `RequestBody::Rewindable`, whose factory
/// itself returns another `Rewindable`. There's no legitimate scenario for
/// this — a factory calling another factory referring to a third one buys
/// the caller nothing — so past this depth `resolve_payload` stops with a
/// typed error instead of a silent `None` or unbounded recursion (review
/// resolution, finding B-11).
const MAX_REWIND_DEPTH: u8 = 16;

#[derive(Debug, thiserror::Error)]
#[error(
    "RequestBody::Rewindable factory nested more than {MAX_REWIND_DEPTH} levels deep \
     (each factory call returned another Rewindable instead of a terminal body)"
)]
pub(crate) struct RewindTooDeep;

/// Unwraps `RequestBody` into what actually needs to be sent: `None` for
/// an empty body, otherwise bytes or a stream.
///
/// `Rewindable` is unwrapped by calling its factory — the same way
/// `RequestBody::rewind()` in `hclient-core` unwraps it for a retry (see
/// the doc comment on `RequestBody::Rewindable`: the factory's contract
/// is a pure function, each call produces an equivalent body). Without
/// this step the body simply wouldn't show up in the outgoing request at
/// all — this was exactly the shape of the task draft's defect:
/// `Rewindable`- and `Streaming`-bodies were silently collapsed into an
/// empty body via `_ => None`, even though `WasiHttp::new()` declares
/// `Capabilities::streaming_request_body = true`. Here both cases are a
/// real path, not a silent data loss: `Streaming` is passed to
/// `BodyWriter::send_http_body` as-is (which reads any `http_body::Body`
/// frame by frame and writes to the stream as frames arrive, without
/// buffering the whole thing — i.e. it genuinely streams, living up to
/// the declared capability), and `Rewindable` after the factory unwraps
/// it.
///
/// Iterative, not recursive, and bounded by `MAX_REWIND_DEPTH` — a
/// factory that itself returns `Rewindable` would unwrap forever (or
/// until the stack overflowed) without it.
pub(crate) fn resolve_payload(body: RequestBody) -> Result<Option<Payload>, Error> {
    let mut body = body;
    for _ in 0..MAX_REWIND_DEPTH {
        match body {
            RequestBody::Empty => return Ok(None),
            RequestBody::Full(b) if b.is_empty() => return Ok(None),
            RequestBody::Full(b) => return Ok(Some(Payload::Bytes(b))),
            RequestBody::Rewindable(f) => body = f(),
            RequestBody::Streaming(s) => return Ok(Some(Payload::Streaming(s))),
        }
    }
    Err(Error::new(ErrorKind::Other, RewindTooDeep))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_methods_and_passes_through_unknown() {
        use wasip3::http::types::Method as WM;
        assert!(matches!(to_wasi_method(&http::Method::GET), WM::Get));
        assert!(matches!(to_wasi_method(&http::Method::DELETE), WM::Delete));
        let query = http::Method::from_bytes(b"QUERY").unwrap();
        assert!(matches!(to_wasi_method(&query), WM::Other(ref s) if s == "QUERY"));
    }

    #[test]
    fn rejects_non_http_schemes() {
        let ftp: http::Uri = "ftp://a/x".parse().unwrap();
        assert!(scheme_of(&ftp).is_err());
        let none: http::Uri = "/relative".parse().unwrap();
        assert!(scheme_of(&none).is_err());
    }

    /// `BadScheme` is the same class of
    /// failure as `Rejected`/`TimeoutRejected` — should be classified as
    /// `Unsupported`, not flattened into `Other`.
    #[test]
    fn bad_scheme_is_classified_as_unsupported_not_other() {
        let ftp: http::Uri = "ftp://a/x".parse().unwrap();
        let err = scheme_of(&ftp).unwrap_err();
        assert!(err.is_unsupported(), "{err:?}");
    }

    /// The category above says "the backend doesn't take this"; the
    /// message is the only place the caller is told WHICH schemes it does
    /// take. `Error`'s own `Display` prefixes the kind, so the check goes
    /// one hop down, at the concrete type.
    #[test]
    fn bad_scheme_message_names_the_schemes_that_are_accepted() {
        let ftp: http::Uri = "ftp://a/x".parse().unwrap();
        let err = scheme_of(&ftp).unwrap_err();
        let bad = std::error::Error::source(&err)
            .expect("Error::new always has a source")
            .downcast_ref::<BadScheme>()
            .expect("the source of a scheme rejection is BadScheme");
        assert_eq!(bad.to_string(), "URI scheme must be http or https");
    }

    #[test]
    fn capabilities_declare_what_wasi_http_actually_does() {
        let c = super::super::WasiHttp::new();
        let caps = hclient_core::unversioned::Transport::capabilities(&c);
        // wasi:http 0.3 is richer than native for request body streaming…
        assert!(caps.streaming_request_body);
        assert!(caps.request_trailers && caps.response_trailers);
        // …but NOT for body duplex: review resolution, finding B-2.
        // `WasiHttp::execute` doesn't return a response until the body
        // write has finished (or failed) — `race_send_with_body` waits
        // for both arms, except for an early `send` rejection (B-5). This
        // is a limitation of THIS implementation: the host does provide
        // duplex, and the limitation can be lifted without touching
        // `Transport` — carry the write future into `Body` and poll it
        // further from `poll_frame`. Full justification and the three
        // costs it's deferred for — in `WasiHttp::new` (M1 of the
        // branch's final review).
        assert!(!caps.full_duplex);
        // And poorer everywhere else.
        // `Transparent`, not `None`: a
        // 3xx reaches the guest as an ordinary response and the
        // `Client`'s redirect stage handles it in full. `None` is what
        // `Capabilities::none()` returns, and would mean "there are no
        // redirects here".
        assert_eq!(caps.redirects, hclient_core::RedirectSupport::Transparent);
        assert_ne!(
            caps.redirects,
            hclient_core::Capabilities::none().redirects,
            "a declared capability must differ from \"the field was never filled in\""
        );
        assert_eq!(caps.tls_config, hclient_core::TlsSupport::None);
        assert!(!caps.proxy);
        // five headers the host actually
        // refuses to accept from the guest.
        for name in [
            http::header::CONNECTION,
            http::header::HeaderName::from_static("keep-alive"),
            http::header::TRANSFER_ENCODING,
            http::header::UPGRADE,
            http::header::HOST,
        ] {
            assert!(
                caps.forbidden_request_headers.contains(&name),
                "{name} should be in forbidden_request_headers"
            );
        }
    }

    /// all of `wasi_err`'s
    /// classification (39 `ErrorCode` variants into eight `ErrorKind`s)
    /// only matters because this backend's `Transport::to_error` is the
    /// identity. With the default `Client::execute` implementation it
    /// would wrap it again, into `ErrorKind::Other`, and everything the
    /// tests above check about `is_connect()`/`is_unsupported()` would be
    /// true at the `wasi_err` level and false for the caller.
    ///
    /// Checks both observable consequences at once: the category and the
    /// `Display` (which, if wrapped, would print `Other: Tls: …` — a
    /// minor deferred to Task 6, about the doubling).
    #[test]
    fn to_error_is_the_identity_so_the_classification_survives_the_client() {
        use hclient_core::unversioned::Transport as _;

        let t = super::super::WasiHttp::new();
        let classified = wasi_err(ErrorCode::TlsProtocolError);
        let seen = t.to_error(classified);

        assert_eq!(seen.kind(), &ErrorKind::Tls);
        assert!(
            !seen.to_string().starts_with("Other:"),
            "the category must not be nested a second time: {seen}"
        );
    }

    /// Review (team lead resolution, item 1): in the task draft, `join!`
    /// in `Transport::execute` discarded the body write's result via
    /// `let (resp, _written) = ..`. `resolve_send` is the point where
    /// that no longer happens; tested here as a pure function (generic
    /// over `T`, so no live-host `Response` is needed) across all three
    /// outcomes.
    #[test]
    fn resolve_send_prefers_the_response_when_both_succeed() {
        let got = resolve_send::<u32>(Ok(7), Ok(3));
        assert_eq!(got.unwrap(), 7);
    }

    #[test]
    fn resolve_send_surfaces_the_send_error_even_if_the_body_wrote_fine() {
        let err = resolve_send::<u32>(Err(ErrorCode::ConnectionRefused), Ok(3)).unwrap_err();
        assert!(err.is_connect(), "{err:?}");
    }

    /// Team lead resolution, item 1: the policy for when `send` returned
    /// `Ok` but the body write returned `Err`. The choice is not to treat
    /// this as a success: a response received while the request body was
    /// left unfinished can't be handed to the caller without a signal
    /// that the body didn't go out in full.
    #[test]
    fn resolve_send_does_not_trust_a_response_that_arrived_over_a_failed_body_write() {
        let write_err = wasip3::http_compat::Error::StreamReaderClosed {
            written: 2,
            unwritten: vec![9, 9],
        };
        let err = resolve_send::<u32>(Ok(7), Err(write_err)).unwrap_err();
        assert_eq!(err.kind(), &ErrorKind::Body);
    }

    /// `BodyWriteFailed` exists to wrap the whole
    /// `wasip3::http_compat::Error` rather than its `Display` — the
    /// reason (which end closed, how far the write got) is only in the
    /// value. `StreamReaderClosed` carries `written`/`unwritten`, so a
    /// truncated chain here loses a byte count no message reproduces.
    #[test]
    fn a_failed_body_write_keeps_the_hosts_own_error_reachable() {
        let write_err = wasip3::http_compat::Error::StreamReaderClosed {
            written: 2,
            unwritten: vec![9, 9],
        };
        let err = resolve_send::<u32>(Ok(7), Err(write_err)).unwrap_err();
        let failed = std::error::Error::source(&err)
            .expect("Error::new always has a source")
            .downcast_ref::<BodyWriteFailed>()
            .expect("the source of a body-write failure is BodyWriteFailed");
        assert_eq!(
            failed.to_string(),
            "failed to write request body: stream reader closed"
        );
        let host = std::error::Error::source(failed)
            .expect("BodyWriteFailed must keep the host's own error as its source");
        assert!(
            matches!(
                host.downcast_ref::<wasip3::http_compat::Error>(),
                Some(wasip3::http_compat::Error::StreamReaderClosed {
                    written: 2,
                    unwritten
                }) if unwritten == &[9, 9]
            ),
            "the write's progress must survive whole: {host}"
        );
    }

    #[test]
    fn resolve_send_send_error_wins_over_a_body_write_error_too() {
        // Both failed: the `send` error still takes priority (see the
        // `resolve_send` doc comment — a write failure here is almost
        // always a symptom of the same disconnect that took down `send`).
        let write_err = wasip3::http_compat::Error::StreamReaderClosed {
            written: 0,
            unwritten: vec![],
        };
        let err =
            resolve_send::<u32>(Err(ErrorCode::ConnectionTerminated), Err(write_err)).unwrap_err();
        assert!(err.is_connect(), "{err:?}");
    }

    /// Polls the future by hand a bounded number of times instead of an
    /// unbounded loop — if `race_send_with_body` really did wait on a
    /// future that "never completes" (the bug this test is meant to
    /// catch), an unbounded loop would hang forever instead of the test
    /// failing honestly.
    fn poll_bounded<F: Future>(mut fut: Pin<&mut F>, max_polls: usize) -> Option<F::Output> {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        for _ in 0..max_polls {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return Some(v);
            }
        }
        None
    }

    /// Review resolution, finding B-5, proved deterministically rather
    /// than by wall-clock time: `write_fut` is `std::future::pending()`,
    /// i.e. it literally never completes on its own. If
    /// `race_send_with_body` waited for both arms via `join!` (the old
    /// behavior), the result would never arrive — `poll_bounded` would
    /// return `None` within the allotted attempts, and the `.expect(..)`
    /// below would fail the test. The fact that it returns is direct
    /// proof of the short-circuit on an early `send` error, not a
    /// measurement of timing on a live host that could vary between runs.
    #[test]
    fn race_send_with_body_short_circuits_on_early_send_failure() {
        let send_fut = std::future::ready(Err::<u32, ErrorCode>(ErrorCode::ConnectionRefused));
        let write_fut = std::future::pending::<Result<u64, wasip3::http_compat::Error>>();
        let mut fut = std::pin::pin!(race_send_with_body(send_fut, write_fut));
        let got = poll_bounded(fut.as_mut(), 64).expect(
            "race_send_with_body must resolve promptly on an early send failure, \
             not hang waiting for a body write that never finishes",
        );
        assert!(got.unwrap_err().is_connect());
    }

    /// Symmetry: when `send` finishes first and succeeds,
    /// `race_send_with_body` must still wait for the write, not return
    /// success prematurely — the short-circuit only applies to a `send`
    /// failure, see the doc comment.
    #[test]
    fn race_send_with_body_still_waits_for_the_body_when_send_succeeds() {
        let send_fut = std::future::ready(Ok::<u32, ErrorCode>(7));
        let write_fut = std::future::ready(Ok::<u64, wasip3::http_compat::Error>(3));
        let mut fut = std::pin::pin!(race_send_with_body(send_fut, write_fut));
        let got = poll_bounded(fut.as_mut(), 64).expect("must resolve when both are ready");
        assert_eq!(got.unwrap(), 7);
    }

    /// A controllable future for tests: `Pending` for the first `delay`
    /// polls, then `Ready(value)` — needed to deterministically drive the
    /// race into the `Either::Right` branch (the body write resolves
    /// before `send`), rather than relying on `futures::future::select`
    /// polling its first argument first (with two `std::future::ready(..)`
    /// it would always land in `Either::Left`, never actually checking
    /// the second branch).
    struct DelayedReady<T> {
        delay: usize,
        value: Option<T>,
    }
    impl<T: Unpin> Future for DelayedReady<T> {
        type Output = T;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
            if self.delay > 0 {
                self.delay -= 1;
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(self.value.take().expect("polled again after ready"))
            }
        }
    }

    /// Symmetry: when the body write finishes first (successfully), the
    /// race still has to wait for `send` — it isn't guaranteed to succeed
    /// too. Checks the `Either::Right` branch, which the previous two
    /// tests don't reach (there `send` is always ready on the first
    /// poll).
    #[test]
    fn race_send_with_body_waits_for_send_when_the_write_finishes_first() {
        let send_fut = DelayedReady {
            delay: 5,
            value: Some(Err::<u32, ErrorCode>(ErrorCode::ConnectionRefused)),
        };
        let write_fut = std::future::ready(Ok::<u64, wasip3::http_compat::Error>(3));
        let mut fut = std::pin::pin!(race_send_with_body(send_fut, write_fut));
        let got = poll_bounded(fut.as_mut(), 64).expect("must resolve");
        assert!(got.unwrap_err().is_connect());
    }

    #[test]
    fn timeout_rejection_names_the_capability_and_keeps_the_host_reason_as_source() {
        let e = unsupported_timeout("connect_timeout", RequestOptionsError::NotSupported);
        assert!(e.is_unsupported());
        let msg = e.to_string();
        assert!(msg.contains("connect_timeout"), "{msg}");
        assert!(msg.contains("wasi:http"), "{msg}");
        // Level 1: `Error::source()` is `TimeoutRejected` itself (what was
        // passed to `Error::new`). Level 2: its own `source()` is the
        // host's real `RequestOptionsError`, the whole reason this type
        // exists (see the `TimeoutRejected` doc comment).
        let level1 = std::error::Error::source(&e).expect("must preserve the host's reason");
        let rejected = level1
            .downcast_ref::<TimeoutRejected>()
            .expect("top-level source is TimeoutRejected");
        let level2 = std::error::Error::source(rejected)
            .expect("TimeoutRejected must store the host's reason as its own source");
        assert!(
            matches!(
                level2.downcast_ref::<RequestOptionsError>(),
                Some(RequestOptionsError::NotSupported),
            ),
            "not just SOME RequestOptionsError — the one the host actually sent, unaltered"
        );
        assert_eq!(
            rejected.to_string(),
            "backend `wasi:http` does not support `connect_timeout`: \
             RequestOptionsError::NotSupported",
            "the message reports both WHAT was rejected and WHY"
        );
    }

    #[test]
    fn setter_rejection_names_the_field_wasi_refused() {
        let e = rejected("scheme");
        assert!(e.is_unsupported());
        assert!(e.to_string().contains("scheme"));
    }

    /// The counterpart of the test above, and the reason these are two
    /// types rather than one: `set_method`/`set_scheme`/`set_authority`/
    /// `set_path_with_query` return a bare `Result<(), ()>`, so the host
    /// sends back no reason at all and there is nothing to put in
    /// `source()`. Pinned so the chain cannot later be given a fabricated
    /// link, and so the field name stays in the message — the only place
    /// it is reported.
    #[test]
    fn a_bare_setter_rejection_has_no_reason_to_expose_as_a_source() {
        let e = rejected("authority");
        let r = std::error::Error::source(&e)
            .expect("Error::new always has a source")
            .downcast_ref::<Rejected>()
            .expect("the source of a setter rejection is Rejected");
        assert_eq!(r.to_string(), "wasi:http host rejected setting `authority`");
        assert!(
            std::error::Error::source(r).is_none(),
            "the host sent no reason — the chain must end here rather than invent one"
        );
    }

    /// `HeaderError::Forbidden`/`::Immutable`
    /// — the host refused — the same class as `Rejected` — must be
    /// `Unsupported`, not `Other`.
    #[test]
    fn fields_error_classifies_host_refusals_as_unsupported() {
        let forbidden = fields_error(HeaderError::Forbidden);
        assert!(forbidden.is_unsupported(), "{forbidden:?}");
        let immutable = fields_error(HeaderError::Immutable);
        assert!(immutable.is_unsupported(), "{immutable:?}");
    }

    #[test]
    fn fields_error_leaves_genuine_input_defects_as_other() {
        let bad = fields_error(HeaderError::InvalidSyntax);
        assert!(!bad.is_unsupported(), "{bad:?}");
        assert_eq!(bad.kind(), &ErrorKind::Other);
    }

    /// `fields_error` classifies by variant, so the variant
    /// itself is information the caller may want back — and the chain is
    /// the only way to get it: the two tests above read `kind()`, which
    /// collapses five `HeaderError` variants into two categories.
    /// `SizeExceeded` on purpose: it is one of the variants that shares
    /// `ErrorKind::Other` with two others, so `kind()` alone cannot tell
    /// them apart.
    #[test]
    fn fields_error_keeps_the_hosts_header_error_reachable_below_the_category() {
        let e = fields_error(HeaderError::SizeExceeded);
        let fields = std::error::Error::source(&e)
            .expect("Error::new always has a source")
            .downcast_ref::<FieldsError>()
            .expect("the source of a header rejection is FieldsError");
        assert_eq!(
            fields.to_string(),
            "invalid headers: HeaderError::SizeExceeded"
        );
        let host = std::error::Error::source(fields)
            .expect("FieldsError must keep the host's HeaderError as its own source");
        assert!(
            matches!(
                host.downcast_ref::<HeaderError>(),
                Some(HeaderError::SizeExceeded)
            ),
            "the variant must survive whole, not as a substring of the message"
        );
    }

    /// `ErrorCode` variants that clearly
    /// belong to the same "body or trailer size/presence" family that
    /// already gives `Body` for the response — folded into the same
    /// category on the request side.
    #[test]
    fn wasi_err_categorizes_request_and_trailer_size_errors_as_body() {
        for code in [
            ErrorCode::HttpRequestLengthRequired,
            ErrorCode::HttpRequestBodySize(Some(1)),
            ErrorCode::HttpRequestTrailerSectionSize(Some(1)),
            ErrorCode::HttpResponseTrailerSize(wasip3::http::types::FieldSizePayload {
                field_name: None,
                field_size: None,
            }),
        ] {
            let e = wasi_err(code);
            assert_eq!(e.kind(), &ErrorKind::Body, "{e:?}");
        }
    }

    #[test]
    fn wasi_err_leaves_genuinely_uncategorized_codes_as_other() {
        // `HttpProtocolError` doesn't obviously belong to any existing
        // category — stays an honest `Other` rather than being forced
        // into an ill-fitting bucket.
        let e = wasi_err(ErrorCode::HttpProtocolError);
        assert_eq!(e.kind(), &ErrorKind::Other);
    }

    /// The whole classification, one row per `ErrorCode` variant.
    ///
    /// The three tests above sample it; this one pins it. B2 of the
    /// branch's final review was a layer discarding exactly this
    /// classification, and the two `_ => `-style fallbacks it is built on
    /// (`wasi_err`'s own `_ => Other`, and `to_error` being the identity)
    /// mean a variant can change category — or slide into `Other` — with
    /// nothing failing to compile. Nine categories over 39 variants; the
    /// count is asserted below so a row deleted by accident does not
    /// simply shrink the table.
    ///
    /// Variant list checked against
    /// `wasip3-0.7.0+wasi-0.3.0/src/service.rs:161-206`, the same source
    /// `wasi_err`'s own doc comment cites.
    #[test]
    fn wasi_err_gives_every_error_code_variant_the_category_it_is_documented_to_have() {
        use hclient_core::Phase;
        use wasip3::http::types::{DnsErrorPayload, FieldSizePayload, TlsAlertReceivedPayload};

        fn field_size() -> FieldSizePayload {
            FieldSizePayload {
                field_name: None,
                field_size: None,
            }
        }

        let table = [
            (ErrorCode::DnsTimeout, ErrorKind::Resolve),
            (
                ErrorCode::DnsError(DnsErrorPayload {
                    rcode: None,
                    info_code: None,
                }),
                ErrorKind::Resolve,
            ),
            (ErrorCode::DestinationNotFound, ErrorKind::Connect),
            (ErrorCode::DestinationUnavailable, ErrorKind::Connect),
            (ErrorCode::DestinationIpProhibited, ErrorKind::Connect),
            (ErrorCode::DestinationIpUnroutable, ErrorKind::Connect),
            (ErrorCode::ConnectionRefused, ErrorKind::Connect),
            (ErrorCode::ConnectionTerminated, ErrorKind::Connect),
            (ErrorCode::ConnectionLimitReached, ErrorKind::Connect),
            (
                ErrorCode::ConnectionTimeout,
                ErrorKind::Timeout(Phase::Connect),
            ),
            (
                ErrorCode::ConnectionReadTimeout,
                ErrorKind::Timeout(Phase::FirstByte),
            ),
            (
                ErrorCode::HttpResponseTimeout,
                ErrorKind::Timeout(Phase::FirstByte),
            ),
            (
                ErrorCode::ConnectionWriteTimeout,
                ErrorKind::Timeout(Phase::BetweenBytes),
            ),
            (ErrorCode::TlsProtocolError, ErrorKind::Tls),
            (ErrorCode::TlsCertificateError, ErrorKind::Tls),
            (
                ErrorCode::TlsAlertReceived(TlsAlertReceivedPayload {
                    alert_id: None,
                    alert_message: None,
                }),
                ErrorKind::Tls,
            ),
            (ErrorCode::HttpRequestDenied, ErrorKind::Status),
            (ErrorCode::LoopDetected, ErrorKind::Redirect),
            (ErrorCode::HttpUpgradeFailed, ErrorKind::Unsupported),
            (ErrorCode::ConfigurationError, ErrorKind::Unsupported),
            (ErrorCode::HttpResponseIncomplete, ErrorKind::Body),
            (ErrorCode::HttpResponseBodySize(Some(1)), ErrorKind::Body),
            (ErrorCode::HttpResponseTransferCoding(None), ErrorKind::Body),
            (ErrorCode::HttpResponseContentCoding(None), ErrorKind::Body),
            (ErrorCode::HttpRequestLengthRequired, ErrorKind::Body),
            (ErrorCode::HttpRequestBodySize(Some(1)), ErrorKind::Body),
            (
                ErrorCode::HttpRequestTrailerSectionSize(Some(1)),
                ErrorKind::Body,
            ),
            (
                ErrorCode::HttpRequestTrailerSize(field_size()),
                ErrorKind::Body,
            ),
            (
                ErrorCode::HttpResponseTrailerSectionSize(Some(1)),
                ErrorKind::Body,
            ),
            (
                ErrorCode::HttpResponseTrailerSize(field_size()),
                ErrorKind::Body,
            ),
            // Honest `Other`: no existing category fits, and finding B-8
            // deliberately stopped short of inventing one. A row moving
            // out of this block is a real decision, and has to be made
            // here as well as in `wasi_err`.
            (ErrorCode::HttpRequestMethodInvalid, ErrorKind::Other),
            (ErrorCode::HttpRequestUriInvalid, ErrorKind::Other),
            (ErrorCode::HttpRequestUriTooLong, ErrorKind::Other),
            (
                ErrorCode::HttpRequestHeaderSectionSize(Some(1)),
                ErrorKind::Other,
            ),
            (
                ErrorCode::HttpRequestHeaderSize(Some(field_size())),
                ErrorKind::Other,
            ),
            (
                ErrorCode::HttpResponseHeaderSectionSize(Some(1)),
                ErrorKind::Other,
            ),
            (
                ErrorCode::HttpResponseHeaderSize(field_size()),
                ErrorKind::Other,
            ),
            (ErrorCode::HttpProtocolError, ErrorKind::Other),
            (ErrorCode::InternalError(None), ErrorKind::Other),
        ];

        assert_eq!(
            table.len(),
            39,
            "`wasi:http` 0.3 has 39 error codes — a shorter table means a variant \
             stopped being checked, not that the protocol shrank"
        );
        for (code, expected) in table {
            let described = format!("{code:?}");
            assert_eq!(
                wasi_err(code).kind(),
                &expected,
                "{described} must stay {expected:?}"
            );
        }
    }

    #[test]
    fn resolve_payload_treats_empty_and_absent_bodies_alike() {
        assert!(resolve_payload(RequestBody::Empty).unwrap().is_none());
        assert!(
            resolve_payload(RequestBody::Full(Bytes::new()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn resolve_payload_keeps_a_non_empty_full_body() {
        match resolve_payload(RequestBody::Full(Bytes::from_static(b"abc"))).unwrap() {
            Some(Payload::Bytes(b)) => assert_eq!(&b[..], b"abc"),
            _ => panic!("expected Payload::Bytes"),
        }
    }

    /// Team lead resolution, item 2: `Rewindable` must not collapse into
    /// `None` — the factory has to be called, not ignored.
    #[test]
    fn resolve_payload_calls_the_rewindable_factory_instead_of_dropping_it() {
        let body = RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"replayed")));
        match resolve_payload(body).unwrap() {
            Some(Payload::Bytes(b)) => assert_eq!(&b[..], b"replayed"),
            _ => panic!("expected Payload::Bytes"),
        }
    }

    /// Team lead resolution, item 2: `Streaming` must not collapse into
    /// `None` either — otherwise `Capabilities::streaming_request_body =
    /// true` would be a lie.
    #[test]
    fn resolve_payload_keeps_a_streaming_body_instead_of_dropping_it() {
        struct OneShot(Option<Bytes>);
        impl http_body::Body for OneShot {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
                std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
            }
        }
        let body = RequestBody::Streaming(Box::new(OneShot(Some(Bytes::from_static(b"s")))));
        assert!(matches!(
            resolve_payload(body).unwrap(),
            Some(Payload::Streaming(_))
        ));
    }

    /// the old recursive implementation
    /// would have unwrapped a factory like this until the stack
    /// overflowed. `infinite` is a function item (not a closure), so it's
    /// trivially `Fn + Send + Sync + 'static` without manual bounds — and
    /// each call returns YET ANOTHER `Rewindable` referring to itself,
    /// i.e. this chain has no legitimate end at all.
    #[test]
    fn resolve_payload_stops_at_a_bounded_depth_instead_of_recursing_forever() {
        fn infinite() -> RequestBody {
            RequestBody::rewindable(infinite)
        }
        let err = resolve_payload(RequestBody::rewindable(infinite)).unwrap_err();
        assert_eq!(err.kind(), &ErrorKind::Other);
    }

    /// `ErrorKind::Other` above is shared with several unrelated
    /// failures, so the message is what actually tells the caller a
    /// bound was hit and which one — it has to carry the number, not
    /// just the word "deep".
    #[test]
    fn the_rewind_depth_error_names_the_bound_it_stopped_at() {
        fn infinite() -> RequestBody {
            RequestBody::rewindable(infinite)
        }
        let err = resolve_payload(RequestBody::rewindable(infinite)).unwrap_err();
        let too_deep = std::error::Error::source(&err)
            .expect("Error::new always has a source")
            .downcast_ref::<RewindTooDeep>()
            .expect("the source of a nesting-bound failure is RewindTooDeep");
        let msg = too_deep.to_string();
        assert!(
            msg.contains(&MAX_REWIND_DEPTH.to_string()),
            "the bound that was hit must appear in the message: {msg}"
        );
        assert!(msg.contains("Rewindable"), "{msg}");
    }

    struct DataThenTrailers {
        data: Option<Bytes>,
        trailers: Option<http::HeaderMap>,
    }
    impl HttpBody for DataThenTrailers {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
            if let Some(d) = self.data.take() {
                return Poll::Ready(Some(Ok(Frame::data(d))));
            }
            if let Some(t) = self.trailers.take() {
                return Poll::Ready(Some(Ok(Frame::trailers(t))));
            }
            Poll::Ready(None)
        }
    }

    fn poll_once<B: HttpBody<Data = Bytes, Error = Error> + Unpin>(
        b: &mut B,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        Pin::new(b).poll_frame(&mut cx)
    }

    /// the body emits a trailers frame —
    /// `TrailerWatch` must notice it (by field name) without touching the
    /// frames themselves.
    #[test]
    fn trailer_watch_records_the_field_name_without_altering_the_frame() {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("x-checksum", "deadbeef".parse().unwrap());
        let body = DataThenTrailers {
            data: Some(Bytes::from_static(b"x")),
            trailers: Some(trailers),
        };
        let (mut watched, seen) = TrailerWatch::new(body);
        assert!(seen.lock().unwrap().is_empty());

        // Data frame: the list should still be empty.
        match poll_once(&mut watched) {
            Poll::Ready(Some(Ok(f))) => assert!(f.is_data()),
            other => panic!("expected a data frame, got {other:?}"),
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "data frame is not trailers"
        );

        // Trailers frame: the field name should show up, and the frame
        // itself should reach the caller untouched.
        match poll_once(&mut watched) {
            Poll::Ready(Some(Ok(f))) => {
                assert!(f.is_trailers());
                assert_eq!(
                    f.trailers_ref().unwrap().get("x-checksum").unwrap(),
                    "deadbeef"
                );
            }
            other => panic!("expected a trailers frame, got {other:?}"),
        }
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[http::header::HeaderName::from_static("x-checksum")]
        );
    }

    /// An empty trailers frame
    /// (`Frame::trailers(HeaderMap::new())`) loses nothing on the wire —
    /// `TrailerWatch` must not register it as "there were trailers".
    /// Before the fix, this is exactly what flagged a request that
    /// actually lost nothing: a regression this test now prevents.
    #[test]
    fn trailer_watch_ignores_an_empty_trailers_frame() {
        let body = DataThenTrailers {
            data: Some(Bytes::from_static(b"x")),
            trailers: Some(http::HeaderMap::new()),
        };
        let (mut watched, seen) = TrailerWatch::new(body);
        let _ = poll_once(&mut watched); // data frame
        match poll_once(&mut watched) {
            Poll::Ready(Some(Ok(f))) => assert!(f.is_trailers()),
            other => panic!("expected a (empty) trailers frame, got {other:?}"),
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "an empty trailers frame loses nothing on the wire and must not be flagged"
        );
    }

    #[test]
    fn declared_trailer_names_parses_a_comma_separated_list() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::TRAILER,
            "X-Checksum, X-Other".parse().unwrap(),
        );
        let declared = declared_trailer_names(&headers);
        assert!(declared.contains(&http::HeaderName::from_static("x-checksum")));
        assert!(declared.contains(&http::HeaderName::from_static("x-other")));
        assert_eq!(declared.len(), 2);
    }

    #[test]
    fn declared_trailer_names_merges_repeated_headers() {
        let mut headers = http::HeaderMap::new();
        headers.append(http::header::TRAILER, "X-Checksum".parse().unwrap());
        headers.append(http::header::TRAILER, "X-Other".parse().unwrap());
        let declared = declared_trailer_names(&headers);
        assert_eq!(declared.len(), 2);
    }

    #[test]
    fn declared_trailer_names_is_empty_without_a_trailer_header() {
        let headers = http::HeaderMap::new();
        assert!(declared_trailer_names(&headers).is_empty());
    }

    /// the message must name
    /// the specific field — "generic refusal" isn't enough — and
    /// explicitly warn about the error's after-the-fact nature (finding
    /// 4).
    #[test]
    fn undeclared_trailers_error_names_the_field_and_warns_about_retrying() {
        let e = undeclared_trailers(vec![http::HeaderName::from_static("x-checksum")]);
        assert_eq!(e.kind(), &ErrorKind::Body);
        let msg = e.to_string();
        assert!(msg.contains("x-checksum"), "{msg}");
        assert!(msg.contains("Trailer"), "{msg}");
        assert!(
            msg.to_lowercase().contains("retry"),
            "must warn against blind retries: {msg}"
        );
    }

    /// Two fields, not one: the joining is part of the message, and with a
    /// single name (as in the test above) a broken separator — or a
    /// message that only ever reports the first field — is invisible.
    #[test]
    fn undeclared_trailers_lists_every_field_it_saw_not_just_the_first() {
        let e = undeclared_trailers(vec![
            http::HeaderName::from_static("x-checksum"),
            http::HeaderName::from_static("x-signature"),
        ]);
        let msg = e.to_string();
        assert!(
            msg.contains("[x-checksum, x-signature]"),
            "every undeclared field, comma-separated: {msg}"
        );
    }
}
