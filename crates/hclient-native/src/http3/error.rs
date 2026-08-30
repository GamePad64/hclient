//! What the QUIC stack refuses, and why it is not in the crate's
//! `error.rs`.
//!
//! The boundary `crate::error`'s own doc states: a type stays under
//! `src/http3/` when it is about **this protocol** rather than about a
//! phase of the transport. These three are — an unsent trailer frame and
//! an unknown body frame are RFC 9114 shapes, and `ConnectTimedOut` here
//! bounds a QUIC handshake rather than a TCP connect, which is why the
//! crate root has a type of the same name and a different subject.
//!
//! `src/http2/error.rs` is the same arrangement one protocol over.

use std::time::Duration;

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

/// The failure `within_connect` ends in when the timer wins.
///
/// A named type rather than a string, for the reason
/// `hclient_native::FirstByteTimedOut` gives: a caller must be able to tell
/// the phases apart with `Error::source().downcast_ref()`, and to read the
/// bound that was actually in force rather than parse it back out of a
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no HTTP/3 connection within the connect timeout of {0:?}")]
pub struct ConnectTimedOut(pub Duration);
