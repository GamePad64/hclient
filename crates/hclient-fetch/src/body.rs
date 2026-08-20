//! Response body over `ReadableStream`.
//!
//! **Trailers aren't supported.** Fetch has none in either direction
//! (whatwg/fetch#772 proposes removing the trailers API altogether).
//! `Capabilities` declares this (Task 2), so `poll_frame` never produces
//! `Frame::trailers` — every frame this type emits is `Frame::data`.
//!
//! # Two failure modes, two `ErrorKind`s — a defect in this task's own brief
//!
//! The brief's reference implementation maps EVERY failure out of the
//! underlying `ReadableStream` — a rejected `read()` (the browser giving up
//! on the exchange) and a chunk that isn't the `Uint8Array` a body stream is
//! specified to always yield (this crate's own defensive check, since
//! nothing else here can produce that shape) — through one call to
//! `convert::js_err`, which always returns `ErrorKind::Other`. That throws
//! away exactly the distinction this vertical's own dispatch calls out
//! ("a stream failure is not the same as a decode failure; say which is
//! which and test it") and that five tasks across two verticals were fixed
//! to preserve (`ErrorKind` surviving `hyper::Error`, `wasi:http`'s
//! `ErrorCode`, and so on — see `hclient-native/src/body.rs` and
//! `hclient-wasi/src/body.rs` for the established precedent). This file
//! does not repeat that: [`StreamRead`] (a rejected `read()`, the transport
//! itself failing) is `ErrorKind::Body`; [`NotAByteChunk`] (the read
//! SUCCEEDED, but what it handed back isn't bytes) is `ErrorKind::Decode` —
//! the same category `hclient`'s `Response::text`/`Response::json` already
//! use for "the bytes don't parse as promised". `tests/body.rs` proves both
//! independently and proves they're distinct, not merely present.
//!
//! Neither typed cause reuses `convert::js_err`/`convert::JsError`: this
//! file was written while another task was actively editing `convert.rs` in
//! the same working tree (see the task report), so rather than extract a
//! shared helper out of a file mid-edit by someone else, the small
//! JS-value-to-`String` extraction below is duplicated locally. It is
//! intentionally the same three-step fallback (`as_string` → `.message` →
//! `Debug`) `js_err` already uses, not a new policy.
//!
//! # `size_hint`: honest, not merely present
//!
//! A body that keeps promising its original `Content-Length` after the
//! stream has already ended is exactly the defect vertical 1's `wasi` body
//! shipped and vertical 1's review caught (see
//! `hclient-wasi/src/body.rs::size_hint_honoring_end`). This file applies
//! the same discipline, plus one trap specific to `fetch` that the `wasi`
//! precedent never had to consider: **`Content-Length`, when present,
//! describes the bytes the SERVER put on the wire — under a
//! `Content-Encoding` the browser transparently reverses before this
//! stream ever sees a byte, that is the COMPRESSED size, not the decoded
//! byte count this stream actually yields.** Reporting it as `exact` in
//! that case would be precisely the "capability that lies" this project
//! forbids. [`content_length_hint`] therefore only trusts `Content-Length`
//! when `Content-Encoding` is absent or `identity`; otherwise it makes no
//! promise at all (`SizeHint::default()`, not a guess) — and either way,
//! [`Body::size_hint`] forces an honest, exact zero once [`Inner`] has
//! moved to [`Inner::Done`], on a natural end AND on an error alike (an
//! errored body has no more bytes coming either).
//!
//! # Why `is_end_stream` needs no help from the underlying stream
//!
//! `hclient-wasi`'s `Body::is_end_stream` has to delegate to
//! `IncomingResponseBody::is_end_stream()` because the WASI host can know
//! the stream is finished (e.g. after a trailers frame) one `poll_frame`
//! BEFORE this crate's own state catches up. `wasm-streams` 0.5.0's
//! `IntoStream::poll_next` (`src/readable/into_stream.rs`, verified against
//! that exact source while writing this) has no such lag: it sets
//! `self.reader = None` — its own "finished" signal, read by its
//! `FusedStream::is_terminated` — in the SAME branch, of the SAME
//! `poll_next` call, that returns `Poll::Ready(None)` (a clean end) or
//! `Poll::Ready(Some(Err(_)))` (an error). Both happen synchronously,
//! before `poll_next` returns control to us — so the moment we observe
//! either of those results and transition `self.inner` to [`Inner::Done`],
//! our own state and `wasm-streams`'s internal one are ALREADY in
//! agreement; there is no window in which delegating to it would say
//! anything our own `matches!(self.inner, Inner::Done)` doesn't already
//! say. Plain state tracking is therefore not a simplification that loses
//! precision here — it's the same answer, arrived at without reaching back
//! into a library internal that doesn't expose `is_terminated` through the
//! `Stream` adapter's own public surface in the first place.
//!
//! # Cancellation: dropping a `Body` mid-stream
//!
//! `ReadableStream::into_stream()` (used by [`Body::from_response`], not
//! `get_reader().into_stream()`) sets `cancel_on_drop = true`
//! (`wasm-streams` 0.5.0, `ReadableStream::try_into_stream`). Its `Drop`
//! impl calls the reader's `cancel()` and swallows any rejection with a
//! `Closure::once` `.catch()` — so dropping a `Body` before it's exhausted
//! (a caller losing interest early, e.g. an SSE consumer that stops
//! reading) releases the underlying `ReadableStream`'s lock and signals
//! cancellation to it, without this file writing a single line of that
//! logic itself and without ever panicking or leaving an unhandled
//! rejection in the console. `tests/body.rs`'s
//! `dropping_a_pending_body_cancels_the_underlying_reader` proves this
//! reaches the actual JS-level `cancel()` callback, not just that
//! `wasm-streams` claims to call it — guarding against a future refactor
//! (e.g. switching to `get_reader().into_stream()`, which sets
//! `cancel_on_drop = false`) silently turning this off.
use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use hclient_core::{DecompressionSupport, Error, ErrorKind};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};
use wasm_bindgen::{JsCast, JsValue};

