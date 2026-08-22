//! `wasi:http` response body.

use bytes::Bytes;
use hclient_core::{Error, ErrorKind};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};
use wasip3::http_compat::IncomingResponseBody;

/// `wasi:http` response body. Reads the stream inline, with no background
/// task — meaning the transport doesn't need the `spawn` capability.
pub struct Body {
    inner: Inner,
}

enum Inner {
    Incoming(IncomingResponseBody),
    /// Buffered outgoing bytes as a single frame: a `Bytes` ->
    /// `http_body::Body` adapter that `Transport::execute` needs to pass
    /// `convert::Payload::Bytes` into `BodyWriter::send_http_body` — which
    /// needs an actual `http_body::Body`, not raw bytes. Not part of the
    /// RESPONSE body's public API (see `from_bytes` — `pub(crate)`, not
    /// `pub`); it occupies a variant of this same `enum` rather than a
    /// separate type, so as not to introduce a second `http_body::Body`
    /// just for one frame. `None` inside means the frame has already been
    /// handed out.
    Buffered(Option<Bytes>),
    Done,
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner {
            Inner::Incoming(_) => f.write_str("Body(incoming)"),
            Inner::Buffered(_) => f.write_str("Body(buffered)"),
            Inner::Done => f.write_str("Body(done)"),
        }
    }
}

impl Body {
    pub(crate) fn from_incoming(i: IncomingResponseBody) -> Self {
        Self {
            inner: Inner::Incoming(i),
        }
    }

    /// `Bytes` -> `http_body::Body` adapter, exactly one data frame. Only
    /// for `Transport::execute` (see `Inner::Buffered`) — not meant for
    /// callers, hence `pub(crate)`.
    pub(crate) fn from_bytes(b: Bytes) -> Self {
        Self {
            inner: Inner::Buffered(Some(b)),
        }
    }

