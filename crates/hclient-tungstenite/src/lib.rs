//! RFC 6455 framing on an already-upgraded byte stream, and the connector
//! that opens one over `hclient-native`.
//!
//! Two halves, and the seam between them is the whole reason this crate
//! exists rather than a `websocket` feature on `hclient-native`
//! (`docs/w4-upgrade-seam.md` §8):
//!
//! - [`TungsteniteWebSocket`], the framing: generic over the IO and the
//!   clock, it names no transport at all, and it is what
//!   [`hclient_core::unversioned::WebSocket`] is implemented on. Give it
//!   an upgraded stream and the bytes hyper had already read past, and it
//!   speaks WebSocket over them.
//! - [`Tungstenite`], the connector: it borrows a
//!   [`hclient_native::Native`], asks it for the h1 upgrade, and hands the
//!   result to the framing. It is the only thing here that knows what a
//!   socket is.
//!
//! # The seam this crate takes from `hclient-native` is "an upgraded byte
//! stream, plus the `read_buf`"
//!
//! Which is exactly the shape `docs/w4-upgrade-seam.md` §2 rejected — as
//! the **public** seam, where it excludes three of four backends and the
//! browser among them. As the seam between a transport and a framing
//! crate it is right, because it is only ever asked of the one backend
//! that can answer it. A shape can be wrong at one level and correct at
//! the next; §2's argument was about which level.
//!
//! The proof that the arrangement is the right way round is
//! `hclient-fetch`: a browser hands back **messages**, so that backend
//! implements `WebSocketConnect` itself and needs no adapter at all. The
//! adapter exists exactly where the platform hands back **bytes**. If
//! both needed one, the seam would be in the wrong place.
//!
//! What stays on the other side of it is in
//! [`hclient_native::Upgrading`]: the connection of its own (its
//! connector, its TLS, `http/1.1` alone on the ALPN list, and deliberately
//! never the pool), and the h1 upgrade — `poll_without_shutdown` +
//! `into_parts`, and the `101` recognised by **status** before the
//! connection is polled out, which is the trap that module exists to
//! avoid. The three checks that are about *WebSocket* rather than about
//! *an upgrade* — `Upgrade:`, `Connection:` and `Sec-WebSocket-Accept` —
//! are [`Handshake::accept`]'s, here, and they run on the response head
//! **before** [`hclient_native::Upgrading::finish`] takes the connection
//! apart. That ordering is structural rather than remembered: this crate
//! cannot reach the socket without having been handed the head first.
//!
//! # Framing: `tungstenite`, driven by us
//!
//! `docs/w4-upgrade-seam.md` §6 has the measurement. In one line:
//! `TcpConnect::Stream` is bounded by `hyper::rt::{Read, Write}`, so an
//! adapter is needed whichever crate is picked, and the adapter that faces
//! `std::io` removes an `unsafe`. `tungstenite::protocol::WebSocketContext`
//! takes the stream as a *parameter* rather than owning it, so the
//! persistent protocol state and the transient IO are separate values and
//! the `Shim` handed to each call borrows the poll `Context` for exactly
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
//! the last one stopped. That property is the whole reason `Shim`'s
//! `write` must return the real `n` rather than `buf.len()`;
//! `a_message_larger_than_the_socket_buffer_arrives_whole` is the test,
//! and the mutation it catches.
//!
//! # Why a WebSocket is never pooled, at either end
//!
//! It is opened on a connection of its own and it never goes back,
//! because a socket that has stopped speaking HTTP is not a connection any
//! later request could use. That is the same conclusion
//! `hclient-native`'s `tests/switching_protocols.rs` reached from the
//! other side, and neither half of this arrangement can undo it: the pool
//! is not consulted on the way in ([`hclient_native::Native::upgrade`]
//! never asks it), and nothing here can put a connection back, because
//! this crate is never handed one — only an `I` that used to be one.
//!
//! # The bound an open socket has: liveness, and only when it is asked for
//!
//! Steps 1-3 shipped with none — only the handshake read
//! `Timeouts::connect`, so a peer that vanished without a `FIN` left a
//! `Stream` that never yielded and never errored.
//! `docs/w4-upgrade-seam.md` §7 decided the shape and left two questions
//! open; both are answered below, and the answers are what this code
//! does.
//!
//! **Ping/pong, not a timeout.** `Timeouts::total` is meaningless for a
//! connection whose whole point is to outlive the exchange that opened
//! it, and a gap bound would be actively *wrong*: silence is the normal
//! state of a WebSocket, so `between_bytes` here would kill healthy
//! connections. The question is not "is this transfer taking too long",
//! it is **is the peer still there**, and RFC 6455 §5.5.2 answers exactly
//! that. It is configured on the connector
//! ([`Tungstenite::keep_alive`]) rather than on the seam or in
//! the request's extensions, because `hclient-fetch` implements the same
//! seam and a browser has no `send(ping)` at all — §7's own reasoning.
//!
//! **`poll_next` is the only thing driving the socket, and that is a real
//! difference from `hclient-h3`.** Nothing is spawned here; this crate has
//! no `Spawn` bound anywhere, deliberately. So a ping can only be written
//! while the caller is polling, and **a caller that stops polling gets no
//! keep-alive.** That is accepted openly rather than worked around: a
//! caller that is not polling is not waiting for anything. It is genuinely
//! unlike `hclient_h3`, where a spawned driver keeps a *pooled* connection
//! alive on behalf of requests nobody has made yet — there the connection
//! has no caller, here it always has one.
//! `tests/websocket.rs`'s `a_socket_nobody_polls_gets_no_keep_alive` pins
//! it from the server's side of the wire, so it is a stated property
//! rather than an unstated consequence.
//!
//! **What `tungstenite` already does, and the one thing it does not.**
//! `WebSocketContext::read` answers a peer's `Ping` with a `Pong` itself
//! — its own doc says "This function sends pong and close responses
//! automatically", and `tests/websocket.rs` watches that pong leave from
//! the server's side. What it does **not** do is keep any record of pings
//! *we* send: `read` hands an inbound `Pong` straight back out as
//! `Message::Pong`, solicited or not, and there is no outstanding-ping
//! state anywhere in `tungstenite 0.30`. So the frame this file writes is
//! a `Ping`, and every bit of bookkeeping that decides whether a pong
//! answered it is ours. That is the same division HTTP/3 met with quinn:
//! driving a connection is what lets it *send* a keep-alive, not what
//! makes it *decide* to.
//!
//! The ping also has to be **flushed**, which is not what the shape
//! suggests and was read out of `tungstenite 0.30.0` rather than assumed:
//! `write` formats a `Ping` into `out_buffer` and calls through to the
//! socket only when the buffer passes `write_buffer_size` (128 KiB by
//! default) or when it had a queued pong or close of its own to send
//! (`_write`'s `should_flush`). A ping written and not flushed never
//! leaves.
//!
//! ## §7's first open question: no, an unanswered ping is not surfaced
//! before its deadline
//!
//! The `Stream` can yield exactly two things, and neither can carry it.
//! `Message` has no `Ping`/`Pong` variant and must not gain one — the
//! seam's own doc records why (the browser can neither send nor receive
//! one), and a native-only concern is not a reason to change a seam three
//! other backends implement. And an `Err` on this stream is **terminal**
//! by that same seam's contract ("the connection broke and the error has
//! already been reported"; "a `Stream` that has ended stays ended"), so a
//! warning delivered as an error would break the contract for every
//! caller, not only the ones who asked for a keep-alive.
//!
//! A third channel — a callback, a watch handle — would be a second
//! vocabulary for information nobody can act on. The only action available
//! on "the ping has not come back *yet*" is to wait, which is precisely
//! what `within` already does; and a pong that arrives one millisecond
//! inside the deadline is a perfectly healthy connection, so an early
//! signal would report ordinary jitter as a fault. What a caller can read
//! is the configuration in force ([`TungsteniteWebSocket::keep_alive`]) and the
//! failure when it happens. Between those two there is nothing true to
//! say.
//!
//! ## §7's second open question: the interval resets on **any** inbound
//! frame, the deadline **only** on the pong
//!
//! They are two clocks answering two different questions, so they reset on
//! different events.
//!
//! `every` measures **silence**. Any inbound frame at all — text, binary,
//! ping, pong, close — is proof the peer is there and ends the silence, so
//! it restarts the interval. The consequence is the one that matters for
//! the "off by default" argument: **a busy connection sends no keep-alive
//! traffic whatsoever.** Resetting only on a pong would make a chatty
//! socket ping every `every` for ever, which is exactly the traffic nobody
//! asked for.
//!
//! `within` measures **an unanswered probe**, and only the answer RFC 6455
//! §5.5.2 makes a MUST answers it: a `Pong` carrying the ping's own
//! payload. A text frame does not, because the two say different things —
//! data comes from the peer's application, a pong comes from its WebSocket
//! layer, and it is the layer that has to be alive for anything we send to
//! be read at all. Letting any frame clear the deadline would turn the
//! probe back into the gap bound §7 rejected, restricted to the window
//! after a ping.
//!
//! The payload is **matched**, not accepted on the opcode alone, because
//! RFC 6455 §5.5.3 explicitly allows *unsolicited* pongs as a
//! unidirectional heartbeat: a peer that emits one every second would keep
//! our probe permanently "answered" without ever having answered it. The
//! payload is the ping's sequence number, so a stale pong for an earlier
//! ping does not answer a later one either.
//!
//! One consequence to state rather than leave to be discovered: **both
//! sleeps are polled only when the read side has nothing**, because
//! `Pending` is the only moment `poll_next` has to poll them in. So the
//! deadline cannot fire in the middle of a stream of data; it fires after
//! `within` of *silence* following a ping. That falls straight out of
//! "`poll_next` is the only driver", and it is what makes the paragraph
//! above safe: a peer that answers a ping with data and keeps talking is
//! not killed, while one that answers with data and then stops is.
//!
//! **The keep-alive stops at our own close.** `tungstenite` refuses every
//! write once a close frame has gone out (`ProtocolError::
//! SendAfterClosing`), and RFC 6455 makes a ping after a close meaningless
//! anyway, so no probe follows one — a probe already in flight still has
//! its deadline. The closing handshake is therefore unbounded, which is
//! the same gap [`Sink::poll_close`] already records for itself.
#![forbid(unsafe_code)]