// ---------------------------------------------------------------------
// Typed failure causes — see the module doc comment for why there are two,
// not one, and which `ErrorKind` each carries.
// ---------------------------------------------------------------------

/// The underlying `ReadableStream` rejected a `read()` — the browser's own
/// signal that the exchange didn't finish cleanly (a network failure, the
/// connection closing before the promised length, an explicit upstream
/// `controller.error()`). A transport failure, not a decode problem:
/// `ErrorKind::Body`.
#[derive(Debug, thiserror::Error)]
#[error("reading the response body stream failed: {0}")]
struct StreamRead(String);

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
struct NotAByteChunk;

/// Best-effort human-readable text for a `JsValue` a promise rejected with.
/// Deliberately NOT imported from `convert.rs` — see the module doc comment
/// for why this is a local, three-line duplicate rather than a shared
/// helper extracted from a file this task did not otherwise touch.
fn js_message(v: &JsValue) -> String {
    v.as_string()
        .or_else(|| {
            js_sys::Reflect::get(v, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{v:?}"))
}

/// A response body over `ReadableStream`.
pub struct Body {
    inner: Inner,
}

enum Inner {
    Stream {
        stream: Pin<Box<dyn Stream<Item = Result<Bytes, Error>>>>,
        /// Computed once, in [`Body::from_response`] — see
        /// [`content_length_hint`] and the module doc comment's `size_hint`
        /// section. Read as-is until the stream ends; from then on
        /// [`Body::size_hint`] overrides it with an exact zero rather than
        /// consulting this stale value.
        hint: SizeHint,
    },
    Done,
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            Inner::Stream { .. } => f.write_str("Body(stream)"),
            Inner::Done => f.write_str("Body(done)"),
        }
    }
}

/// What this transport does with a response `Content-Encoding` — declared
/// once, here, and read by the two places in this crate that depend on it.
///
/// The browser reverses a content coding before the `ReadableStream` this
/// module wraps yields a single byte; the JS side never sees the encoded
/// form and has no way to ask for it. That one fact drives two different
/// behaviours, and they must not be able to drift apart:
///
/// - [`content_length_hint`] below refuses to trust `Content-Length` under
///   a `Content-Encoding`, because that number describes the wire and this
///   stream yields the decoded bytes. If the browser did NOT decode,
///   `Content-Length` would be exactly right and distrusting it would be
///   the lie.
/// - `caps::probe` reports it as
///   [`Capabilities::response_decompression`](hclient_core::Capabilities::response_decompression),
///   which is what stops `hclient`'s `Client` decoding a second time and
///   corrupting every compressed response.
///
/// Hence a constant read by both rather than a literal at each site — the
/// same recipe `hclient-native`'s `reuse_of` uses for
/// [`ReuseSupport`](hclient_core::ReuseSupport), so that "what this
/// transport does" and "what it declares" are one fact read twice. Flipping
/// this to `None` makes `tests/body.rs`'s
/// `size_hint_does_not_trust_content_length_under_content_encoding` fail,
/// which is what "read twice" is worth.
pub(crate) const RESPONSE_DECOMPRESSION: DecompressionSupport = DecompressionSupport::Internal;

/// The `size_hint` this body can honestly offer BEFORE any byte has been
/// read — see the module doc comment's `size_hint` section for why
/// `Content-Encoding` gates trusting `Content-Length` at all.
fn content_length_hint(resp: &web_sys::Response) -> SizeHint {
    let headers = resp.headers();
    // Not `true` unconditionally: `Content-Length` is only untrustworthy
    // here BECAUSE the browser already reversed the coding — see
    // [`RESPONSE_DECOMPRESSION`], the single declaration of that fact.
    let already_decoded = matches!(RESPONSE_DECOMPRESSION, DecompressionSupport::Internal);
    let trustworthy = match headers.get("content-encoding") {
        Ok(Some(v)) => {
            let v = v.trim();
            v.is_empty() || v.eq_ignore_ascii_case("identity") || !already_decoded
        }
        Ok(None) => true,
        // `Headers.get` on a plain lowercase ASCII name has no realistic
        // failure mode — but if it ever did, "no promise" is the honest
        // answer, not a guess in either direction.
        Err(_) => false,
    };
    if !trustworthy {
        return SizeHint::default();
    }
    match headers.get("content-length") {
        Ok(Some(v)) => v
            .trim()
            .parse::<u64>()
            .map(SizeHint::with_exact)
            .unwrap_or_default(),
        _ => SizeHint::default(),
    }
}

impl Body {
    /// A body with nothing in it: `is_end_stream()` is `true` from the
    /// start, `size_hint()` is an exact zero.
    pub fn empty() -> Self {
        Self { inner: Inner::Done }
    }

    /// Builds a `Body` from a `web_sys::Response`'s own `.body()`.
    /// `pub(crate)`, not `pub`: the only construction path a caller outside
    /// this crate needs is through the eventual `Transport` impl — the same
    /// pattern `convert::to_web_request` already follows, exposed to tests
    /// only via `testing::body_from_response`.
    pub(crate) fn from_response(resp: &web_sys::Response) -> Result<Self, Error> {
        let Some(raw) = resp.body() else {
            return Ok(Self::empty());
        };
        let hint = content_length_hint(resp);
        // `raw` is already `web_sys::ReadableStream` — `wasm-streams`
        // 0.5.0's `sys::ReadableStream` is a plain `pub use
        // web_sys::ReadableStream`, the identical type, not a distinct
        // extension type reached through a cast (verified against that
        // source directly; see the Cargo.toml MSRV comment).
        let stream = wasm_streams::ReadableStream::from_raw(raw)
            .into_stream()
            .map(|chunk| match chunk {
                Ok(v) => v
                    .dyn_into::<js_sys::Uint8Array>()
                    .map(|a| Bytes::from(a.to_vec()))
                    .map_err(|_| Error::new(ErrorKind::Decode, NotAByteChunk)),
                Err(e) => Err(Error::new(ErrorKind::Body, StreamRead(js_message(&e)))),
            });
        Ok(Self {
            inner: Inner::Stream {
                stream: Box::pin(stream),
                hint,
            },
        })
    }
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        match &mut self.inner {
            Inner::Stream { stream, .. } => match stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(b))) => Poll::Ready(Some(Ok(Frame::data(b)))),
                // An error is terminal — the same "one shot, then Done" rule
                // a natural end gets below, not a state a caller could poll
                // past. See the module doc comment: this is exactly the
                // point where a lesser implementation could quietly stop
                // producing frames without an error; this one doesn't.
                Poll::Ready(Some(Err(e))) => {
                    self.inner = Inner::Done;
                    Poll::Ready(Some(Err(e)))
                }
                Poll::Ready(None) => {
                    self.inner = Inner::Done;
                    Poll::Ready(None)
                }
                Poll::Pending => Poll::Pending,
            },
            Inner::Done => Poll::Ready(None),
        }
    }

    /// See the module doc comment's `is_end_stream` section for why plain
    /// state tracking is provably not a lesser answer than delegating to
    /// the underlying stream here.
    fn is_end_stream(&self) -> bool {
        matches!(self.inner, Inner::Done)
    }

    /// See the module doc comment's `size_hint` section.
    fn size_hint(&self) -> SizeHint {
        match &self.inner {
            Inner::Done => SizeHint::with_exact(0),
            // `SizeHint: Copy` — `*hint`, not `.clone()` (clippy
            // `clone_on_copy`).
            Inner::Stream { hint, .. } => *hint,
        }
    }
}
