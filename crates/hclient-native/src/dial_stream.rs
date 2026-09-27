//! The stream a [`Native`](crate::Native) lends an egress filter.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_io::{AsyncRead, AsyncWrite};
use hclient_proxy::Io;
use hclient_rt::Shutdown;
use hclient_tls::TlsConnect;

/// The stream a [`Native`](crate::Native) lends an egress filter: the
/// runtime's socket, or TLS the filter asked the transport to run over
/// another one of these — so TLS to a proxy, a tunnel, and TLS to the
/// origin compose without a filter naming a TLS type.
///
/// Opaque, like [`Conn`](crate::Conn): public because it is the lower half
/// of [`NativeIo`](crate::NativeIo). What a caller does with one is the
/// byte-stream seam: `futures_io::{AsyncRead, AsyncWrite}` and
/// `hclient_rt::Shutdown`, forwarded to whichever layer it holds.
pub struct DialStream<S: Io, L: TlsConnect>(Layer<S, L>);

// Maintainer notes (not rendered):
// The `Box` is for recursion only — `L::Stream<DialStream<S, L>>` names
// this type — so no auto trait is declared and none is lost: `Send`
// follows `S` and `L`, which is what keeps a `!Send` runtime a transport.
// Erasing into a `BoxIo` instead would have demanded `Send` of every
// runtime that proxies over TLS.
enum Layer<S: Io, L: TlsConnect> {
    Raw(S),
    Tls(Box<L::Stream<DialStream<S, L>>>),
}

impl<S: Io, L: TlsConnect> DialStream<S, L> {
    pub(crate) fn raw(s: S) -> Self {
        Self(Layer::Raw(s))
    }

    pub(crate) fn tls(t: L::Stream<Self>) -> Self {
        Self(Layer::Tls(Box::new(t)))
    }
}

impl<S: Io, L: TlsConnect> std::fmt::Debug for DialStream<S, L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0 {
            Layer::Raw(_) => "DialStream::Raw",
            Layer::Tls(_) => "DialStream::Tls",
        })
    }
}

impl<S: Io, L: TlsConnect> AsyncRead for DialStream<S, L> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().0 {
            Layer::Raw(s) => Pin::new(s).poll_read(cx, buf),
            Layer::Tls(t) => Pin::new(&mut **t).poll_read(cx, buf),
        }
    }
}

impl<S: Io, L: TlsConnect> AsyncWrite for DialStream<S, L> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().0 {
            Layer::Raw(s) => Pin::new(s).poll_write(cx, buf),
            Layer::Tls(t) => Pin::new(&mut **t).poll_write(cx, buf),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().0 {
            Layer::Raw(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            Layer::Tls(t) => Pin::new(&mut **t).poll_write_vectored(cx, bufs),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().0 {
            Layer::Raw(s) => Pin::new(s).poll_flush(cx),
            Layer::Tls(t) => Pin::new(&mut **t).poll_flush(cx),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().0 {
            Layer::Raw(s) => Pin::new(s).poll_close(cx),
            Layer::Tls(t) => Pin::new(&mut **t).poll_close(cx),
        }
    }
}

impl<S: Io, L: TlsConnect> Shutdown for DialStream<S, L> {
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().0 {
            Layer::Raw(s) => Pin::new(s).poll_shutdown(cx),
            Layer::Tls(t) => Pin::new(&mut **t).poll_shutdown(cx),
        }
    }
}
