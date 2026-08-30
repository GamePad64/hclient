//! WebSocket over WinHTTP's own framing.
//!
//! # Why this is in this crate and not one of its own
//!
//! `hclient-tungstenite` is a crate because it carries `tungstenite`, and
//! a feature on `hclient-native` would have put that dependency into every
//! build in any graph that switched it on. **That argument has no subject
//! here**: `WinHttpWebSocketSend` and its five neighbours are in
//! `windows-sys`'s `Win32_Networking_WinHttp`, which this crate already
//! names, so the whole feature costs **zero crates**. `hclient-fetch` is
//! the precedent that fits — the browser's `WebSocket` is a `web-sys`
//! feature it already has, and its seam impl lives in the crate.
//!
//! This crate's own *deliberately not done* list said the opposite, and
//! said it by citing the rule rather than by applying it.
//!
//! # What WinHTTP does and what is left here
//!
//! Almost all of it is WinHTTP's. `WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET`
//! makes the handshake — the `Upgrade` and `Connection` headers, the
//! nonce, the version, and checking the `Sec-WebSocket-Accept` that comes
//! back. Masking, the frame headers, and the ping/pong exchange are all
//! inside the DLL. **That is why the message-oriented seam fits a second
//! backend it was not designed for**: `WebSocketConnect` was shaped around
//! the browser, which also hands over messages rather than bytes, and
//! WinHTTP turns out to be the same shape. It is the strongest evidence
//! the seam has had that its shape is not the browser's accident.
//!
//! Three things are left to this file:
//!
//! - **Fragments become messages.** WinHTTP reports a message longer than
//!   the read buffer as `*_FRAGMENT` parts followed by a `*_MESSAGE`, and
//!   the seam's `Message` is whole. So the parts are accumulated here.
//! - **UTF-8 is checked.** `Message::Text` is a `String`, and the seam
//!   says a backend that reads invalid UTF-8 off the wire owes an error
//!   rather than a lossy conversion. WinHTTP does not check.
//! - **One event queue, two readers.** `Stream` and `Sink` are on one
//!   value and both drain the same completion channel, so each stashes a
//!   completion that belongs to the other rather than dropping it.
//!
//! # Ping and pong are absent here for the reason they are absent from
//! the seam
//!
//! `Message` has no `Ping`/`Pong` because a browser has neither
//! `send(ping)` nor `onping`. WinHTTP is the same: it answers pings itself
//! and reports nothing, and `WINHTTP_OPTION_WEB_SOCKET_KEEPALIVE_INTERVAL`
//! is how a caller asks for its own — a knob on the session rather than a
//! message on the wire. So the variant would have had no honest right-hand
//! side here either.

use std::pin::Pin;
use std::task::{Context, Poll, ready};

use bytes::{Bytes, BytesMut};
use futures_core::Stream;
use futures_sink::Sink;
use hclient_core::unversioned::{CloseFrame, Message, WebSocket, WebSocketConnect};
use hclient_core::{Error, ErrorKind};
use windows_sys::Win32::Networking::WinHttp as w;

use crate::body::event_name;
use crate::error::{Win32Error, WinHttpError};
use crate::session::{WinHttp, expect, header_block, setup, split_uri};
use crate::sys::{Event, Exchange};

/// An open WebSocket over WinHTTP.
///
/// **Every handle is held, and the order they are declared in is the order
/// they close in.** The socket first, so its close frame goes before the
/// request handle it was upgraded from is torn down, and the connect
/// handle last because the other two are derived from it.
pub struct WinHttpWebSocket {
    socket: crate::sys::WebSocket,
    /// Kept alive rather than used: closing the request handle before the
    /// socket would take the connection with it.
    _request: crate::sys::Request,
    _connect: crate::sys::Connect,
    ex: std::sync::Arc<Exchange>,
    rx: Rx,
    tx: Tx,
    /// A completion the *sending* half pulled off the queue that belongs
    /// to the receiving half, and the reverse.
    ///
    /// One queue serves both halves, because one `Exchange` serves one
    /// handle and the socket is one handle. `Sink::poll_flush` waiting for
    /// a `WsWrote` can therefore see a `WsRead`, and dropping it would
    /// lose a message that is already in the buffer.
    stashed_read: Option<Event>,
    stashed_write: Option<Event>,
    /// The parts of a message WinHTTP has reported so far.
    partial: BytesMut,
    /// Whether the message being assembled is text — decided by the first
    /// fragment, because RFC 6455 §5.4 fixes a message's type on its
    /// first frame.
    partial_text: bool,
}

/// What the receiving half is doing.
#[derive(Debug, PartialEq, Eq)]
enum Rx {
    /// Nothing asked for yet.
    Idle,
    /// A `WinHttpWebSocketReceive` is in flight.
    Reading,
    /// The peer's close has been delivered, or the connection broke and
    /// the error was reported. A stream that has ended stays ended.
    Ended,
}

