//! The response body of whichever stack answered.
//!
//! # Why not `http_body_util::Either`
//!
//! It exists and it is the obvious reach, and it has the wrong error type:
//! `Either`'s `type Error = Box<dyn std::error::Error + Send + Sync>`. Both
//! members here already produce `http_ng_core::Error` — classified, with
//! `ErrorKind::Timeout(Phase::BetweenBytes)` off `http-ng-native`'s idle
//! bound and h3's own categories — and boxing flattens that taxonomy into a
//! string-shaped thing one layer above. It is `Transport::to_error`'s
//! finding (a backend's whole classification discarded one seam up) with
//! the body in place of the head.
//!
//! The bound also would not hold: `http_ng::Deadline` requires `B::Error:
//! std::error::Error`, and `Box<dyn Error + Send + Sync>` does not
//! implement `Error`, so a client built over this transport would not
//! compile.
//!
//! # No `unsafe`, and therefore an `Unpin` bound
//!
//! Projecting a `Pin<&mut Self>` into one of two variants needs either
//! `unsafe` (which this workspace forbids by declaration) or a
//! pin-projection macro. Neither is needed: both member bodies are `Unpin`
//! already — `http_ng::Deadline` requires it of any body a `Client` wraps,
//! so a body that were not `Unpin` could not reach a caller through this
//! library at all — and with `Self: Unpin` the projection is
//! `Pin::new(&mut …)`.

use bytes::Bytes;
use http_ng_core::Error;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A response body from one of the two stacks, named for the stack rather
/// than for `Left`/`Right`.
///
/// A caller that wants to know which one answered reads
/// `Response::version()`, which both members report honestly
/// (`Capabilities::version_reported` is `true` on both, so the conjunction
/// this transport stores is `true` too). This type is not that answer: it
/// is reachable only after the head, and matching on it would make the
/// protocol a property of the body.
#[derive(Debug)]
pub enum SelectedBody<A, B> {
    /// `http-ng-native` answered: HTTP/1.1, or HTTP/2 where its `http2`
    /// feature is on and ALPN chose it.
    Tcp(A),
    /// `http-ng-h3` answered: HTTP/3 over QUIC.
    Quic(B),
}

impl<A, B> http_body::Body for SelectedBody<A, B>
where
    A: http_body::Body<Data = Bytes, Error = Error> + Unpin,
    B: http_body::Body<Data = Bytes, Error = Error> + Unpin,
{
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        match self.get_mut() {
            Self::Tcp(b) => Pin::new(b).poll_frame(cx),
            Self::Quic(b) => Pin::new(b).poll_frame(cx),
        }
    }

    /// Delegated rather than defaulted, and it is not decoration: the
    /// default is `false`, and a caller that reads it — `http-body-util`'s
    /// `collect` does — would keep polling a body that had already said it
    /// was finished.
    fn is_end_stream(&self) -> bool {
        match self {
            Self::Tcp(b) => b.is_end_stream(),
            Self::Quic(b) => b.is_end_stream(),
        }
    }

    /// Also delegated. The default is "unknown", which costs a caller
    /// sizing a buffer the `Content-Length` the member already read.
    fn size_hint(&self) -> http_body::SizeHint {
        match self {
            Self::Tcp(b) => b.size_hint(),
            Self::Quic(b) => b.size_hint(),
        }
    }
}
