//! WebSocket over an HTTP/1.1 upgrade, behind
//! [`http_ng_core::unversioned::WebSocketConnect`].
//!
//! # The trap this file exists to avoid
//!
//! hyper answers a `101` from `Connection`'s `Future` impl with
//! `pending.manual(); Poll::Ready(Ok(()))` (`client/conn/http1.rs:310-320`,
//! under its own comment *"With no `Send` bound on `I`, we can't try to do
//! upgrades here"*). So **"the exchange finished" and "the upgrade was
//! destroyed" are the same observation**, and [`crate::h1::exchange`] polls
//! `Connection` exactly that way — which is why a `101` there is a response
//! with an empty body and a socket that closes when the locals drop
//! (`tests/switching_protocols.rs`).
//!
//! Nothing in that shape can be reused here. This file therefore does its
//! own exchange, on three rules:
//!
//! 1. **The `101` is recognised by its status and its handshake headers,
//!    never by "the connection future completed".** That distinction is
//!    the whole trap: hyper reports a finished ordinary exchange and a
//!    destroyed upgrade with the same `Ready(Ok(()))`, so a client taking
//!    the completion as its signal would upgrade onto any response at
//!    all. [`upgrade`] reads four things — the status, `Upgrade:`,
//!    `Connection:` and `Sec-WebSocket-Accept` — and returns a typed
//!    error on each; `tests/websocket.rs` has a server for every one, and
//!    deleting any of the four kills a named test.
//!
//!    Measured, because the stronger claim an earlier draft of this
//!    paragraph made is false: **moving all four checks to after
//!    `into_parts` changes nothing any test can see**, and that mutation
//!    survives. The reason is `drop(body)` below — dropping hyper's
//!    `Incoming` finishes the dispatcher whatever the response was, so
//!    `poll_without_shutdown` returns `Ready` on a `200` as readily as on
//!    a `101`, and an upgrade that is then refused drops its socket
//!    either way. The checks stay where they are because reading a
//!    response before dismantling the connection that produced it is the
//!    order that stays correct if hyper's does not; they are not load
//!    bearing *today*, and `docs/v03-acceptance.md` says so rather than
//!    letting this file imply otherwise.
//! 2. **`poll_without_shutdown` + `into_parts`, never
//!    `hyper::upgrade::{on, Upgraded}`.** `Upgraded` holds
//!    `Rewind<Box<dyn Io + Send>>` (`hyper/src/upgrade.rs:66-67`), which
//!    would put a `Send` bound on this crate's IO and shut out
//!    single-threaded runtimes — the same objection that disqualified
//!    `hyper/http2` in v0.2 W3. `poll_without_shutdown` and `into_parts`
//!    are bounded by `T: Read + Write + Unpin` alone.
//!
//!    Worth knowing, because it is not what the shape suggests: at
//!    hyper 1.11 `poll_without_shutdown` and `Connection`'s `Future` impl
//!    behave *identically* on a `101` — `poll_inner` returns
//!    `Dispatched::Upgrade` before it ever looks at `should_shutdown`, and
//!    both then call `pending.manual()`. Swapping one for the other here
//!    is a mutation no test in this workspace kills, and it is written
//!    down in `docs/v03-acceptance.md` rather than left for the next
//!    reader to rediscover. `poll_without_shutdown` is still the right
//!    call: it is the API hyper documents for this ("Once the upgrade is
//!    completed … you would take it back using `into_parts`"), it does not
//!    require `B::Data: Send` where the `Future` impl does, and on any
//!    completion that is *not* an upgrade it is the one that leaves the
//!    socket alone.
//! 3. **`Parts::read_buf` is carried into the framing layer.** A server is
//!    free to put its first frames in the same flight as the `101`, and
//!    hyper will have read them already. Dropping that buffer works in
//!    every test where the server pauses first, which is what makes it
//!    worth a test where it does not
//!    (`the_first_frame_may_arrive_in_the_same_flight_as_the_101`).
//!
//! # Framing: `tungstenite`, driven by us
//!
//! `docs/w4-upgrade-seam.md` §6 has the measurement. In one line:
//! `TcpConnect::Stream` is bounded by `hyper::rt::{Read, Write}`, so an
//! adapter is needed whichever crate is picked, and the adapter that faces
//! `std::io` removes an `unsafe`. `tungstenite::protocol::WebSocketContext`
//! takes the stream as a *parameter* rather than owning it, so the
//! persistent protocol state and the transient IO are separate values and
//! the [`Shim`] handed to each call borrows the poll `Context` for exactly
//! that call. `tokio-tungstenite`'s `AllowStd` owns the stream across
//! calls and therefore has to smuggle a `*mut Context` in; this crate
//! forbids `unsafe` and does not have to.
//!
//! This is the third time this workspace drives someone else's state
//! machine by hand rather than adopting their runtime glue — `h2`'s
//! `Connection` and hyper's h1 `Connection` are the other two.
//!
//! # What §6 asked to be checked when the code was written
//!
//! Both were read out of `tungstenite 0.30.0`'s source rather than
//! assumed, and both hold.
//!
//! **`WouldBlock` out of the shim leaves `WebSocketContext` resumable on
//! every path.** `read` catches a `WouldBlock` from its own flush and sets
//! `unflushed_additional`, so the queued pong or close is retried on the
//! next call rather than lost; a `WouldBlock` that escapes `read` can
//! therefore only have come from the read side, which is what makes it
//! safe to report as `Poll::Pending` — the read waker has been registered
//! by then. `write` formats the frame into `out_buffer` *before* anything
//! can block, and `close` sets `ClosedByUs` before it can block and takes
//! its `if let Active = state` branch only once, so a resumed close does
//! not queue a second close frame.
//!
//! **A partial write is not lost between polls.** `FrameCodec::
//! write_out_buffer` loops `stream.write(&out_buffer)` and does
//! `out_buffer.drain(0..len)` for each partial write *before* the `?` on
//! the next one propagates — so what was written is dropped from the
//! buffer, what was not stays in it, and the next flush continues where
//! the last one stopped. That property is the whole reason [`Shim`]'s
//! `write` must return the real `n` rather than `buf.len()`;
//! `a_message_larger_than_the_socket_buffer_arrives_whole` is the test,
//! and the mutation it catches.
//!
//! # Why a WebSocket is never pooled, at either end
//!
//! It is opened on a connection of its own ([`crate::pool`] is not
//! consulted) and it never goes back, because a socket that has stopped
//! speaking HTTP is not a connection any later request could use. That is
//! the same conclusion `tests/switching_protocols.rs` reached from the
//! other side, and here it costs nothing to arrange: this file never
//! builds a `CheckIn`.
use crate::connect;
use bytes::Bytes;
use futures_core::Stream;
use futures_sink::Sink;
use http::HeaderValue;
use http_ng_core::unversioned::{CloseFrame, Message, WebSocket, WebSocketConnect};
use http_ng_core::{Error, ErrorKind, Timeouts};
use http_ng_dns::Resolve;
use http_ng_rt::{TcpConnect, Timer};
use http_ng_tls::TlsConnect;
use hyper::client::conn::http1;
use hyper::rt::{Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll};
use tungstenite::Message as Frame;
use tungstenite::protocol::{Role, WebSocketConfig, WebSocketContext};