/// What the sending half is doing.
#[derive(Debug, PartialEq, Eq)]
enum Tx {
    Idle,
    /// A `WinHttpWebSocketSend` is in flight; its buffer is lent.
    Sending,
    /// A close frame has been sent, so nothing more may be.
    Closed,
}

impl std::fmt::Debug for WinHttpWebSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinHttpWebSocket")
            .field("rx", &self.rx)
            .field("tx", &self.tx)
            .finish_non_exhaustive()
    }
}

impl WinHttpWebSocket {
    /// The next completion for one half, stashing the other half's.
    ///
    /// `want_read` picks which queue this call is draining for. The event
    /// that does not belong goes to the other half's slot; one slot each
    /// is enough because WinHTTP allows one receive and one send in flight
    /// at a time, so there can be at most one of each outstanding.
    fn poll_event(&mut self, cx: &mut Context<'_>, want_read: bool) -> Poll<Event> {
        if let Some(e) = if want_read {
            self.stashed_read.take()
        } else {
            self.stashed_write.take()
        } {
            return Poll::Ready(e);
        }
        loop {
            let e = ready!(self.ex.poll_next(cx));
            let is_read = matches!(e, Event::WsRead { .. });
            // A failure belongs to whoever is waiting: it ends the socket
            // either way, and handing it to the other half would leave
            // this one waiting for a completion that will never come.
            let is_failure = matches!(e, Event::Failed(_) | Event::SecureFailure(_));
            if is_failure || is_read == want_read {
                return Poll::Ready(e);
            }
            if is_read {
                self.stashed_read = Some(e);
            } else {
                self.stashed_write = Some(e);
            }
        }
    }
}

/// A WinHTTP failure, as this seam's error.
fn failed(e: &Event) -> Error {
    match e {
        Event::Failed(code) => Error::new(
            ErrorKind::Body,
            WinHttpError::Call {
                call: "WinHttpWebSocket*",
                source: Win32Error(*code),
            },
        ),
        Event::SecureFailure(flags) => Error::new(
            ErrorKind::Tls,
            WinHttpError::Call {
                call: "SECURE_FAILURE",
                source: Win32Error(*flags),
            },
        ),
        other => Error::new(
            ErrorKind::Body,
            WinHttpError::OutOfOrder {
                got: event_name(other),
                expected: "a WebSocket completion",
            },
        ),
    }
}

impl Stream for WinHttpWebSocket {
    type Item = Result<Message, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match this.rx {
                Rx::Ended => return Poll::Ready(None),
                Rx::Idle => {
                    if let Err(e) = this.socket.receive(&this.ex) {
                        this.rx = Rx::Ended;
                        return Poll::Ready(Some(Err(Error::new(
                            ErrorKind::Body,
                            WinHttpError::Call {
                                call: "WinHttpWebSocketReceive",
                                source: Win32Error(e.0),
                            },
                        ))));
                    }
                    this.rx = Rx::Reading;
                }
                Rx::Reading => {
                    let e = ready!(this.poll_event(cx, true));
                    let Event::WsRead { bytes, kind } = e else {
                        this.rx = Rx::Ended;
                        return Poll::Ready(Some(Err(failed(&e))));
                    };
                    this.rx = Rx::Idle;
                    let chunk = this.ex.take_read(bytes);

                    if kind == w::WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE {
                        this.rx = Rx::Ended;
                        // The code and reason are a separate query: the
                        // completion says only that a close arrived.
                        let frame = match this.socket.close_status() {
                            Ok((code, reason)) => Some(CloseFrame {
                                code,
                                // Lossy, and it is the one place in this
                                // file where that is right: the message
                                // is already ending, and a reason that
                                // will not decode is not worth turning a
                                // clean close into an error over. RFC
                                // 6455 §5.5.1 requires UTF-8 and a peer
                                // that broke that has told us enough.
                                reason: String::from_utf8_lossy(&reason).into_owned(),
                            }),
                            Err(_) => None,
                        };
                        return Poll::Ready(Some(Ok(Message::Close(frame))));
                    }

                    let text = kind == w::WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE
                        || kind == w::WINHTTP_WEB_SOCKET_UTF8_FRAGMENT_BUFFER_TYPE;
                    let whole = kind == w::WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE
                        || kind == w::WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE;

                    if this.partial.is_empty() {
                        this.partial_text = text;
                    }
                    this.partial.extend_from_slice(&chunk);
                    if !whole {
                        // A fragment: ask for the rest. The loop starts
                        // another receive rather than returning
                        // `Pending`, because there is nothing for the
                        // caller to be told yet.
                        continue;
                    }
                    let body = std::mem::take(&mut this.partial).freeze();
                    let msg = if this.partial_text {
                        match String::from_utf8(body.to_vec()) {
                            Ok(s) => Message::Text(s),
                            Err(_) => {
                                this.rx = Rx::Ended;
                                return Poll::Ready(Some(Err(Error::new(
                                    ErrorKind::Body,
                                    WinHttpError::Unsupported(
                                        "the peer sent a text message that is not UTF-8".to_owned(),
                                    ),
                                ))));
                            }
                        }
                    } else {
                        Message::Binary(body)
                    };
                    return Poll::Ready(Some(Ok(msg)));
                }
            }
        }
    }
}

