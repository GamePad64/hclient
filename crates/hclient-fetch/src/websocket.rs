//! WebSocket over the browser's own `WebSocket` global, behind
//! [`hclient_core::unversioned::WebSocketConnect`].
//!
//! # Why this file is the acceptance for the seam rather than a second backend
//!
//! The seam is message oriented rather than "hand back the socket after
//! the 101", on one argument: a byte-stream seam is
//! implementable by exactly one of this project's four backends, and the
//! three it excludes include the browser. This crate is where that
//! argument was written down — `caps.rs` used to set
//! `UpgradeSupport::None` with the comment *"`WebSocket` in the browser is
//! a wholly separate global, unreachable from a `fetch`-shaped
//! `Transport`"* — and this file is the same sentence with the conclusion
//! turned round: unreachable from a `Transport`, reachable from a trait of
//! its own.
//!
//! Nothing here can produce a byte. The `WebSocket` global hands out
//! *messages*, framed and unmasked by the browser, and there is no API at
//! any level that would give this crate the socket underneath. So the
//! seam's shape is not a preference this backend happens to share; it is
//! the only shape this backend could have implemented at all.
//!
//! # What it cost: the headers, and refusing them precisely
//!
//! [`WebSocketConnect::websocket`] takes an `http::Request<()>` and puts
//! one duty on the implementer — *a backend that cannot send a header the
//! request carries must fail rather than drop it*. The browser's
//! constructor is `new WebSocket(url, protocols)`: a URL and a subprotocol
//! list, and **nothing else**. There is no `Authorization`, no `Origin`
//! (the browser sets its own), no `Cookie` (likewise), no
//! `Sec-WebSocket-Key` (the browser generates one and checks the answer
//! itself), no `Host`.
//!
//! So [`subprotocols`] refuses **every** header except
//! `Sec-WebSocket-Protocol`, with an error naming the header it could not
//! send. That is a lot to refuse, and it is the honest amount:
//! `hclient-native` pays the same duty in the other direction — it refuses
//! the four headers its own handshake owns rather than overwriting them —
//! and the rule is the same rule. The one header that *is* reachable is
//! not special-cased into silence either: its comma-separated tokens
//! become the constructor's second argument, and
//! `a_subprotocol_reaches_the_constructor` reads them back from the
//! constructor's own arguments.
//!
//! Note what this makes impossible, because it is a real cost rather than
//! a formality: **a token in an `Authorization` header cannot open a
//! WebSocket from a browser.** That is the browser's limitation, not this
//! crate's, and the way round it — a ticket in the URL's query, or a
//! subprotocol carrying the credential — is the caller's decision to make.
//! A backend that dropped the header would have made it silently.
//!
//! # The bridge: events in, `Stream`/`Sink` out
//!
//! The browser's `WebSocket` is event driven (`onopen`, `onmessage`,
//! `onclose`), not poll driven, which is the same shape [`crate::promise`]
//! already crosses for a `Promise`: a shared state cell, a `Waker` parked
//! in it, and closures that fill it and wake. This file follows that
//! idiom, with two deliberate differences.
//!
//! **`Rc<RefCell<..>>`, not `Arc<Mutex<..>>` — so no `unsafe`.**
//! `promise.rs` carries this project's one `unsafe impl Send`, needed
//! because `Transport::execute`'s future has to be `Send` for
//! `Client::execute`. The WebSocket seam declares no `Send` anywhere —
//! `hclient-core/tests/shape.rs`'s
//! `a_non_send_backend_still_satisfies_the_websocket_seam` pinned that
//! against a synthetic `!Send` backend when the trait landed, and
//! [`FetchWebSocket`] is the real one it was predicting. The seam did not
//! have to be widened for it, and no second `unsafe` was needed to make
//! this backend fit.
//!
//! **The queue is a queue.** Messages arrive when the browser says so, not
//! when the caller polls, and a caller that awaits something else between
//! two `next()` calls must not lose what came in meanwhile. `Shared::queue`
//! is a `VecDeque` and every message goes on the back of it;
//! `two_messages_that_arrive_before_the_first_poll_are_both_kept` is the
//! test, and a single-slot state is the mutation it kills.
//!
//! # Three event handlers, and why there is no fourth
//!
//! `onerror` is deliberately **not** installed. The `error` event on a
//! `WebSocket` is a bare `Event` carrying no information at all — that is
//! deliberate in the standard, so a page cannot use a WebSocket to probe
//! the network it is on — and the standard fires `close` after every
//! `error` it fires ("fail the WebSocket connection" is *error, then
//! close*). So everything an `onerror` could tell us, `onclose` tells us
//! with a close code attached, and a handler that only ever produced "it
//! failed, and the browser will not say why" would be a second, worse
//! answer racing the first.
//!
//! What `onclose` does with that is the one place this file interprets
//! rather than forwards:
//!
//! - **before the socket ever opened** — the handshake failed, and
//!   [`WebSocketConnect::websocket`] returns an error. There is no other
//!   outcome: a server that accepted and then closed would have fired
//!   `open` first.
//! - **`wasClean`** — the close handshake completed, so this is the peer's
//!   [`Message::Close`], and the `Stream` ends after it. Close code `1005`
//!   becomes `Close(None)` rather than `Close(Some(1005))`, because 1005
//!   is RFC 6455 §7.4.1's *"no status code was actually present"* and is
//!   the browser's way of reporting the empty close payload that
//!   `tungstenite` reports to `hclient-native` as `None`.
//! - **not `wasClean`** — the connection broke (code `1006`, and no close
//!   frame was received). That is an error on the `Stream`, not a
//!   `Close(1006)` message: 1006 is a code RFC 6455 forbids on the wire,
//!   and delivering it as a close *message* would tell a caller that only
//!   inspects `Message::Close` that its peer said goodbye.
//!
//! # What the browser will not give back, and is not faked here
//!
//! **Backpressure.** [`Sink::poll_ready`] and [`Sink::poll_flush`] are
//! `Ready(Ok(()))` whenever the socket is open, and `bufferedAmount` is
//! not consulted — because there is no event, anywhere in the platform,
//! that fires when a `WebSocket`'s send buffer drains. Returning `Pending`
//! on a non-empty buffer would be returning it with no waker that anything
//! will ever wake; the alternative is a `setTimeout` poll loop, which is
//! the busy-spin this workspace measured and rejected once already
//! (`hclient_native::testing::blocking_io`, 600 ms of wall time and 600 ms
//! of CPU). `hclient-native`'s `Sink` gives real backpressure from the
//! socket; this one gives none and the browser's own buffer is unbounded
//! — a difference between the two backends rather than a property of the
//! seam.
//!
//! **A send that goes nowhere.** The standard says `send()` on a `CLOSING`
//! or `CLOSED` socket discards the data and *does not throw* — it only
//! adds to `bufferedAmount`, which is the closest thing to a silent drop
//! in the whole API. [`FetchWebSocket::sendable`] checks `readyState`
//! first and returns a typed error instead, so a caller learns its message
//! did not go out.
use crate::convert;
use bytes::Bytes;
use futures_core::Stream;
use futures_sink::Sink;
use hclient_core::unversioned::{CloseFrame, Message, WebSocket, WebSocketConnect};
use hclient_core::{Error, ErrorKind};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