/// The handshake headers this crate owns.
///
/// A request that already carries one is refused rather than silently
/// overwritten: overwriting is dropping a header the caller set, and
/// [`WebSocketConnect::websocket`]'s own contract says a header that
/// cannot be sent must fail rather than disappear. `Host` is deliberately
/// not on this list — it is defaulted when absent and honoured when
/// present, the same rule `established::Rewritten::to_origin_form` already
/// follows for every other request this crate sends.
const OURS: [http::HeaderName; 4] = [
    http::header::CONNECTION,
    http::header::UPGRADE,
    http::header::SEC_WEBSOCKET_KEY,
    http::header::SEC_WEBSOCKET_VERSION,
];

#[derive(Debug, thiserror::Error)]
#[error("the WebSocket handshake sets {0} itself; a request may not carry one")]
struct ReservedHeader(http::HeaderName);

#[derive(Debug, thiserror::Error)]
#[error("unsupported URI scheme for a WebSocket: {0:?} (expected ws, wss, http or https)")]
struct UnsupportedScheme(String);

#[derive(Debug, thiserror::Error)]
#[error("the server answered {0} rather than 101 Switching Protocols")]
struct NotSwitchingProtocols(http::StatusCode);

#[derive(Debug, thiserror::Error)]
#[error("the 101 response is missing or misspells its {0} header")]
struct BadUpgradeHeader(&'static str);

#[derive(Debug, thiserror::Error)]
#[error("the server's Sec-WebSocket-Accept does not match the Sec-WebSocket-Key this client sent")]
struct AcceptKeyMismatch;

#[derive(Debug, thiserror::Error)]
#[error("the connection ended before the handshake response arrived")]
struct EndedBeforeTheResponse;

/// `ws`/`wss` are RFC 6455's schemes; `http`/`https` are accepted as the
/// same two, because a caller who already holds an origin should not have
/// to rewrite its scheme to open a socket to it. The result is what
/// [`connect::connect`] understands, which is also what leaves the port
/// defaulting (`80`/`443`) in that module rather than in a second copy of
/// it here.
fn as_http_uri(uri: &http::Uri) -> Result<http::Uri, Error> {
    let scheme = match uri.scheme_str() {
        Some("ws" | "http") => "http",
        Some("wss" | "https") => "https",
        other => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                UnsupportedScheme(other.unwrap_or("").to_string()),
            ));
        }
    };
    let mut parts = uri.clone().into_parts();
    parts.scheme = Some(scheme.parse().expect("http and https are valid schemes"));
    if parts.path_and_query.is_none() {
        parts.path_and_query = Some(http::uri::PathAndQuery::from_static("/"));
    }
    http::Uri::from_parts(parts).map_err(|e| Error::new(ErrorKind::Unsupported, e))
}

