//! This workspace's byte-stream seam → `hyper::rt::{Read, Write}`.
//!
//! **The one place hyper's IO traits are named, and it is deliberately
//! this crate.** The seam every runtime and every TLS backend implements
//! is [`futures_io::AsyncRead`], [`AsyncWrite`](futures_io::AsyncWrite)
//! and [`hclient_rt::Shutdown`], so hyper's major version stays out of
//! nine crates' manifests and out of the manifest of anybody outside who
//! writes a runtime or a TLS backend. `hclient-native` is where hyper is
//! actually used — `hyper::client::conn::http1::handshake` accepts
//! `hyper::rt::Read + Write` and nothing else — so the conversion lives
//! at that call rather than in every implementor's public signature.
//!
//! # The copy this costs, and why it is one copy rather than two
//!
//! `hyper::rt::Read::poll_read` hands out a
//! [`ReadBufCursor`](hyper::rt::ReadBufCursor) over memory that may never
//! have been initialised, whose only safe entrance is `put_slice`.
//! `futures_io::AsyncRead::poll_read` takes an initialised `&mut [u8]`.
//! So this adapter owns a zeroed buffer, reads into it, and copies out —
//! which is exactly what `hclient-rt-tokio` and `hclient-rt-embassy` each
//! used to do behind the old seam, once per runtime. Doing it here does
//! it once for all of them, and only on the path that reaches hyper: a
//! WebSocket over `hclient-tungstenite`, a TLS handshake, and every
//! runtime's own tests now read straight into the caller's buffer.
//!
//! The `unsafe` route — `ReadBufCursor::as_mut` and `advance` — would
//! remove the copy and is not taken: this workspace forbids `unsafe`
//! outside the amendments in `docs/exceptions.md`, and **measured before
//! the seam changed, no implementation here ever took it** — 39
//! `put_slice` call sites, zero uses of the cursor's unsafe pair.

use std::pin::Pin;
use std::task::{Context, Poll, ready};

/// The default scratch, one TCP segment's worth over sixteen.
///
/// The figure is `hclient-tls-rustls`'s `SCRATCH` rather than a new
/// guess: the buffer bounds one `poll_read`, and hyper asks for a head
/// at a time.
const SCRATCH: usize = 16 * 1024;

/// Wraps a stream on this workspace's seam so hyper can drive it.
///
/// Auto traits follow `S`, because that is the whole reason this is a
/// concrete wrapper rather than a `Box<dyn ..>`: amendment C15's rule
/// that naming is not requiring, met one layer further out — a `!Send`
/// stream stays `!Send` here and a `Send` one stays `Send`, with nothing
/// declared either way.
#[derive(Debug)]
pub struct HyperIo<S> {
    inner: S,
    /// Zeroed once per connection rather than per read.
    ///
    /// A stack array here compiled to a `memset` per call, which is
    /// `hclient-rt-embassy`'s measurement rather than a guess — the same
    /// reason its scratch was a field for as long as it had one.
    scratch: Box<[u8]>,
}

impl<S> HyperIo<S> {
    /// A stream with 16 KiB of read buffer — one TCP segment's worth
    /// over sixteen, which is `hclient-tls-rustls`'s own figure rather
    /// than a new guess.
    pub fn new(inner: S) -> Self {
        Self::with_capacity(inner, SCRATCH)
    }

    /// A stream with a read buffer of the caller's size.
    ///
    /// A zero is raised to one: a `poll_read` that could only ever report
    /// nothing is an end of stream hyper would believe, and a mis-sized
    /// bound becoming a silent EOF is the worst available answer.
    pub fn with_capacity(inner: S, cap: usize) -> Self {
        Self {
            inner,
            scratch: vec![0u8; cap.max(1)].into_boxed_slice(),
        }
    }

    /// The stream back, for a caller who wants it after the exchange.
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: futures_io::AsyncRead + Unpin> hyper::rt::Read for HyperIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        let want = buf.remaining().min(self.scratch.len());
        if want == 0 {
            return Poll::Ready(Ok(()));
        }
        let this = &mut *self;
        let n = ready!(Pin::new(&mut this.inner).poll_read(cx, &mut this.scratch[..want]))?;
        // `futures-io` spells end of stream as a count of zero and hyper
        // spells it as a cursor left untouched, so a zero here is simply
        // a `put_slice` of nothing.
        buf.put_slice(&this.scratch[..n]);
        Poll::Ready(Ok(()))
    }
}

impl<S: futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin> hyper::rt::Write for HyperIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    /// **hyper's shutdown is the half-close, not `futures-io`'s
    /// `poll_close`**, which is the distinction
    /// [`hclient_rt::Shutdown`] exists to carry: hyper sends its request,
    /// shuts the writing half, and goes on reading the response. A
    /// forward to `poll_close` here would end the stream hyper is still
    /// reading from.
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
