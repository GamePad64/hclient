//! The adapter `http-ng-rt` does not have: `hyper::rt` → `futures_io`.
//!
//! `FuturesIo` in `http-ng-rt` goes the other way, wrapping a futures-io
//! stream so hyper can drive it. This crate needs both directions at once:
//! the `TlsConnect` seam hands us a `hyper::rt` stream, `async-native-tls`
//! speaks futures-io, and the handshake's result has to come back out as
//! `hyper::rt` again. This module is the inbound half; `FuturesIo` is the
//! outbound half, reused rather than reimplemented.
//!
//! `unsafe`-free, by the same technique `FuturesIo` uses: read into a
//! scratch buffer and copy out through the safe `ReadBufCursor::put_slice`.
//! One copy per read; zero-copy would need `ReadBufCursor::as_mut`, which
//! is `unsafe`, and this crate has no FFI boundary to justify it.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Wraps a `hyper::rt::Read + Write` stream and presents futures-io.
#[derive(Debug)]
pub struct HyperIo<S> {
    inner: S,
}

impl<S> HyperIo<S> {
    pub(crate) fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S: hyper::rt::Read + Unpin> futures_io::AsyncRead for HyperIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut read_buf = hyper::rt::ReadBuf::new(buf);
        match Pin::new(&mut self.inner).poll_read(cx, read_buf.unfilled()) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(read_buf.filled().len())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S: hyper::rt::Write + Unpin> futures_io::AsyncWrite for HyperIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    /// futures-io's `close` maps to hyper's `shutdown`: both mean "no more
    /// writes", and for TLS that is what triggers `close_notify` rather
    /// than a bare FIN.
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