    /// Empty body: no frames at all, `is_end_stream()` is true from the
    /// start, `size_hint()` is an exact zero.
    pub fn empty() -> Self {
        Self { inner: Inner::Done }
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
            Inner::Incoming(i) => match Pin::new(i).poll_frame(cx) {
                Poll::Ready(Some(Ok(f))) => Poll::Ready(Some(Ok(f))),
                Poll::Ready(Some(Err(e))) => {
                    // `ErrorCode` goes into `Error::new` as-is, unwrapped.
                    // A wrapper would be pure machinery with no payoff:
                    // `ErrorCode` already implements `Debug`/`Display`/
                    // `core::error::Error` by hand (wasip3 `service.rs`) —
                    // and its own `Display` is itself just
                    // `write!(f, "{:?}", self)`, so a wrapper wouldn't have
                    // made the output any more informative. And since
                    // `Error::new` erases the source into `Arc<dyn Error +
                    // Send + Sync>` anyway, the concrete `ErrorCode` type
                    // doesn't show up in this crate's public API either
                    // with or without the wrapper; the only difference is
                    // that without the wrapper the caller can honestly
                    // downcast to the real type via `Error::source()`,
                    // while a wrapper would close off that option.
                    self.inner = Inner::Done;
                    Poll::Ready(Some(Err(Error::new(ErrorKind::Body, e))))
                }
                Poll::Ready(None) => {
                    self.inner = Inner::Done;
                    Poll::Ready(None)
                }
                Poll::Pending => Poll::Pending,
            },
            // Exactly one frame, then `None` forever — `Option::take`
            // already gives us that behavior without a separate transition
            // to `Inner::Done`.
            Inner::Buffered(slot) => Poll::Ready(slot.take().map(|b| Ok(Frame::data(b)))),
            Inner::Done => Poll::Ready(None),
        }
    }

    /// Delegates to the inner body instead of re-deriving its own state.
    /// `IncomingResponseBody` knows the stream has ended from its own state
    /// earlier than we do — only after WE ourselves have polled
    /// `poll_frame` once and seen `Ready(None)`. The weaker version
    /// (`matches!(self.inner, Inner::Done)`) is exactly the defect on the
    /// host side, `act` (`http_body_util::StreamBody` always returns
    /// `false`), that made guests trap mid-read on HTTP/2 responses;
    /// reproducing it here would be pointless — it's the entire motivation
    /// for this task.
    ///
    /// **Branch not covered by unit tests, guarded by an integration
    /// test.** `Inner::Incoming(i) => i.is_end_stream()` — the central
    /// line of the whole task — is not covered by the unit tests below:
    /// `IncomingResponseBody` has no constructor without a real
    /// `wasi:http` host (`wasip3::http::types::Response` is an opaque WIT
    /// resource). The review's mutation run confirmed the gap: replacing
    /// this branch with a hard `false` (the very `act` bug) doesn't fail a
    /// single test in the `#[cfg(test)] mod tests` below.
    ///
    /// `crates/hclient-wasi/tests/live_roundtrip.rs` +
    /// `examples/live_roundtrip_guest.rs` close it: a real request through
    /// `WasiHttp::execute` under `wasmtime` (`.cargo/config.toml`, `runner
    /// = "wasmtime run -S http --"`) against a mock server that responds
    /// `chunked` with a trailer — the trailer is exactly what opens the
    /// window where `i.is_end_stream()` is already `true` while
    /// `self.inner` on this object is still `Inner::Incoming` (see the
    /// module doc comment on `live_roundtrip_guest.rs` for why that
    /// window doesn't exist at all without a trailer — with a plain
    /// `Content-Length` this branch and a hardcoded `false` are
    /// indistinguishable even live). The same mutation run, applied to
    /// this test, is red.
    fn is_end_stream(&self) -> bool {
        match &self.inner {
            Inner::Done => true,
            Inner::Buffered(slot) => slot.is_none(),
            Inner::Incoming(i) => i.is_end_stream(),
        }
    }

    /// Forwards the host's estimate (`content-length`) instead of
    /// discarding it: `IncomingResponseBody` has already computed
    /// `size_hint()` from the response headers, nothing to recompute —
    /// except for one case: `i.size_hint()` itself never shrinks (it's
    /// computed once from `content-length` and doesn't change after
    /// that), while `poll_frame` keeps `self.inner` in `Incoming` for one
    /// more call AFTER `i.is_end_stream()` has already become true — a
    /// trailers frame (or the final `Ready(None)`) has been seen by the
    /// inner body, but our own transition to `Inner::Done` only happens
    /// on the next `poll_frame`. In that window, an uncorrected
    /// `i.size_hint()` would promise an upper bound of bytes that will
    /// never come — an over-estimate, not an under-estimate, and it's the
    /// over-estimate that hurts the caller (allocating for a promised
    /// remainder, waiting for bytes that won't arrive). See
    /// `size_hint_honoring_end` — the logic is pulled out into a pure
    /// function so it can be checked without a live
    /// `IncomingResponseBody`.
    fn size_hint(&self) -> SizeHint {
        match &self.inner {
            Inner::Done => SizeHint::with_exact(0),
            Inner::Buffered(Some(b)) => SizeHint::with_exact(b.len() as u64),
            Inner::Buffered(None) => SizeHint::with_exact(0),
            Inner::Incoming(i) => size_hint_honoring_end(i.is_end_stream(), i.size_hint()),
        }
    }
}

