//! The WebSocket seam: a message channel, and the thing that opens one.
//!
//! # Why this is not a method on [`Transport`](super::Transport)
//!
//! The same reasoning `hclient-tls-quic`'s `QuicTlsConnect` rests on, and
//! `hclient_rt::TcpAdoptStd` before it: the intersection between "send a
//! request, read a response" and "exchange messages until somebody closes"
//! is empty, and an adapter between them would type-check *with an empty
//! body*. A `Transport::websocket` returning `Err(Unsupported)` would push
//! the same failure from compile time to run time, on a feature a caller
//! either has or has not.
//!
//! **So the seam expresses itself by being implemented.** A backend that
//! can do WebSocket implements [`WebSocketConnect`]; one that cannot does
//! not, and asking it for a WebSocket does not compile. There is
//! deliberately no capability field to read: a runtime `Unsupported`
//! would move the same failure from compile time to run time.
//!
//! # Why message oriented, rather than "hand back the socket"
//!
//! A byte-stream seam is implementable by exactly one of this project's
//! four backends, and the three it excludes include the browser — the
//! target whose inclusion is the whole claim. `WebSocket` in a browser is
//! a wholly separate global reached from no `fetch`-shaped API, it hands
//! back no bytes, and on Apple platforms `NSURLSessionWebSocketTask` is
//! message-framed too. So the h1 upgrade is an implementation detail
//! *underneath* this seam on native, not the seam.
//!
//! # What is deliberately not here
//!
//! - **`Ping` and `Pong` are not [`Message`] variants.** RFC 6455 §5.5.2
//!   makes answering a ping the *endpoint's* duty, not the caller's, and
//!   `hclient-tungstenite` discharges it without telling anybody
//!   (`crates/hclient-tungstenite/tests/websocket.rs` watches the pong
//!   leave from the server's side of the wire). A caller-visible `Ping`
//!   would be
//!   a variant the browser can neither send nor ever receive, which is the
//!   capability lie this workspace has caught four times. If a caller
//!   decision ever turns on one, adding the variant is a compile error at
//!   every backend — which is the right way round, and why this enum is
//!   not `#[non_exhaustive]`.
//! - **Permessage-deflate and subprotocol negotiation** are not
//!   supported. A subprotocol *can* be asked for, because the request
//!   carries headers; nothing here checks what came back.
use crate::Error;
use alloc::string::String;
use core::future::Future;
use futures_core::Stream;
use futures_sink::Sink;

/// One WebSocket message, in the vocabulary every backend can speak.
///
/// Not `#[non_exhaustive]`: see the module doc. Nothing here is published,
/// so a new variant costs a rebase inside this workspace and a compile
/// error is what a backend author should get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A text message. RFC 6455 §5.6 requires it to be valid UTF-8, which
    /// `String` is by construction — a backend that reads invalid UTF-8
    /// off the wire owes an error, not a lossy conversion.
    Text(String),
    /// A binary message.
    Binary(bytes::Bytes),
    /// The close handshake, in whichever direction it was seen.
    ///
    /// Received: the peer is closing, and the [`Stream`] ends after this.
    /// Sent: the close frame is queued; keep polling the [`Stream`] until
    /// it ends if the peer's answer matters.
    Close(Option<CloseFrame>),
}

/// The close code and reason of a [`Message::Close`].
///
/// `u16` rather than an enum of the RFC 6455 §7.4 codes: this seam does
/// not interpret them, and an enum would have to decide what a reserved
/// or application-defined code means, and this seam has not decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseFrame {
    /// RFC 6455 §7.4 status code.
    pub code: u16,
    /// The reason, which RFC 6455 requires to be valid UTF-8.
    pub reason: String,
}

/// An open WebSocket: messages out, messages in.
///
/// `Stream` for the receiving half and `Sink` for the sending half, on one
/// value rather than a split pair, because splitting is something
/// `futures_util::StreamExt::split` already does for any `Stream + Sink`
/// and a seam that pre-split would take that choice away from the caller.
///
/// # The error type is concrete, unlike [`Transport::Error`](crate::unversioned::Transport::Error)
///
/// [`Transport`](super::Transport) carries `type Error` and a `to_error`
/// hook so a backend whose error is genuinely `!Send` can still implement
/// it. That escape hatch has no subject here: it exists so a backend can
/// keep its own *typed source* while `Client` classifies, and there is no
/// `Client` between this trait and its caller — whatever a backend would
/// put in its own error, it can put in [`Error`]'s source, which is where
/// a caller would read it from anyway. One concrete type also keeps
/// `Stream::Item`'s error and `Sink::Error` the same type without an
/// associated-type equality the caller has to spell out.
///
/// # Ending
///
/// The `Stream` ends (`None`) when the connection is finished: after the
/// peer's [`Message::Close`] has been delivered, or when the connection
/// broke and the error has already been reported. A `Stream` that has
/// ended stays ended.
pub trait WebSocket: Stream<Item = Result<Message, Error>> + Sink<Message, Error = Error> {}

/// A backend that can open a WebSocket.
///
/// Implemented either by a transport itself (`hclient_fetch::Fetch`, where
/// the platform hands back messages) or by a connector over one
/// (`hclient_tungstenite::Tungstenite`, where it hands back bytes and the
/// framing is a crate of its own).
/// Either way a WebSocket opened this way inherits everything the
/// transport already knows: its runtime, its TLS configuration, its
/// resolver.
pub trait WebSocketConnect {
    /// The open connection.
    type WebSocket: WebSocket;

    /// Open one.
    ///
    /// # What `req` is for, and the duty it puts on the implementer
    ///
    /// The URI is the only field with a required interpretation: `ws://`
    /// and `wss://`, and `http://`/`https://` read as the same two, since
    /// a caller who already holds an origin should not have to rewrite its
    /// scheme. Everything else the request carries — headers in
    /// particular — is a request to the implementer, and
    /// **a backend that cannot send a header the request carries must
    /// fail rather than drop it.** That is the rule `hclient-wasi` already
    /// follows for `wasi:http`'s request options, and it is what keeps
    /// this seam from becoming the place where an `Authorization` header
    /// silently does not go out. It is also the whole of the answer for a
    /// browser backend, which can send no headers at all beyond the
    /// subprotocol list.
    ///
    /// The method and version are ignored: RFC 6455 §4.1 fixes both, and
    /// a backend is free to build the handshake it must build.
    ///
    /// # Cancellation
    ///
    /// Dropping this future before it completes stops the attempt, on
    /// exactly the terms [`Transport::execute`](super::Transport::execute)
    /// states: no further bytes, nothing waited for, and the socket torn
    /// down rather than left running.
    fn websocket(
        &self,
        req: http::Request<()>,
    ) -> impl Future<Output = Result<Self::WebSocket, Error>>;
}