/// The only header the browser can send, and it does not send it as a
/// header: `new WebSocket(url, protocols)`'s second argument is what
/// becomes `Sec-WebSocket-Protocol` on the wire.
const SENDABLE: http::HeaderName = http::header::SEC_WEBSOCKET_PROTOCOL;

/// RFC 6455 §7.4.1: *"reserved… indicates that no status code was actually
/// present"*. It is never sent on the wire; the browser reports it when the
/// peer's close frame carried an empty payload, which is exactly the case
/// `tungstenite` reports to `hclient-native` as `Message::Close(None)`.
const NO_STATUS_RECEIVED: u16 = 1005;

#[derive(Debug, thiserror::Error)]
#[error(
    "the browser's WebSocket cannot send a `{0}` header: `new WebSocket(url, protocols)` takes a \
     URL and a subprotocol list, and nothing else reaches the handshake"
)]
struct HeaderNotSendable(http::HeaderName);

#[derive(Debug, thiserror::Error)]
#[error("a `{0}` value that is not visible ASCII cannot become a subprotocol")]
struct BadSubprotocol(http::HeaderName);

#[derive(Debug, thiserror::Error)]
#[error("unsupported URI scheme for a WebSocket: {0:?} (expected ws, wss, http or https)")]
struct UnsupportedScheme(String);

