//! `embedded_io_async::{Read, Write}` -> `hyper::rt::{Read, Write}`.
//!
//! The same bridge `hclient-rt-embassy` writes for one concrete socket,
//! written once for every stack instead — which is the whole point of
//! adapting the abstraction rather than an implementation.

use hyper::rt::ReadBufCursor;
use std::io;
use std::pin::{Pin, pin};
use std::task::{Context, Poll, ready};

/// The default read chunk, and it is a **default rather than a constant**:
/// [`NalIo::with_capacity`] takes the size at run time.
///
/// The buffer is a field rather than a local zeroed on every `poll_read`,
/// which is `hclient-rt-embassy`'s measurement rather than a guess: a
/// stack array there compiled to a `memset` per call. On a device the
/// figure that matters is bytes per open connection, so the caller sets
/// it — 2 KiB is one TCP segment's worth and a reasonable floor for a
/// response head, not a number this crate wants anybody stuck with.
pub const DEFAULT_CHUNK: usize = 2048;

/// A NAL connection speaking hyper's IO traits.
///
/// # The contract this bridge needs, which the trait does not give
///
/// `poll_read` builds an `embedded_io_async::Read::read` future, polls it
/// once, and drops it if it is pending — so a pending read that consumed
/// bytes and then got dropped would lose them. `embedded-io-async` 0.7.0
/// **encourages** implementations to be side-effect-free on cancel and
/// requires nothing: *"Implementations should document whether they're
/// actually side-effect-free on cancel or not."* Its own `read_exact` and
/// `write_all` are documented as **not** cancel-safe, which is why this
/// bridge uses neither.
///
/// So: **the stack's `read` and `write` must be cancel-safe**, and that is
/// a fact about the stack rather than about the trait. `embassy-net`'s are
/// — its futures register a waker and do nothing else. A stack whose are
/// not cannot be adapted this way at all, and there is no check that can
/// tell them apart, which is why this is stated here rather than asserted
/// somewhere.
#[derive(Debug)]
pub struct NalIo<C> {
    conn: C,
    scratch: Box<[u8]>,
}

impl<C> NalIo<C> {
    /// A connection with [`DEFAULT_CHUNK`] bytes of read buffer.
    pub fn new(conn: C) -> Self {
        Self::with_capacity(conn, DEFAULT_CHUNK)
    }

    /// A connection with a read buffer of the caller's size.
    ///
    /// A zero is raised to one: `poll_read` would otherwise report a
    /// filled-nothing, which `hyper::rt::Read` reads as end of stream, and
    /// a mis-sized buffer becoming a silent EOF is the worst available
    /// answer.
    pub fn with_capacity(conn: C, chunk: usize) -> Self {
        Self {
            conn,
            scratch: vec![0u8; chunk.max(1)].into_boxed_slice(),
        }
    }

    /// The connection back, for a caller who wants it after the exchange.
    pub fn into_inner(self) -> C {
        self.conn
    }
}

fn io_err<E: embedded_io_async::Error>(e: E) -> io::Error {
    // `embedded_io_async::ErrorKind` is a superset of what `io::ErrorKind`
    // spells the same way, so the kind is carried rather than flattened
    // into `Other` — a transport above this can then tell a refused
    // connection from a reset one.
    let kind = match e.kind() {
        embedded_io_async::ErrorKind::NotConnected => io::ErrorKind::NotConnected,
        embedded_io_async::ErrorKind::ConnectionReset => io::ErrorKind::ConnectionReset,
        embedded_io_async::ErrorKind::ConnectionRefused => io::ErrorKind::ConnectionRefused,
        embedded_io_async::ErrorKind::ConnectionAborted => io::ErrorKind::ConnectionAborted,
        embedded_io_async::ErrorKind::TimedOut => io::ErrorKind::TimedOut,
        embedded_io_async::ErrorKind::Interrupted => io::ErrorKind::Interrupted,
        embedded_io_async::ErrorKind::InvalidInput => io::ErrorKind::InvalidInput,
        embedded_io_async::ErrorKind::InvalidData => io::ErrorKind::InvalidData,
        embedded_io_async::ErrorKind::WriteZero => io::ErrorKind::WriteZero,
        embedded_io_async::ErrorKind::Unsupported => io::ErrorKind::Unsupported,
        embedded_io_async::ErrorKind::OutOfMemory => io::ErrorKind::OutOfMemory,
        embedded_io_async::ErrorKind::BrokenPipe => io::ErrorKind::BrokenPipe,
        embedded_io_async::ErrorKind::AlreadyExists => io::ErrorKind::AlreadyExists,
        embedded_io_async::ErrorKind::AddrInUse => io::ErrorKind::AddrInUse,
        embedded_io_async::ErrorKind::AddrNotAvailable => io::ErrorKind::AddrNotAvailable,
        embedded_io_async::ErrorKind::PermissionDenied => io::ErrorKind::PermissionDenied,
        embedded_io_async::ErrorKind::NotFound => io::ErrorKind::NotFound,
        // `Other` and any variant a later `embedded-io` adds: the kind is
        // unknown rather than absent, and `Other` says exactly that.
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, "embedded-io")
}

impl<C: embedded_io_async::Read + Unpin> hyper::rt::Read for NalIo<C> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let want = buf.remaining().min(self.scratch.len());
        if want == 0 {
            return Poll::Ready(Ok(()));
        }
        // Disjoint fields, borrowed at the same time and independently.
        let Self { conn, scratch } = &mut *self;
        let n = {
            let mut fut = pin!(conn.read(&mut scratch[..want]));
            ready!(fut.as_mut().poll(cx)).map_err(io_err)?
        };
        // `n == 0` is end of stream, and filling nothing is how a
        // `hyper::rt::Read` reports one.
        buf.put_slice(&scratch[..n]);
        Poll::Ready(Ok(()))
    }
}

impl<C: embedded_io_async::Write + Unpin> hyper::rt::Write for NalIo<C> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut fut = pin!(self.conn.write(buf));
        Poll::Ready(ready!(fut.as_mut().poll(cx)).map_err(io_err))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut fut = pin!(self.conn.flush());
        Poll::Ready(ready!(fut.as_mut().poll(cx)).map_err(io_err))
    }

    /// **A flush, and hyper will believe it was a half-close.**
    ///
    /// `embedded_io_async::Write` is `write` and `flush`, and
    /// `embedded_nal_async::TcpConnect::Connection` is bounded on `Read +
    /// Write` and nothing more — so a NAL connection cannot send a FIN
    /// while still reading, by the trait's own definition. There is no
    /// honest implementation of this method here, and returning an error
    /// would be worse: hyper treats a failed shutdown as a connection
    /// error and would kill exchanges that are otherwise fine.
    ///
    /// `hclient-rt-embassy` avoids this by owning the
    /// `embassy_net::tcp::TcpSocket` and calling `close()`; that is the
    /// price of adapting the abstraction instead of an implementation, and
    /// it is stated here rather than left to be discovered from a peer
    /// that never sees the FIN.
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut fut = pin!(self.conn.flush());
        Poll::Ready(ready!(fut.as_mut().poll(cx)).map_err(io_err))
    }
}