impl Sink<Message> for WinHttpWebSocket {
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.poll_flush(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        let this = self.get_mut();
        if this.tx == Tx::Closed {
            return Err(Error::new(
                ErrorKind::Body,
                WinHttpError::Unsupported("the WebSocket is closing or closed".to_owned()),
            ));
        }
        let (kind, payload) = match item {
            Message::Text(s) => (
                w::WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE,
                Bytes::from(s.into_bytes()),
            ),
            Message::Binary(b) => (w::WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE, b),
            Message::Close(frame) => {
                // `WinHttpWebSocketClose` rather than a send: WinHTTP
                // builds the frame, and it is synchronous in the sense
                // that matters here — the reason buffer is copied before
                // it returns.
                let (code, reason) = frame.map_or((1000, String::new()), |f| (f.code, f.reason));
                this.tx = Tx::Closed;
                return this.socket.close(code, reason.as_bytes()).map_err(|e| {
                    Error::new(
                        ErrorKind::Body,
                        WinHttpError::Call {
                            call: "WinHttpWebSocketClose",
                            source: Win32Error(e.0),
                        },
                    )
                });
            }
        };
        this.socket
            .send(&this.ex, kind, payload)
            .map_err(|e| {
                Error::new(
                    ErrorKind::Body,
                    WinHttpError::Call {
                        call: "WinHttpWebSocketSend",
                        source: Win32Error(e.0),
                    },
                )
            })
            .inspect(|()| this.tx = Tx::Sending)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let this = self.get_mut();
        if this.tx != Tx::Sending {
            return Poll::Ready(Ok(()));
        }
        let e = ready!(this.poll_event(cx, false));
        match e {
            Event::WsWrote => {
                this.tx = Tx::Idle;
                Poll::Ready(Ok(()))
            }
            other => {
                this.tx = Tx::Closed;
                Poll::Ready(Err(failed(&other)))
            }
        }
    }

    /// **Closing the sink does not wait for the peer's answer**, and the
    /// seam says why: `Message::Close` is queued, and a caller who needs
    /// the peer's close keeps polling the `Stream` until it ends.
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        ready!(self.as_mut().poll_flush(cx))?;
        let this = self.get_mut();
        if this.tx == Tx::Closed {
            return Poll::Ready(Ok(()));
        }
        this.tx = Tx::Closed;
        Poll::Ready(this.socket.close(1000, b"").map_err(|e| {
            Error::new(
                ErrorKind::Body,
                WinHttpError::Call {
                    call: "WinHttpWebSocketClose",
                    source: Win32Error(e.0),
                },
            )
        }))
    }
}

impl WebSocket for WinHttpWebSocket {}

impl WebSocketConnect for WinHttp {
    type WebSocket = WinHttpWebSocket;

