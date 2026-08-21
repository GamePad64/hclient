//! A ceiling on how many bytes a response body may yield.
//!
//! # Which bytes, and why that is a decision rather than a wrapper
//!
//! `ClientBody` is `Limited<Decompressed<Deadline<Cached<B>, Tm>>>`, and
//! this being the **outermost** wrapper is the whole of the design: it
//! counts what the caller receives, after any `Content-Encoding` has been
//! reversed.
//!
//! That is the axis the threat lives on. A decompression bomb is small on
//! the wire and enormous in memory — that is its definition — so a limit
//! applied inside the decompressor would pass it. `Deadline` sits the
//! other way round for the mirror-image reason, written down in
//! `decompress.rs`: it is polled once per **compressed** frame, because a
//! server sending well-compressing padding would otherwise walk around a
//! `total` bound by producing very few decompressed frames very slowly.
//! One wrapper counts what arrives, the other times what is sent, and each
//! is on the side its own threat is.
//!
//! The cost of that choice is worth stating because a caller will meet it:
//! **a limit of N does not promise that fewer than N bytes crossed the
//! wire.** For every real coding the wire count is smaller, so the bound
//! holds in the direction that matters; a pathological coding that
//! expands could exceed it, and nothing here would notice.
//!
//! # Why a wrapper rather than a check in `Client::execute`
//!
//! Because the body is handed to the caller and drained by them, at their
//! pace, long after `execute` has returned — the same fact that put
//! `Deadline` and `IdleTimeout` in the body rather than around the call.
//! A check at the head could only ever read `Content-Length`, which a
//! server is free not to send and free to lie in.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Buf;
use hclient_core::{Error, ErrorKind};
use http_body::{Body, Frame};

/// The body yielded more bytes than the client's limit allowed.
///
/// Carries both numbers because the pair is what a caller acts on: the
/// limit tells them what they asked for, and the count tells them how
/// close the truth was to it.
#[derive(Debug, thiserror::Error)]
#[error("the response body exceeded the {limit}-byte limit (stopped at {seen})")]
#[non_exhaustive]
pub struct ResponseTooLarge {
    pub limit: u64,
    pub seen: u64,
}

// Hand-written, so that it carries **no `B: Debug` bound**. `#[derive]`
// would add one, and after erasure the body in a real client is
// `dyn http_body::Body`, which is not `Debug` — so the derive would take
// `.unwrap()` on a response away from every caller. What a `{:?}` wants
// here is the head anyway: a body is a stream, and its contents were never
// printable without consuming them.
impl<B> std::fmt::Debug for Limited<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Limited").finish_non_exhaustive()
    }
}

/// Stops a response body at a byte ceiling.
///
/// Inert with no limit set, in the shape [`crate::body::Deadline`] already uses:
/// the type is always present, because a type cannot appear and disappear
/// with a runtime value, and the cost of that is one `Option` test per
/// frame.
pub struct Limited<B> {
    /// `None` once the limit has fired, which drops the inner body — and
    /// dropping it is what stops the exchange, exactly as `Deadline`'s
    /// expiry does. A limit that reported an error and left the transfer
    /// running would bound memory and nothing else.
    inner: Option<B>,
    limit: Option<u64>,
    seen: u64,
}

impl<B> Limited<B> {
    /// The body underneath, for the type-shape test that pins this
    /// wrapper's position — see `tests/compression_client_type.rs`, where
    /// each layer's place is an argument rather than an arrangement.
    ///
    /// Panics if the limit has already fired, which is unreachable from a
    /// caller: the error that fires it is terminal, so a body that
    /// yielded one is not one anybody unwraps.
    pub fn into_inner(self) -> B {
        self.inner
            .expect("the limit had already fired, so there is no body left")
    }

    pub(crate) fn new(inner: B, limit: Option<u64>) -> Self {
        Self {
            inner: Some(inner),
            limit,
            seen: 0,
        }
    }
}

impl<B> Body for Limited<B>
where
    B: Body<Data = bytes::Bytes, Error = Error> + Unpin,
{
    type Data = bytes::Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<bytes::Bytes>, Error>>> {
        let this = self.get_mut();
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        match Pin::new(inner).poll_frame(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Some(Ok(frame))) => {
                let Some(limit) = this.limit else {
                    return Poll::Ready(Some(Ok(frame)));
                };
                let added = frame.data_ref().map_or(0, |d| d.remaining() as u64);
                this.seen += added;
                if this.seen > limit {
                    // The frame is dropped rather than truncated: half a
                    // frame is not a thing this crate hands anyone, and a
                    // caller who asked for a bound wants the refusal, not
                    // the first `limit` bytes of something they cannot
                    // trust the rest of.
                    this.inner = None;
                    return Poll::Ready(Some(Err(Error::new(
                        ErrorKind::Body,
                        ResponseTooLarge {
                            limit,
                            seen: this.seen,
                        },
                    ))));
                }
                Poll::Ready(Some(Ok(frame)))
            }
        }
    }

    /// Narrowed by the limit, so a caller who reserves from the hint does
    /// not reserve more than they permitted.
    ///
    /// The **lower** bound is narrowed too, and that is the half worth
    /// naming: a lower bound above the limit would promise bytes this
    /// wrapper has already decided never to yield.
    fn size_hint(&self) -> http_body::SizeHint {
        let Some(inner) = self.inner.as_ref() else {
            return http_body::SizeHint::with_exact(0);
        };
        let mut hint = inner.size_hint();
        let Some(limit) = self.limit else {
            return hint;
        };
        let left = limit.saturating_sub(self.seen);
        if hint.lower() > left {
            hint.set_lower(left);
        }
        match hint.upper() {
            Some(u) if u <= left => {}
            _ => hint.set_upper(left),
        }
        hint
    }

    fn is_end_stream(&self) -> bool {
        match self.inner.as_ref() {
            None => true,
            Some(inner) => inner.is_end_stream(),
        }
    }
}
