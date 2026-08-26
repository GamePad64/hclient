//! The HTTP/1.1 upgrade, and the seam a framing crate is handed.
//!
//! **The connection and the upgrade, without a word about frames.** The
//! framing lives in
//! `hclient-tungstenite`, which depends on this crate and not the other
//! way round, so `tungstenite` cannot be switched on in this one by a
//! neighbour in the graph — which a `websocket` feature here could not
//! prevent, Cargo's features being additive.
//!
//! # The seam
//!
//! **"Here is an upgraded byte stream, plus the `read_buf` hyper had
//! already read past."** That shape is rejected as the *public* seam,
//! where it excludes three of four backends and the browser among them.
//! Between this crate and a framing
//! crate it is correct, because it is only ever asked of the one backend
//! that can answer it. §2's argument was about which level, not about the
//! shape.
//!
//! It is deliberately not "here is a WebSocket", and not "here is a
//! `hyper::upgrade::Upgraded`" either — see rule 2 below.
//!
//! # The trap this module exists to avoid
//!
//! hyper answers a `101` from `Connection`'s `Future` impl with
//! `pending.manual(); Poll::Ready(Ok(()))` (`client/conn/http1.rs:310-320`,
//! under its own comment *"With no `Send` bound on `I`, we can't try to do
//! upgrades here"*). So **"the exchange finished" and "the upgrade was
//! destroyed" are the same observation**, and [`crate::http1::exchange`] polls
//! `Connection` exactly that way — which is why a `101` there is a response
//! with an empty body and a socket that closes when the locals drop
//! (`tests/switching_protocols.rs`).
//!
//! Nothing in that shape can be reused here. This module therefore does
//! its own exchange, on three rules:
//!
//! 1. **The `101` is recognised by its status, never by "the connection
//!    future completed".** That distinction is the whole trap: hyper
//!    reports a finished ordinary exchange and a destroyed upgrade with
//!    the same `Ready(Ok(()))`, so a client taking the completion as its
//!    signal would upgrade onto any response at all. [`Native::upgrade`]
//!    reads the status and returns [`NotSwitchingProtocols`] on anything
//!    else, and it is the *only* check here: what makes a `101` a
//!    **WebSocket** `101` — `Upgrade:`, `Connection:` and
//!    `Sec-WebSocket-Accept` — belongs to whoever knows what WebSocket is,
//!    and is `hclient_tungstenite::Handshake::accept`'s.
//!
//!    Those three still run **before** the connection is taken apart, and
//!    that ordering is now structural rather than remembered: what
//!    [`Native::upgrade`] hands back is an [`Upgrading`], which lends out
//!    the head and can only be dismantled by a separate
//!    [`Upgrading::finish`]. There is no way to reach the socket without
//!    having been handed the head first.
//!
//!    Measured, because the stronger claim an earlier draft of this
//!    paragraph made is false: **moving all four checks to after
//!    `into_parts` changes nothing any test can see**, and that mutation
//!    survives. The reason is `drop(body)` below — dropping hyper's
//!    `Incoming` finishes the dispatcher whatever the response was, so
//!    `poll_without_shutdown` returns `Ready` on a `200` as readily as on
//!    a `101`, and an upgrade that is then refused drops its socket
//!    either way. The order is kept because reading a response before
//!    dismantling the connection that produced it stays correct if
//!    hyper's behaviour does not; it is not load bearing *today*, and
//!    this module does not imply otherwise.
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
//!    is a mutation no test in this workspace kills, recorded here rather
//!    than left for the next reader to rediscover.
//!    `poll_without_shutdown` is still the right
//!    call: it is the API hyper documents for this ("Once the upgrade is
//!    completed … you would take it back using `into_parts`"), it does not
//!    require `B::Data: Send` where the `Future` impl does, and on any
//!    completion that is *not* an upgrade it is the one that leaves the
//!    socket alone.
//! 3. **`Parts::read_buf` is handed on.** A server is free to put its
//!    first frames in the same flight as the `101`, and hyper will have
//!    read them already. Dropping that buffer works in every test where
//!    the server pauses first, which is what makes it worth a test where
//!    it does not (`hclient-tungstenite`'s
//!    `the_first_frame_may_arrive_in_the_same_flight_as_the_101`, and
//!    `the_bytes_that_arrived_with_the_101_are_handed_on` here).
//!
//! # The connection is its own, and never the pool's
//!
//! [`crate::pool`] is not consulted on the way in and nothing can put the
//! connection back, because what leaves this module is an `I` rather than
//! a connection: a socket that has stopped speaking HTTP is not a
//! connection any later request could use. That is the same conclusion
//! `tests/switching_protocols.rs` reached from the other side, and here it
//! costs nothing to arrange — this module never builds a `CheckIn`.
use crate::connect;
use crate::{Native, NativeIo};
use bytes::Bytes;
use hclient_core::unversioned::NoHooks;
use hclient_core::{Error, ErrorKind, Timeouts};
use hclient_dns::Resolve;
use hclient_rt::{TcpConnect, Timer};
use hclient_tls::TlsConnect;
use hyper::client::conn::http1;
use hyper::rt::{Read, Write};
use std::fmt::Debug;
use std::future::poll_fn;
use std::task::Poll;

