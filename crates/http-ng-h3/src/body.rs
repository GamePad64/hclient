//! The response body, over an h3 request stream — and the request body
//! still being written on the other half of it.

use bytes::{Buf, Bytes};
use http_ng_core::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};

/// The receive half of a split request stream.
pub(crate) type RecvHalf = h3::client::RequestStream<
    <<h3_quinn::OpenStreams as h3::quic::OpenStreams<Bytes>>::BidiStream as h3::quic::BidiStream<
        Bytes,
    >>::RecvStream,
    Bytes,
>;

/// An HTTP/3 response body.
///
/// # It also carries the rest of the request
///
/// `full_duplex` is `true` on this transport, which means the head can be
/// delivered while the request body is still going out. The unfinished
/// write is a future ([`crate::pump`]) and it comes here, because this is
/// the object the caller still holds: `poll_frame` polls it first, so the
/// upload advances every time the download is read.
///
/// **The pump's `Pending` is not this body's `Pending`.** A server that
/// stops granting stream window while the caller is waiting for response
/// bytes would otherwise deadlock the exchange it is part of, so the pump
/// is polled for progress and then the response is polled regardless of
/// what it answered.
///
/// **The response ending does not wait for the request to finish.** When
/// the response body ends with a pump still running, this yields `None`
/// anyway rather than holding the caller until the upload completes. The
/// alternative hangs: a server that sent a complete response and then
/// neither reads the request stream nor stops it would leave a body that
/// never ends. What the caller gets instead is the honest shape — the
/// response is over — and the request stream is reset when this body is
/// dropped.
///
/// # Dropping it cancels the stream, and only the stream
///
/// `Transport::execute`'s contract passes to the body once the head has
/// arrived, and here it is discharged by the two halves' own `Drop`:
/// dropping the receive half sends `STOP_SENDING`, and dropping the pump
/// drops the send half, which sends `RESET_STREAM` for a request that was
/// never finished. **The connection is untouched**, which is the point of
/// HTTP/3: a cancelled request must not disturb its neighbours on the same
/// connection, and on this transport there genuinely are neighbours — see
/// [`crate::H3`]'s pool policy. Nothing is spawned, so there is no pump
/// left running behind a dropped body either.
pub struct H3Body {
    stream: RecvHalf,
    /// `None` once the request body has been written in full — which for
    /// an empty body is before this type exists, and for a large one may
    /// be long after.
    pump: Option<crate::pump::Pump>,
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
    pub(crate) fn new(stream: RecvHalf, pump: Option<crate::pump::Pump>) -> Self {
        Self {
            stream,
            pump,
            phase: Phase::Data,
        }
    }

    /// Drives the request body write, and reports only a failure.
    ///
    /// Deliberately not a `Poll<..>`: the caller must not be able to return
    /// this function's `Pending` as its own. See the type's doc comment.
    fn poll_pump(&mut self, cx: &mut Context<'_>) -> Option<Error> {
        let pump = self.pump.as_mut()?;
        match pump.as_mut().poll(cx) {
            Poll::Ready(Ok(())) => {
                self.pump = None;
                None
            }
            Poll::Ready(Err(e)) => {
                self.pump = None;
                Some(e)
            }
            Poll::Pending => None,
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
            .field("still_sending_the_request", &self.pump.is_some())
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
        if let Some(e) = self.poll_pump(cx) {
            self.phase = Phase::Done;
            return Poll::Ready(Some(Err(e)));
        }
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