/// Doesn't let a stale host estimate outlive the stream's own end — see
/// the comment on `Body::size_hint`. Deliberately a pure function: the
/// actual scenario (`Inner::Incoming` with `is_end_stream() == true`) is
/// unreachable without a live `IncomingResponseBody`, while this logic is
/// reachable and needs to be checked on its own.
fn size_hint_honoring_end(is_end_stream: bool, upstream: SizeHint) -> SizeHint {
    if is_end_stream {
        SizeHint::with_exact(0)
    } else {
        upstream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Everything below is constructed without a `wasi:http` host —
    // `Body::empty()` and `Body::from_bytes()` never touch
    // `IncomingResponseBody`. The `Inner::Incoming` branches (the real
    // `poll_frame`, delegating `is_end_stream`/`size_hint` to a live body)
    // can't be checked here: `IncomingResponseBody` is only ever built
    // from `wasip3::http::types::Response`, an opaque WIT resource that
    // has nowhere to come from without a real `wasi:http` host.
    // `is_end_stream`, where this is especially sensitive, is flagged with
    // its own doc comment on the method, which is also where the
    // integration coverage under wasmtime is recorded. The exception is
    // `size_hint_honoring_end`:
    // its decision, whether to return a stale estimate or not, is pulled
    // out into a pure function precisely so it doesn't have to share the
    // fate of the rest of the `Inner::Incoming` branches — it's checked
    // below.

    #[test]
    fn empty_body_yields_no_frames() {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut b = std::pin::pin!(Body::empty());
        match b.as_mut().poll_frame(&mut cx) {
            Poll::Ready(None) => {}
            other => panic!("expected end of stream, got {other:?}"),
        }
    }

    #[test]
    fn empty_body_reports_end_of_stream_up_front() {
        assert!(
            Body::empty().is_end_stream(),
            "an empty body has nothing left to read from the very start"
        );
    }

    #[test]
    fn empty_body_has_an_exact_zero_size_hint() {
        let hint = Body::empty().size_hint();
        assert_eq!(hint.lower(), 0);
        assert_eq!(hint.upper(), Some(0));
    }

    /// `IncomingBody::size_hint()` is computed once from `content-length`
    /// and never shrinks on its own, while
    /// `Body::poll_frame` keeps `self.inner` in `Incoming` for one more
    /// call after the inner body has already reported `is_end_stream() ==
    /// true`. Without a correction, a stale upper bound would leak out in
    /// that window — a promise of bytes that will never come. We set a
    /// deliberately "rich" estimate (`4096`) so the test can't
    /// accidentally match the correct answer (`0`) under broken logic.
    #[test]
    fn size_hint_does_not_over_promise_once_the_stream_has_ended() {
        let mut stale = SizeHint::new();
        stale.set_upper(4096);

        let hint = size_hint_honoring_end(true, stale);

        assert_eq!(hint.lower(), 0);
        assert_eq!(hint.upper(), Some(0));
    }

    /// Symmetric to the test above: while the stream hasn't ended, the
    /// host's estimate needs to pass through as-is, not get zeroed too.
    #[test]
    fn size_hint_passes_through_the_upstream_estimate_mid_stream() {
        let mut mid = SizeHint::new();
        mid.set_upper(4096);

        let hint = size_hint_honoring_end(false, mid);

        assert_eq!(hint.lower(), mid.lower());
        assert_eq!(hint.upper(), mid.upper());
    }

    // `Inner::Buffered` — a `Bytes` -> `http_body::Body` adapter for
    // `Transport::execute` (`convert::Payload::Bytes` goes
    // into `BodyWriter::send_http_body`, which needs an `http_body::Body`,
    // not raw bytes). Doesn't need a host — tested right alongside
    // `empty()`.

    #[test]
    fn buffered_body_yields_exactly_one_data_frame_then_ends() {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut b = std::pin::pin!(Body::from_bytes(Bytes::from_static(b"abc")));

        match b.as_mut().poll_frame(&mut cx) {
            Poll::Ready(Some(Ok(f))) => {
                assert_eq!(f.into_data().ok().as_deref(), Some(&b"abc"[..]));
            }
            other => panic!("expected one data frame, got {other:?}"),
        }
        match b.as_mut().poll_frame(&mut cx) {
            Poll::Ready(None) => {}
            other => panic!("expected end of stream after the one frame, got {other:?}"),
        }
    }

    #[test]
    fn buffered_body_reports_end_of_stream_only_after_the_frame_is_taken() {
        let mut b = Body::from_bytes(Bytes::from_static(b"abc"));
        assert!(!b.is_end_stream(), "frame not yet handed out");

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let _ = std::pin::Pin::new(&mut b).poll_frame(&mut cx);

        assert!(
            b.is_end_stream(),
            "the one frame has already been handed out"
        );
    }

    #[test]
    fn buffered_body_size_hint_is_exact_and_shrinks_to_zero_after_the_frame() {
        let mut b = Body::from_bytes(Bytes::from_static(b"abcd"));
        let before = b.size_hint();
        assert_eq!(before.lower(), 4);
        assert_eq!(before.upper(), Some(4));

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let _ = std::pin::Pin::new(&mut b).poll_frame(&mut cx);

        let after = b.size_hint();
        assert_eq!(after.lower(), 0);
        assert_eq!(after.upper(), Some(0));
    }
}
