//! HTTP/2 exchange, driven all the way to the response without a single
//! `spawn` — the same property [`crate::h1`] holds, on a protocol that
//! makes it much harder to keep.
//!
//! # Why this is not `hyper/http2`, which the v0.2 design document proposed
//!
//! The design document's §W3 wrote the feature as `http2 =
//! ["hyper/http2"]`. That does not work here, and the reason is not a
//! preference: **hyper's HTTP/2 client hands the connection driver to an
//! executor, and the trait that names an executor is sealed.**
//!
//! `hyper::client::conn::http2::handshake` takes the executor as a type
//! parameter, `E: Http2ClientConnExec<B, T>` (hyper 1.11.0,
//! `src/client/conn/http2.rs:75-84`), and `Http2ClientConnExec` is
//! declared `pub trait Http2ClientConnExec<B, T>: ... +
//! sealed_client::Sealed<(B, T)>` (`src/rt/bounds.rs:51-52`) — sealed,
//! with the only implementation a blanket one over `hyper::rt::Executor`.
//! What that executor receives is not an optional extra: the handshake
//! calls `exec.execute_h2_future(H2ClientFuture::Task { .. })`
//! (`src/proto/h2/client.rs:192`) and that task *is* the h2 connection.
//! The `Connection` value handshake returns to the caller is a different
//! future — the request dispatcher. Nothing moves unless both are polled,
//! and the first one is only reachable through an executor.
//!
//! An executor that queues futures instead of spawning them (this crate
//! has nowhere to spawn — see [`crate::pool`]'s module doc for why `Spawn`
//! is not on this seam) would have to store `H2ClientFuture<B, T, E>`,
//! whose type lives in hyper's private `proto` module and cannot be named
//! from outside. So the queue would have to be `Box<dyn Future>`, and that
//! costs either `NativeBody: Send` (a plain `dyn Future` is never `Send`)
//! or a `Send` bound on this crate's IO (`dyn Future + Send` demands it) —
//! and under Cargo's feature unification a crate that never asked for h2
//! pays whichever one is chosen. That trade never has to be made: the
//! blanket impl requires `E: Executor<H2ClientFuture<B, T, E>>`, and the
//! sealing means no other route exists. `B::Data: Send`, also on
//! `handshake`, is a third bound of the same kind.
//!
//! The [`h2`] crate underneath hyper has no such requirement.
//! `h2::client::Connection<T, B>` is a concrete future, and this module
//! polls it by hand next to the request — exactly what `h1::exchange` and
//! `h1::H1Body` already do with hyper's HTTP/1 `Connection`. Nothing
//! is boxed, so auto traits keep reaching the response body, and no bound
//! anywhere in this crate's public shape changes when the feature is on.
//!
//! # One stream per connection, and what W1 is resting on
//!
//! HTTP/2 multiplexes; this transport does not use that, and the omission
//! is deliberate rather than pending. A connection is **checked out of the
//! pool exclusively**, exactly as an HTTP/1 one is, and carries one stream
//! at a time — see [`crate::pool`]'s module doc, section "What an h2
//! connection is checked out for", which is where that policy is written
//! down and where it would have to be changed.
//!
//! Two consequences, and the second one is a load-bearing coincidence that
//! must not be mistaken for a proof:
//!
//! 1. **Dropping an exchange closes its own connection.** The
//!    `Connection` future lives inside `execute`'s future and then inside
//!    [`H2Body`]; dropping either drops it, which drops the socket. h2's
//!    stream references also emit `RST_STREAM` as they drop, but on this
//!    path the connection goes with them, so the server sees the socket
//!    close — the same thing `Capabilities::cancel_on_drop` promises for
//!    HTTP/1, and the same test observes it from the far end.
//! 2. **No neighbouring stream can be torn down by that, because there is
//!    never one.** This is true *because* of the check-out policy above,
//!    not because of anything in this file. The day somebody hands the
//!    same connection to two concurrent requests, W1's rule ("cancelling
//!    one stream must not tear down the others") stops holding here, and
//!    it will stop holding silently: nothing in this module would fail to
//!    compile. Whoever lifts the exclusivity owns re-establishing it —
//!    by keeping the connection alive past the drop of one stream, which
//!    means somebody has to keep polling it, which is the `Spawn` question
//!    again.
//!
//! # `full_duplex` is `false`, and here that is not merely a declaration
//!
//! [`exchange`] writes the whole request body before it waits for the
//! response. h2 would permit otherwise, and a later version may; today the
//! floor `Capabilities` reports is literally what this code does, so a
//! caller that believed a `true` would deadlock exactly as the v0.2 design
//! document says. See `Native::new`'s comment on `full_duplex` for why the
//! capability reports the floor and not the protocol's best case.
use crate::body::OutgoingBody;
use crate::pool::CheckIn;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_ng_core::{Error, ErrorKind};
use hyper::rt::{Read, Write};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// The one conversion point from [`h2::Error`] to [`http_ng_core::Error`]
/// in this file, so a category is chosen once per site rather than
/// scattered.
fn from_h2_error(e: h2::Error, fallback: ErrorKind) -> Error {
    Error::new(fallback, e)
}

