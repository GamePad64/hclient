//! WebSocket over `NSURLSessionWebSocketTask`.
//!
//! # Why this is in this crate
//!
//! The rule that puts framing in a crate of its own is about a
//! **dependency**: `hclient-tungstenite` exists because a `websocket`
//! feature on `hclient-native` would have put `tungstenite` into every
//! build in any graph that switched it on. There is nothing to spread
//! here — `NSURLSessionWebSocketTask` is in the `objc2-foundation`
//! feature this crate already names, so the whole thing costs **zero
//! crates**. `hclient-fetch` and `hclient-winhttp` keep their seam impls
//! at home for the same reason.
//!
//! # The third platform to fit a seam shaped around the first
//!
//! `WebSocketConnect` hands over **messages**, not an upgraded socket,
//! because that is all a browser can give. WinHTTP turned out to be the
//! same shape, and so is this: Foundation delivers an
//! `NSURLSessionWebSocketMessage` and takes one back, and the framing,
//! the masking, the handshake and the ping/pong are all inside the
//! system. Three platforms that share no code agree on the shape, which
//! is what says the seam is not the browser's accident.
//!
//! `Message` has no `Ping`/`Pong` here either, and for once that is not
//! an absence: `sendPingWithPongReceiveHandler:` exists, but it is a
//! *health check* with its own callback rather than a message in the
//! stream — the same thing `WebSocketKeepAlive` is one crate over, and
//! not something a `Stream` of messages should yield.
//!
//! # Pull, not pump
//!
//! `receiveMessageWithCompletionHandler:` delivers **one** message and
//! must be called again for the next, so a backend has a choice: call it
//! in a loop and buffer whatever arrives, or call it when the caller asks.
//! This asks. A pump would read ahead of a caller who stopped reading,
//! which is the unbounded queue `hclient-fetch`'s body pump exists to
//! avoid — and here the choice is free, because the seam's `poll_next` is
//! already the request.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};

use block2::RcBlock;
use futures_channel::oneshot;
use hclient_core::unversioned::{CloseFrame, Message, WebSocket, WebSocketConnect};
use hclient_core::{Error, ErrorKind};
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_foundation::{
    NSData, NSMutableURLRequest, NSString, NSURL, NSURLSessionWebSocketCloseCode,
    NSURLSessionWebSocketMessage, NSURLSessionWebSocketMessageType, NSURLSessionWebSocketTask,
};

use crate::error::UrlSessionError;
use crate::session::UrlSession;

/// An open WebSocket over `NSURLSession`.
pub struct UrlSessionWebSocket {
    task: Retained<NSURLSessionWebSocketTask>,
    /// The receive in flight, if the caller has asked for one.
    receiving: Option<oneshot::Receiver<Result<Message, Error>>>,
    /// The send in flight, if there is one.
    sending: Option<oneshot::Receiver<Result<(), Error>>>,
    /// Set once the peer's close has been delivered or the connection
    /// broke and the error was reported. A stream that has ended stays
    /// ended.
    ended: bool,
    /// Set once a close has been sent, so nothing more may be.
    closed: bool,
}

impl std::fmt::Debug for UrlSessionWebSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UrlSessionWebSocket")
            .field("ended", &self.ended)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

/// A completion handler's half of a `oneshot`.
///
/// **`Arc<Mutex<Option<..>>>` rather than the sender itself**, because a
/// completion handler is an `FnMut` that Foundation may hold after it has
/// run, and a `oneshot::Sender` is consumed by sending. Taking it out of
/// the slot makes a second call a no-op rather than a panic — and a second
/// call is Foundation's to make, not this crate's to rule out.
type Slot<T> = Arc<Mutex<Option<oneshot::Sender<T>>>>;

fn slot<T>() -> (Slot<T>, oneshot::Receiver<T>) {
    let (tx, rx) = oneshot::channel();
    (Arc::new(Mutex::new(Some(tx))), rx)
}

/// Delivers into a slot, ignoring a second delivery and a dropped
/// receiver — both are ordinary: the caller may have gone away.
fn deliver<T>(slot: &Slot<T>, value: T) {
    if let Some(tx) = slot.lock().expect("urlsession slot poisoned").take() {
        let _ = tx.send(value);
    }
}

/// An `NSError` as this seam's error.
fn ns_error(e: &objc2_foundation::NSError, what: &str) -> Error {
    Error::new(
        ErrorKind::Body,
        UrlSessionError(format!("{what}: {}", e.localizedDescription())),
    )
}

