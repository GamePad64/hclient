//! `embassy_net::tcp::TcpSocket` → `futures_io::{AsyncRead, AsyncWrite}`
//! plus [`hclient_rt::Shutdown`].
//!
//! # Why a one-shot future per poll is sound here
//!
//! embassy's socket futures carry no state of their own: `TcpIo::read`,
//! `write` and `flush` are plain `poll_fn` closures that either complete or
//! call `register_recv_waker`/`register_send_waker` — and the registration
//! lives **in the socket**, not in the future
//! (`embassy-net-0.9.1/src/tcp.rs:518,556,630`). So a fresh future can be
//! built, pinned to the stack with `core::pin::pin!`, polled exactly once,
//! and dropped, without losing a wakeup. The alternative — storing a
//! self-referential future next to the socket it borrows — would need
//! either `unsafe` or a box per connection.
//!
//! # `poll_shutdown` is a real half-close here
//!
//! The W7 research spike forwarded `poll_shutdown` to `flush`, because
//! `embedded_io_async::Write` has no shutdown and `TcpConnection` (from
//! embassy's own `TcpClient`) exposes nothing else — and recorded it as "a
//! half-close hyper believes it performed and did not". This crate owns the
//! `TcpSocket` itself, so `close()` is available: `poll_shutdown` sends the
//! FIN and then waits for it, which is what hyper asks for. `close()` is
//! idempotent for our purposes — smoltcp's `close` does nothing at all in
//! `FinWait1`/`FinWait2`/`Closing`/`LastAck`/`TimeWait`/`Closed`
//! (`smoltcp-0.13.1/src/socket/tcp.rs:1068`) — so re-polling is safe.

use crate::sockets::PooledSocket;
use core::pin::{Pin, pin};
use core::task::{Context, Poll, ready};

// **The read scratch is gone, and on this backend that is RAM rather than
// a copy.** It existed because `hyper::rt::ReadBufCursor` hands out
// possibly-uninitialised memory whose only safe entrance is `put_slice`, so
// every read went through an initialised buffer of ours first — `SCRATCH`
// was 2048 bytes of application-owned RAM per connection, on a part that
// may have 256 KiB in total. `futures_io::AsyncRead` hands over an
// initialised `&mut [u8]`, which is exactly what `TcpSocket::read` wants,
// so the socket now reads straight into the caller's buffer.

/// A pooled embassy-net socket, speaking this workspace's IO traits.
pub struct EmbassyIo<const N: usize, const TX: usize, const RX: usize> {
    sock: PooledSocket<N, TX, RX>,
}

impl<const N: usize, const TX: usize, const RX: usize> core::fmt::Debug for EmbassyIo<N, TX, RX> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EmbassyIo")
            .field("sock", &self.sock)
            .finish()
    }
}

impl<const N: usize, const TX: usize, const RX: usize> EmbassyIo<N, TX, RX> {
    pub(crate) fn new(sock: PooledSocket<N, TX, RX>) -> Self {
        Self { sock }
    }
}

/// smoltcp reports exactly one IO failure, and it is a reset.
fn io_err(e: embassy_net::tcp::Error) -> std::io::Error {
    match e {
        embassy_net::tcp::Error::ConnectionReset => {
            std::io::Error::new(std::io::ErrorKind::ConnectionReset, e)
        }
    }
}

impl<const N: usize, const TX: usize, const RX: usize> futures_io::AsyncRead
    for EmbassyIo<N, TX, RX>
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let mut fut = pin!(self.sock.get_mut().read(buf));
        // `n == 0` is embassy's EOF, and `futures-io` reports one the same
        // way — a count of zero rather than a buffer left untouched.
        let n = ready!(fut.as_mut().poll(cx)).map_err(io_err)?;
        Poll::Ready(Ok(n))
    }
}

impl<const N: usize, const TX: usize, const RX: usize> futures_io::AsyncWrite
    for EmbassyIo<N, TX, RX>
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut fut = pin!(self.sock.get_mut().write(buf));
        Poll::Ready(ready!(fut.as_mut().poll(cx)).map_err(io_err))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut fut = pin!(self.sock.get_mut().flush());
        Poll::Ready(ready!(fut.as_mut().poll(cx)).map_err(io_err))
    }

    /// `futures-io` calls the end of the stream `poll_close`, and here it
    /// is the same half-close [`hclient_rt::Shutdown`] asks for, so this
    /// forwards rather than saying it twice.
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        hclient_rt::Shutdown::poll_shutdown(self, cx)
    }

    // No `poll_write_vectored`: the default writes the first non-empty
    // buffer through `poll_write`, and smoltcp's `send_slice` takes one
    // slice, so a vectored write here would be a loop pretending to be a
    // syscall.
}

/// **The half-close this backend exists to be able to perform.**
///
/// `embassy_net::tcp::TcpSocket::close` sends FIN and leaves the read half
/// open, which is why this crate owns the socket rather than reaching it
/// through an `embedded-nal-async` `Connection` — that seam has `write` and
/// `flush` and nothing else, so an adapter over it can only forward a
/// shutdown to `flush` and report "a half-close hyper believes it performed
/// and did not". `CLAUDE.md` records that as blocker two against a NAL
/// adapter, and this impl is the other side of it.
impl<const N: usize, const TX: usize, const RX: usize> hclient_rt::Shutdown
    for EmbassyIo<N, TX, RX>
{
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.sock.get_mut().close();
        let mut fut = pin!(self.sock.get_mut().flush());
        Poll::Ready(ready!(fut.as_mut().poll(cx)).map_err(io_err))
    }
}