/// `hyper::rt::{Read, Write}` → `tokio::io::{AsyncRead, AsyncWrite}`.
///
/// [`h2`] is written against tokio's IO traits, and every stream in this
/// vertical is normalized to hyper's (`http_ng_rt::FuturesIo` bridges
/// futures-io into them, `http_ng_rt_tokio` hands tokio's over directly).
/// This is the fourth side of that square, and the only one h2 needs.
///
/// **`unsafe`-free, and without the copy `FuturesIo` pays.** The
/// interesting direction is the read: `tokio::io::ReadBuf` hands out its
/// unfilled tail as an initialized `&mut [u8]`
/// (`ReadBuf::initialize_unfilled`, which remembers how far it has
/// initialized and does not re-zero), a `hyper::rt::ReadBuf` is built over
/// that same slice, and whatever the inner IO filled is handed back to the
/// tokio buffer with `advance`. One borrow, no scratch buffer, no
/// `unsafe`.
#[derive(Debug)]
pub(crate) struct TokioIo<I> {
    inner: I,
}

impl<I> TokioIo<I> {
    pub(crate) fn new(inner: I) -> Self {
        Self { inner }
    }
}

impl<I> tokio::io::AsyncRead for TokioIo<I>
where
    I: Read + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let filled = {
            let dst = buf.initialize_unfilled();
            let mut hyper_buf = hyper::rt::ReadBuf::new(dst);
            match Pin::new(&mut this.inner).poll_read(cx, hyper_buf.unfilled()) {
                Poll::Ready(Ok(())) => hyper_buf.filled().len(),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        };
        buf.advance(filled);
        Poll::Ready(Ok(()))
    }
}

impl<I> tokio::io::AsyncWrite for TokioIo<I>
where
    I: Write + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    /// Forwarded rather than left at the default `false`: h2 writes a
    /// frame header and its payload as separate buffers, so a transport
    /// that can gather them into one syscall should be told it may.
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }
}

/// A connection that has completed its HTTP/2 handshake: the two halves
/// [`h2::client::handshake`] hands back, kept together for the same reason
/// [`crate::h1::Established`] keeps hyper's two — neither is usable alone.
pub(crate) struct Established<I>
where
    I: Read + Write + Unpin,
{
    sender: h2::client::SendRequest<Bytes>,
    conn: h2::client::Connection<TokioIo<I>, Bytes>,
}

impl<I> std::fmt::Debug for Established<I>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("h2::Established").finish_non_exhaustive()
    }
}

/// The HTTP/2 connection preface and the first `SETTINGS` exchange.
///
/// Split out from [`exchange`] for the same reason the HTTP/1 handshake
/// is: a pooled connection has already had one.
pub(crate) async fn handshake<I>(io: I) -> Result<Established<I>, Error>
where
    I: Read + Write + Unpin,
{
    let (sender, conn) = h2::client::handshake(TokioIo::new(io))
        .await
        .map_err(|e| from_h2_error(e, ErrorKind::Connect))?;
    Ok(Established { sender, conn })
}

/// Whether a connection taken out of the pool is still worth a request.
///
/// **Exactly one poll, and it never suspends** — the same contract as
/// [`crate::h1::is_reusable`], for the same two reasons (nothing polls an
/// idle connection, so this is the only moment a `GOAWAY` or a closed
/// socket is noticed; and a checkout that suspended could cost the whole
/// request rather than one socket).
///
/// The order matters here too, for a different reason than on HTTP/1: h2's
/// `SendRequest::poll_ready` answers from the peer's `SETTINGS`
/// (`MAX_CONCURRENT_STREAMS`) and from the connection's own state, and
/// both of those are updated from inside `Connection::poll`.
pub(crate) async fn is_reusable<I>(est: &mut Established<I>) -> bool
where
    I: Read + Write + Unpin,
{
    std::future::poll_fn(|cx| {
        if Pin::new(&mut est.conn).poll(cx).is_ready() {
            return Poll::Ready(false);
        }
        Poll::Ready(matches!(est.sender.poll_ready(cx), Poll::Ready(Ok(()))))
    })
    .await
}