#[derive(Debug, thiserror::Error)]
#[error("the server answered {0} rather than 101 Switching Protocols")]
pub struct NotSwitchingProtocols(pub http::StatusCode);

#[derive(Debug, thiserror::Error)]
#[error("the connection ended before the handshake response arrived")]
pub struct EndedBeforeTheResponse;

/// A `101` that has arrived, on a connection not yet taken apart.
///
/// Two steps rather than one, and the split is the point: a caller reads
/// [`Upgrading::head`] to decide whether this `101` is the one it asked
/// for, and only then calls [`Upgrading::finish`] to get the socket. A
/// caller that refuses simply drops this value, which closes the
/// connection.
///
/// # Why the head and not the whole response
///
/// A `101` carries no body — hyper decodes it as zero-length — and the
/// body is dropped inside [`Native::upgrade`] because dropping it is what
/// lets the dispatcher finish. `http::response::Parts` is what is left,
/// and it is what the three WebSocket checks read.
pub struct Upgrading<I: Read + Write> {
    conn: http1::Connection<I, http_body_util::Empty<Bytes>>,
    /// `poll_without_shutdown` has already answered `Ready(Ok(()))`, and
    /// `Future`'s own rule says it must not be polled again.
    conn_done: bool,
    head: http::response::Parts,
}

/// Hand-written because hyper's `Connection` is not `Debug`, and because
/// the useful thing to print is the answer rather than the machinery.
impl<I: Read + Write> Debug for Upgrading<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upgrading")
            .field("status", &self.head.status)
            .field("headers", &self.head.headers)
            .finish_non_exhaustive()
    }
}

impl<I> Upgrading<I>
where
    I: Read + Write + Unpin + 'static,
{
    /// The `101`'s head, for whoever knows what the upgrade was *to*.
    pub fn head(&self) -> &http::response::Parts {
        &self.head
    }

    /// Take the connection apart: the socket, and whatever the server had
    /// already sent past the response head.
    ///
    /// `read_buf` is rule 3 — bytes hyper read off the socket in the same
    /// flight as the `101`, unreachable from anywhere else once this value
    /// is dropped. A caller that ignores it loses the peer's first frames
    /// for good, and no later read can recover them.
    pub async fn finish(mut self) -> Result<(I, Bytes), Error> {
        if !self.conn_done {
            poll_fn(|cx| self.conn.poll_without_shutdown(cx))
                .await
                .map_err(|e| Error::new(ErrorKind::Connect, e))?;
        }
        let http1::Parts { io, read_buf, .. } = self.conn.into_parts();
        Ok((io, read_buf))
    }
}

/// The clock and the upgrade: what a framing crate needs from this one.
impl<R, T, D, H, P> Native<R, T, D, H, P>
where
    R: TcpConnect + Timer,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
    P: crate::proxy::ProxyProtocol,
{
    /// Send `req` on a connection of this transport's own and hand back
    /// the `101` it was answered with, undismantled.
    ///
    /// The request is the caller's, with the URI absolute and `http`/
    /// `https` — a framing crate maps its own schemes (`ws`/`wss`) before
    /// it gets here. What this method adds is what it adds to every other
    /// HTTP/1 request it sends and for the same reasons: the origin-form
    /// request line hyper's HTTP/1 client requires, and `Host:` when the
    /// caller did not set one (honoured when they did).
    /// `Timeouts::connect` is read out of the request's extensions.
    ///
    /// `http/1.1` alone goes on the ALPN list: RFC 8441 extended CONNECT
    /// is the HTTP/2 answer to upgrades and this is not it. The pool is
    /// not consulted, and nothing here can give the connection back — see
    /// the module doc.
    ///
    /// # Errors
    ///
    /// [`NotSwitchingProtocols`] for any status but `101`, which is rule 1
    /// and the one check this method makes about the answer;
    /// [`EndedBeforeTheResponse`] when the connection finishes with the
    /// request still queued on it; and whatever connecting failed with.
    pub async fn upgrade(
        &self,
        req: http::Request<()>,
    ) -> Result<Upgrading<NativeIo<R, T>>, Error> {
        let uri = req.uri().clone();
        let timeouts = req
            .extensions()
            .get::<Timeouts>()
            .copied()
            .unwrap_or_default();
        let wire = for_http1(req);

        // A connection of its own, and never `self.pool` — see the module
        // doc.
        let now = self.rt.elapsed_since(self.epoch);
        // `NoHooks`, deliberately: the observability seam is
        // about HTTP requests, and this connection is not one. It is
        // never pooled, so no `Reused` can follow it; there is no
        // response head to report beyond the 101 the handshake consumes;
        // and its end is the framing layer's business, which the caller
        // sees directly. Reporting `Connected` alone would put an id into
        // a caller's log that no later event ever mentions again.
        let connect_fut = connect::connect::<_, _, _, _, NoHooks>(
            &self.rt,
            &self.dns,
            &self.tls,
            &self.proxies,
            self.unix_socket.as_deref(),
            &uri,
            &self.opts,
            &[b"http/1.1"],
            &self.svcb_failures,
            now,
            // Nothing prepared, and no way to prepare one: `Prepared`
            // holds an `http::Request<RequestBody>` and an upgrade
            // handshake is not one. The connector does its own discovery
            // here, exactly as it did before that type existed.
            crate::discovery::Prefetched::NotConsulted,
            // No `Timeouts` here, and none to read: an upgrade
            // handshake is not an `http::Request<RequestBody>`, so
            // there is no extension bag carrying one — the same fact
            // that makes `Prepared` unreachable two lines up.
            None,
        );
        let (conn, _tls_info, _facts) =
            crate::with_connect_timeout(&self.rt, timeouts.connect, connect_fut).await?;

        exchange(conn, wire, |status| {
            (status != http::StatusCode::SWITCHING_PROTOCOLS)
                .then(|| Error::new(ErrorKind::Status, NotSwitchingProtocols(status)))
        })
        .await
    }

    /// This transport's clock, for a socket that outlives the call that
    /// opened it.
    ///
    /// A WebSocket opened from a transport inherits everything that
    /// transport already knows — its runtime, its TLS configuration, its
    /// resolver — which is the seam's own claim
    /// (`hclient_core::unversioned::WebSocketConnect`); the first of those
    /// is the one a framing crate has to *hold*, because a keep-alive
    /// sleeps long after `upgrade` has returned and a `&R` cannot.
    pub fn runtime(&self) -> &R {
        &self.rt
    }
}

