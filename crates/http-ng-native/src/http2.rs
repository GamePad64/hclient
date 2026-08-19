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
//! # One stream per connection by default, and what W1 is resting on
//!
//! HTTP/2 multiplexes; this transport does not use that **unless
//! [`crate::Native::multiplexed`] was asked for** (v0.4), and the default
//! is deliberate rather than pending. A connection is then **checked out of
//! the pool exclusively**, exactly as an HTTP/1 one is, and carries one
//! stream at a time — see [`crate::pool`]'s module doc, section "What an h2
//! connection is checked out for", which is where that policy is written
//! down.
//!
//! Everything in this section is about that default. What the opt-in
//! changes is at the end of it, and the code it adds is below the
//! `Sharing one connection between concurrent requests` line in this file:
//! [`H2Driver`], [`Shared`], [`exchange_shared`].
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
//! **`docs/h2-multiplexing.md` is that question answered, and then
//! built.** [`crate::Native::multiplexed`] lifts the exclusivity, and the
//! two consequences above are exactly what it costs:
//!
//! 1. Dropping an exchange no longer closes the connection — the driver
//!    owns it — so `Pump`'s `Drop` becomes the thing the peer sees. Its
//!    own doc comment said the `RST_STREAM(CANCEL)` it queues was
//!    unobservable and was kept for this day; it is observed now, in
//!    `tests/grpc_shape.rs`'s
//!    `a_multiplexed_cancellation_resets_the_stream_and_leaves_its_neighbour_alone`,
//!    which is where that file's `Ending::Reset` variant finally has a
//!    producer.
//! 2. There **are** neighbours, so W1's rule stops holding vacuously and
//!    is a test instead:
//!    `tests/http2.rs`'s
//!    `dropping_one_exchange_leaves_a_concurrent_one_alone_on_a_shared_connection`
//!    is its exclusive sibling with one call added to the client and
//!    `accepted == 1`.
//!
//! # Full duplex, and why `Capabilities::full_duplex` still reports `false`
//!
//! [`exchange`] used to write the whole request body before it waited for
//! the response, and that arrangement is what `full_duplex: false` used to
//! describe. It no longer is. The body is written by a `Pump` polled
//! *beside* the response future, and a pump still running when the head
//! arrives **moves into [`H2Body`]**, which drives it from `poll_frame`.
//! v0.3 did the same thing for HTTP/3 (`http_ng_h3::pump` is the
//! template), and nothing is spawned here either — this crate has nowhere
//! to spawn, and a spawned pump would go on uploading behind a caller that
//! walked away, with nowhere for its errors to go.
//!
//! **The capability still reports `false`, and that is not a stale
//! declaration.** [`Capabilities`](http_ng_core::Capabilities) is a
//! *static* answer for the whole transport, and this transport speaks
//! HTTP/1.1 whenever ALPN says so — what it reports is the value that
//! holds on the worst protocol it might negotiate. Cargo also unifies
//! features across a graph, so a library built on `http-ng` can never know
//! whether some other crate turned `http2` on: `true` here would be a
//! promise made on behalf of builds that cannot keep it, and over-claiming
//! `full_duplex` costs a caller a deadlock rather than a degradation.
//! `tests/http2.rs`'s `capabilities_report_the_floor_with_the_feature_on`
//! pins that, with this feature compiled in.
//!
//! What a caller gets instead is `Response::version()`, after the fact —
//! `version_reported` is `true` and the value comes off the wire. Whether
//! a caller who has to decide *in advance* can be told the truth is v0.4
//! W2's second deliverable; what this code could offer, and what each
//! shape would cost, is written up in `docs/v03-acceptance.md` and is
//! deliberately not decided here.
//!
//! ## What duplex costs, said out loud
//!
//! **A caller that never reads the response body never finishes sending
//! the request body.** That is inherent to duplex with no spawned writer —
//! `http_ng_h3::pump` and `http-ng-wasi`'s module doc record the same
//! consequence for the same technique — and it is why the sequential
//! arrangement was fine for as long as nothing depended on the head
//! arriving early. A caller that wants to finish an upload reads the
//! response; a caller that does not read the response has said it does not
//! care about the rest of the exchange.
//!
//! **A write failure after the head has arrived is a response-body
//! error.** Before duplex, everything the write side could say was said by
//! [`exchange`]'s return value. Now the head can be delivered while the
//! write is still in flight, so a later failure has one channel left: the
//! response body's terminal error.
use crate::body::OutgoingBody;
use crate::pool::CheckIn;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_ng_core::unversioned::{CloseReason, Closed, ConnectionId, Event, Hooks};
use http_ng_core::{Error, ErrorKind};
use hyper::rt::{Read, Write};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Which connection this is, for the observability seam — the same
    /// field, for the same reason, as `crate::h1::Established::id`. This
    /// path reports `Connected`, `Reused` and `Head` (all of which
    /// `Native::execute` emits, knowing nothing about the protocol) and
    /// no `Closed`: see `crate::established::Inner`.
    pub(crate) id: ConnectionId,
}

impl<I> std::fmt::Debug for Established<I>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("h2::Established").finish_non_exhaustive()
    }
}