/// Headers HTTP/2 forbids on the wire (RFC 9113 §8.2.2), removed rather
/// than passed on.
///
/// A caller that set `Connection: close` against an HTTP/1 transport, or a
/// middleware that added `Transfer-Encoding`, must not turn the request
/// into a protocol error the moment ALPN happens to pick h2 — which is a
/// decision the caller did not make and cannot see in advance. `TE` is
/// left alone: RFC 9113 permits it with the single value `trailers`, and a
/// caller who set it to something else gets the refusal they asked for.
fn strip_connection_headers(headers: &mut http::HeaderMap) {
    for name in [
        http::header::CONNECTION,
        http::header::TRANSFER_ENCODING,
        http::header::UPGRADE,
        http::header::PROXY_AUTHENTICATE,
        http::header::PROXY_AUTHORIZATION,
        http::HeaderName::from_static("keep-alive"),
        http::HeaderName::from_static("proxy-connection"),
    ] {
        headers.remove(name);
    }
}

/// One request over one connection — pooled or fresh, this function cannot
/// tell and does not need to.
///
/// # What it does with `Failed`, and where it is weaker than HTTP/1
///
/// [`crate::h1::exchange`] can report `Failed::NotSent` on hyper's own
/// verdict, because `SendRequest::try_send_request` hands the request
/// object back when nothing of it reached the wire. h2 has no equivalent:
/// `SendRequest::send_request` consumes the head and returns only an
/// error. So the one moment this function can honestly say "not sent" is
/// the poll of `Connection` *before* the head is handed over — which is
/// also the moment that matters, because the failure the pool creates is
/// "the server closed this connection while it was idle". Past that point
/// every failure is [`Failed::Sent`]: conservative in the right direction,
/// since `Sent` is the verdict that suppresses a retry.
pub(crate) async fn exchange<I>(
    est: Established<I>,
    req: http::Request<OutgoingBody>,
    checkin: Option<CheckIn<I>>,
) -> Result<http::Response<H2Body<I>>, crate::established::Failed>
where
    I: Read + Write + Unpin,
{
    use crate::established::Failed;

    let Established {
        mut sender,
        mut conn,
    } = est;

    // One last look at the connection while the request is still OURS —
    // the same move, in the same place and for the same reason, as
    // `h1::exchange`'s. Exactly one poll, and it never suspends.
    let dead =
        std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut conn).poll(cx).is_ready())).await;
    if dead {
        return Err(Failed::NotSent {
            error: Error::new(ErrorKind::Connect, ConnectionWentAwayBeforeTheRequest),
            request: Box::new(req),
        });
    }

    // `poll_ready` before `send_request` is not optional in h2 — it is how
    // the peer's `MAX_CONCURRENT_STREAMS` is respected. It is driven
    // alongside `Connection` because that is what updates it, and a
    // connection that ends while we wait is still a request we own.
    let mut conn_done = false;
    let ready = std::future::poll_fn(|cx| {
        if !conn_done {
            match Pin::new(&mut conn).poll(cx) {
                Poll::Ready(Ok(())) => conn_done = true,
                Poll::Ready(Err(e)) => {
                    conn_done = true;
                    return Poll::Ready(Err(from_h2_error(e, ErrorKind::Connect)));
                }
                Poll::Pending => {}
            }
        }
        match sender.poll_ready(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(from_h2_error(e, ErrorKind::Connect))),
            Poll::Pending if conn_done => Poll::Ready(Err(Error::new(
                ErrorKind::Connect,
                ConnectionWentAwayBeforeTheRequest,
            ))),
            Poll::Pending => Poll::Pending,
        }
    })
    .await;
    if let Err(error) = ready {
        return Err(Failed::NotSent {
            error,
            request: Box::new(req),
        });
    }

    let (mut parts, mut outgoing) = req.into_parts();
    strip_connection_headers(&mut parts.headers);
    // The URI reaches this function in absolute form — `Native::execute`
    // runs `origin_form` on the HTTP/1 path only, because h2 builds
    // `:scheme`/`:authority`/`:path` out of exactly this URI and an
    // origin-form one would leave it with neither scheme nor authority.
    parts.version = http::Version::HTTP_2;
    let eos = outgoing.is_end_stream();
    let head = http::Request::from_parts(parts, ());

    let (mut resp_fut, mut send_stream) = match sender.send_request(head, eos) {
        Ok(pair) => pair,
        Err(e) => return Err(Failed::Sent(from_h2_error(e, ErrorKind::Connect))),
    };

    let mut pumping = !eos;
    let mut pending: Option<Bytes> = None;
    let resp = std::future::poll_fn(|cx| {
        loop {
            if !conn_done {
                match Pin::new(&mut conn).poll(cx) {
                    Poll::Ready(Ok(())) => conn_done = true,
                    Poll::Ready(Err(e)) => {
                        conn_done = true;
                        return Poll::Ready(Err(Failed::Sent(from_h2_error(e, ErrorKind::Body))));
                    }
                    Poll::Pending => {}
                }
            }
            if pumping {
                match poll_pump(&mut outgoing, &mut send_stream, &mut pending, cx) {
                    // `Done` and `PeerStoppedReading` alike: there is
                    // nothing more to write, and whether the exchange
                    // succeeded is `resp_fut`'s answer, not the pump's.
                    // RFC 9113 §8.1 — see `poll_pump`'s doc comment.
                    Poll::Ready(Ok(_)) => {
                        pumping = false;
                        // Round the loop rather than falling through: the
                        // frames the pump just queued only reach the
                        // socket from inside `Connection::poll`.
                        continue;
                    }
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(Failed::Sent(e))),
                    Poll::Pending if conn_done => {
                        return Poll::Ready(Err(Failed::Sent(Error::new(
                            ErrorKind::Connect,
                            ConnectionEndedWithTheRequestQueued,
                        ))));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
            return match Pin::new(&mut resp_fut).poll(cx) {
                Poll::Ready(Ok(r)) => Poll::Ready(Ok(r)),
                Poll::Ready(Err(e)) => {
                    Poll::Ready(Err(Failed::Sent(from_h2_error(e, ErrorKind::Connect))))
                }
                // The connection is over and the response never came: the
                // exchange cannot be resolved by anything still running,
                // so returning here is the alternative to hanging. The
                // same shape, and the same reasoning, as `h1::exchange`'s
                // `Poll::Pending if conn_done`.
                Poll::Pending if conn_done => Poll::Ready(Err(Failed::Sent(Error::new(
                    ErrorKind::Connect,
                    ConnectionEndedWithTheRequestQueued,
                )))),
                Poll::Pending => Poll::Pending,
            };
        }
    })
    .await;
    let resp = resp?;

    let (parts, recv) = resp.into_parts();
    // The same condition for both, and for the same reason as on HTTP/1: a
    // finished connection is nothing to keep polling and nothing to pool,
    // and two conditions that must agree are an invitation to disagree.
    let (conn, reuse) = if conn_done {
        (None, None)
    } else {
        (Some(conn), checkin.map(|checkin| Reuse { checkin, sender }))
    };

    Ok(http::Response::from_parts(
        parts,
        H2Body {
            recv,
            conn,
            reuse,
            data_done: false,
        },
    ))
}

/// Why [`poll_pump`] finished, and the reason the two are not the same
/// thing said twice.
///
/// [`exchange`] stops pumping on either, so the distinction buys no
/// branch there — it is here because the *request* is complete in one
/// case and truncated in the other, and a later reader deciding what to
/// do about that (a `content-length` guard, a capability, a log line)
/// needs the answer to still exist.
enum Pumped {
    /// The whole body reached h2, end-of-stream included.
    Done,
    /// The peer stopped reading before it was all written. See
    /// [`poll_pump`]'s "the peer stopped reading" section.
    PeerStoppedReading,
}

/// Writes as much of the request body as flow control currently allows.
///
/// `pending` is the one chunk that has been taken off the body and not yet
/// fully written — the state that has to survive a `Pending`, since
/// `http_body::Body::poll_frame` cannot be asked to hand the same frame
/// back twice.
///
/// **Capacity is reserved rather than assumed.** `SendStream::send_data`
/// is willing to accept data with no capacity available and buffer it,
/// with, in h2's own words, unbounded buffering — which would turn
/// `Capabilities::streaming_request_body` into a promise to read a stream
/// as fast as the producer can make it and hold it in memory. Reserving
/// and waiting is what makes the declared streaming real: a slow server
/// stops granting window, `poll_capacity` goes `Pending`, and the caller's
/// stream stops being polled.
///
/// # "The peer stopped reading" is not a failure of the request
///
/// RFC 9113 §8.1: *"A server MAY request that the client abort
/// transmission of a request without error by sending a `RST_STREAM` with
/// an error code of `NO_ERROR` after sending a complete response … Clients
/// **MUST NOT** discard responses as a result of receiving such a
/// `RST_STREAM`."* Every server that answers a `404`, a `401` or a `413`
/// without reading the body does exactly this, and h2's own server does it
/// for them without being asked: dropping the request's `RecvStream` while
/// the response is already complete schedules a `RST_STREAM(NO_ERROR)`
/// (`h2-0.4.15/src/proto/streams/streams.rs:1601-1618`, `maybe_cancel`).
///
/// That reset closes the **send** half only. The response the peer already
/// sent stays in `pending_recv` and is still handed out by `poll_response`
/// (`.../recv.rs:336-365`) — measured, not assumed: with the request body
/// abandoned mid-write, `resp_fut` still resolves `200`. But every write
/// this function could make now fails, this function turned that into an
/// error, and [`exchange`] returned on it **without ever polling
/// `resp_fut`**. The complete response was one poll away and was thrown
/// away. That is the defect [`Pumped::PeerStoppedReading`] exists for.
///
/// # One question, asked before every write, rather than four verdicts
///
/// The four writes below fail *differently* when the peer has stopped
/// reading, and only one of the four failures is identifiable from
/// outside h2:
///
/// | site | how it fails on a reset stream | tellable apart? |
/// |---|---|---|
/// | `poll_capacity` | `Poll::Ready(None)` — no longer `is_send_streaming` (`.../send.rs:363-370`) | yes, but `None` also means an API misuse of ours |
/// | `send_data(now, false)` | `UserError::InactiveStreamId` | no — `reason() == None`, `is_reset() == false` |
/// | `send_data(Bytes::new(), true)` (end of stream) | the same | no |
/// | `send_trailers(..)` | the same | no |
///
/// So the question is asked once, at the top of the loop, of the one API
/// that answers it in types: [`h2::SendStream::poll_reset`]. `Ready(Ok(_))`
/// is a `RST_STREAM` or a `GOAWAY` from the peer; `Ready(Err(_))` is the
/// stream closed by something that is not a reset (a dead connection);
/// `Pending` is a stream that is still open (`ensure_reason`,
/// `.../state.rs:446-462`). Placing it there also means the three
/// unidentifiable sites never see the reset at all — the pump returns
/// before reaching them — so they keep propagating their errors unchanged
/// and no `Display`-string matching is needed anywhere.
///
/// hyper asks the same question in the same place
/// (`hyper-1.11.0/src/proto/h2/mod.rs:145-156`, the first thing
/// `PipeToSendStream::poll` does) and answers it the *opposite* way, with
/// an error. That is not a disagreement about the RFC: hyper's body pipe
/// is a **separate spawned task** from its response future, so an error
/// there fails the write and leaves the response untouched. Here the pump
/// and the response are the same future — this module has nowhere to spawn
/// — so an error there is exactly what discards the response.
///
/// # Why this is safe without deciding anything
///
/// The tolerance is a *deferral*, not a verdict. Stopping the pump does
/// not declare the exchange a success; it hands the question to
/// `resp_fut`, which is the only side that knows whether a response
/// exists. When the stream was reset for a reason that left no response —
/// a `RST_STREAM` with a real error code, a `GOAWAY` past our stream id —
/// `poll_response` finds `pending_recv` empty, `ensure_recv_open` returns
/// the stream's own error (`.../state.rs:433-443`), and the exchange fails
/// as it did before. A dead connection is caught one branch earlier still,
/// by the poll of `Connection` at the top of [`exchange`]'s loop.
///
/// It also cannot slide *before* the head by accident: `send_request`
/// yields the `SendStream` this takes, so there is no `send` to ask until
/// the head is on the wire.
fn poll_pump(
    body: &mut OutgoingBody,
    send: &mut h2::SendStream<Bytes>,
    pending: &mut Option<Bytes>,
    cx: &mut Context<'_>,
) -> Poll<Result<Pumped, Error>> {
    loop {
        // Before every write, and not only before the first: the peer may
        // stop reading at any point, and each of the four writes below
        // fails differently when it has. This is the one place that asks.
        match send.poll_reset(cx) {
            Poll::Ready(Ok(_)) => return Poll::Ready(Ok(Pumped::PeerStoppedReading)),
            Poll::Ready(Err(e)) => return Poll::Ready(Err(from_h2_error(e, ErrorKind::Body))),
            Poll::Pending => {}
        }
        if let Some(mut chunk) = pending.take() {
            if send.capacity() == 0 {
                send.reserve_capacity(chunk.len());
                match send.poll_capacity(cx) {
                    Poll::Ready(Some(Ok(_))) => {}
                    Poll::Ready(Some(Err(e))) => {
                        return Poll::Ready(Err(from_h2_error(e, ErrorKind::Body)));
                    }
                    // Not "no more capacity for now": there will never
                    // be any. With `peer_reset` above having already
                    // answered `Pending`, the stream is not closed by the
                    // peer, so this is our own send half — an API misuse
                    // rather than a message from the far end.
                    Poll::Ready(None) => {
                        return Poll::Ready(Err(Error::new(
                            ErrorKind::Body,
                            StreamClosedWhileSendingTheRequestBody,
                        )));
                    }
                    Poll::Pending => {
                        *pending = Some(chunk);
                        return Poll::Pending;
                    }
                }
            }
            let now = chunk.split_to(send.capacity().min(chunk.len()));
            if let Err(e) = send.send_data(now, false) {
                return Poll::Ready(Err(from_h2_error(e, ErrorKind::Body)));
            }
            if !chunk.is_empty() {
                *pending = Some(chunk);
                continue;
            }
        }
        match Pin::new(&mut *body).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                Ok(data) => {
                    if !data.is_empty() {
                        *pending = Some(data);
                    }
                }
                // Trailers close the send half by themselves — there is no
                // empty end-of-stream frame to follow them with.
                Err(frame) => {
                    return Poll::Ready(match frame.into_trailers() {
                        Ok(trailers) => send
                            .send_trailers(trailers)
                            .map(|()| Pumped::Done)
                            .map_err(|e| from_h2_error(e, ErrorKind::Body)),
                        // `http_body::Frame` is non-exhaustive: a frame
                        // that is neither data nor trailers is one this
                        // version of the crate has no name for, and
                        // guessing what to put on the wire for it is how a
                        // body silently loses content.
                        Err(_) => Err(Error::new(ErrorKind::Body, UnknownRequestBodyFrame)),
                    });
                }
            },
            Poll::Ready(Some(Err(e))) => {
                // The caller's own body failed. The stream is reset rather
                // than left half-written, so the server learns that what
                // it has is not the whole request.
                send.send_reset(h2::Reason::CANCEL);
                return Poll::Ready(Err(e));
            }
            Poll::Ready(None) => {
                return Poll::Ready(
                    send.send_data(Bytes::new(), true)
                        .map(|()| Pumped::Done)
                        .map_err(|e| from_h2_error(e, ErrorKind::Body)),
                );
            }
            Poll::Pending => return Poll::Pending,
        }
    }
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
fn stopped_after_a_complete_response(e: &h2::Error) -> bool {
    e.is_reset() && e.reason() == Some(h2::Reason::NO_ERROR)
}

