//! The request body, written on the send half of a split stream.
//!
//! # Why this is a separate object rather than three lines in `one_attempt`
//!
//! A QUIC stream's two halves are independent (RFC 9000 §2.1), and
//! [`h3::client::RequestStream::split`] hands them over as two values. That
//! is what makes duplex possible at all: the write side can be a future
//! that is still running while the read side is delivering a response head.
//! A body written by `await`ing inside `one_attempt` could not be, because
//! `one_attempt` returns.
//!
//! So the pump is an owned future. It is polled beside `recv_response`
//! until the head arrives, and then **moves into [`crate::H3Body`]** and is
//! polled from `poll_frame`. Nothing spawns it, and that is deliberate:
//! this crate does have a `Spawn`, and a spawned pump would keep writing
//! after the caller dropped the response — the request would go on being
//! sent to a server nobody is listening to, and there would be nowhere for
//! its errors to go. Dropping the response future or the body drops this,
//! which resets the request stream.
//!
//! # What it costs, said out loud
//!
//! **A caller that never polls the response body never finishes sending the
//! request body.** That is inherent to duplex without a spawned writer —
//! `hclient-wasi`'s module doc records the same consequence for the same
//! technique — and it is the reason the sequential arrangement was fine for
//! as long as `full_duplex` was `false`. In practice a caller that wants to
//! finish an upload reads the response, and a caller that does not read the
//! response has told the transport it does not care about the rest of the
//! exchange.
//!
//! **A write failure after the head has arrived is a response-body error.**
//! Before duplex, everything the write side could say was said by
//! `execute`. Now the head can be delivered while the body write is still
//! in flight, so a later failure has only one channel left: the response
//! body's terminal error. This is cost (2) from `hclient-wasi`'s deferral
//! note, paid rather than deferred.

use bytes::Bytes;
use hclient_core::{Error, ErrorKind, RequestBody};
use std::pin::Pin;

/// The send half of a split request stream.
pub(crate) type SendHalf = h3::client::RequestStream<
    <<h3_quinn::OpenStreams as h3::quic::OpenStreams<Bytes>>::BidiStream as h3::quic::BidiStream<
        Bytes,
    >>::SendStream,
    Bytes,
>;

/// The request-body write, in flight.
///
/// `Send` is not decoration here: this `dyn` sits on the `Client ->
/// Transport` path inside [`crate::H3Body`], and amendment C2's rule is
/// that erasing a type there cuts the auto-traits off everything above it.
/// `H3Body` was `Send` before this type existed and has to stay `Send`, or
/// a caller who spawns a request stops compiling for a reason nothing
/// announces. `an_h3_body_is_still_send` in `tests/streaming.rs` is the
/// compile-time check amendment C2 asks for, in `tests/` as C3 requires.
pub(crate) type Pump = Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>; // send-bound-exception: amendment-C2

/// What is left of a [`RequestBody`] once the variants that are really the
/// same thing have been collapsed.
enum Outgoing {
    /// One buffered chunk, or nothing at all.
    Buffered(Option<Bytes>),
    /// `Unpin + Send` — the bounds `RequestBody::Streaming` already
    /// carries (amendment C2, same place), forwarded rather than restated.
    Streaming(Box<dyn http_body::Body<Data = Bytes, Error = Error> + Unpin + Send>), // send-bound-exception: amendment-C2
}