use bytes::Bytes;
use futures_core::Stream;
use futures_sink::Sink;
use hclient_core::unversioned::{CloseFrame, Message, WebSocket, WebSocketConnect};
use hclient_core::{Error, ErrorKind};
use hclient_dns::Resolve;
use hclient_native::{Native, NativeIo};
use hclient_rt::{TcpConnect, Timer};
use hclient_tls::TlsConnect;
use http::HeaderValue;
use hyper::rt::{Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tungstenite::Message as Frame;
use tungstenite::protocol::{Role, WebSocketConfig, WebSocketContext};

/// The handshake headers this crate owns.
///
/// A request that already carries one is refused rather than silently
/// overwritten: overwriting is dropping a header the caller set, and
/// [`WebSocketConnect::websocket`]'s own contract says a header that
/// cannot be sent must fail rather than disappear. `Host` is deliberately
/// not on this list — it is defaulted when absent and honoured when
/// present, and it is not set here at all: it is
/// [`hclient_native::Native::upgrade`]'s, which does it for a WebSocket
/// handshake by the same rule and in the same place as for every other
/// HTTP/1 request that transport sends.
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
#[error("the 101 response is missing or misspells its {0} header")]
struct BadUpgradeHeader(&'static str);

#[derive(Debug, thiserror::Error)]
#[error("the server's Sec-WebSocket-Accept does not match the Sec-WebSocket-Key this client sent")]
struct AcceptKeyMismatch;

/// `ws`/`wss` are RFC 6455's schemes; `http`/`https` are accepted as the
/// same two, because a caller who already holds an origin should not have
/// to rewrite its scheme to open a socket to it. The result is what
/// `hclient-native`'s connector understands, which is also what leaves the
/// port defaulting (`80`/`443`) over there rather than in a second copy of
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

/// The RFC 6455 §4.1 opening handshake: the request one end of it sends,
/// and the check the other end's answer has to pass.
///
/// **This is the half of a WebSocket that is neither framing nor a
/// socket.** It is public because it is what a connector other than
/// [`Tungstenite`] would need: `http::Request` in, `http::Request` out,
/// `http::Response` checked, and no IO of any kind. The nonce it holds is
/// why it is a value rather than two free functions — [`Handshake::accept`]
/// can only check `Sec-WebSocket-Accept` against the key that actually
/// went out.
#[derive(Debug)]
pub struct Handshake {
    key: String,
}

impl Handshake {
    /// The handshake request, built from the caller's.
    ///
    /// The URI comes back with `ws`/`wss` mapped to `http`/`https` and a
    /// path if it had none, because that is what a connector resolves and
    /// connects with. It is deliberately **absolute** and carries no
    /// `Host:`: turning it into the origin-form request line HTTP/1
    /// requires, and defaulting `Host:` from the authority, is the
    /// transport's job for every request it sends, and a WebSocket
    /// handshake is not the exception
    /// ([`hclient_native::Native::upgrade`] does it).
    ///
    /// The caller's extensions travel on the request, so a `Timeouts` in
    /// them still reaches whoever connects.
    pub fn start(req: http::Request<()>) -> Result<(Self, http::Request<()>), Error> {
        for name in OURS {
            if req.headers().contains_key(&name) {
                return Err(Error::new(ErrorKind::Unsupported, ReservedHeader(name)));
            }
        }
        let uri = as_http_uri(req.uri())?;
        let key = tungstenite::handshake::client::generate_key();

        let (parts, ()) = req.into_parts();
        let mut headers = parts.headers;
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
            HeaderValue::from_str(&key).map_err(|e| Error::new(ErrorKind::Connect, e))?,
        );

        let mut out = http::Request::new(());
        *out.method_mut() = http::Method::GET;
        *out.version_mut() = http::Version::HTTP_11;
        *out.uri_mut() = uri;
        *out.headers_mut() = headers;
        *out.extensions_mut() = parts.extensions;
        Ok((Self { key }, out))
    }

    /// The three checks a `101` has to pass to be *this* handshake's
    /// answer — and deliberately not the fourth.
    ///
    /// "Is this a `101` at all" belongs to whoever ran the exchange, and
    /// it is the one check that has to happen before the connection is
    /// polled out: hyper reports a finished ordinary exchange and a
    /// destroyed upgrade with the same `Ready(Ok(()))`, so a client taking
    /// the completion as its signal would upgrade onto any response at
    /// all. [`hclient_native::Native::upgrade`] carries that one and hands
    /// back a head only for a `101`.
    ///
    /// What is left is what makes a `101` a *WebSocket* `101`. All three
    /// are refusals rather than warnings: `tests/websocket.rs` has a
    /// server for each, and deleting any of them kills a named test.
    pub fn accept(&self, head: &http::response::Parts) -> Result<(), Error> {
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
        // RFC 6455 §4.1 step 5. Without it, a server that never saw the
        // key — a cache, a proxy, anything replaying a recorded `101` —
        // is indistinguishable from one that did.
        let expected = tungstenite::handshake::derive_accept_key(self.key.as_bytes());
        if head
            .headers
            .get(http::header::SEC_WEBSOCKET_ACCEPT)
            .map(HeaderValue::as_bytes)
            != Some(expected.as_bytes())
        {
            return Err(Error::new(ErrorKind::Status, AcceptKeyMismatch));
        }
        Ok(())
    }
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

/// The liveness bound on an open WebSocket: how long the socket may be
/// silent before a `Ping` goes out, and how long the peer then has to
/// answer it.
///
/// **Off by default**, and [`Tungstenite::keep_alive`] is the
/// only way to turn it on. A default that pings is a default that sends
/// traffic nobody asked for, and on a metered radio that is not free.
///
/// **This is not `TcpOpts::keepalive`, and neither substitutes for the
/// other.** That one is the kernel's, on the TCP connection, and a
/// middlebox that has forgotten the WebSocket while holding the socket
/// open answers it happily. This one is RFC 6455 §5.5.2's ping/pong,
/// written and answered by the WebSocket endpoints themselves, so its
/// answer says something about the peer that is actually reading the
/// messages.
///
/// The two fields reset on different events, deliberately — the module
/// doc is where the reasoning for both is written down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct WebSocketKeepAlive {
    /// How long the socket may be **silent** before a `Ping` goes out.
    ///
    /// Silence, not "since the last ping": any inbound frame restarts it,
    /// so a busy connection never pings at all.
    pub every: Duration,
    /// How long the peer then has to answer with a `Pong` carrying that
    /// ping's payload, before the `Stream` fails with [`PongNotReceived`].
    ///
    /// Only a matching pong answers it. A data frame arriving in the
    /// meantime is delivered to the caller and leaves the probe standing.
    pub within: Duration,
}

impl WebSocketKeepAlive {
    /// `every` of silence, then `within` to answer.
    pub const fn new(every: Duration, within: Duration) -> Self {
        Self { every, within }
    }
}

/// The source of the [`ErrorKind::Body`] error a missed pong produces.
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
/// [`hclient_core::Timeouts`] is in force here, and `Phase::BetweenBytes` in particular
/// would name a bound `docs/w4-upgrade-seam.md` §7 explicitly refused to
/// give this seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the peer did not answer a keep-alive ping within {0:?}")]
pub struct PongNotReceived(pub Duration);

/// The keep-alive's state, and the one sleep it runs at a time.
///
/// Which question that sleep answers depends on `outstanding`: with no
/// probe in flight it measures silence against
/// [`WebSocketKeepAlive::every`], with one it is that probe's deadline
/// against [`WebSocketKeepAlive::within`]. One sleep rather than two,
/// because the two states are exclusive — a socket cannot be both waiting
/// to probe and waiting for an answer.
///
/// `Pin<Box<Tm::Sleep>>` for the reason [`hclient_native::IdleTimeout`]'s is: a box
/// around a **concrete** type, so auto traits pass straight through it and
/// a `TungsteniteWebSocket` over a `Send` runtime stays `Send`.
struct Liveness<Tm: Timer> {
    timer: Tm,
    config: WebSocketKeepAlive,
    sleep: Option<Pin<Box<Tm::Sleep>>>,
    /// The payload of the ping waiting for an answer, and `None` when none
    /// is.
    outstanding: Option<u64>,
    /// The payload the next ping will carry. A sequence number rather than
    /// a constant, so a pong for an earlier ping cannot answer a later one.
    next: u64,
}

impl<Tm: Timer> Liveness<Tm> {
    fn new(timer: Tm, config: WebSocketKeepAlive) -> Self {
        Self {
            timer,
            config,
            sleep: None,
            outstanding: None,
            next: 0,
        }
    }

    /// A frame arrived — and the two clocks answer to different events,
    /// which is §7's second open question. See the module doc.
    fn saw(&mut self, frame: &Frame) {
        if let Frame::Pong(payload) = frame
            && self
                .outstanding
                .is_some_and(|seq| payload.as_ref() == seq.to_be_bytes())
        {
            self.outstanding = None;
        }
        // Any inbound frame ends the silence `every` measures — but only
        // once the probe, if there was one, has been answered. A text
        // frame arriving while a ping is outstanding is delivered to the
        // caller and leaves the deadline running.
        if self.outstanding.is_none() {
            self.sleep = None;
        }
    }

    /// The next ping's payload, and the record that it is waiting.
    fn probe(&mut self) -> Bytes {
        let seq = self.next;
        self.next = self.next.wrapping_add(1);
        self.outstanding = Some(seq);
        Bytes::copy_from_slice(&seq.to_be_bytes())
    }

    fn no_pong(&self) -> Error {
        Error::new(ErrorKind::Body, PongNotReceived(self.config.within))
    }
}

/// An open WebSocket on a native socket.
///
/// The IO and the protocol state are separate fields on purpose: that is
/// what lets `Shim` borrow the poll `Context` for one call, and it is
/// the whole of §6's argument for driving `tungstenite` rather than
/// wrapping it.
pub struct TungsteniteWebSocket<I, Tm: Timer> {
    io: I,
    ctx: WebSocketContext,
    /// A `Stream` that has ended stays ended — the contract
    /// `hclient_core::unversioned::WebSocket` states, held here rather
    /// than left to whatever `tungstenite` answers on a second call.
    ended: bool,
    /// `None` — no keep-alive was asked for, and nothing here ever builds
    /// a sleep. That matters beyond an allocation: `Tokio::sleep` panics
    /// outside a runtime, and a socket that asked for nothing must not
    /// need one. Still in the type, because a type cannot appear and
    /// disappear with a runtime value.
    live: Option<Liveness<Tm>>,
}

impl<I, Tm: Timer> TungsteniteWebSocket<I, Tm> {
    /// The negotiated connection, from the pieces an upgrade hands back:
    /// the socket, whatever the server had already sent past the `101`,
    /// a clock, and the liveness bound if the caller asked for one.
    ///
    /// **This is the internal seam of `docs/w4-upgrade-seam.md` §8**, and
    /// it is public for that reason: everything above it is RFC 6455 and
    /// nothing above it is a socket, so anything that can upgrade an
    /// HTTP/1 connection can frame one — [`Tungstenite`] is this
    /// workspace's one caller and not a privileged one. `read_buf` is what
    /// the server put in the same flight as the `101`; passing an empty
    /// `Bytes` when there was none is correct, dropping a non-empty one
    /// loses the peer's first frames for good.
    pub fn new(io: I, read_buf: Bytes, timer: Tm, keep_alive: Option<WebSocketKeepAlive>) -> Self {
        Self {
            io,
            ctx: WebSocketContext::from_partially_read(
                read_buf.to_vec(),
                Role::Client,
                Some(WebSocketConfig::default()),
            ),
            ended: false,
            live: keep_alive.map(|c| Liveness::new(timer, c)),
        }
    }

    /// The liveness bound in force on this socket, and `None` — the
    /// default — when there is none.
    ///
    /// The default being *readable* is the point: "off by default" is then
    /// a claim a caller can check rather than one it has to believe, and
    /// `tests/websocket.rs` checks it here as well as on the wire.
    pub fn keep_alive(&self) -> Option<WebSocketKeepAlive> {
        self.live.as_ref().map(|l| l.config)
    }
}

/// Hand-written for the reason [`hclient_native::IdleTimeout`]'s is:
/// `#[derive(Debug)]` would demand `Debug` of the clock, which [`Timer`]
/// does not ask for. The keep-alive state is in it because an outstanding
/// probe is exactly what a reader debugging a stalled socket wants to see
/// — and, per §7's first question, the only place it is visible.
impl<I: std::fmt::Debug, Tm: Timer> std::fmt::Debug for TungsteniteWebSocket<I, Tm> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TungsteniteWebSocket")
            .field("io", &self.io)
            .field("ended", &self.ended)
            .field("keep_alive", &self.keep_alive())
            .field(
                "ping_awaiting_a_pong",
                &self.live.as_ref().and_then(|l| l.outstanding),
            )
            .finish()
    }
}