/// The h2 counterpart of [`crate::h1`]'s connection-went-away error: the
/// pooled connection turned out to be finished at the last moment before
/// its request was handed over.
#[derive(Debug, thiserror::Error)]
#[error("the pooled HTTP/2 connection was closed before the request was sent")]
struct ConnectionWentAwayBeforeTheRequest;

/// The residual race the pool cannot close, on h2: the connection ended
/// between the request being handed to h2 and the response arriving.
#[derive(Debug, thiserror::Error)]
#[error("the HTTP/2 connection ended while the request was still in flight")]
struct ConnectionEndedWithTheRequestQueued;

/// The peer reset the stream, or the connection went away, while the
/// request body was still being written.
#[derive(Debug, thiserror::Error)]
#[error("the HTTP/2 stream closed while the request body was still being sent")]
struct StreamClosedWhileSendingTheRequestBody;

/// A request body produced a frame that is neither data nor trailers.
#[derive(Debug, thiserror::Error)]
#[error("the request body produced a frame that is neither data nor trailers")]
struct UnknownRequestBodyFrame;

/// The half of [`CheckIn`] that only exists once the request has been
/// sent — see [`crate::h1`]'s `Reuse`, which this mirrors.
struct Reuse<I>
where
    I: Read + Write + Unpin,
{
    checkin: CheckIn<I>,
    sender: h2::client::SendRequest<Bytes>,
}