/// The RFC 6455 §4.1 opening handshake, built from the caller's request.
///
/// Origin-form URI and a `Host:`, because this goes out on hyper's HTTP/1
/// client, which requires exactly that — the same shape
/// [`crate::established`] puts an ordinary request into, and for the same
/// reason.
fn handshake_request(
    req: http::Request<()>,
    uri: &http::Uri,
    key: &str,
) -> Result<http::Request<http_body_util::Empty<Bytes>>, Error> {
    for name in OURS {
        if req.headers().contains_key(&name) {
            return Err(Error::new(ErrorKind::Unsupported, ReservedHeader(name)));
        }
    }
    let (parts, ()) = req.into_parts();
    let mut headers = parts.headers;

    let host = uri.host().unwrap_or_default();
    let https = uri.scheme_str() == Some("https");
    let default_port = if https { 443 } else { 80 };
    let port = uri.port_u16().unwrap_or(default_port);
    if !headers.contains_key(http::header::HOST) {
        let authority = if port == default_port {
            host.to_owned()
        } else {
            format!("{host}:{port}")
        };
        if let Ok(v) = HeaderValue::from_str(&authority) {
            headers.insert(http::header::HOST, v);
        }
    }
    headers.insert(
        http::header::CONNECTION,
        HeaderValue::from_static("Upgrade"),
    );
    headers.insert(http::header::UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(
        http::header::SEC_WEBSOCKET_VERSION,
        HeaderValue::from_static("13"),
    );
    headers.insert(
        http::header::SEC_WEBSOCKET_KEY,
        HeaderValue::from_str(key).map_err(|e| Error::new(ErrorKind::Connect, e))?,
    );

    let target = uri
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
    let mut origin_form = http::uri::Parts::default();
    origin_form.path_and_query = Some(target);

    let mut out = http::Request::new(http_body_util::Empty::<Bytes>::new());
    *out.method_mut() = http::Method::GET;
    *out.version_mut() = http::Version::HTTP_11;
    *out.uri_mut() =
        http::Uri::from_parts(origin_form).map_err(|e| Error::new(ErrorKind::Connect, e))?;
    *out.headers_mut() = headers;
    *out.extensions_mut() = parts.extensions;
    Ok(out)
}

/// `true` when `Connection:` carries an `upgrade` token.
///
/// A token list, not an equality test: RFC 9110 §7.6.1 makes `Connection`
/// a comma-separated list, and `Connection: keep-alive, Upgrade` is a
/// legal answer that `tungstenite`'s own `verify_response` — which
/// compares the whole value with `eq_ignore_ascii_case` — would reject.
fn has_upgrade_token(v: Option<&HeaderValue>) -> bool {
    v.and_then(|v| v.to_str().ok()).is_some_and(|v| {
        v.split(',')
            .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
    })
}

/// The exchange, from a connected socket to the bytes that follow the
/// `101` — see the module doc for the three rules it keeps.
///
/// Returns the IO, whatever hyper had already read past the response head,
/// and the response head itself, so a caller can read what was negotiated.
async fn upgrade<I>(
    io: I,
    req: http::Request<http_body_util::Empty<Bytes>>,
    key: &str,
) -> Result<(I, Bytes, http::Response<()>), Error>
where
    I: Read + Write + Unpin + 'static,
{
    let (mut sender, mut conn) = http1::handshake::<I, http_body_util::Empty<Bytes>>(io)
        .await
        .map_err(|e| Error::new(ErrorKind::Connect, e))?;

    let mut conn_done = false;
    let resp = {
        let mut send = std::pin::pin!(sender.send_request(req));
        std::future::poll_fn(|cx| {
            // `poll_without_shutdown`, never `Pin::new(&mut conn).poll(cx)`
            // — see the module doc. `conn` is not polled again once it has
            // answered `Ready`, for `Future`'s own reason.
            if !conn_done {
                match conn.poll_without_shutdown(cx) {
                    Poll::Ready(Ok(())) => conn_done = true,
                    Poll::Ready(Err(e)) => {
                        return Poll::Ready(Err(Error::new(ErrorKind::Connect, e)));
                    }
                    Poll::Pending => {}
                }
            }
            match send.as_mut().poll(cx) {
                Poll::Ready(Ok(r)) => Poll::Ready(Ok(r)),
                Poll::Ready(Err(e)) => Poll::Ready(Err(Error::new(ErrorKind::Connect, e))),
                // The same dead end `h1::exchange` documents: the
                // dispatcher is finished with our request still queued on
                // it, and the callback that would resolve this future is
                // inside the very value we are holding.
                Poll::Pending if conn_done => {
                    Poll::Ready(Err(Error::new(ErrorKind::Connect, EndedBeforeTheResponse)))
                }
                Poll::Pending => Poll::Pending,
            }
        })
        .await?
    };
    drop(sender);

    // Rule 1, first of four: by status. Not by "hyper is finished with
    // this connection", which is the same observation for an ordinary
    // exchange — see the module doc.
    let (head, body) = resp.into_parts();
    if head.status != http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(Error::new(
            ErrorKind::Status,
            NotSwitchingProtocols(head.status),
        ));
    }
    // A `101` carries no body (hyper decodes it as zero-length), and the
    // connection underneath it is about to be taken apart.
    drop(body);

    if !head
        .headers
        .get(http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
    {
        return Err(Error::new(ErrorKind::Status, BadUpgradeHeader("Upgrade")));
    }
    if !has_upgrade_token(head.headers.get(http::header::CONNECTION)) {
        return Err(Error::new(
            ErrorKind::Status,
            BadUpgradeHeader("Connection"),
        ));
    }
    // RFC 6455 §4.1 step 5. Without it, a server that never saw the key —
    // a cache, a proxy, anything replaying a recorded `101` — is
    // indistinguishable from one that did.
    let expected = tungstenite::handshake::derive_accept_key(key.as_bytes());
    if head
        .headers
        .get(http::header::SEC_WEBSOCKET_ACCEPT)
        .map(HeaderValue::as_bytes)
        != Some(expected.as_bytes())
    {
        return Err(Error::new(ErrorKind::Status, AcceptKeyMismatch));
    }

    if !conn_done {
        std::future::poll_fn(|cx| conn.poll_without_shutdown(cx))
            .await
            .map_err(|e| Error::new(ErrorKind::Connect, e))?;
    }
    // Rule 3: `read_buf` is whatever the server put in the same flight as
    // the `101`. hyper has already read it off the socket, and it is
    // unreachable from anywhere else once this value is dropped.
    let http1::Parts { io, read_buf, .. } = conn.into_parts();
    Ok((io, read_buf, http::Response::from_parts(head, ())))
}

/// `std::io` over `hyper::rt`, for exactly one call.
///
/// The `Context` is borrowed rather than stored, which is what makes this
/// safe code — see the module doc. `Poll::Pending` becomes `WouldBlock`,
/// which is the answer `tungstenite` is written against.
struct Shim<'a, 'b, I> {
    io: &'a mut I,
    cx: &'a mut Context<'b>,
}

impl<I> std::io::Read for Shim<'_, '_, I>
where
    I: Read + Unpin,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut rb = hyper::rt::ReadBuf::new(buf);
        match Pin::new(&mut *self.io).poll_read(self.cx, rb.unfilled()) {
            // Nothing filled is EOF, which is what `std::io::Read` means
            // by `Ok(0)` and what `tungstenite` reads as the peer having
            // gone away.
            Poll::Ready(Ok(())) => Ok(rb.filled().len()),
            Poll::Ready(Err(e)) => Err(e),
            Poll::Pending => Err(std::io::ErrorKind::WouldBlock.into()),
        }
    }
}