impl UrlSessionWebSocket {
    /// Asks Foundation for the next message, if nothing is asked yet.
    fn start_receive(&mut self) {
        if self.receiving.is_some() {
            return;
        }
        let (slot, rx) = slot::<Result<Message, Error>>();
        let handler = RcBlock::new(
            move |msg: *mut NSURLSessionWebSocketMessage, err: *mut objc2_foundation::NSError| {
                // SAFETY: Foundation passes either a message or an error,
                // both autoreleased and valid for the call. Null-checked
                // rather than trusted, because "either" is a documented
                // contract and not a type.
                let msg = unsafe { msg.as_ref() }; // unsafe-code-exception: amendment-C11
                let err = unsafe { err.as_ref() }; // unsafe-code-exception: amendment-C11
                let out = match (msg, err) {
                    (_, Some(e)) => Err(ns_error(e, "receiving a WebSocket message")),
                    (Some(m), None) => convert(m),
                    (None, None) => Err(Error::new(
                        ErrorKind::Body,
                        UrlSessionError(
                            "NSURLSession delivered neither a message nor an error".to_owned(),
                        ),
                    )),
                };
                deliver(&slot, out);
            },
        );
        // SAFETY: the block is retained by Foundation for the length of
        // the call and released after; `RcBlock` is what owns it here.
        unsafe { self.task.receiveMessageWithCompletionHandler(&handler) }; // unsafe-code-exception: amendment-C11
        self.receiving = Some(rx);
    }

    /// The peer's close code and reason, once the task has one.
    ///
    /// Foundation reports a close through the *error* of a pending
    /// receive rather than as a message, so this is asked after that
    /// error rather than instead of it — which is why the code below
    /// turns one particular error into `Message::Close` rather than
    /// reporting it.
    fn close_frame(&self) -> Option<CloseFrame> {
        let code = self.task.closeCode();
        if code == NSURLSessionWebSocketCloseCode::Invalid {
            return None;
        }
        let reason = self.task.closeReason().map_or_else(String::new, |d| {
            String::from_utf8_lossy(unsafe { d.as_bytes_unchecked() }).into_owned() // unsafe-code-exception: amendment-C11
        });
        Some(CloseFrame {
            code: u16::try_from(code.0).unwrap_or(1005),
            reason,
        })
    }
}

/// An `NSURLSessionWebSocketMessage` as this seam's [`Message`].
fn convert(m: &NSURLSessionWebSocketMessage) -> Result<Message, Error> {
    match m.r#type() {
        // `type` says which accessor is populated, which is
        // Foundation's own discriminator — the same shape
        // `hclient-winhttp` reads off `WINHTTP_WEB_SOCKET_BUFFER_TYPE`.
        // Neither accessor needs `unsafe`: `objc2` 0.6 declares both
        // safe, which is why this file has fewer `unsafe` blocks than
        // the FFI it wraps would suggest.
        NSURLSessionWebSocketMessageType::String => {
            let s = m.string();
            s.map(|s| Message::Text(s.to_string())).ok_or_else(|| {
                Error::new(
                    ErrorKind::Body,
                    UrlSessionError("a string message carried no string".to_owned()),
                )
            })
        }
        _ => {
            let d = m.data();
            d.map(|d| {
                // SAFETY: the bytes are read and copied before this
                // returns; `NSData` owns them for the length of the
                // borrow.
                //
                // **The borrow is bound to a local on its own line**, and
                // that is not style: `cargo fmt` reflows a longer
                // expression and carries the trailing marker off the
                // `unsafe` line with it, which `just unsafe-policy` then
                // reports — this workspace's own recurring finding about
                // markers and the formatter, met here for the fourth
                // time.
                let raw = unsafe { d.as_bytes_unchecked() }; // unsafe-code-exception: amendment-C11
                Message::Binary(bytes::Bytes::copy_from_slice(raw))
            })
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Body,
                    UrlSessionError("a data message carried no data".to_owned()),
                )
            })
        }
    }
}

impl futures_core::Stream for UrlSessionWebSocket {
    type Item = Result<Message, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.ended {
            return Poll::Ready(None);
        }
        this.start_receive();
        let rx = this.receiving.as_mut().expect("just started");
        let out = ready!(Pin::new(rx).poll(cx));
        this.receiving = None;
        match out {
            Ok(Ok(msg)) => Poll::Ready(Some(Ok(msg))),
            Ok(Err(e)) => {
                this.ended = true;
                // **A close arrives as a failed receive, not as a
                // message.** Foundation reports the peer's close by
                // failing whatever receive was outstanding, and puts the
                // code on the task — so the honest reading of that error
                // is `Message::Close`, and only an error with no close
                // code behind it is a failure to report.
                match this.close_frame() {
                    Some(frame) => Poll::Ready(Some(Ok(Message::Close(Some(frame))))),
                    None => Poll::Ready(Some(Err(e))),
                }
            }
            // The completion handler was dropped without running, which
            // happens when the task is torn down under us.
            Err(oneshot::Canceled) => {
                this.ended = true;
                Poll::Ready(None)
            }
        }
    }
}

