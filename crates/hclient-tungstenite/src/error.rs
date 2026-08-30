//! Every way this crate refuses a handshake or ends a socket.
//!
//! **Five types and thirty-eight lines**, which is a weaker case for a
//! module than `hclient-webtransport`'s eight types and two hundred and
//! fifty — and it is done anyway, for two reasons that are not size. The
//! four handshake refusals are one subject and were already contiguous:
//! each is a header or a scheme the seam will not accept, and reading
//! them together is reading the crate's admission policy. And a reader
//! who has learned where a sibling crate keeps its errors should not have
//! to learn again; the WebSocket pair is exactly the place that
//! comparison gets made.
//!
//! Four of the five are private — they reach a caller only through
//! `Error::source` — and that is unchanged. [`PongNotReceived`] is the
//! one that is public, and it moved by the rule its sibling crate's split
//! established: **a type goes by what it is, not by which half of a
//! `Result` it appears in.** It is an error, so it is here, where
//! `hclient-webtransport`'s `SessionClose` is a value and stayed with the
//! session it describes.

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
#[error("the WebSocket handshake sets {0} itself; a request may not carry one")]
pub(crate) struct ReservedHeader(pub(crate) http::HeaderName);

#[derive(Debug, thiserror::Error)]
#[error("unsupported URI scheme for a WebSocket: {0:?} (expected ws, wss, http or https)")]
pub(crate) struct UnsupportedScheme(pub(crate) String);

#[derive(Debug, thiserror::Error)]
#[error("the 101 response is missing or misspells its {0} header")]
pub(crate) struct BadUpgradeHeader(pub(crate) &'static str);

#[derive(Debug, thiserror::Error)]
#[error("the server's Sec-WebSocket-Accept does not match the Sec-WebSocket-Key this client sent")]
pub(crate) struct AcceptKeyMismatch;

/// The source of the [`hclient_core::ErrorKind::Body`] error a missed pong produces.
///
/// A named public type rather than a message, for the reason
/// [`hclient_native::BetweenBytesElapsed`] is one: a caller must be able to tell
/// this apart from every other way a connection can fail with
/// `Error::source().downcast_ref()`, and to read the bound that was
/// actually in force rather than parse it out of a string.
///
/// # Why it is an error and not a `Message::Close`
///
/// Because those are different facts and a caller acts differently on
/// them: a `Close` is the peer saying goodbye, this is the network having
/// gone away underneath a peer that never did. `hclient-fetch` already
/// draws that line in the same place and in the same vocabulary — a
/// browser `CloseEvent` with `wasClean == false` becomes an
/// `ErrorKind::Body` on the `Stream` rather than a `Message::Close(1006)`,
/// precisely so that a caller inspecting `Message::Close` is not told its
/// peer said goodbye when it did not. This agrees with that rather than
/// inventing a second vocabulary.
///
/// It is deliberately **not** an `ErrorKind::Timeout`: no field of
/// [`hclient_core::Timeouts`] is in force here, and `Phase::BetweenBytes`
/// in particular would name a bound this seam deliberately does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the peer did not answer a keep-alive ping within {0:?}")]
pub struct PongNotReceived(pub Duration);