/// **A `Rewindable` is unpacked recursively, and that fixes a body this
/// crate used to drop on the floor.**
///
/// The factory may legally return any `RequestBody`, including another
/// `Rewindable` and including a `Streaming` — `RequestBody`'s own doc says
/// so. Before this change `one_attempt` matched `if let RequestBody::Full(b)
/// = f()`, so a factory returning `Streaming` sent **nothing**: no bytes, no
/// error, a `200` for a request whose body silently vanished. It was
/// unreachable in practice only because `Streaming` was refused outright one
/// arm above.
///
/// The recursion is the same shape `hclient_native::body::Inner::
/// from_request_body` uses, including its one sharp edge: a factory that
/// returns a `Rewindable` for ever recurses for ever. That is the factory
/// contract being broken rather than a case to defend against, and
/// defending against it here would mean picking a depth limit nobody can
/// justify.
fn flatten(body: RequestBody) -> Outgoing {
    match body {
        RequestBody::Empty => Outgoing::Buffered(None),
        RequestBody::Full(b) if b.is_empty() => Outgoing::Buffered(None),
        RequestBody::Full(b) => Outgoing::Buffered(Some(b)),
        RequestBody::Rewindable(f) => flatten(f()),
        RequestBody::Streaming(s) => Outgoing::Streaming(s),
    }
}

/// The send half, and the duty that dropping it discharges.
///
/// # An abandoned upload must be **reset**, not finished
///
/// This is a guard rather than a field because the moment it has to act at
/// is the moment nothing is running: the caller dropped the response
/// future, or the body, and this future is being dropped with a request
/// half written.
///
/// **quinn's `SendStream::drop` finishes the stream** —
/// `quinn-0.11.11/src/send_stream.rs:354` calls `finish()`, and only falls
/// back to `reset` when the peer had already stopped it. So a request
/// abandoned in the middle of a DATA frame terminates *cleanly*, with a
/// frame whose length header promises bytes that never come. RFC 9114 §7.1:
/// *"When a stream terminates cleanly, if the last frame on the stream was
/// truncated, this MUST be treated as a connection error of type
/// H3_FRAME_ERROR."* h3's own server does exactly that
/// (`h3-0.0.8/src/connection.rs:564`), and on this transport a connection
/// error is not one request's problem — requests share connections, so it
/// takes every neighbour down with it.
///
/// Measured before it was fixed, with the debug print in `checkout`: an
/// upload dropped after 150 ms left the pooled connection reading
/// `ApplicationClosed { error_code: 262, reason: "received incomplete
/// frame" }` — 262 is `H3_FRAME_ERROR` — and the next request opened a
/// second connection. **The defect predates streaming**: the same drop of
/// the same `execute` future in the middle of a large `RequestBody::Full`
/// did it too, and nothing reached it because the one cancellation test in
/// the suite used an empty body. Streaming is what makes it ordinary.
///
/// A reset reaches the peer as a stream error (`StreamErrorIncoming::
/// StreamTerminated` -> `StreamError::RemoteTerminate`,
/// `h3-0.0.8/src/error/connection_error_creators.rs:125`) and the
/// connection is untouched — which is the property [`crate::H3Body`]'s doc
/// claims for cancellation, now true rather than assumed.
struct Writer {
    send: SendHalf,
    /// `true` once this stream owes the peer nothing: the body was written
    /// and finished, the peer stopped reading, or it has already been
    /// reset with a code that says more than the default.
    settled: bool,
}

impl Writer {
    /// Reset now, with a code that names the reason, and take the guard out
    /// of the way.
    fn cancel(&mut self, code: h3::error::Code) {
        self.send.stop_stream(code);
        self.settled = true;
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if !self.settled {
            // RFC 9114 §4.1.1: an implementation cancels a request by
            // resetting the sending part of the stream. `H3_REQUEST_
            // CANCELLED` is what tells the server it may stop processing.
            self.send.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
        }
    }
}

/// The whole request body, as a future that can outlive `one_attempt`.
pub(crate) fn pump(send: SendHalf, body: RequestBody) -> Pump {
    Box::pin(write_body(
        Writer {
            send,
            settled: false,
        },
        body,
    ))
}