impl futures_sink::Sink<Message> for UrlSessionWebSocket {
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.poll_flush(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        let this = self.get_mut();
        if this.closed {
            return Err(Error::new(
                ErrorKind::Body,
                UrlSessionError("the WebSocket is closing or closed".to_owned()),
            ));
        }
        let msg = match item {
            Message::Text(s) => NSURLSessionWebSocketMessage::initWithString(
                NSURLSessionWebSocketMessage::alloc(),
                &NSString::from_str(&s),
            ),
            Message::Binary(b) => NSURLSessionWebSocketMessage::initWithData(
                NSURLSessionWebSocketMessage::alloc(),
                &NSData::with_bytes(&b),
            ),
            Message::Close(frame) => {
                // `cancelWithCloseCode:reason:` is the close handshake —
                // a *task* operation rather than a message, which is why
                // this arm sends nothing.
                let (code, reason) = frame.map_or((1000, String::new()), |f| (f.code, f.reason));
                let data = NSData::with_bytes(reason.as_bytes());
                this.closed = true;
                {
                    this.task.cancelWithCloseCode_reason(
                        NSURLSessionWebSocketCloseCode(
                            // RFC 6455 §7.4 codes are 1000..=4999, so this
                            // cannot truncate; the fallback is the "normal
                            // closure" the arm above defaults to anyway.
                            isize::try_from(code).unwrap_or(1000),
                        ),
                        Some(&data),
                    );
                }
                return Ok(());
            }
        };
        let (slot, rx) = slot::<Result<(), Error>>();
        let handler = RcBlock::new(move |err: *mut objc2_foundation::NSError| {
            // SAFETY: null on success, an autoreleased `NSError` on
            // failure — Foundation's own convention for a completion
            // handler with no value.
            let err = unsafe { err.as_ref() }; // unsafe-code-exception: amendment-C11
            deliver(
                &slot,
                err.map_or(Ok(()), |e| Err(ns_error(e, "sending a WebSocket message"))),
            );
        });
        // SAFETY: the message and the block are both retained for the
        // length of the call by Foundation.
        unsafe { this.task.sendMessage_completionHandler(&msg, &handler) }; // unsafe-code-exception: amendment-C11
        this.sending = Some(rx);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let this = self.get_mut();
        let Some(rx) = this.sending.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        let out = ready!(Pin::new(rx).poll(cx));
        this.sending = None;
        match out {
            Ok(r) => Poll::Ready(r),
            Err(oneshot::Canceled) => Poll::Ready(Err(Error::new(
                ErrorKind::Body,
                UrlSessionError("the send was abandoned by NSURLSession".to_owned()),
            ))),
        }
    }

    /// **Does not wait for the peer's answer**, which the seam requires:
    /// a close is queued, and a caller who needs the peer's own keeps
    /// polling the `Stream` until it ends.
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        ready!(self.as_mut().poll_flush(cx))?;
        let this = self.get_mut();
        if this.closed {
            return Poll::Ready(Ok(()));
        }
        this.closed = true;
        this.task
            .cancelWithCloseCode_reason(NSURLSessionWebSocketCloseCode(1000), None);
        Poll::Ready(Ok(()))
    }
}

impl WebSocket for UrlSessionWebSocket {}

impl WebSocketConnect for UrlSession {
    type WebSocket = UrlSessionWebSocket;

    /// RFC 6455's handshake, none of which is here.
    ///
    /// `ws://` and `wss://` are what `NSURLSessionWebSocketTask` takes,
    /// and `http://`/`https://` are accepted by it as well — so the seam's
    /// rule that all four name the same two costs nothing to keep.
    ///
    /// **Headers go out**, which is the seam's rule and one this backend
    /// can keep where a browser cannot: `NSMutableURLRequest` carries
    /// them, and Foundation adds its own handshake fields around them.
    async fn websocket(&self, req: http::Request<()>) -> Result<Self::WebSocket, Error> {
        let url = NSString::from_str(&req.uri().to_string());
        let Some(url) = NSURL::URLWithString(&url) else {
            return Err(Error::new(
                ErrorKind::Connect,
                UrlSessionError(format!("`{}` is not a URL NSURL accepts", req.uri())),
            ));
        };
        let request = NSMutableURLRequest::requestWithURL(&url);
        for (name, value) in req.headers() {
            let Ok(value) = value.to_str() else { continue };
            request.setValue_forHTTPHeaderField(
                Some(&NSString::from_str(value)),
                &NSString::from_str(name.as_str()),
            );
        }
        let task = self.session.webSocketTaskWithRequest(&request);
        // **`resume` and no wait.** `NSURLSessionWebSocketTask` opens
        // lazily: the handshake runs with the first send or receive, and
        // its failure is reported through that call rather than here.
        // Waiting for an open would mean a delegate callback and a
        // second state machine for a fact the first operation already
        // carries.
        task.resume();
        Ok(UrlSessionWebSocket {
            task,
            receiving: None,
            sending: None,
            ended: false,
            closed: false,
        })
    }
}