#[derive(Debug, thiserror::Error)]
#[error("a WebSocket URL needs a host: `{0}`")]
struct NoAuthority(String);

#[derive(Debug, thiserror::Error)]
#[error("the browser refused to construct a WebSocket: {0}")]
struct ConstructorRefused(String);

/// The browser tells us a close code and nothing else — see the module
/// doc's section on why there is no `onerror` handler to ask.
#[derive(Debug, thiserror::Error)]
#[error(
    "the WebSocket handshake failed (close code {0}); a browser reports no reason for a failed \
     WebSocket handshake, deliberately"
)]
struct HandshakeFailed(u16);

#[derive(Debug, thiserror::Error)]
#[error("the WebSocket closed without a close handshake (code {0})")]
struct ConnectionLost(u16);

#[derive(Debug, thiserror::Error)]
#[error("a WebSocket message was neither a string nor an ArrayBuffer")]
struct NotAMessage;

#[derive(Debug, thiserror::Error)]
#[error(
    "the WebSocket is not open (readyState {0}), and the browser would have discarded this \
     message without reporting anything"
)]
struct NotOpen(u16);

#[derive(Debug, thiserror::Error)]
#[error("the browser refused to send on the WebSocket: {0}")]
struct SendRefused(String);

/// `ws`/`wss` are RFC 6455's schemes; `http`/`https` are accepted as the
/// same two, for the reason [`WebSocketConnect::websocket`] gives and
/// `hclient-native`'s `as_http_uri` follows — a caller who already holds an
/// origin should not have to rewrite its scheme.
///
/// The result is a string rather than an `http::Uri` because the browser's
/// constructor takes one, and because `http::Uri` has no `ws` scheme
/// constant to round-trip through.
fn ws_url(uri: &http::Uri) -> Result<String, Error> {
    let scheme = match uri.scheme_str() {
        Some("ws" | "http") => "ws",
        Some("wss" | "https") => "wss",
        other => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                UnsupportedScheme(other.unwrap_or("").to_owned()),
            ));
        }
    };
    let authority = uri
        .authority()
        .ok_or_else(|| Error::new(ErrorKind::Unsupported, NoAuthority(uri.to_string())))?;
    let target = uri.path_and_query().map_or("/", |p| p.as_str());
    Ok(format!("{scheme}://{authority}{target}"))
}

/// The subprotocol list, and the refusal of everything else.
///
/// See the module doc: this function *is* the duty
/// [`WebSocketConnect::websocket`] puts on a backend, discharged in the
/// only direction the browser leaves open. The first header that is not
/// `Sec-WebSocket-Protocol` ends the call — naming itself, so a caller
/// learns which one rather than that there was one.
fn subprotocols(headers: &http::HeaderMap) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for (name, value) in headers {
        if name != SENDABLE {
            return Err(Error::new(
                ErrorKind::Unsupported,
                HeaderNotSendable(name.clone()),
            ));
        }
        let value = value
            .to_str()
            .map_err(|_| Error::new(ErrorKind::Unsupported, BadSubprotocol(name.clone())))?;
        // RFC 6455 §4.1: one header, a comma-separated list, and it may
        // appear more than once. `HeaderMap`'s iterator yields each
        // occurrence, so both spellings arrive here.
        out.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned),
        );
    }
    Ok(out)
}