/// What this client puts in its `SETTINGS` frame, where it does not want
/// h2's default.
///
/// Every field is `None` by default, meaning *whatever `h2` chooses*, and
/// that is deliberate in the direction [`TcpOpts`](http_ng_rt::TcpOpts)
/// already establishes: a value set here goes on the wire, so a default of
/// ours would change what a caller who asked for nothing announces to
/// every server. The difference from `TcpOpts` is that these cannot be
/// refused — a `SETTINGS` frame is written by this crate, not by a runtime
/// that may or may not support the option, so there is no `…Support`
/// mirror and nothing to error at connect.
///
/// # What they are for, and the one that motivates the rest
///
/// [`initial_window_size`](Self::initial_window_size) is the flow-control
/// window the **server** may fill on one stream before waiting for a
/// `WINDOW_UPDATE`, and RFC 9113 §6.9.2 fixes its default at 65 535
/// bytes. On a long fat pipe that number is the throughput ceiling
/// outright: a peer can have at most a window in flight, so the best
/// achievable rate is `window / RTT` however much bandwidth there is —
/// 65 535 bytes over a 100 ms round trip is about 5 Mbit/s, whatever the
/// link can do. Nothing about this is specific to this client, and the
/// only fix is a bigger number.
///
/// # What is deliberately not here
///
/// - **An adaptive window.** hyper computes one from measured RTT; `h2`
///   has no such thing, so it would be ours to write, and a wrong
///   estimator is worse than an honest constant.
/// - **Keepalive pings.** Sending one needs somebody polling an idle
///   connection, which here means
///   [`Native::multiplexed`](crate::Native::multiplexed) and the `Spawn`
///   bound that comes with it. It is a
///   feature of the driver rather than of the settings frame, and it
///   belongs on that constructor if it arrives.
/// - **`max_concurrent_streams`.** A client's own limit governs streams
///   the *server* opens, i.e. server push, which `h2` does not enable and
///   RFC 9113 §8.4 deprecates. Announcing a number for something that
///   cannot happen is a knob with no subject.
///
/// # Not `#[non_exhaustive]`, and that is deliberate
///
/// Not `#[non_exhaustive]`, unlike most public structs added here late —
/// deliberately, and [`TcpOpts`](http_ng_rt::TcpOpts) is the precedent it
/// is copied from. The whole use of this type is
/// `H2Opts { one_field: Some(n), ..Default::default() }`, which
/// `#[non_exhaustive]` forbids from outside the crate; a caller would be
/// left with per-field setters that exist only to work around the
/// attribute. What the attribute buys — a new field not breaking a literal
/// nobody should have written — is bought instead by every field being an
/// `Option` and by nothing here being published yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct H2Opts {
    /// `SETTINGS_INITIAL_WINDOW_SIZE`, RFC 9113 §6.5.2 — per stream.
    /// Default 65 535; see the type's doc for why that is a ceiling.
    pub initial_window_size: Option<u32>,
    /// The whole connection's receive window, which is not a `SETTINGS`
    /// parameter at all: RFC 9113 §6.9.2 says the connection window can
    /// only be changed with `WINDOW_UPDATE`, so `h2` sends one straight
    /// after the preface. Worth setting with the one above rather than
    /// instead of it — raising only the stream window leaves the
    /// connection's 65 535 as the ceiling for everything sharing it.
    pub initial_connection_window_size: Option<u32>,
    /// `SETTINGS_MAX_FRAME_SIZE`, RFC 9113 §6.5.2: the largest frame this
    /// client will accept, between 16 384 and 16 777 215.
    pub max_frame_size: Option<u32>,
    /// `SETTINGS_MAX_HEADER_LIST_SIZE`, RFC 9113 §6.5.2: an advisory
    /// ceiling on a response head's uncompressed size.
    pub max_header_list_size: Option<u32>,
}