async fn write_body(mut w: Writer, body: RequestBody) -> Result<(), Error> {
    let stopped = match flatten(body) {
        Outgoing::Buffered(None) => false,
        Outgoing::Buffered(Some(b)) => crate::write_after_head(w.send.send_data(b).await)?,
        Outgoing::Streaming(mut s) => write_stream(&mut w, &mut *s).await?,
    };
    if !stopped {
        // Not `let _ =`: everything except the one tolerated failure still
        // propagates, and the `?` is what says so. On that `?` the guard
        // above fires, which is right — a request whose write failed is not
        // a request that ended.
        crate::write_after_head(w.send.finish().await)?;
    }
    w.settled = true;
    Ok(())
}

/// Frame by frame, with the peer's flow control as the only pacing.
///
/// **Backpressure is the protocol's, not a buffer of ours.**
/// `RequestStream::send_data` hands the frame to `h3-quinn`'s `SendStream`
/// and then awaits its `poll_ready`, which is `quinn::SendStream::
/// poll_write` — so it does not return until the peer's stream window has
/// room. Nothing is pulled from the caller's body until the previous frame
/// has been accepted, which is what makes the declared
/// `streaming_request_body` a stream rather than a promise to buffer
/// whatever a producer can make. (This is the same property
/// `hclient-native`'s h2 pump has to build by hand with
/// `reserve_capacity` + `poll_capacity`, because h2's `send_data` will
/// buffer without bound instead.)
///
/// Returns `true` when the peer stopped reading — see
/// [`crate::write_after_head`]. **The loop ends there rather than draining
/// the caller's body into a stream nobody is reading**, which with a
/// streaming body is the difference between a request that ends and one
/// that pulls for as long as the producer keeps producing.
async fn write_stream(
    w: &mut Writer,
    body: &mut (dyn http_body::Body<Data = Bytes, Error = Error> + Unpin + Send), // send-bound-exception: amendment-C2
) -> Result<bool, Error> {
    loop {
        let frame = std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await;
        let Some(frame) = frame else { return Ok(false) };
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                // The caller's own body failed. The stream is reset rather
                // than finished, so the server learns that what it has is
                // not the whole request — `H3_REQUEST_INCOMPLETE` is RFC
                // 9114 §8.1's code for exactly that, and it is a different
                // statement from the guard's `H3_REQUEST_CANCELLED`, which
                // says somebody decided to abandon a request that could
                // have gone on.
                w.cancel(h3::error::Code::H3_REQUEST_INCOMPLETE);
                return Err(e);
            }
        };
        match frame.into_data() {
            // An empty data frame is a legal thing for a producer to yield
            // and not a thing to put on the wire: a zero-length DATA frame
            // costs a header and says nothing.
            Ok(data) if data.is_empty() => continue,
            Ok(data) => {
                if crate::write_after_head(w.send.send_data(data).await)? {
                    return Ok(true);
                }
            }
            Err(other) => {
                w.cancel(h3::error::Code::H3_REQUEST_INCOMPLETE);
                return Err(match other.into_trailers() {
                    // `Capabilities::request_trailers` is `false`, and this
                    // is what makes that declaration cost something. h3 can
                    // send trailers — `RequestStream::send_trailers` is one
                    // call — so the `false` is a scope decision rather than
                    // an inability, and until it is made `true` in the same
                    // change that implements and measures it, a caller who
                    // sent trailers is told they were not sent.
                    Ok(_) => Error::new(ErrorKind::Unsupported, RequestTrailersNotSent),
                    // `http_body::Frame` is `#[non_exhaustive]`: a frame
                    // that is neither data nor trailers is one this version
                    // has no name for, and guessing what to put on the wire
                    // for it is how a body silently loses content.
                    Err(_) => Error::new(ErrorKind::Body, UnknownRequestBodyFrame),
                });
            }
        }
    }
}

/// The request body carried trailers and this transport does not send any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "hclient-h3 does not send request trailers; Capabilities::request_trailers reports false, \
     and the request stream was reset rather than finished without them"
)]
pub struct RequestTrailersNotSent;

/// A frame kind `http_body` has and this crate has no wire form for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the request body yielded a frame that is neither data nor trailers")]
pub struct UnknownRequestBodyFrame;