impl<I> std::io::Write for Shim<'_, '_, I>
where
    I: Write + Unpin,
{
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // The real count, never `buf.len()`. `FrameCodec::write_out_buffer`
        // drains exactly this many bytes and keeps the rest for the next
        // flush; claiming more is how a partial write turns into bytes
        // missing from the middle of a message.
        match Pin::new(&mut *self.io).poll_write(self.cx, buf) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(std::io::ErrorKind::WouldBlock.into()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match Pin::new(&mut *self.io).poll_flush(self.cx) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(std::io::ErrorKind::WouldBlock.into()),
        }
    }
}

/// The stream [`Sink::start_send`] writes into: nothing, ever.
///
/// `start_send` is handed no `Context`, so it has no waker to register and
/// must not touch the socket. Refusing every byte makes `tungstenite`
/// format the frame into its own `out_buffer` and report `WouldBlock`,
/// which is precisely the state `poll_flush` knows how to finish. A fake
/// `Context` built from `Waker::noop` would be the alternative and a worse
/// one: it would let the socket be written with a waker nothing can ever
/// wake.
struct Unwritable;

impl std::io::Read for Unwritable {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}

impl std::io::Write for Unwritable {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(e) if e.kind() == std::io::ErrorKind::WouldBlock)
}

/// The one conversion point from `tungstenite::Error` to this workspace's,
/// on the same principle as `h1::from_hyper_error`: a caller must be able
/// to tell "the peer went away" from "the peer sent something that is not
/// WebSocket" by `kind()` alone, without a downcast.
fn ws_error(e: tungstenite::Error) -> Error {
    let kind = match e {
        tungstenite::Error::Io(_) | tungstenite::Error::ConnectionClosed => ErrorKind::Body,
        tungstenite::Error::Protocol(_) | tungstenite::Error::Capacity(_) => ErrorKind::Decode,
        _ => ErrorKind::Other,
    };
    Error::new(kind, e)
}