/// The HTTP/2 connection preface and the first `SETTINGS` exchange.
///
/// Split out from [`exchange`] for the same reason the HTTP/1 handshake
/// is: a pooled connection has already had one.
pub(crate) async fn handshake<I>(
    io: I,
    id: ConnectionId,
    opts: H2Opts,
) -> Result<Established<I>, Error>
where
    I: Read + Write + Unpin,
{
    let mut builder = h2::client::Builder::new();
    // **One stream until the peer has said how many it allows.**
    //
    // `h2`'s default for this is `usize::MAX`: until the server's SETTINGS
    // frame arrives, a client may open as many streams as it likes, and
    // `SendRequest::poll_ready` — which does respect this counter
    // (`proto/streams/counts.rs`, `can_inc_num_send_streams`) — says yes
    // to all of them. A caller who fires a burst of concurrent requests at
    // a fresh connection therefore races the SETTINGS frame, and a server
    // that allows fewer answers `RST_STREAM(REFUSED_STREAM)`.
    //
    // That failure is not one this client can repair after the fact:
    // `send_request` consumes the head, so `exchange` can only report
    // `Failed::Sent` and `Native::run` will not retry — see this module's
    // own note on where it is weaker than HTTP/1. RFC 9113 §8.7 says the
    // request was *not* processed and may be safely retried, which makes
    // the hard failure a wrong answer rather than a cautious one.
    //
    // So the stream is not opened on a guess. The cost is one round trip
    // once per connection — the peer's SETTINGS arrives in its first
    // flight, and `counts.rs` overwrites this the moment it does, even
    // where the frame names no limit at all.
    //
    // Not a `H2Opts` field, deliberately: every field there is `None`
    // meaning *whatever h2 chooses*, and this is a correctness choice
    // rather than a tuning knob. A caller who guessed high would be
    // choosing a failure they cannot retry.
    builder.initial_max_send_streams(1);
    // Field by field rather than through a helper, because each `Some`
    // has to reach a differently named method and there is nothing to
    // share between them but the shape.
    if let Some(n) = opts.initial_window_size {
        builder.initial_window_size(n);
    }
    if let Some(n) = opts.initial_connection_window_size {
        builder.initial_connection_window_size(n);
    }
    if let Some(n) = opts.max_frame_size {
        builder.max_frame_size(n);
    }
    if let Some(n) = opts.max_header_list_size {
        builder.max_header_list_size(n);
    }
    let (sender, conn) = builder
        .handshake(TokioIo::new(io))
        .await
        .map_err(|e| from_h2_error(e, ErrorKind::Connect))?;
    Ok(Established { sender, conn, id })
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
/// What a `1xx` is reported through on this path.
///
/// A `&dyn Fn` rather than the hook itself, and that is the whole reason
/// HTTP/2 costs no bound where HTTP/1 costs `Send + Sync + 'static`: the
/// callback is called from inside the future that awaits the response, so
/// it neither outlives this call nor crosses a thread, and nothing here
/// has to name `H`.
pub(crate) type On1xx<'a> = &'a dyn Fn(http::StatusCode, &http::HeaderMap);

pub(crate) async fn exchange<I>(
    est: Established<I>,
    req: http::Request<OutgoingBody>,
    checkin: Option<CheckIn<I>>,
    on_1xx: Option<On1xx<'_>>,
) -> Result<http::Response<H2Body<I>>, crate::established::Failed>
where
    I: Read + Write + Unpin,
{
    use crate::established::Failed;

    let Established {
        mut sender,
        mut conn,
        id,
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

    let (mut parts, outgoing) = req.into_parts();
    strip_connection_headers(&mut parts.headers);
    // The URI reaches this function in absolute form — `Native::execute`
    // runs `origin_form` on the HTTP/1 path only, because h2 builds
    // `:scheme`/`:authority`/`:path` out of exactly this URI and an
    // origin-form one would leave it with neither scheme nor authority.
    parts.version = http::Version::HTTP_2;
    let eos = outgoing.is_end_stream();
    let head = http::Request::from_parts(parts, ());

    let (mut resp_fut, send_stream) = match sender.send_request(head, eos) {
        Ok(pair) => pair,
        Err(e) => return Err(Failed::Sent(from_h2_error(e, ErrorKind::Connect))),
    };

    // From here on the head is on the wire, and the write side has a life
    // of its own: it is polled beside `resp_fut` below, and whatever is
    // left of it when the head arrives goes into `H2Body`.
    let mut pump = (!eos).then(|| Pump::new(outgoing, send_stream));
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
            if let Some(p) = pump.as_mut() {
                match p.poll(cx) {
                    // `Done` and `PeerStoppedReading` alike: there is
                    // nothing more to write, and whether the exchange
                    // succeeded is `resp_fut`'s answer, not the pump's.
                    // RFC 9113 §8.1 — see [`Pump::poll`]'s doc comment.
                    Poll::Ready(Ok(_)) => {
                        pump = None;
                        // Round the loop rather than falling through: the
                        // frames the pump just queued only reach the
                        // socket from inside `Connection::poll`.
                        continue;
                    }
                    Poll::Ready(Err(e)) => {
                        // Dropped here rather than at the end of the
                        // function, and it is the drop that resets the
                        // stream: a request whose write failed is not a
                        // request that ended. See `Pump`'s `Drop`.
                        pump = None;
                        return Poll::Ready(Err(Failed::Sent(e)));
                    }
                    // **The duplex line.** A write that cannot proceed
                    // must not stop the response from arriving — the two
                    // halves of a stream are independent, which is the
                    // whole of what `full_duplex` means. This used to be
                    // `return Poll::Pending`, and that one branch was the
                    // implementation `full_duplex: false` described.
                    //
                    // The `conn_done` arm that used to sit beside it is
                    // gone rather than lost: falling through reaches
                    // `resp_fut`'s own `Poll::Pending if conn_done`, which
                    // returns the identical error one branch later — and
                    // reaches it *after* giving a response that did arrive
                    // before the connection ended the chance to be seen.
                    Poll::Pending => {}
                }
            }
            // **After the connection, before the response**, and the
            // order is the whole of it. `conn` is the only thing driving
            // this connection's IO, so polling for interim heads ahead of
            // it asks before the frames have been read — measured, not
            // reasoned: written that way first, the `103` never arrived.
            // And it must come before `resp_fut` resolves, because the
            // loop returns on `Ready` and would never ask again — a `103`
            // that shared a flight with the final head would be lost,
            // which is the one thing it cannot survive, since it exists
            // to be acted on *before* the response.
            if let Some(report) = on_1xx {
                while let Poll::Ready(Some(Ok(interim))) = resp_fut.poll_informational(cx) {
                    report(interim.status(), interim.headers());
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
            id,
            // Whatever is left of the request. `None` for a request whose
            // body was over before the head went out, and for one whose
            // body finished while the head was on its way.
            pump,
            data_done: false,
            ended: false,
        },
    ))
}

/// Why [`Pump::poll`] finished, and the reason the two are not the same
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
    /// [`Pump::poll`]'s "the peer stopped reading" section.
    PeerStoppedReading,
}

/// The request body, in flight — and the reason it is a value rather than
/// three locals inside [`exchange`].
///
/// Duplex means the write outlives the wait for the response head, so the
/// state it needs has to outlive [`exchange`] too: the caller's body, the
/// send half of the h2 stream, and the one chunk that has been taken off
/// the body and not yet fully written. [`H2Body`] takes ownership of all
/// three when the head arrives first.
///
/// **Not a boxed future**, which is what `http_ng_h3::pump` had to use.
/// There the write is an `async fn` and has to be erased to be stored;
/// here it was already a poll function, so keeping it as a struct costs
/// nothing and puts no `dyn` on the `Client -> Transport` path — which,
/// by amendment C2, would cut the auto traits off everything above it.
/// [`H2Body`] stays generic and unboxed, exactly as this module's doc
/// comment says it must.
struct Pump {
    body: OutgoingBody,
    send: h2::SendStream<Bytes>,
    /// The one chunk taken off the body and not yet fully written — the
    /// state that has to survive a `Pending`, since
    /// `http_body::Body::poll_frame` cannot be asked to hand the same
    /// frame back twice.
    pending: Option<Bytes>,
    /// `true` once this stream owes the peer nothing: the body was written
    /// and finished, the peer stopped reading, or it has already been
    /// reset.
    settled: bool,
}

