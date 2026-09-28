//! A CONNECT or extended CONNECT (RFC 8441) to a proxy spoken to over
//! HTTP/2, and the stream it opens.
//!
//! The connection is driven inline: every poll of the tunnel's stream —
//! and every wait before it exists — polls the `h2` connection first, the
//! way this crate's exclusive HTTP/2 connections are driven by the request
//! futures that hold them. Nothing is spawned, so a runtime without
//! `Spawn` lends tunnels as well as one with it, and a tunnel nobody polls
//! is a connection nobody drives — which, for a connection that carries
//! exactly one stream, is the caller's own choice.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use bytes::{Buf as _, Bytes};
use hclient_core::error::{Error, ErrorKind};
use hclient_proxy::{BoxIo, BoxTunnelling, Tunnel, TunnelRequest};
use hclient_rt::TcpConnect;
use hclient_tls::{TlsConnect, TlsRequest};

use crate::http2::TokioIo;

/// What a tunnel to a proxy offers in ALPN: HTTP/2 and nothing else,
/// because nothing else here can carry a CONNECT stream.
const ALPN: &[&[u8]] = &[b"h2"];

/// The HTTP/2 connection one tunnel owns.
type Connection<S> = h2::client::Connection<TokioIo<S>, Bytes>;

/// [`crate::TunnelH2`]'s one body, instantiated in `Native::egress`, where
/// the runtime's stream and the TLS backend over an erased stream are
/// proven `Send`.
///
/// `raw` is a connection to the proxy the lent context already opened by
/// name; TLS, the HTTP/2 handshake and the CONNECT all happen here.
pub(crate) fn open<'a, R, L>(
    tls: &'a L,
    raw: R::Stream,
    req: TunnelRequest<'a>,
) -> BoxTunnelling<'a>
where
    R: TcpConnect,
    R::Stream: Send + 'static,        // send-bound-exception: amendment-C15
    L: TlsConnect + Sync,             // send-bound-exception: amendment-C15
    L::Stream<BoxIo>: Send + 'static, // send-bound-exception: amendment-C15
    for<'x> L::Handshake<'x, BoxIo>: Send, // send-bound-exception: amendment-C15
{
    Box::pin(connect(tls, BoxIo::new(raw), req))
}

async fn connect<'a, L>(tls: &'a L, io: BoxIo, req: TunnelRequest<'a>) -> Result<Tunnel, Error>
where
    L: TlsConnect,
    L::Stream<BoxIo>: Send + 'static, // send-bound-exception: amendment-C15
{
    let tls_req = TlsRequest::new(req.tls.server_name, ALPN).identity(req.tls.identity);
    let (stream, _info) = tls.connect(io, tls_req).await?;
    let (mut client, mut conn) = h2::client::Builder::new()
        .handshake::<_, Bytes>(TokioIo::new(stream))
        .await
        .map_err(|e| Error::new(ErrorKind::Connect, e))?;

    if req.protocol.is_some() {
        // The setting arrives in the proxy's SETTINGS frame, and until it
        // has, `h2` answers `false` — the same value as a proxy that said
        // no. A PING is answered only after the peer's SETTINGS (it must
        // be the peer's first frame), so its PONG is when the answer is
        // known rather than guessed.
        settle(&mut conn).await?;
        if !client.is_extended_connect_protocol_enabled() {
            return Err(Error::new(
                ErrorKind::Unsupported,
                io::Error::other(
                    "the proxy does not announce SETTINGS_ENABLE_CONNECT_PROTOCOL, \
                     so an extended CONNECT cannot be sent to it over HTTP/2",
                ),
            ));
        }
    }

    let mut b = http::Request::builder().method(http::Method::CONNECT);
    b = match req.protocol {
        None => b.uri(req.authority),
        Some(p) => b
            .uri(format!(
                "https://{}{}",
                req.authority,
                req.path.unwrap_or("/")
            ))
            .extension(h2::ext::Protocol::from(p)),
    };
    let mut head = b.body(()).map_err(|e| Error::new(ErrorKind::Connect, e))?;
    head.headers_mut().extend(req.headers);

    std::future::poll_fn(|cx| {
        drive_setup(&mut conn, cx)?;
        client
            .poll_ready(cx)
            .map_err(|e| Error::new(ErrorKind::Connect, e))
    })
    .await?;
    let (mut response, send) = client
        .send_request(head, false)
        .map_err(|e| Error::new(ErrorKind::Connect, e))?;
    let response = std::future::poll_fn(|cx| {
        drive_setup(&mut conn, cx)?;
        Pin::new(&mut response)
            .poll(cx)
            .map_err(|e| Error::new(ErrorKind::Connect, e))
    })
    .await?;
    let (parts, recv) = response.into_parts();
    Ok(Tunnel::new(
        parts,
        BoxIo::new(H2Stream {
            conn,
            conn_done: false,
            _client: client,
            send,
            recv,
            chunk: Bytes::new(),
            shut: false,
        }),
        None,
    ))
}

