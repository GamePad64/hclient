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

    /// Test-only: the adapter's own behaviour can only be checked by
    /// inspecting what reached the transport underneath it.
    #[cfg(test)]
    fn into_inner(self) -> S {
        self.inner
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures_io::AsyncRead;

    /// Hands out at most `chunk` bytes per poll, then EOF. Real transports
    /// do this constantly — a TLS record boundary, a short TCP read — and
    /// an adapter that mishandles it truncates every response.
    struct Chunked {
        data: &'static [u8],
        pos: usize,
        chunk: usize,
    }

    impl hyper::rt::Read for Chunked {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            let remaining = &self.data[self.pos..];
            let n = remaining.len().min(self.chunk).min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            self.pos += n;
            Poll::Ready(Ok(()))
        }
    }

    /// Accepts at most `chunk` bytes per poll and records what it was
    /// given. A transport doing this is ordinary — a full socket buffer —
    /// and an adapter that reports the whole slice as written drops the
    /// tail of every such write silently.
    #[derive(Default)]
    struct ShortWriter {
        written: Vec<u8>,
        chunk: usize,
        shutdown_called: bool,
        flush_called: bool,
    }

    impl hyper::rt::Read for ShortWriter {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl hyper::rt::Write for ShortWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            let n = buf.len().min(self.chunk);
            let taken = buf[..n].to_vec();
            self.written.extend_from_slice(&taken);
            Poll::Ready(Ok(n))
        }
        fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.flush_called = true;
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.shutdown_called = true;
            Poll::Ready(Ok(()))
        }
    }

    /// A partial write must be reported as partial. Claiming the whole
    /// slice went out drops the remainder with no error anywhere — the
    /// write-side twin of the silent-EOF read above, and just as invisible.
    #[test]
    fn a_partial_write_is_reported_as_partial_not_as_the_whole_slice() {
        use futures_io::AsyncWrite;
        let mut io = HyperIo::new(ShortWriter {
            chunk: 4,
            ..Default::default()
        });
        let mut cx = Context::from_waker(std::task::Waker::noop());

        match Pin::new(&mut io).poll_write(&mut cx, b"hello world") {
            Poll::Ready(Ok(n)) => assert_eq!(
                n, 4,
                "the transport took 4 bytes; reporting 11 would drop 7 with no error"
            ),
            other => panic!("expected a ready write, got {other:?}"),
        }
    }

    /// futures-io's `close` must reach hyper's `shutdown`, not its `flush`.
    ///
    /// For TLS this is the difference between sending `close_notify` and
    /// closing with a bare FIN, and a peer cannot tell a bare FIN from a
    /// truncation attack. This branch has already fixed one truncation bug
    /// that turned on exactly that distinction, so an adapter quietly
    /// mapping `close` onto `flush` would reintroduce it one layer down —
    /// invisibly, since both return `Ok(())`.
    #[test]
    fn close_reaches_shutdown_and_not_merely_flush() {
        use futures_io::AsyncWrite;
        let mut io = HyperIo::new(ShortWriter::default());
        let mut cx = Context::from_waker(std::task::Waker::noop());

        match Pin::new(&mut io).poll_close(&mut cx) {
            Poll::Ready(Ok(())) => {}
            other => panic!("expected a ready close, got {other:?}"),
        }
        let inner = io.into_inner();
        assert!(
            inner.shutdown_called,
            "close() must reach poll_shutdown — otherwise TLS closes with a bare FIN and no close_notify"
        );
        assert!(
            !inner.flush_called,
            "and must not be silently downgraded to a flush, which returns Ok(()) just the same"
        );
    }

    /// The adapter must report the number of bytes it actually filled, and
    /// `Ok(0)` ONLY at genuine exhaustion.
    ///
    /// The fourth assertion is the load-bearing one. Without it, an adapter
    /// that returns `Ok(0)` for every successful read — silent truncation
    /// of every response that crosses it — satisfies the first three by
    /// accident of ordering. That mutation was measured to pass the entire
    /// 453-test workspace suite before this test existed: the two ECH tests
    /// reach `HyperIo` only far enough to hit their panicking transport, so
    /// none of this bookkeeping ever ran.
    #[test]
    fn reads_report_what_was_filled_and_zero_only_at_the_end() {
        let mut io = HyperIo::new(Chunked {
            data: b"hello world",
            pos: 0,
            chunk: 5,
        });
        let mut buf = [0u8; 8];
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let mut read = |io: &mut HyperIo<Chunked>, buf: &mut [u8]| match Pin::new(io)
            .poll_read(&mut cx, buf)
        {
            Poll::Ready(Ok(n)) => n,
            other => panic!("expected a ready read, got {other:?}"),
        };

        assert_eq!(read(&mut io, &mut buf), 5, "first chunk");
        assert_eq!(&buf[..5], b"hello");
        assert_eq!(read(&mut io, &mut buf), 5, "second chunk");
        assert_eq!(&buf[..5], b" worl");
        assert_eq!(read(&mut io, &mut buf), 1, "the tail, shorter than a chunk");
        assert_eq!(&buf[..1], b"d");
        assert_eq!(
            read(&mut io, &mut buf),
            0,
            "zero means exhausted — and must not appear before it"
        );
    }
}