fn construct(url: &str, protocols: &[String]) -> Result<web_sys::WebSocket, Error> {
    let made = if protocols.is_empty() {
        web_sys::WebSocket::new(url)
    } else {
        let list = js_sys::Array::new();
        for p in protocols {
            list.push(&JsValue::from_str(p));
        }
        web_sys::WebSocket::new_with_str_sequence(url, list.as_ref())
    };
    made.map_err(|e| {
        Error::new(
            ErrorKind::Connect,
            ConstructorRefused(convert::js_message(&e)),
        )
    })
}

/// What the browser handed us in a `message` event.
///
/// Text is a JS string; binary is an `ArrayBuffer`, because
/// [`WebSocketConnect::websocket`] sets `binaryType = "arraybuffer"` before
/// anything can arrive. Left at the default `"blob"`, every binary message
/// would need an asynchronous read before its bytes existed, and this
/// function would see a `Blob` and report [`NotAMessage`] — which is why
/// `binary_type_is_set_to_arraybuffer_before_anything_can_arrive` asserts
/// the setting rather than trusting it.
fn decode(data: JsValue) -> Result<Message, Error> {
    if let Some(text) = data.as_string() {
        return Ok(Message::Text(text));
    }
    if let Some(buf) = data.dyn_ref::<js_sys::ArrayBuffer>() {
        return Ok(Message::Binary(Bytes::from(
            js_sys::Uint8Array::new(buf).to_vec(),
        )));
    }
    Err(Error::new(ErrorKind::Decode, NotAMessage))
}

/// Everything the event handlers write and the `Stream`/connect future
/// read. One cell, one waker — the shape `promise::State` already has.
#[derive(Default)]
struct Shared {
    /// Received messages and the one terminal error, oldest first. A queue
    /// rather than a slot: see the module doc.
    queue: VecDeque<Result<Message, Error>>,
    /// The handshake's verdict, taken exactly once by [`Opening`].
    open: Option<Result<(), Error>>,
    /// Set by `onopen`. `onclose` reads it to tell a failed handshake from
    /// a closed connection — the two arrive through the same event.
    opened: bool,
    /// Set when the socket has produced its last item. A `Stream` that has
    /// ended stays ended, which this flag is what makes true.
    ended: bool,
    waker: Option<Waker>,
}

impl Shared {
    /// Takes the waker out under the borrow and hands it back to be woken
    /// *after* the borrow is released — the same order `promise::State`'s
    /// `finish` uses, and for the same reason: a waker is free to poll the
    /// task that owns this `RefCell`.
    fn wake_later(&mut self) -> Option<Waker> {
        self.waker.take()
    }
}

fn wake(w: Option<Waker>) {
    if let Some(w) = w {
        w.wake();
    }
}

/// The three closures, kept alive for exactly as long as the socket they
/// are installed on.
///
/// Held by [`Socket`] rather than by [`Shared`], which is not an
/// arrangement detail: a closure holds an `Rc<RefCell<Shared>>` of its
/// own, so `Shared` holding the closures back would be a reference cycle
/// and every WebSocket a browser opened would leak.
struct Handlers {
    _open: Closure<dyn FnMut()>,
    _message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _close: Closure<dyn FnMut(web_sys::CloseEvent)>,
}

/// The browser's socket and the closures installed on it, dropped
/// together and in that order.
///
/// [`Drop`] detaches the handlers **before** the [`Handlers`] fields are
/// dropped, because dropping a `Closure` invalidates the JS function it
/// installed and a later event would then throw inside the browser's own
/// dispatch, where nothing here could see it.
///
/// It also closes the socket, which is what discharges
/// [`WebSocketConnect::websocket`]'s cancellation contract: the connect
/// future owns this value while it waits for `open`, so dropping that
/// future tears the socket down rather than leaving a handshake running
/// for nobody. The same `Drop` is what closes an open socket whose
/// [`FetchWebSocket`] the caller dropped.
struct Socket {
    ws: web_sys::WebSocket,
    _handlers: Handlers,
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        // `close()` on an already-closed socket does nothing, per the
        // standard; there is nothing to check first.
        let _ = self.ws.close();
    }
}