/// An open WebSocket on a native socket.
///
/// The IO and the protocol state are separate fields on purpose: that is
/// what lets [`Shim`] borrow the poll `Context` for one call, and it is
/// the whole of §6's argument for driving `tungstenite` rather than
/// wrapping it.
#[derive(Debug)]
pub struct NativeWebSocket<I> {
    io: I,
    ctx: WebSocketContext,
    /// A `Stream` that has ended stays ended — the contract
    /// `http_ng_core::unversioned::WebSocket` states, held here rather
    /// than left to whatever `tungstenite` answers on a second call.
    ended: bool,
}

impl<I> NativeWebSocket<I> {
    /// The negotiated connection, from the pieces [`upgrade`] hands back.
    fn new(io: I, read_buf: Bytes) -> Self {
        Self {
            io,
            ctx: WebSocketContext::from_partially_read(
                read_buf.to_vec(),
                Role::Client,
                Some(WebSocketConfig::default()),
            ),
            ended: false,
        }
    }
}

impl<I> WebSocket for NativeWebSocket<I> where I: Read + Write + Unpin {}

impl<I> Stream for NativeWebSocket<I>
where
    I: Read + Write + Unpin,
{
    type Item = Result<Message, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Self { io, ctx, ended } = self.get_mut();
        if *ended {
            return Poll::Ready(None);
        }
        loop {
            match ctx.read(&mut Shim { io, cx }) {
                Ok(Frame::Text(t)) => return Poll::Ready(Some(Ok(Message::Text(t.to_string())))),
                Ok(Frame::Binary(b)) => return Poll::Ready(Some(Ok(Message::Binary(b)))),
                Ok(Frame::Close(f)) => {
                    return Poll::Ready(Some(Ok(Message::Close(f.map(|f| CloseFrame {
                        code: f.code.into(),
                        reason: f.reason.to_string(),
                    })))));
                }
                // **The loop is the pong.** RFC 6455 §5.5.2 makes
                // answering a ping this endpoint's duty; `read` queues the
                // pong but only *writes* it at the start of a later call,
                // and a caller awaiting the next message is not a caller
                // that flushes. So the answer is to read again
                // immediately — which is also the only correct thing to do
                // with a frame nobody is going to be handed, because
                // returning `Pending` here would be returning it with no
                // waker registered anywhere: `read` succeeded, so nothing
                // was left waiting on the socket.
                //
                // `Frame::Frame` cannot come out of `read` at all — it is
                // a write-side variant — and is folded in here rather than
                // given an `unreachable!`.
                Ok(Frame::Ping(_) | Frame::Pong(_) | Frame::Frame(_)) => continue,
                Err(e) if would_block(&e) => return Poll::Pending,
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    *ended = true;
                    return Poll::Ready(None);
                }
                Err(e) => {
                    *ended = true;
                    return Poll::Ready(Some(Err(ws_error(e))));
                }
            }
        }
    }
}