/// **An abandoned upload is reset, not left half-written.**
///
/// A guard rather than a line at a call site, because the moment it has to
/// act at is the moment nothing is running: the caller dropped the
/// `execute` future, or the response body, with a request half written.
///
/// h2 does something similar on its own, and it is not enough. Dropping
/// the *last* reference to a stream that is not closed schedules a
/// `RST_STREAM` — `maybe_cancel`, `h2-0.4.15/src/proto/streams/streams.
/// rs:1601-1620`, with `Reason::CANCEL` for a client — but only once every
/// reference is gone, and the receive half holds one of its own. A pump
/// dropped while the response body lives on (the case this type exists
/// for: the response ended and the request had not) would otherwise leave
/// the stream open with nobody left to write it.
///
/// **This is the h3 defect's neighbourhood, and the answer differs — which
/// is worth writing down rather than assuming.** On quinn,
/// `SendStream::drop` calls `finish()`, so an abandoned HTTP/3 request
/// terminated *cleanly* carrying a DATA frame whose length header promised
/// bytes that never came: RFC 9114 §7.1 makes that a connection error, and
/// on a shared connection it took every neighbour with it. Neither half of
/// that reproduces here. h2 queues whole DATA frames — `send_data` hands
/// `prioritize` a complete `frame::Data` (`.../prioritize.rs:145-222`) and
/// a partly written frame is never a state the peer can observe — so there
/// is nothing truncated to leave behind, and h2's own drop resets rather
/// than finishes.
///
/// # It is not observable today, and that is worth saying rather than
/// implying
///
/// Removing this impl leaves all 23 tests in the three h2 files green,
/// measured rather than assumed, and the reason is the pool policy rather
/// than anything here. The `RST_STREAM` this queues can only reach the
/// wire from inside `Connection::poll` — and **an h2 connection is checked
/// out exclusively** (see [`crate::pool`]'s module doc), so the connection
/// is owned by the same future or the same [`H2Body`] that owns the pump
/// and is dropped in the same breath. The peer learns the request was
/// abandoned from the socket closing instead, which is what
/// `Capabilities::cancel_on_drop` promises and what `tests/cancel.rs`
/// already observes from the far end.
///
/// It is kept for the day that stops being true. `pool.rs` records that a
/// build with a spawner could multiplex, and on that day the connection
/// outlives the stream: an abandoned upload would leave a stream open with
/// nobody left to write it, and h2's own `maybe_cancel` would not fire
/// either, because the response half's reference is what keeps the count
/// above zero. Recorded in `docs/v03-acceptance.md`'s unverified list, not
/// claimed as tested.
impl Drop for Pump {
    fn drop(&mut self) {
        if !self.settled {
            // `CANCEL` is what h2 itself sends for an abandoned client
            // stream, and it is a different statement from the
            // `RST_STREAM(NO_ERROR)` a server sends after a complete
            // response: this one says the request will not be finished.
            self.send.send_reset(h2::Reason::CANCEL);
        }
    }
}

impl Pump {
    fn new(body: OutgoingBody, send: h2::SendStream<Bytes>) -> Self {
        Self {
            body,
            send,
            pending: None,
            settled: false,
        }
    }

    /// Reset now, with a reason that names why, and take the guard out of
    /// the way.
    fn cancel(&mut self, reason: h2::Reason) {
        self.send.send_reset(reason);
        self.settled = true;
    }

