//! `embassy_net::tcp::TcpSocket` → `hyper::rt::{Read, Write}`.
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
use hyper::rt::ReadBufCursor;

/// Read scratch. `hyper::rt::Read` hands out possibly-uninitialised memory
/// and the only safe way in is `ReadBufCursor::put_slice`, so a read goes
/// through an initialised buffer of ours first — the same copy, for the
/// same reason, as `hclient_rt::FuturesIo`.
///
/// Sized to fit the socket's own receive buffer rather than hyper's usual
/// 8 KiB: on this backend the far side of the copy is `RX` bytes of
/// application-owned RAM, typically 1-2 KiB, and a scratch larger than that
/// can never be filled in one go. It is a field, allocated and zeroed once
/// per connection, not a local zeroed on every `poll_read` (measured in
/// `FuturesIo`'s doc comment: a stack array there compiled to a `memset`
/// per call).
const SCRATCH: usize = 2048;

/// A pooled embassy-net socket, speaking hyper's IO traits.
pub struct EmbassyIo<const N: usize, const TX: usize, const RX: usize> {
    sock: PooledSocket<N, TX, RX>,
    scratch: Box<[u8]>,
}

impl<const N: usize, const TX: usize, const RX: usize> core::fmt::Debug for EmbassyIo<N, TX, RX> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EmbassyIo")
            .field("sock", &self.sock)
            .field("scratch_len", &self.scratch.len())
            .finish()
    }
}

impl<const N: usize, const TX: usize, const RX: usize> EmbassyIo<N, TX, RX> {
    pub(crate) fn new(sock: PooledSocket<N, TX, RX>) -> Self {
        Self {
            sock,
            scratch: vec![0u8; SCRATCH.min(RX)].into_boxed_slice(),
        }
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

impl<const N: usize, const TX: usize, const RX: usize> hyper::rt::Read for EmbassyIo<N, TX, RX> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        let want = buf.remaining().min(self.scratch.len());
        if want == 0 {
            return Poll::Ready(Ok(()));
        }
        // Disjoint fields, borrowed at the same time and independently.
        let Self { sock, scratch } = &mut *self;
        let n = {
            let mut fut = pin!(sock.get_mut().read(&mut scratch[..want]));
            ready!(fut.as_mut().poll(cx)).map_err(io_err)?
        };
        // `n == 0` is embassy's EOF, and filling nothing is how a
        // `hyper::rt::Read` reports one.
        buf.put_slice(&scratch[..n]);
        Poll::Ready(Ok(()))
    }
}

impl<const N: usize, const TX: usize, const RX: usize> hyper::rt::Write for EmbassyIo<N, TX, RX> {
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

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.sock.get_mut().close();
        let mut fut = pin!(self.sock.get_mut().flush());
        Poll::Ready(ready!(fut.as_mut().poll(cx)).map_err(io_err))
    }

    // No `poll_write_vectored` and no `is_write_vectored`: hyper's default
    // for the former writes the first non-empty buffer through
    // `poll_write`, and the latter defaults to `false`, which is the honest
    // answer — smoltcp's `send_slice` takes one slice, so a vectored write
    // here would be a loop pretending to be a syscall.
}