/// Installs the three handlers. Called before the first `await`, so no
/// event can be delivered before all three exist — the browser's event
/// loop cannot run while this synchronous code does.
fn install(ws: &web_sys::WebSocket, shared: &Rc<RefCell<Shared>>) -> Handlers {
    let on_open = {
        let shared = Rc::clone(shared);
        Closure::wrap(Box::new(move || {
            let waker = {
                let mut s = shared.borrow_mut();
                s.opened = true;
                s.open = Some(Ok(()));
                s.wake_later()
            };
            wake(waker);
        }) as Box<dyn FnMut()>)
    };

    let on_message = {
        let shared = Rc::clone(shared);
        Closure::wrap(Box::new(move |e: web_sys::MessageEvent| {
            let item = decode(e.data());
            let waker = {
                let mut s = shared.borrow_mut();
                if s.ended {
                    None
                } else {
                    s.queue.push_back(item);
                    s.wake_later()
                }
            };
            wake(waker);
        }) as Box<dyn FnMut(web_sys::MessageEvent)>)
    };

    let on_close = {
        let shared = Rc::clone(shared);
        Closure::wrap(Box::new(move |e: web_sys::CloseEvent| {
            let waker = {
                let mut s = shared.borrow_mut();
                if s.ended {
                    None
                } else {
                    s.ended = true;
                    if !s.opened {
                        s.open = Some(Err(Error::new(
                            ErrorKind::Connect,
                            HandshakeFailed(e.code()),
                        )));
                    } else if e.was_clean() {
                        let frame = (e.code() != NO_STATUS_RECEIVED).then(|| CloseFrame {
                            code: e.code(),
                            reason: e.reason(),
                        });
                        s.queue.push_back(Ok(Message::Close(frame)));
                    } else {
                        s.queue
                            .push_back(Err(Error::new(ErrorKind::Body, ConnectionLost(e.code()))));
                    }
                    s.wake_later()
                }
            };
            wake(waker);
        }) as Box<dyn FnMut(web_sys::CloseEvent)>)
    };

    ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    Handlers {
        _open: on_open,
        _message: on_message,
        _close: on_close,
    }
}

/// Resolves when `onopen` or `onclose` has spoken, whichever comes first.
struct Opening<'a>(&'a Rc<RefCell<Shared>>);

impl Future for Opening<'_> {
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut s = self.0.borrow_mut();
        match s.open.take() {
            Some(verdict) => Poll::Ready(verdict),
            None => {
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// An open WebSocket in a browser tab.
///
/// `!Send`, and deliberately: it holds `Rc`s and `Closure`s, both of which
/// are the honest shape for an object bound to one JS event loop. The seam
/// declares no `Send` bound, so this costs nothing — see the module doc.
pub struct FetchWebSocket {
    socket: Socket,
    shared: Rc<RefCell<Shared>>,
}

impl Debug for FetchWebSocket {
    // `Closure` has no `Debug` (checked), so this is written out rather
    // than derived — the same reason `promise::SendJsFuture` writes its
    // own.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchWebSocket")
            .field("ready_state", &self.socket.ws.ready_state())
            .field("queued", &self.shared.borrow().queue.len())
            .field("ended", &self.shared.borrow().ended)
            .finish()
    }
}

impl FetchWebSocket {
    /// `Ok` only while the socket is `OPEN`.
    ///
    /// The standard makes `send()` on a `CLOSING`/`CLOSED` socket discard
    /// the data *without throwing*, so without this check a message a
    /// caller sent after the peer went away would report success and go
    /// nowhere — see the module doc.
    fn sendable(&self) -> Result<(), Error> {
        let state = self.socket.ws.ready_state();
        if state == web_sys::WebSocket::OPEN {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Body, NotOpen(state)))
        }
    }
}

impl WebSocket for FetchWebSocket {}