/// A response body that **polls the connection itself**, for the same
/// reason [`crate::h1::H1Body`] does: nothing else will.
///
/// Generic over the IO rather than boxed — see this module's doc comment
/// on what boxing would have cost.
pub struct H2Body<I>
where
    I: Read + Write + Unpin,
{
    recv: h2::RecvStream,
    /// `None` — the connection has already finished, and `recv` can be
    /// drained of whatever already arrived without it.
    conn: Option<h2::client::Connection<TokioIo<I>, Bytes>>,
    /// `None` — this connection will not be reused. As on HTTP/1, it is
    /// deliberately easier to lose reuse than to gain it.
    reuse: Option<Reuse<I>>,
    /// `poll_data` has answered `None`; what is left is the trailers.
    data_done: bool,
}

impl<I> std::fmt::Debug for H2Body<I>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H2Body")
            .field("still_driving_connection", &self.conn.is_some())
            .field("may_be_reused", &self.reuse.is_some())
            .finish()
    }
}

impl<I> H2Body<I>
where
    I: Read + Write + Unpin,
{
    /// The one and only check-in, called when the stream has ended
    /// cleanly and from nowhere else.
    fn hand_back_to_pool(&mut self) {
        let (Some(conn), Some(reuse)) = (self.conn.take(), self.reuse.take()) else {
            return;
        };
        reuse
            .checkin
            .put(crate::established::Established::H2(Box::new(Established {
                sender: reuse.sender,
                conn,
            })));
    }
}