/// Wait for the proxy's SETTINGS: send a PING and drive the connection
/// until its PONG.
async fn settle<S>(conn: &mut Connection<S>) -> Result<(), Error>
where
    S: hclient_rt::Io,
{
    let mut ping = conn.ping_pong().ok_or_else(|| {
        Error::new(
            ErrorKind::Connect,
            io::Error::other("the HTTP/2 connection's ping handle was already taken"),
        )
    })?;
    ping.send_ping(h2::Ping::opaque())
        .map_err(|e| Error::new(ErrorKind::Connect, e))?;
    std::future::poll_fn(|cx| {
        drive_setup(conn, cx)?;
        ping.poll_pong(cx)
            .map_err(|e| Error::new(ErrorKind::Connect, e))
    })
    .await?;
    Ok(())
}

/// Poll the connection once while the tunnel is being set up, where its
/// ending is an error: nothing that is still being waited for can arrive
/// on a connection that has closed.
fn drive_setup<S>(conn: &mut Connection<S>, cx: &mut Context<'_>) -> Result<(), Error>
where
    S: hclient_rt::Io,
{
    match Pin::new(conn).poll(cx) {
        Poll::Pending => Ok(()),
        Poll::Ready(Ok(())) => Err(Error::new(
            ErrorKind::Connect,
            io::Error::other("the proxy closed the HTTP/2 connection before the tunnel opened"),
        )),
        Poll::Ready(Err(e)) => Err(Error::new(ErrorKind::Connect, e)),
    }
}

fn io_error(e: h2::Error) -> io::Error {
    if e.is_io() {
        e.into_io()
            .unwrap_or_else(|| io::Error::other("an h2 I/O error with no I/O error in it"))
    } else {
        io::Error::other(e)
    }
}

/// One CONNECT stream as a byte stream: the request body is what is
/// written, the response body what is read, and a half-close is
/// `END_STREAM`.
struct H2Stream<S>
where
    S: hclient_rt::Io,
{
    /// The connection, polled before every operation on the stream.
    conn: Connection<S>,
    /// Whether the connection has finished, after which it is not polled
    /// again; the stream's own operations report why.
    conn_done: bool,
    /// Held so the connection is not told every handle is gone while the
    /// stream is still in use.
    _client: h2::client::SendRequest<Bytes>,
    send: h2::SendStream<Bytes>,
    recv: h2::RecvStream,
    /// What is left of the last DATA frame read, not yet handed out.
    chunk: Bytes,
    /// Whether `END_STREAM` has been sent.
    shut: bool,
}

impl<S> H2Stream<S>
where
    S: hclient_rt::Io,
{
    fn drive(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.conn_done {
            return Ok(());
        }
        match Pin::new(&mut self.conn).poll(cx) {
            Poll::Pending => Ok(()),
            Poll::Ready(Ok(())) => {
                self.conn_done = true;
                Ok(())
            }
            Poll::Ready(Err(e)) => {
                self.conn_done = true;
                Err(io_error(e))
            }
        }
    }

    fn end(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.drive(cx)?;
        if !self.shut {
            self.send.send_data(Bytes::new(), true).map_err(io_error)?;
            self.shut = true;
            self.drive(cx)?;
        }
        Poll::Ready(Ok(()))
    }
}

impl<S> futures_io::AsyncRead for H2Stream<S>
where
    S: hclient_rt::Io,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.drive(cx)?;
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        loop {
            if !this.chunk.is_empty() {
                let n = buf.len().min(this.chunk.len());
                buf[..n].copy_from_slice(&this.chunk[..n]);
                this.chunk.advance(n);
                // Released as it is handed out, not as it arrives: the
                // window is what bounds how far the proxy may run ahead of
                // a reader, and bytes sitting here have not been read.
                this.recv
                    .flow_control()
                    .release_capacity(n)
                    .map_err(io_error)?;
                return Poll::Ready(Ok(n));
            }
            match ready!(this.recv.poll_data(cx)) {
                Some(Ok(data)) => this.chunk = data,
                None => return Poll::Ready(Ok(0)),
                // RFC 9113 §8.1: a reset with NO_ERROR after the response
                // ends it rather than failing it.
                Some(Err(e)) if e.reason() == Some(h2::Reason::NO_ERROR) => {
                    return Poll::Ready(Ok(0));
                }
                Some(Err(e)) => return Poll::Ready(Err(io_error(e))),
            }
        }
    }
}

impl<S> futures_io::AsyncWrite for H2Stream<S>
where
    S: hclient_rt::Io,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.drive(cx)?;
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if this.shut {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        this.send.reserve_capacity(buf.len());
        let n = loop {
            match ready!(this.send.poll_capacity(cx)) {
                // Capacity went up and was taken again; the next poll
                // registers for the next change.
                Some(Ok(0)) => {}
                Some(Ok(n)) => break n.min(buf.len()),
                Some(Err(e)) => return Poll::Ready(Err(io_error(e))),
                None => return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
            }
        };
        this.send
            .send_data(Bytes::copy_from_slice(&buf[..n]), false)
            .map_err(io_error)?;
        // Once more, so the frame just queued reaches the socket now
        // rather than on the next call.
        this.drive(cx)?;
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().drive(cx)?;
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().end(cx)
    }
}

impl<S> hclient_rt::Shutdown for H2Stream<S>
where
    S: hclient_rt::Io,
{
    /// `END_STREAM` on the request body: the proxy goes on sending, and
    /// the stream goes on being read.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().end(cx)
    }
}