/// `Unpin` whenever the IO is, stated rather than derived — the same
/// reasoning, in the same words, as [`hclient_native::IdleTimeout`]'s: the
/// derivation would also demand it of the clock, which [`Timer`] does not
/// require, and every `Stream`/`Sink` method here starts with
/// `self.get_mut()`. Sound because nothing in this type is ever pinned in
/// place: the only projections are `Pin::new(&mut io)`, which needs
/// `I: Unpin` on its own, and the sleep, which is behind its own
/// `Pin<Box<_>>`.
impl<I: Unpin, Tm: Timer> Unpin for TungsteniteWebSocket<I, Tm> {}

impl<I, Tm> WebSocket for TungsteniteWebSocket<I, Tm>
where
    I: Read + Write + Unpin,
    Tm: Timer,
{
}

impl<I, Tm> Stream for TungsteniteWebSocket<I, Tm>
where
    I: Read + Write + Unpin,
    Tm: Timer,
{
    type Item = Result<Message, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Self {
            io,
            ctx,
            ended,
            live,
        } = self.get_mut();
        if *ended {
            return Poll::Ready(None);
        }
        loop {
            // A ping the socket refused is formatted into `tungstenite`'s
            // own `out_buffer`, and `read` flushes only what *it* queued
            // (`additional_send`/`unflushed_additional`) — never a frame
            // this file wrote. So while a probe is outstanding the retry
            // is ours, and it is cheap: with an empty buffer `flush` is a
            // `poll_flush` on the socket and nothing more.
            if live.as_ref().is_some_and(|l| l.outstanding.is_some())
                && let Err(e) = ctx.flush(&mut Shim { io, cx })
                && !would_block(&e)
            {
                *ended = true;
                return Poll::Ready(Some(Err(ws_error(e))));
            }
            let read = ctx.read(&mut Shim { io, cx });
            // Before the match, and on every `Ok` arm including the ones
            // that `continue`: the interval measures silence on the wire,
            // not silence a caller could see.
            if let (Ok(frame), Some(l)) = (&read, live.as_mut()) {
                l.saw(frame);
            }
            match read {
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
                // The socket has nothing, so this is the only moment
                // `poll_next` has a `Pending` to spend on the keep-alive —
                // and the read waker has just been registered, which is
                // what makes it safe to park here at all.
                Err(e) if would_block(&e) => {
                    let Some(l) = live.as_mut() else {
                        return Poll::Pending;
                    };
                    // At most two turns: arm the sleep, then either park
                    // on it or spend it and arm the next one.
                    loop {
                        if l.outstanding.is_none() && !ctx.can_write() {
                            // A close of ours has gone out. `tungstenite`
                            // refuses every write past that
                            // (`SendAfterClosing`) and RFC 6455 makes a
                            // ping after a close meaningless anyway, so
                            // the keep-alive is over — with nothing armed,
                            // so nothing wakes this task but the peer's
                            // answer. The closing handshake is therefore
                            // unbounded, the same gap `poll_close` records
                            // for itself.
                            l.sleep = None;
                            return Poll::Pending;
                        }
                        if l.sleep.is_none() {
                            let due = if l.outstanding.is_some() {
                                l.config.within
                            } else {
                                l.config.every
                            };
                            l.sleep = Some(Box::pin(l.timer.sleep(due)));
                        }
                        let sleep = l.sleep.as_mut().expect("just set");
                        if sleep.as_mut().poll(cx).is_pending() {
                            return Poll::Pending;
                        }
                        l.sleep = None;
                        if l.outstanding.is_some() {
                            *ended = true;
                            return Poll::Ready(Some(Err(l.no_pong())));
                        }
                        let payload = l.probe();
                        match ctx.write(&mut Shim { io, cx }, Frame::Ping(payload)) {
                            // Formatted into `out_buffer`; the retry at
                            // the top of the outer loop finishes it.
                            Ok(()) => {}
                            Err(e) if would_block(&e) => {}
                            Err(e) => {
                                *ended = true;
                                return Poll::Ready(Some(Err(ws_error(e))));
                            }
                        }
                        // Not optional: `write` only *buffers* a ping —
                        // `_write` flushes just when it had a pong or
                        // close of its own to send, and the default write
                        // buffer is 128 KiB. A ping written and not
                        // flushed never leaves.
                        match ctx.flush(&mut Shim { io, cx }) {
                            Ok(()) => {}
                            Err(e) if would_block(&e) => {}
                            Err(e) => {
                                *ended = true;
                                return Poll::Ready(Some(Err(ws_error(e))));
                            }
                        }
                        // Round again, to arm and poll the deadline: the
                        // read waker alone would never bring this task
                        // back to notice it expire.
                    }
                }
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

impl<I, Tm> Sink<Message> for TungsteniteWebSocket<I, Tm>
where
    I: Read + Write + Unpin,
    Tm: Timer,
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

/// A WebSocket connector over a [`Native`] transport: the thing that
/// implements the seam.
///
/// # Why it borrows rather than owns
///
/// A `Native` is not `Clone`, and `hclient::Client::builder` takes its
/// transport by value, so a connector that *owned* one would leave a
/// caller who also makes HTTP requests holding a second transport with a
/// second connection pool — or would have to implement `Transport` itself
/// and forward every request through a type that has nothing to do with
/// requests. Borrowing costs one expression at the call site and keeps one
/// transport, one pool, one resolver:
///
/// ```no_run
/// # async fn doc<R, T, D>(client: &hclient::Client<hclient_native::Native<R, T, D>>)
/// # -> Result<(), Box<dyn std::error::Error>>
/// # where R: hclient_rt::TcpConnect + hclient_rt::Timer + Clone + 'static,
/// #       R::Stream: 'static,
/// #       T: hclient_tls::TlsConnect, T::Stream<R::Stream>: 'static,
/// #       D: hclient_dns::Resolve {
/// use hclient_core::unversioned::WebSocketConnect;
/// use hclient_tungstenite::Tungstenite;
///
/// let req = http::Request::builder().uri("wss://example.com/chat").body(())?;
/// let ws = Tungstenite::new(client.transport()).websocket(req).await?;
/// # let _ = ws; Ok(()) }
/// ```
///
/// # What it is not
///
/// It is not a `Transport` and does not pretend to be one: nothing here
/// sends an HTTP request, and the connection it opens is never pooled at
/// either end (see the module doc). Nor is it a second configuration of
/// the transport — everything the socket is opened with (the resolver, the
/// TLS backend, `TcpOpts`, `Timeouts::connect` out of the request's
/// extensions) is the `Native`'s. The one thing that is this type's own is
/// [`WebSocketKeepAlive`], because pings are frames.
pub struct Tungstenite<'a, R, T, D, H = hclient_core::unversioned::NoHooks>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    native: &'a Native<R, T, D, H>,
    keep_alive: Option<WebSocketKeepAlive>,
}

/// Hand-written for the reason [`TungsteniteWebSocket`]'s is, one level
/// out: `#[derive(Debug)]` would demand it of `Native`, whose own derived
/// `Debug` demands `R::Instant: Debug` — which [`Timer`] does not ask for.
/// What is this type's own is the keep-alive, and that is what a reader
/// debugging a socket that never pings is looking for.
impl<R, T, D, H> std::fmt::Debug for Tungstenite<'_, R, T, D, H>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tungstenite")
            .field("keep_alive", &self.keep_alive)
            .finish_non_exhaustive()
    }
}