    /// Writes as much of the request body as flow control currently allows.
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
    /// the head is on the wire. The mutation that would prove it cannot be
    /// written at all — the tolerance *is* "stop pumping and let `resp_fut`
    /// answer", and before `send_request` returns there is neither a `send` to
    /// ask nor a `resp_fut` to defer to.
    ///
    /// # What the tests pin, and what they do not
    ///
    /// Measured, and re-measured when duplex moved the ground under two of
    /// these. Stated this way round because a guard that is only named is
    /// worse than none.
    ///
    /// - **Deleting this gate** is killed three times:
    ///   `a_server_that_stops_reading_the_body_still_gets_its_response_read`
    ///   in `tests/stream_reset.rs`, and
    ///   `a_reset_while_the_body_drives_the_pump_does_not_discard_the_
    ///   response` and `a_body_that_ends_just_after_the_peer_stopped_reading_
    ///   is_not_an_error` in `tests/http2_duplex.rs`.
    /// - **Moving the tolerance to `poll_capacity`** — where it sat before
    ///   c56cbc9 — is killed by the last of those alone, and *only* by it.
    ///   `a_stalled_streaming_body_…` used to be what killed this, by
    ///   hanging; duplex ends that hang, so the discrimination had to move.
    ///   It moved to the site the placement is really about: a reset stream
    ///   has no capacity, so every *large* body meets a reset at
    ///   `poll_capacity` and a tolerance placed there covers it — but a body
    ///   that simply **ends** while the stream is reset fails at
    ///   `send_data(Bytes::new(), true)` with the `InactiveStreamId` no
    ///   public h2 API can tell from an API misuse of ours.
    /// - **Widening it by treating every pump error in [`exchange`] as "stop
    ///   pumping"** is killed by
    ///   `a_request_body_that_fails_fails_the_request_with_the_callers_own_
    ///   error`, **on whose error comes back and not on whether one does**.
    ///   The widened version resets the stream and defers to `resp_fut`,
    ///   which answers with h2's reset rather than with the caller's body
    ///   error, so an `expect_err` alone passes either way.
    ///
    ///   This bullet used to name `a_connection_that_dies_mid_request_is_
    ///   still_an_error` and it was **wrong**: measured, that mutation left
    ///   all 14 tests green on the code as it stood before duplex, and all
    ///   22 after. A dead connection is `Connection::poll`'s verdict, and
    ///   that poll comes first, so the pump's is never consulted for it.
    /// - **Widening `Ready(Err(_))` into the tolerance survives**, and is
    ///   recorded in `docs/v03-acceptance.md`'s unverified list rather than
    ///   claimed. It survived before this branch too. The arm is shadowed:
    ///   `poll_reset` answers `Err` from a connection error
    ///   (`ensure_reason`), and both callers poll `Connection` immediately
    ///   before the pump, so the connection reports it first — no fixture
    ///   here reaches the arm at all.
    /// - The rows of the table above are read from h2's source, not measured,
    ///   except `poll_capacity`'s — that one is the defect, and it reproduced
    ///   on every run.
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<Pumped, Error>> {
        loop {
            // Before every write, and not only before the first: the peer may
            // stop reading at any point, and each of the four writes below
            // fails differently when it has. This is the one place that asks.
            match self.send.poll_reset(cx) {
                Poll::Ready(Ok(_)) => {
                    // The peer reset the stream, so there is nothing left
                    // to say to it and nothing for the guard to do.
                    self.settled = true;
                    return Poll::Ready(Ok(Pumped::PeerStoppedReading));
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(from_h2_error(e, ErrorKind::Body))),
                Poll::Pending => {}
            }
            if let Some(mut chunk) = self.pending.take() {
                if self.send.capacity() == 0 {
                    self.send.reserve_capacity(chunk.len());
                    match self.send.poll_capacity(cx) {
                        Poll::Ready(Some(Ok(_))) => {}
                        Poll::Ready(Some(Err(e))) => {
                            return Poll::Ready(Err(from_h2_error(e, ErrorKind::Body)));
                        }
                        // Not "no more capacity for now": there will never
                        // be any. With `poll_reset` at the top of the loop
                        // having already answered `Pending`, the stream is not
                        // closed by the peer, so this is our own send half —
                        // an API misuse rather than a message from the far end.
                        Poll::Ready(None) => {
                            return Poll::Ready(Err(Error::new(
                                ErrorKind::Body,
                                StreamClosedWhileSendingTheRequestBody,
                            )));
                        }
                        Poll::Pending => {
                            self.pending = Some(chunk);
                            return Poll::Pending;
                        }
                    }
                }
                let now = chunk.split_to(self.send.capacity().min(chunk.len()));
                if let Err(e) = self.send.send_data(now, false) {
                    return Poll::Ready(Err(from_h2_error(e, ErrorKind::Body)));
                }
                if !chunk.is_empty() {
                    self.pending = Some(chunk);
                    continue;
                }
            }
            match Pin::new(&mut self.body).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                    Ok(data) => {
                        if !data.is_empty() {
                            self.pending = Some(data);
                        }
                    }
                    // Trailers close the send half by themselves — there is no
                    // empty end-of-stream frame to follow them with.
                    Err(frame) => {
                        return Poll::Ready(match frame.into_trailers() {
                            Ok(trailers) => match self.send.send_trailers(trailers) {
                                Ok(()) => {
                                    // The request is over: trailers close
                                    // the send half, so there is nothing
                                    // left for the guard to reset.
                                    self.settled = true;
                                    Ok(Pumped::Done)
                                }
                                Err(e) => Err(from_h2_error(e, ErrorKind::Body)),
                            },
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
                    // it has is not the whole request. Through `cancel`
                    // rather than `send_reset` directly, because marking
                    // the pump settled is what stops the guard asking the
                    // same thing again on the way out.
                    self.cancel(h2::Reason::CANCEL);
                    return Poll::Ready(Err(e));
                }
                Poll::Ready(None) => {
                    return Poll::Ready(match self.send.send_data(Bytes::new(), true) {
                        Ok(()) => {
                            // End-of-stream is on the wire: the request
                            // ended, and an ended request is not one to
                            // reset.
                            self.settled = true;
                            Ok(Pumped::Done)
                        }
                        Err(e) => Err(from_h2_error(e, ErrorKind::Body)),
                    });
                }
                Poll::Pending => return Poll::Pending,
            }
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
    /// The rest of the request body, when the head arrived before it had
    /// all been written. `None` for a request that had no body, and for
    /// one whose body finished inside [`exchange`] — which, against a
    /// server that reads a request before answering it, is the ordinary
    /// case rather than the exception.
    pump: Option<Pump>,
    /// `poll_data` has answered `None`; what is left is the trailers.
    data_done: bool,
    /// This body has already answered `None` or an error once.
    ///
    /// Not bookkeeping for its own sake: [`H2Body::end`] may reset the
    /// request stream, and a later poll would read our own `RST_STREAM`
    /// back off `recv` and turn a response that finished into one that
    /// failed. `http_body` leaves polling past the end unspecified, so
    /// this makes it total rather than relying on every wrapper above to
    /// stop.
    ended: bool,
    /// Carried, not used: the id has to travel back to the pool with the
    /// connection so a later `Reused` names the same one its `Connected`
    /// did. This body emits no event of its own — see
    /// `crate::established::Inner`.
    id: ConnectionId,
}

impl<I> std::fmt::Debug for H2Body<I>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H2Body")
            .field("still_driving_connection", &self.conn.is_some())
            .field("still_sending_the_request", &self.pump.is_some())
            .field("may_be_reused", &self.reuse.is_some())
            .finish()
    }
}

impl<I> H2Body<I>
where
    I: Read + Write + Unpin,
{
    /// Drives the rest of the request body, and reports only a failure.
    ///
    /// **Deliberately not a `Poll<..>`**, so that the caller cannot return
    /// this function's `Pending` as its own — the two halves of a stream
    /// are independent, and a write that cannot proceed must leave a
    /// response that is already on the wire alone. `http_ng_h3::H3Body`
    /// has the same shape for the same reason, and there the mutation that
    /// returned the pump's `Pending` left every test green until one was
    /// written for it.
    fn drive_pump(&mut self, cx: &mut Context<'_>) -> Option<Error> {
        let pump = self.pump.as_mut()?;
        match pump.poll(cx) {
            // `Done` and `PeerStoppedReading` alike — the same non-verdict
            // as in `exchange`, one step later.
            Poll::Ready(Ok(_)) => {
                self.pump = None;
                None
            }
            Poll::Ready(Err(e)) => {
                self.pump = None;
                Some(e)
            }
            Poll::Pending => None,
        }
    }

    /// The response is over. Called on every path that ends this body, and
    /// on no other.
    ///
    /// **An upload still in flight has nowhere left to go.** Nothing polls
    /// a body that has answered `None`, so the pump would never be driven
    /// to its end; dropping it resets the stream (see `Pump`'s `Drop`),
    /// which is what tells the server that what it has is not the whole
    /// request. The alternative — holding the response open until the
    /// upload finishes — hangs against exactly the server that produces
    /// this case: one that answered in full and then neither reads the
    /// request stream nor resets it.
    ///
    /// Reuse goes with it, for the reason the whole file gives: a stream
    /// that ended by reset is not the evidence a check-in is made of, and
    /// it is deliberately easier to lose reuse than to gain it.
    fn end(&mut self) {
        self.ended = true;
        if self.pump.take().is_some() {
            self.reuse = None;
        }
    }

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
                id: self.id,
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
        // A body that has ended stays ended — see the field's doc.
        if this.ended {
            return Poll::Ready(None);
        }
        // The connection, then the rest of the request — the same order as
        // [`exchange`]'s loop, and for the same two reasons, both of which
        // are load-bearing rather than tidy.
        //
        // Nothing new ever arrives in `recv` unless `Connection` is polled
        // (the module doc comment, and `h1`'s identical move) — and that
        // is also the only thing that *decodes* an incoming `RST_STREAM`.
        // With the pump polled first it would be asking about a stream
        // state one poll out of date, so a peer that stopped reading would
        // be noticed by `recv` in the same poll that ends the response
        // body, and [`Pump::poll`]'s gate would never be consulted about
        // it at all. Measured, and this is what the order is worth: with
        // the pump first, moving the reset tolerance from `poll_reset` to
        // `poll_capacity` left all 21 tests green.
        //
        // The `continue` is the other half, and it is `exchange`'s: the
        // frames the pump queues only reach the socket from inside
        // `Connection::poll`, so a pump that just finished rounds the loop
        // rather than leaving its end-of-stream frame for the next
        // wake-up. At most one extra iteration — the pump is `None` by
        // then.
        loop {
            if let Some(conn) = this.conn.as_mut() {
                match Pin::new(conn).poll(cx) {
                    Poll::Ready(Ok(())) => {
                        this.conn = None;
                        this.reuse = None;
                    }
                    Poll::Ready(Err(e)) => {
                        this.conn = None;
                        this.reuse = None;
                        this.end();
                        return Poll::Ready(Some(Err(from_h2_error(e, ErrorKind::Body))));
                    }
                    Poll::Pending => {}
                }
            }
            if this.pump.is_none() {
                break;
            }
            if let Some(e) = this.drive_pump(cx) {
                this.reuse = None;
                this.end();
                return Poll::Ready(Some(Err(e)));
            }
            if this.pump.is_some() {
                break;
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
                    let stopped = stopped_after_a_complete_response(&e);
                    this.end();
                    if stopped {
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
                // `end` before the check-in, never after: an unfinished
                // upload is what makes this connection unfit to be pooled,
                // and `hand_back_to_pool` reads the `reuse` that clears.
                this.end();
                this.hand_back_to_pool();
                Poll::Ready(None)
            }
            Poll::Ready(Err(e)) => {
                this.reuse = None;
                let stopped = stopped_after_a_complete_response(&e);
                this.end();
                if stopped {
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

// ── Sharing one connection between concurrent requests (v0.4) ───────────
//
// Everything below this line is reached only from
// [`crate::Native::multiplexed`], and a build that never calls it runs the
// code above exactly as it did before. `docs/h2-multiplexing.md` is the
// investigation this implements and §8 is its decision list.

/// Which shared connection a pool entry is, for the one operation that
/// needs to name one: removing the entry a borrowed clone came from when
/// that clone turns out to be dead.
///
/// **Not [`ConnectionId`]**, and the difference is the whole reason this
/// exists. `ConnectionId` is the observability seam's name for a
/// connection, and in a build with no hook every connection wears
/// `ConnectionId::UNWATCHED` — so an eviction keyed on it would empty the
/// bucket rather than remove one entry, in precisely the builds that have
/// no way of noticing. This counter is unconditional and private, and
/// costs one relaxed increment per **shared connection**, never per
/// request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SharedId(u64);

impl SharedId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A shared HTTP/2 connection, as the pool holds it and as an exchange
/// borrows it.
///
/// **A `SendRequest` clone and no `Connection`** — the connection is
/// inside the spawned [`H2Driver`], which is what makes this cheap to
/// clone and what makes the pooled copy the connection's *owner*: h2 ends
/// the driver when the last `SendRequest` for it is dropped, so dropping
/// the pooled entry closes the socket. That is the property
/// [`crate::pool`]'s "the idle timeout is a filter, not a reaper" stops
/// applying to.
#[derive(Clone)]
pub(crate) struct Shared {
    sender: h2::client::SendRequest<Bytes>,
    /// The connection's id for [`Hooks`], carried so that a `Reused` names
    /// the same connection its `Connected` did — exactly as
    /// [`Established::id`] does for an exclusive one.
    pub(crate) id: ConnectionId,
    /// See [`SharedId`].
    pub(crate) slot: SharedId,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("h2::Shared")
            .field("slot", &self.slot.0)
            .finish_non_exhaustive()
    }
}

/// Splits a freshly handshaken connection into the half the pool keeps and
/// the half a spawned task drives.
///
/// One function rather than a struct literal at each call site, because
/// the two halves must be made in the same breath: a [`Shared`] published
/// without its driver being spawned is a connection nobody will ever
/// poll — which is [`H2Driver`]'s first failure mode, arrived at by
/// accident instead of by a caller's mistake.
pub(crate) fn share<I, H>(est: Established<I>, hooks: H) -> (Shared, H2Driver<I, H>)
where
    I: Read + Write + Unpin,
{
    let Established { sender, conn, id } = est;
    (
        Shared {
            sender,
            id,
            slot: SharedId::next(),
        },
        H2Driver { conn, hooks, id },
    )
}

/// The future [`crate::Native::multiplexed`] spawns: one HTTP/2
/// connection, polled by nobody's request.
///
/// # Why this is a hand-written struct and not an `async` block
///
/// [`crate::pool::Reaper`]'s reason, verbatim:
/// [`http_ng_rt::Spawn<F>`](http_ng_rt::Spawn) makes the future a type
/// parameter **of the trait**, so a bound has to name it, and an `async`
/// block has no name.
///
/// # What it is for, beyond multiplexing
///
/// Three separate facts follow from a connection being polled by
/// something that is not a request future, and only the first is what the
/// feature is named after:
///
/// 1. Several requests can be in flight on it at once.
/// 2. The `RST_STREAM(CANCEL)` `Pump`'s `Drop` queues **reaches the
///    wire**: the connection outlives the stream, so there is still
///    something to write it. Its own doc comment records that it was
///    unobservable until this existed.
/// 3. A `PING` is answered while the connection is idle, which nothing in
///    this transport could do before — an idle pooled connection has no
///    holder at all.
///
/// # It is at most as good as the executor under it
///
/// `Spawn::spawn` returns `()`. An executor that is never run accepts this
/// task, drops it, and cannot say so — and here that is **worse than the
/// reaper's version of the same mistake**, which costs file descriptors:
/// a request on a connection whose driver is never polled does not fail,
/// it **hangs**. Measured (`docs/h2-multiplexing.md` P8): dropped on the
/// floor, requests fail with a broken pipe; held and never polled, no
/// verdict in 500 ms. `Timeouts::first_byte` is the only bound that cuts
/// it, and it is not a default. Said again on
/// [`crate::Native::multiplexed`], which is where a caller meets it.
pub struct H2Driver<I, H>
where
    I: Read + Write + Unpin,
{
    conn: h2::client::Connection<TokioIo<I>, Bytes>,
    /// **The driver carries the hook, and that is what makes `Closed`
    /// reachable at all for a shared connection.** It dies inside this
    /// future, so nothing else is in a position to report it — the
    /// response bodies that used to own the connection do not, and
    /// `crate::established::Inner::H2`'s doc records that gap. The price
    /// is `H: Send + 'static` wherever the spawner demands it, which is
    /// paid on `multiplexed()` and nowhere else.
    hooks: H,
    id: ConnectionId,
}

impl<I, H> std::fmt::Debug for H2Driver<I, H>
where
    I: Read + Write + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H2Driver")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl<I, H> Future for H2Driver<I, H>
where
    I: Read + Write + Unpin,
    H: Hooks + Unpin,
{
    /// `()`, and reached exactly once: when the connection ends. There is
    /// nothing for a driver to hand back — every error it can see belongs
    /// to a stream, which learns of it through [`h2`] itself, or to the
    /// connection, whose end is what this reports.
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        match Pin::new(&mut this.conn).poll(cx) {
            Poll::Ready(Ok(())) => {
                // `Ended` rather than `Failed`: h2 resolves the connection
                // future with `Ok` for a clean close and for a `GOAWAY`
                // carrying no error, which is exactly the seam's *"nothing
                // went wrong — there is simply no second request to be had
                // on it"*.
                this.hooks.on(Event::Closed(Closed {
                    id: this.id,
                    reason: CloseReason::Ended,
                }));
                Poll::Ready(())
            }
            Poll::Ready(Err(e)) => {
                let error = from_h2_error(e, ErrorKind::Connect);
                this.hooks.on(Event::Closed(Closed {
                    id: this.id,
                    reason: CloseReason::Failed(&error),
                }));
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Whether a **shared** entry is still worth a request.
///
/// One `poll_ready` and no poll of a `Connection`, because there is no
/// `Connection` here to poll and because there no longer needs to be:
/// [`is_reusable`]'s contract says one poll is the only moment a `GOAWAY`
/// is noticed *"because nothing polls an idle connection"*, and on this
/// path something does. Measured (`docs/h2-multiplexing.md` P11): a
/// `GOAWAY` that had arrived 100 ms earlier, while nothing touched the
/// pooled clone, was reported by the first `poll_ready` 20 µs later.
///
/// **`Pending` is not "the connection is full".** h2's `poll_ready`
/// answers from a connection error, the next stream id, and *this clone's
/// own* pending stream — never from the peer's `MAX_CONCURRENT_STREAMS`
/// (P10, read from `h2-0.4.15/src/proto/streams/streams.rs`'s
/// `poll_pending_open` and then measured). A full connection answers
/// `Ready(Ok)` and queues the stream, so treating `Pending` as "not worth
/// a request" — which is [`is_reusable`]'s existing rule — stays right
/// here rather than turning a busy connection into a fresh socket.
pub(crate) async fn shared_is_reusable(shared: &mut Shared) -> bool {
    std::future::poll_fn(|cx| {
        Poll::Ready(matches!(shared.sender.poll_ready(cx), Poll::Ready(Ok(()))))
    })
    .await
}

/// One request over a connection somebody else is driving.
///
/// [`exchange`] with the two things a shared connection does not have
/// taken out, and nothing else changed:
///
/// - **No `Connection` to poll.** Every poll of it in [`exchange`] — the
///   liveness check before the head, the one beside `poll_ready`, the one
///   at the top of the response loop — belongs to the driver now. What
///   made those polls necessary was that nothing else would do them.
/// - **No [`CheckIn`].** The pool never gave this connection away, so
///   there is nothing to hand back: the entry stayed where it was and this
///   function holds a clone of it. [`H2Body::hand_back_to_pool`] is
///   consequently a no-op here, because `reuse` is `None`.
///
/// The `Failed` split is the same one, drawn in the same place. A
/// `poll_ready` that answers `Err` is a connection that went away before
/// the head was handed over, so the request is still ours and comes back
/// as [`Failed::NotSent`](crate::established::Failed::NotSent) — which is
/// what lets `Native::run` retry it on a fresh connection, exactly as it
/// does for a pooled exclusive one.
pub(crate) async fn exchange_shared<I>(
    shared: Shared,
    req: http::Request<OutgoingBody>,
    on_1xx: Option<On1xx<'_>>,
) -> Result<http::Response<H2Body<I>>, crate::established::Failed>
where
    I: Read + Write + Unpin,
{
    use crate::established::Failed;

    let Shared { mut sender, id, .. } = shared;

    // The request is still OURS until `send_request`, and this is the one
    // question that can be asked while it is: is this connection alive.
    let ready = std::future::poll_fn(|cx| match sender.poll_ready(cx) {
        Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
        Poll::Ready(Err(e)) => Poll::Ready(Err(from_h2_error(e, ErrorKind::Connect))),
        Poll::Pending => Poll::Pending,
    })
    .await;
    if let Err(error) = ready {
        return Err(Failed::NotSent {
            error,
            request: Box::new(req),
        });
    }

    let (mut parts, outgoing) = req.into_parts();
    strip_connection_headers(&mut parts.headers);
    parts.version = http::Version::HTTP_2;
    let eos = outgoing.is_end_stream();
    let head = http::Request::from_parts(parts, ());

    let (mut resp_fut, send_stream) = match sender.send_request(head, eos) {
        Ok(pair) => pair,
        Err(e) => return Err(Failed::Sent(from_h2_error(e, ErrorKind::Connect))),
    };

    let mut pump = (!eos).then(|| Pump::new(outgoing, send_stream));
    // **Not a loop, where [`exchange`] is one**, and the difference is the
    // whole of what a driver changes. There, the frames the pump just
    // queued reach the socket only from inside the `Connection::poll` at
    // the top of the same loop, so a pump that finished has to round the
    // loop rather than leave its end-of-stream frame for the next wake-up.
    // Here that poll belongs to the driver, and what wakes the driver is
    // the queueing itself.
    //
    // Written this way because a mutation said so: with the `continue`
    // this function used to have, the whole suite is green either way, and
    // clippy then points out that the loop never loops. The code now says
    // what the tests can see.
    let resp = std::future::poll_fn(|cx| {
        if let Some(p) = pump.as_mut() {
            match p.poll(cx) {
                Poll::Ready(Ok(_)) => pump = None,
                Poll::Ready(Err(e)) => {
                    pump = None;
                    return Poll::Ready(Err(Failed::Sent(e)));
                }
                // The duplex line, unchanged: a write that cannot proceed
                // must not stop the response from arriving.
                Poll::Pending => {}
            }
        }
        // Same rule as `exchange`, and the same reason: after whatever
        // drives the IO — here the spawned driver, not this loop — and
        // before `resp_fut` resolves, because this returns on `Ready`.
        if let Some(report) = on_1xx {
            while let Poll::Ready(Some(Ok(interim))) = resp_fut.poll_informational(cx) {
                report(interim.status(), interim.headers());
            }
        }
        match Pin::new(&mut resp_fut).poll(cx) {
            Poll::Ready(Ok(r)) => Poll::Ready(Ok(r)),
            Poll::Ready(Err(e)) => {
                Poll::Ready(Err(Failed::Sent(from_h2_error(e, ErrorKind::Connect))))
            }
            // No `conn_done` arm, and none is owed. [`exchange`] needs one
            // because it is the only thing polling the connection, so a
            // connection that ended while it waited would leave it waiting
            // for ever; here the driver is still polling, and h2 resolves
            // every live stream with the connection's error when it ends.
            Poll::Pending => Poll::Pending,
        }
    })
    .await;
    let resp = resp?;

    let (parts, recv) = resp.into_parts();
    Ok(http::Response::from_parts(
        parts,
        H2Body {
            recv,
            // Both `None`, and they are the two fields this whole variant
            // is about — see this function's doc comment.
            conn: None,
            reuse: None,
            id,
            pump,
            data_done: false,
            ended: false,
        },
    ))
}