/// Origin-form and `Host:`, the same two things
/// `established::Rewritten::for_http1` does to every other HTTP/1 request
/// this crate sends, and for the same two reasons — hyper's HTTP/1 client
/// requires the first and RFC 9112 §3.2 the second.
///
/// Unlike that one this is one-way: there is no second attempt to undo it
/// for, because an upgrade is never retried over another protocol.
fn for_http1(req: http::Request<()>) -> http::Request<http_body_util::Empty<Bytes>> {
    let uri = req.uri().clone();
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
        // The same judgement `Rewritten::for_http1` records: a host that
        // got this far is in practice always valid as a header value, and
        // if it somehow is not, the request goes out without `Host:` and
        // fails explicitly at the server rather than silently here.
        if let Ok(v) = http::HeaderValue::from_str(&authority) {
            headers.insert(http::header::HOST, v);
        }
    }

    let target = uri
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
    let mut origin_form = http::uri::Parts::default();
    origin_form.path_and_query = Some(target);

    let mut out = http::Request::new(http_body_util::Empty::<Bytes>::new());
    *out.method_mut() = parts.method;
    *out.version_mut() = http::Version::HTTP_11;
    *out.uri_mut() = http::Uri::from_parts(origin_form).expect("a path-only URI is valid");
    *out.headers_mut() = headers;
    *out.extensions_mut() = parts.extensions;
    out
}

/// The exchange, from a connected socket to a `101` — see the module doc
/// for the three rules it keeps.
/// The handshake both callers share: send one bodiless request, keep the
/// connection undismantled, and hand back the head plus the machinery that
/// can still reclaim the socket.
///
/// `accept` is the only thing the two differ in, and it is a parameter
/// rather than a second copy of this function because the forty lines
/// below are the delicate part — `poll_without_shutdown`, the dead-end
/// branch, the order of the drops — and a second copy would be forty
/// lines nobody re-reads. A WebSocket accepts `101` alone (rule 1); an
/// HTTP proxy accepts any `2xx`, which hyper's own h1 client already
/// treats as an upgrade: `role.rs` sets `wants_upgrade` for
/// `Method::CONNECT` and skips the body for `CONNECT` + `is_success`, so
/// `into_parts` yields the tunnel and the bytes read past it exactly as
/// it does for a `101`. Read in `hyper-1.11.0`, not assumed.
pub(crate) async fn exchange<I>(
    io: I,
    req: http::Request<http_body_util::Empty<Bytes>>,
    accept: impl Fn(http::StatusCode) -> Option<Error>,
) -> Result<Upgrading<I>, Error>
where
    I: Read + Write + Unpin + 'static,
{
    let (mut sender, mut conn) = http1::handshake::<I, http_body_util::Empty<Bytes>>(io)
        .await
        .map_err(|e| Error::new(ErrorKind::Connect, e))?;

    let mut conn_done = false;
    let resp = {
        let mut send = std::pin::pin!(sender.send_request(req));
        poll_fn(|cx| {
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

    // Rule 1: by status. Not by "hyper is finished with this connection",
    // which is the same observation for an ordinary exchange — see the
    // module doc.
    let (head, body) = resp.into_parts();
    if let Some(refused) = accept(head.status) {
        return Err(refused);
    }
    // A `101` carries no body (hyper decodes it as zero-length), and the
    // connection underneath it is about to be taken apart.
    drop(body);

    Ok(Upgrading {
        conn,
        conn_done,
        head,
    })
}
