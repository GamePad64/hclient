//! What an `h2::Error` means to this transport, and the four errors it
//! cannot say itself.
//!
//! Split out of `mod.rs` because these are the answers to *"what went
//! wrong"*, and mixing them with the exchange makes both harder to read.
//! Two of them carry the reasoning that is easiest to get wrong and
//! hardest to recover once lost — RFC 9113 §8.1's two halves — so they
//! are where a reader looking for it will be.

use hclient_core::{Error, ErrorKind};

/// The one conversion point from [`h2::Error`] to [`hclient_core::Error`]
/// in this module, so a category is chosen once per site rather than
/// scattered.
pub(super) fn from_h2_error(e: h2::Error, fallback: ErrorKind) -> Error {
    Error::new(fallback, e)
}

/// The other half of RFC 9113 §8.1, on the receiving side: a
/// `RST_STREAM(NO_ERROR)` ends the response body rather than failing it.
///
/// # Why the response body needs a rule of its own
///
/// Fixing [`poll_pump`] alone gets the response *head* back and stops
/// there. h2 records end-of-stream as a state rather than as an event —
/// `recv_data` with `END_STREAM` calls `state.recv_close()`
/// (`h2-0.4.15/src/proto/streams/recv.rs:714`) and queues nothing — and a
/// `RST_STREAM` arriving afterwards **overwrites that state** with
/// `Closed(Cause::Error(Reset(..)))` (`.../state.rs:258-290`). The frames
/// already received stay in `pending_recv` and are still handed out, but
/// the clean end behind them is gone: once the queue drains,
/// `ensure_recv_open` returns the reset as an error
/// (`.../state.rs:433-443`). Measured on this transport before the fix:
/// `status=200`, then the body failing with
/// `Reset(StreamId(1), NO_ERROR, Remote)`. A response whose body cannot be
/// read is a response discarded, whatever the head says.
///
/// # `NO_ERROR` is the server's statement that the response was complete
///
/// It is also the only evidence available: the END_STREAM that would have
/// proved it has been overwritten by the time anything here can look, and
/// the frames arrive in one `Connection::poll` in any case. RFC 9113 §8.1
/// defines the code as meaning exactly this — *"after sending a complete
/// response"* — so a server that sends `NO_ERROR` over a half-written body
/// truncates it silently here. That is the same exposure a client already
/// has to a server that ends a length-less body early, and it is the
/// behaviour hyper chose for the same reason
/// (`hyper-1.11.0/src/body/incoming.rs:250-259`, and again at 273-281 for
/// trailers). Every other reason code still fails the body.
///
/// The connection is **not** handed back to the pool on this path — the
/// two call sites clear `reuse` before asking. A stream that ended by
/// reset is not the evidence a check-in is made of, and h2 permits reuse
/// here (a stream reset says nothing about the connection), so this is
/// deliberately the losing side of "easier to lose reuse than to gain it".
pub(super) fn stopped_after_a_complete_response(e: &h2::Error) -> bool {
    e.is_reset() && e.reason() == Some(h2::Reason::NO_ERROR)
}

/// The h2 counterpart of [`crate::http1`]'s connection-went-away error: the
/// pooled connection turned out to be finished at the last moment before
/// its request was handed over.
#[derive(Debug, thiserror::Error)]
#[error("the pooled HTTP/2 connection was closed before the request was sent")]
pub(super) struct ConnectionWentAwayBeforeTheRequest;

/// The residual race the pool cannot close, on h2: the connection ended
/// between the request being handed to h2 and the response arriving.
#[derive(Debug, thiserror::Error)]
#[error("the HTTP/2 connection ended while the request was still in flight")]
pub(super) struct ConnectionEndedWithTheRequestQueued;

/// The peer reset the stream, or the connection went away, while the
/// request body was still being written.
#[derive(Debug, thiserror::Error)]
#[error("the HTTP/2 stream closed while the request body was still being sent")]
pub(super) struct StreamClosedWhileSendingTheRequestBody;

/// A request body produced a frame that is neither data nor trailers.
#[derive(Debug, thiserror::Error)]
#[error("the request body produced a frame that is neither data nor trailers")]
pub(super) struct UnknownRequestBodyFrame;

/// The peer did not answer a keep-alive `PING` in time.
#[derive(Debug, thiserror::Error)]
#[error("the HTTP/2 peer did not answer a keep-alive PING within the bound")]
pub struct PingNotAnswered;
