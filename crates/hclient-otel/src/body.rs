//! The response body that ends the span.
//!
//! **`docs/otel-design.md` §4's whole point.** A span ended at the head
//! reports the time to first byte and calls it
//! `http.client.request.duration`, which is wrong for every streaming
//! response — and a streaming response is most of what anybody instruments
//! a client to watch. So `Transport::execute` hands the recorder to the
//! body, and the exchange's duration is the exchange's.

use crate::span::Recorder;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};

/// A response body that closes its span when it ends — or when it is
/// dropped, whichever comes first.
///
/// **The pair is the design and neither half covers the other.** A body
/// that closed only on `poll_frame` returning `None` leaves a span open
/// for ever when a caller reads a header and walks away; a body that
/// closed only on `Drop` reports every duration as the caller's lifetime
/// rather than the exchange's, since a `Response` may sit in a variable
/// long after its last byte. Both fronts carry the pair —
/// `the_span_closes_at_the_end_of_the_body_and_not_at_the_head` and
/// `a_body_dropped_before_it_ends_still_closes_the_span`, in
/// `tests/otel_front.rs` and `tests/tracing_front.rs`.
///
/// **The first assertion is deliberately made while the body is still
/// alive**, which is the only thing that makes it fail for a `SpanBody`
/// that closes on `Drop` alone: a body read to its end and then dropped
/// looks the same either way. And the two are asymmetric in what a
/// mutation can reach: emptying `poll_frame`'s end-of-stream arm kills the
/// first on both fronts, where no mutation of this crate kills the second
/// — see `Recorder`'s `Drop`, which records the measurement and why the
/// impl is kept regardless.
///
/// `B: Unpin` is the same bound `hclient::Limited` and its neighbours in
/// the `ClientBody` chain already carry, and for the same reason: it buys
/// `Pin::new(&mut inner)` where the alternative is a projection macro or
/// an `unsafe` this workspace forbids. Every body in this workspace
/// satisfies it, and one that did not could not reach `Client` anyway.
#[derive(Debug)]
pub struct SpanBody<B> {
    inner: B,
    recorder: Recorder,
}

impl<B> SpanBody<B> {
    pub(crate) fn new(inner: B, recorder: Recorder) -> Self {
        Self { inner, recorder }
    }

    /// The body underneath, for a caller who has one of these and wanted
    /// the other.
    pub fn get_ref(&self) -> &B {
        &self.inner
    }
}

impl<B> Body for SpanBody<B>
where
    B: Body<Data = Bytes> + Unpin,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        let this = &mut *self;
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        match &polled {
            Poll::Ready(None) => this.recorder.end(),
            Poll::Ready(Some(Err(_))) => {
                // **`Body` and not the error's own kind, and that is a
                // fact about `http_body` rather than a shortcut.**
                // `http_body::Body::Error` carries no bound at all — not
                // `std::error::Error`, not `Any` — so there is nothing to
                // downcast and nothing even to `Display`. What makes the
                // constant honest rather than a stand-in is that it is
                // the right answer: measured in `hclient-native`, every
                // response-body failure it produces is
                // `ErrorKind::Body` (`body.rs`, `http1.rs`), because a
                // frame that did not arrive is a body failure by
                // construction. The `Timeout` and `Decode` kinds a caller
                // meets on a body come from wrappers `hclient::Client`
                // puts *above* this one and never pass through here.
                this.recorder.failed("Body");
                this.recorder.end();
            }
            Poll::Ready(Some(Ok(_))) | Poll::Pending => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// The `error.type` a body failure reports. Named so the test that pins
/// it and the code that sets it cannot drift apart.
#[cfg(test)]
pub(crate) const BODY_ERROR_TYPE: &str = "Body";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attrs;

    #[test]
    fn the_constant_is_a_kind_the_taxonomy_actually_has() {
        // It is written as a literal above rather than derived, so this
        // is what says the literal is still one of `ErrorKind`'s names.
        assert_eq!(
            attrs::error_type(&hclient_core::ErrorKind::Body),
            BODY_ERROR_TYPE
        );
    }
}