impl<I> Sink<Message> for NativeWebSocket<I>
where
    I: Read + Write + Unpin,
{
    type Error = Error;

    /// Ready means the write buffer is empty.
    ///
    /// The strictest of the shapes `Sink` allows, and deliberately so: it
    /// is the one that gives a caller backpressure from the socket rather
    /// than from memory. `tungstenite`'s own default write buffer is
    /// unbounded (`max_write_buffer_size: usize::MAX`), so a `poll_ready`
    /// that always said yes would let a caller queue a gigabyte against a
    /// peer that reads nothing.
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.poll_flush(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        let Self { ctx, .. } = self.get_mut();
        let frame = match item {
            Message::Text(t) => Frame::Text(t.into()),
            Message::Binary(b) => Frame::Binary(b),
            Message::Close(f) => Frame::Close(f.map(|f| tungstenite::protocol::CloseFrame {
                code: f.code.into(),
                reason: f.reason.into(),
            })),
        };
        match ctx.write(&mut Unwritable, frame) {
            Ok(()) => Ok(()),
            // Expected, and not a failure: [`Unwritable`] refused, so the
            // frame is in `tungstenite`'s own buffer and `poll_flush` is
            // what puts it on the wire.
            Err(e) if would_block(&e) => Ok(()),
            Err(e) => Err(ws_error(e)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let Self { io, ctx, .. } = self.get_mut();
        match ctx.flush(&mut Shim { io, cx }) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(e) if would_block(&e) => Poll::Pending,
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Poll::Ready(Ok(()))
            }
            Err(e) => Poll::Ready(Err(ws_error(e))),
        }
    }

    /// Queues a close frame if one has not gone out yet, then flushes.
    ///
    /// It does **not** wait for the peer's answering close: that arrives
    /// as a [`Message::Close`] on the `Stream`, and a caller who needs the
    /// close handshake completed polls the `Stream` to its end. Waiting
    /// here instead would make `SinkExt::close` a function that cannot
    /// return against a peer that never answers, in a seam that has no
    /// timeout of its own.
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let Self { io, ctx, .. } = self.get_mut();
        match ctx.close(&mut Shim { io, cx }, None) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(e) if would_block(&e) => Poll::Pending,
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Poll::Ready(Ok(()))
            }
            Err(e) => Poll::Ready(Err(ws_error(e))),
        }
    }
}

/// The seam, implemented — which is the whole of how this transport says
/// it can do WebSocket. There is no capability to read and no method that
/// returns `Unsupported`: a backend that cannot do this does not write
/// this `impl`, and asking it does not compile.
impl<R, T, D> WebSocketConnect for crate::Native<R, T, D>
where
    R: TcpConnect + Timer,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
{
    type WebSocket = NativeWebSocket<crate::NativeIo<R, T>>;

    async fn websocket(&self, req: http::Request<()>) -> Result<Self::WebSocket, Error> {
        let uri = as_http_uri(req.uri())?;
        let timeouts = req
            .extensions()
            .get::<Timeouts>()
            .copied()
            .unwrap_or_default();
        let key = tungstenite::handshake::client::generate_key();
        let handshake = handshake_request(req, &uri, &key)?;

        // A connection of its own, and never `self.pool` — see the module
        // doc. `http/1.1` alone on the ALPN list: RFC 8441 extended
        // CONNECT is the h2 answer to this and this is not it.
        let now = self.rt.elapsed_since(self.epoch);
        let connect_fut = connect::connect(
            &self.rt,
            &self.dns,
            &self.tls,
            &uri,
            &self.opts,
            &[b"http/1.1"],
            &self.svcb_failures,
            now,
        );
        let (conn, _tls_info) = match timeouts.connect {
            Some(d) => crate::with_connect_timeout(&self.rt, d, connect_fut).await?,
            None => connect_fut.await?,
        };

        let (io, read_buf, _resp) = upgrade(conn, handshake, &key).await?;
        Ok(NativeWebSocket::new(io, read_buf))
    }
}