impl<I> Body for H2Body<I>
where
    I: Read + Write + Unpin,
{
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let this = &mut *self;
        // The connection first — otherwise nothing new ever arrives in
        // `recv` (see the module doc comment, and `h1`'s identical move).
        if let Some(conn) = this.conn.as_mut() {
            match Pin::new(conn).poll(cx) {
                Poll::Ready(Ok(())) => {
                    this.conn = None;
                    this.reuse = None;
                }
                Poll::Ready(Err(e)) => {
                    this.conn = None;
                    this.reuse = None;
                    return Poll::Ready(Some(Err(from_h2_error(e, ErrorKind::Body))));
                }
                Poll::Pending => {}
            }
        }
        if !this.data_done {
            match this.recv.poll_data(cx) {
                Poll::Ready(Some(Ok(data))) => {
                    // Not optional bookkeeping: the receive window is what
                    // the peer is allowed to send, and a body that never
                    // released it would stall after the first window's
                    // worth of bytes.
                    if let Err(e) = this.recv.flow_control().release_capacity(data.len()) {
                        this.reuse = None;
                        return Poll::Ready(Some(Err(from_h2_error(e, ErrorKind::Body))));
                    }
                    return Poll::Ready(Some(Ok(Frame::data(data))));
                }
                Poll::Ready(Some(Err(e))) => {
                    this.reuse = None;
                    if stopped_after_a_complete_response(&e) {
                        return Poll::Ready(None);
                    }
                    return Poll::Ready(Some(Err(from_h2_error(e, ErrorKind::Body))));
                }
                Poll::Ready(None) => this.data_done = true,
                Poll::Pending => return Poll::Pending,
            }
        }
        match this.recv.poll_trailers(cx) {
            Poll::Ready(Ok(Some(trailers))) => Poll::Ready(Some(Ok(Frame::trailers(trailers)))),
            Poll::Ready(Ok(None)) => {
                this.hand_back_to_pool();
                Poll::Ready(None)
            }
            Poll::Ready(Err(e)) => {
                this.reuse = None;
                if stopped_after_a_complete_response(&e) {
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(Err(from_h2_error(e, ErrorKind::Body))))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.recv.is_end_stream()
    }

    /// h2 carries no `Content-Length` of its own that `RecvStream` would
    /// know about, so there is nothing better than the default to give.
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}