/// Hand-written rather than derived, for the reason
/// [`TungsteniteWebSocket`]'s `Debug` is: the derivation would demand
/// `Clone` of all four type parameters, and this value is a shared
/// reference plus two `Duration`s whatever they are.
impl<R, T, D, H> Clone for Tungstenite<'_, R, T, D, H>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<R, T, D, H> Copy for Tungstenite<'_, R, T, D, H>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
}

impl<'a, R, T, D, H> Tungstenite<'a, R, T, D, H>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    /// Open WebSockets over `native`, with no liveness bound — which is
    /// the default and the whole of it: see [`Tungstenite::keep_alive`].
    pub fn new(native: &'a Native<R, T, D, H>) -> Self {
        Self {
            native,
            keep_alive: None,
        }
    }

    /// Prove the peer of an open WebSocket is still there, with RFC 6455
    /// §5.5.2 ping/pong — **off unless this is called.**
    ///
    /// Without it an open WebSocket has no bound of any kind: only the
    /// handshake reads `Timeouts::connect`, so a peer that vanishes
    /// without a `FIN` leaves a `Stream` that never yields and never
    /// errors. With it, a socket silent for
    /// [`every`](WebSocketKeepAlive::every) sends a `Ping`, and a peer
    /// that does not answer within [`within`](WebSocketKeepAlive::within)
    /// ends the `Stream` with an [`ErrorKind::Body`] whose source is
    /// [`PongNotReceived`] — an error, distinguishable from the peer
    /// having said goodbye, which arrives as `Message::Close`.
    ///
    /// It is **off by default** because a default that pings is a default
    /// that sends traffic nobody asked for, and on a metered radio that is
    /// not free. Two more things a caller should know before turning it
    /// on, both properties of this crate rather than of the seam:
    ///
    /// - **A caller that stops polling gets no keep-alive.** Nothing is
    ///   spawned here — neither this crate nor `hclient-native` has a
    ///   `Spawn` bound, deliberately — so the ping is written from
    ///   `poll_next` or not at all. Unlike `hclient-h3`, where a spawned
    ///   driver keeps a pooled connection alive for requests nobody has
    ///   made yet, a WebSocket always has a caller, and one that is not
    ///   polling is not waiting for anything.
    /// - **A busy connection never pings.** `every` measures silence on
    ///   the wire, and any inbound frame restarts it.
    ///
    /// It applies to every WebSocket opened from this connector
    /// afterwards; [`TungsteniteWebSocket::keep_alive`] reads back what a
    /// given socket got. There is no counterpart on `hclient-fetch`, so
    /// asking a browser for this does not compile —
    /// `docs/w4-upgrade-seam.md` §7.
    ///
    /// **It is here rather than on `Native` because pings and pongs are
    /// frames**, which is §8's reason for this crate existing at all. On
    /// the transport it would be a knob whose type lives in a crate the
    /// transport must not depend on.
    #[must_use]
    pub fn keep_alive(mut self, keep_alive: WebSocketKeepAlive) -> Self {
        self.keep_alive = Some(keep_alive);
        self
    }
}