    /// RFC 6455's handshake, almost none of which is here.
    ///
    /// `ws://` and `wss://` read as `http://` and `https://` — the seam
    /// fixes that, and it costs one match because WinHTTP is told the
    /// scheme as a flag rather than a string. Everything else the request
    /// carries goes out: **the headers are added rather than dropped**,
    /// which is the seam's own rule, and it is a rule this backend can
    /// keep where a browser cannot.
    ///
    /// The `101` is checked by WinHTTP, not here: the
    /// `Sec-WebSocket-Accept` comparison is inside the DLL, and
    /// `WinHttpWebSocketCompleteUpgrade` refuses a response that is not a
    /// completed handshake.
    async fn websocket(&self, req: http::Request<()>) -> Result<Self::WebSocket, Error> {
        let (parts, ()) = req.into_parts();
        let (secure, host, port, target) = split_uri(&ws_scheme(parts.uri)?)?;
        let headers = header_block(&parts.headers)?;

        let connect = self
            .session()
            .connect(&host, port)
            .map_err(|e| setup("WinHttpConnect", e))?;
        // GET, and the seam says the method it was handed is ignored:
        // RFC 6455 §4.1 fixes it.
        let request = connect
            .open_request("GET", &target, secure)
            .map_err(|e| setup("WinHttpOpenRequest", e))?;

        let ex = std::sync::Arc::new(Exchange::new());
        request
            .set_context(&ex)
            .map_err(|e| setup("WinHttpSetOption(CONTEXT_VALUE)", e))?;
        request
            .disable_redirects_and_cookies()
            .map_err(|e| setup("WinHttpSetOption(DISABLE_FEATURE)", e))?;
        // Before the send, and that is the whole of the handshake this
        // crate writes.
        request
            .upgrade_to_websocket()
            .map_err(|e| setup("WinHttpSetOption(UPGRADE_TO_WEB_SOCKET)", e))?;
        request
            .add_headers(&headers)
            .map_err(|e| setup("WinHttpAddRequestHeaders", e))?;
        request
            .send(&ex, None)
            .map_err(|e| setup("WinHttpSendRequest", e))?;

        // Dropping this future from here closes the request handle, which
        // cancels the handshake — `Transport::execute`'s terms, which the
        // seam points at for its own cancellation.
        expect(&ex, "SENDREQUEST_COMPLETE").await?;
        request
            .receive_response()
            .map_err(|e| setup("WinHttpReceiveResponse", e))?;
        expect(&ex, "HEADERS_AVAILABLE").await?;

        let socket = request
            .complete_upgrade(&ex)
            .map_err(|e| setup("WinHttpWebSocketCompleteUpgrade", e))?;

        Ok(WinHttpWebSocket {
            socket,
            _request: request,
            _connect: connect,
            ex,
            rx: Rx::Idle,
            tx: Tx::Idle,
            stashed_read: None,
            stashed_write: None,
            partial: BytesMut::new(),
            partial_text: false,
        })
    }
}

/// `ws://` and `wss://` as the two schemes WinHTTP knows.
///
/// A pure function, and a test of its own, because it is the one part of
/// the handshake this crate decides: everything else is WinHTTP's. An
/// `http://` or `https://` URI passes through, which the seam requires —
/// a caller holding an origin should not have to rewrite its scheme.
fn ws_scheme(uri: http::Uri) -> Result<http::Uri, Error> {
    let swapped = match uri.scheme_str() {
        Some("ws") => "http",
        Some("wss") => "https",
        // `split_uri` refuses anything else by name, so this hands the
        // refusal to the one place that already words it.
        _ => return Ok(uri),
    };
    let mut parts = uri.into_parts();
    parts.scheme = Some(swapped.parse().expect("http and https parse as schemes"));
    http::Uri::from_parts(parts).map_err(|e| {
        Error::new(
            ErrorKind::Unsupported,
            WinHttpError::Unsupported(format!("the WebSocket URI cannot be rewritten: {e}")),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scheme rewrite is the one part of the handshake this crate
    /// decides, so it is the one part with a test.
    #[test]
    fn ws_and_wss_become_http_and_https() {
        let go = |u: &str| {
            ws_scheme(u.parse().expect("a uri"))
                .expect("rewritable")
                .to_string()
        };
        assert_eq!(go("ws://e.test/chat"), "http://e.test/chat");
        assert_eq!(go("wss://e.test/chat"), "https://e.test/chat");
    }

    /// **`http://` and `https://` pass through**, which the seam requires
    /// in as many words: a caller who already holds an origin should not
    /// have to rewrite its scheme to open a socket to it.
    #[test]
    fn http_and_https_are_left_alone() {
        for u in ["http://e.test/x", "https://e.test/x?q=1"] {
            let out = ws_scheme(u.parse().expect("a uri")).expect("unchanged");
            assert_eq!(out.to_string(), u);
        }
    }

    /// The path and query survive the rewrite.
    ///
    /// A rewrite that rebuilt the URI from its scheme and authority would
    /// pass the two tests above and drop the target — which is the
    /// request line, so every socket would open at `/`.
    #[test]
    fn the_target_survives_the_rewrite() {
        let out =
            ws_scheme("wss://e.test/a/b?q=1&r=2".parse().expect("a uri")).expect("rewritable");
        assert_eq!(
            out.path_and_query().map(ToString::to_string).as_deref(),
            Some("/a/b?q=1&r=2")
        );
        assert_eq!(
            out.authority().map(ToString::to_string).as_deref(),
            Some("e.test")
        );
    }

    /// A scheme this crate does not know is handed on unchanged, so that
    /// `split_uri` refuses it in the one place that words the refusal.
    ///
    /// Asserted because the alternative — refusing here — would give two
    /// messages for one mistake, and the one a caller met would depend on
    /// whether they opened a socket or made a request.
    #[test]
    fn an_unknown_scheme_is_left_for_split_uri_to_refuse() {
        let out = ws_scheme("ftp://e.test/x".parse().expect("a uri")).expect("passed through");
        assert_eq!(out.scheme_str(), Some("ftp"));
    }
}
