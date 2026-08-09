//! The response body, over an h3 request stream.

use bytes::{Buf, Bytes};
use http_ng_core::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};

type Stream = h3::client::RequestStream<
    <h3_quinn::OpenStreams as h3::quic::OpenStreams<Bytes>>::BidiStream,
    Bytes,
>;

/// An HTTP/3 response body.
///
/// # Dropping it cancels the stream, and only the stream
///
/// `Transport::execute`'s contract passes to the body once the head has
/// arrived, and here it is discharged by `RequestStream`'s own `Drop`,
/// which sends `STOP_SENDING` for the stream. **The connection is
/// untouched**, which is the point of HTTP/3: a cancelled request must not
/// disturb its neighbours on the same connection, and on this transport
/// there genuinely are neighbours — see [`crate::H3`]'s pool policy.
pub struct H3Body {
    stream: Stream,
    /// Trailers are read after the data has ended, and `http_body` wants
    /// them as one more frame, so the body has a second phase rather than
    /// one loop.
    phase: Phase,
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Data,
    Trailers,
    Done,
}

impl H3Body {
    pub(crate) fn new(stream: Stream) -> Self {
        Self {
            stream,
            phase: Phase::Data,
        }
    }
}

// Hand-written: h3's RequestStream is not Debug, and the useful thing to
// print about a body in flight is which phase it is in, not the QPACK
// state behind it.
impl std::fmt::Debug for H3Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H3Body")
            .field("phase", &self.phase)
            .finish()
    }
}

impl http_body::Body for H3Body {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        loop {
            match self.phase {
                Phase::Done => return Poll::Ready(None),
                Phase::Data => {
                    match std::task::ready!(self.stream.poll_recv_data(cx)) {
                        Ok(Some(mut buf)) => {
                            // `copy_to_bytes` rather than a chunk-by-chunk
                            // walk: h3 hands back an `impl Buf` that may be
                            // several chunks, and `http_body::Frame` carries
                            // exactly one `Bytes`.
                            let n = buf.remaining();
                            let bytes = buf.copy_to_bytes(n);
                            return Poll::Ready(Some(Ok(http_body::Frame::data(bytes))));
                        }
                        // The data half ended. Trailers may still follow,
                        // and "no trailers" is not an error.
                        Ok(None) => self.phase = Phase::Trailers,
                        Err(e) => {
                            self.phase = Phase::Done;
                            return Poll::Ready(Some(Err(stream_error(e))));
                        }
                    }
                }
                Phase::Trailers => {
                    let out = std::task::ready!(self.stream.poll_recv_trailers(cx));
                    self.phase = Phase::Done;
                    return Poll::Ready(match out {
                        Ok(Some(t)) => Some(Ok(http_body::Frame::trailers(t))),
                        Ok(None) => None,
                        Err(e) => Some(Err(stream_error(e))),
                    });
                }
            }
        }
    }
}

/// An h3 stream failure, classified.
///
/// `ErrorKind::Body` rather than `Other`: by the time this can happen the
/// connection is up and the handshake is done, so the failure is in the
/// transfer — and `Other` is reserved for a backend that genuinely has
/// nothing to say about the category, which is not the case here.
pub(crate) fn stream_error(e: h3::error::StreamError) -> Error {
    Error::new(ErrorKind::Body, std::io::Error::other(e.to_string()))
}