/// The seam, implemented — which is the whole of how this crate says it
/// can do WebSocket. There is no capability to read and no method that
/// returns `Unsupported`: something that cannot do this does not write
/// this `impl`, and asking it does not compile.
///
/// `R: Clone` came with the keep-alive and is not a new restriction:
/// `Transport for Native` has required it since v0.2 W2, and every runtime
/// in this workspace is a ZST or a handle. The socket needs its own clock
/// because it outlives the call that opened it, and a `&R` cannot.
impl<R, T, D, H> WebSocketConnect for Tungstenite<'_, R, T, D, H>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
{
    type WebSocket = TungsteniteWebSocket<NativeIo<R, T>, R>;

    async fn websocket(&self, req: http::Request<()>) -> Result<Self::WebSocket, Error> {
        let (handshake, req) = Handshake::start(req)?;
        // The connection, the `101`-by-status check and the h1 upgrade are
        // `Native::upgrade`'s; what comes back is a `101`'s head with the
        // connection still assembled behind it, so the three checks below
        // run *before* anything is taken apart. That ordering is
        // structural here rather than remembered: there is no way to reach
        // the socket without having been handed the head first.
        let upgrading = self.native.upgrade(req).await?;
        handshake.accept(upgrading.head())?;
        let (io, read_buf) = upgrading.finish().await?;
        Ok(TungsteniteWebSocket::new(
            io,
            read_buf,
            self.native.runtime().clone(),
            self.keep_alive,
        ))
    }
}