impl Stream for FetchWebSocket {
    type Item = Result<Message, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut s = self.shared.borrow_mut();
        // The queue first, `ended` second: the close that ended the stream
        // put a `Message::Close` on the queue in the same call that set the
        // flag, and reading the flag first would drop it.
        if let Some(item) = s.queue.pop_front() {
            return Poll::Ready(Some(item));
        }
        if s.ended {
            return Poll::Ready(None);
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl Sink<Message> for FetchWebSocket {
    type Error = Error;

    /// Always ready while the socket is open — the browser exposes no
    /// drain event, so there is nothing to wait for that could be woken.
    /// See the module doc's section on backpressure.
    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(self.sendable())
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        self.sendable()?;
        let ws = &self.socket.ws;
        let sent = match item {
            Message::Text(t) => ws.send_with_str(&t),
            Message::Binary(b) => ws.send_with_u8_array(&b),
            // The close handshake is `close()`, not a frame we format: the
            // browser owns the framing. A code the standard rejects (RFC
            // 6455 §7.4 allows 1000 and 3000-4999 from an endpoint) throws
            // here, and that throw becomes the caller's error rather than a
            // close nobody sent.
            Message::Close(Some(f)) => ws.close_with_code_and_reason(f.code, &f.reason),
            Message::Close(None) => ws.close(),
        };
        sent.map_err(|e| Error::new(ErrorKind::Body, SendRefused(convert::js_message(&e))))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    /// Closes the socket and returns; it does **not** wait for the peer's
    /// answering close, which arrives as a [`Message::Close`] on the
    /// `Stream`. The same decision `hclient-native`'s `poll_close` makes,
    /// and for the same reason: waiting here would be a call that cannot
    /// return against a peer that never answers, in a seam with no timeout
    /// of its own.
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let _ = self.socket.ws.close();
        Poll::Ready(Ok(()))
    }
}

/// The seam, implemented — which is the whole of how this transport says it
/// can do WebSocket. There is no capability to read: `Capabilities` used to
/// carry an `upgrade` field, every backend set it to `None`, and nothing
/// ever branched on it, which is why it is gone.
///
/// Not behind a cargo feature, and that is a measurement rather than a
/// preference. `hclient-native` gates its implementation at `websocket`
/// because `tungstenite` and its RFC 6455 codec are +14 crates in a real
/// client build. Here the protocol implementation is the browser's: this
/// file adds four `web-sys` feature names (`WebSocket`, `BinaryType`,
/// `MessageEvent`, `CloseEvent`) to a crate that already depends on
/// `web-sys`, and **no crate at all** to the graph. A feature gating
/// nothing would still cost something — the browser suite would need it
/// spelled in one more place, which is the drift `justfile`'s own check
/// warns about.
/// Generic in `H` so that a caller who asked for events keeps the seam —
/// but **no event is emitted here**, for the reason `hclient-native`'s own
/// `WebSocketConnect` gives one seam over: the observability vocabulary is
/// about HTTP requests, and a WebSocket is not one. There is no head to
/// report beyond the handshake the browser consumes, nothing is pooled so
/// no `Reused` can follow, and the socket's end is the `Stream`'s end,
/// which the caller already sees. The `H` is threaded rather than pinned
/// to `NoHooks` because a `Fetch<MyHook>` that could no longer open a
/// WebSocket would be a capability lost to an unrelated setting.
impl<H> WebSocketConnect for crate::Fetch<H> {
    type WebSocket = FetchWebSocket;

    async fn websocket(&self, req: http::Request<()>) -> Result<FetchWebSocket, Error> {
        // Both refusals happen before a socket exists, so a request the
        // browser cannot honour reaches no server at all.
        let url = ws_url(req.uri())?;
        let protocols = subprotocols(req.headers())?;

        let ws = construct(&url, &protocols)?;
        // Before any handler is installed, so no message can arrive while
        // the socket is still handing out `Blob`s.
        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

        let shared = Rc::new(RefCell::new(Shared::default()));
        let handlers = install(&ws, &shared);
        // From here on the socket is owned, and dropping this future —
        // which drops `socket` — detaches and closes it.
        let socket = Socket {
            ws,
            _handlers: handlers,
        };

        Opening(&shared).await?;
        Ok(FetchWebSocket { socket, shared })
    }
}
