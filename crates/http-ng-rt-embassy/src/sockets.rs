//! A bounded pool of `embassy_net::tcp::TcpSocket`s, and the reason this
//! crate owns one at all.
//!
//! # Why not `embassy_net::tcp::client::TcpClient`
//!
//! embassy-net already ships a bounded pool with exactly the right shape —
//! `TcpClient` over a `TcpClientState<N, TX, RX>`, handing out
//! `TcpConnection<'d, N, TX, RX>` values that return their buffers on drop
//! — and the W7 research measured `Native<_, NoTls, IpLiteralOnly>` driving
//! six real requests through one. It is not used here, and the reason is
//! one line of embassy-net's own source
//! (`embassy-net-0.9.1/src/tcp.rs:466`):
//!
//! ```text
//! impl<'a> Drop for TcpSocket<'a> {
//!     fn drop(&mut self) {
//!         self.io.stack.with_mut(|i| i.sockets.remove(self.io.handle));
//!     }
//! }
//! ```
//!
//! `TcpConnection::drop` queues a FIN with `close()` and then drops the
//! `TcpSocket`, which **removes the socket from smoltcp's `SocketSet`
//! before the stack is ever polled again**. The queued FIN never becomes a
//! packet. Measured by the research on a live TAP link: after dropping the
//! `execute` future, the server saw nothing at all for two seconds — as far
//! as it was concerned the connection was still open — where the same
//! observation against `http-ng-native` on tokio or smol sees the peer go
//! away in microseconds. A synchronous `Drop` cannot fix this: giving the
//! stack the one poll it needs means awaiting, and `Drop` cannot await.
//!
//! # What this pool does instead
//!
//! [`PooledSocket::drop`] calls `close()` and then **keeps the socket
//! alive**, moved into [`Inner::closing`]. Two things follow, and the
//! second is the one that matters:
//!
//! 1. The socket is still in smoltcp's `SocketSet`, so the FIN goes out on
//!    the stack's very next poll — and `close()` itself wakes the stack
//!    task (`TcpIo::with_mut` ends in `i.waker.wake()`), so that poll
//!    happens immediately rather than at the next unrelated event. The far
//!    end sees the close as promptly as it does for any other backend in
//!    this workspace. **This is what keeps
//!    `CancelSupport::Supported` true**, see the crate doc.
//! 2. The socket's slot is not free yet, and getting it back needs an
//!    `await` — which is why the reclaim lives in [`SocketPool::acquire`]
//!    (a call that already is one) and not in a background task the
//!    application would have to remember to spawn. A forgotten task would
//!    be a silent liveness bug; a forgotten `await` is impossible.
//!
//! # No `unsafe`, and where the buffers live
//!
//! The workspace forbids `unsafe`, so this cannot be embassy's own pool
//! implementation (`TcpClientState` hands out `&'static mut [u8]` through
//! `UnsafeCell` and a `Drop` that frees them again). Instead the sockets
//! are created **once**, at construction, out of a `&'static mut
//! [SocketBuffers<TX, RX>; N]` the application owns, and are then reused:
//! smoltcp's `connect` starts by calling `reset()` on the socket, so a
//! socket that has reached `Closed` or `TimeWait` is as good as a new one
//! (`smoltcp-0.13.1/src/socket/tcp.rs`, `connect` → `self.reset()`, and
//! `is_open()` which is `false` for exactly those two states). N sockets
//! also means N `SocketStorage` slots in the stack for the whole life of
//! the program, which is what `StackResources<N>` is sized for anyway.

use core::cell::RefCell;
use core::task::{Poll, Waker};
use embassy_net::Stack;
use embassy_net::tcp::{State, TcpSocket};
use embassy_time::Duration;

/// One socket's worth of buffers, owned by the application.
///
/// `TX`/`RX` are the send and receive window this backend can offer, fixed
/// at build time. That is also why `TcpOpts::send_buffer_size` and
/// `recv_buffer_size` are refused rather than approximated — see
/// `Embassy::APPLIES`.
#[derive(Debug)]
pub struct SocketBuffers<const TX: usize, const RX: usize> {
    rx: [u8; RX],
    tx: [u8; TX],
}

impl<const TX: usize, const RX: usize> SocketBuffers<TX, RX> {
    /// `const` so a device build can put an array of these in a `static`
    /// (through `StaticCell`) with no runtime initialisation at all.
    pub const fn new() -> Self {
        Self {
            rx: [0; RX],
            tx: [0; TX],
        }
    }
}

impl<const TX: usize, const RX: usize> Default for SocketBuffers<TX, RX> {
    fn default() -> Self {
        Self::new()
    }
}

/// How long a closing socket may wait for a peer that has stopped
/// answering before smoltcp aborts it and the slot comes back.
///
/// Set on the socket in [`PooledSocket::drop`] and cleared again in
/// `Embassy::connect`, so it bounds the closing handshake and nothing else
/// — an *established* connection has no inactivity timeout here, because
/// `TcpOpts` has no way for a caller to ask for one and a long-poll request
/// is not a stalled one.
///
/// # Why smoltcp's own timer and not `embassy_time::with_timeout`
///
/// Because there is no usable `embassy_time` below `TcpConnect::connect`.
/// `http_ng_native::connect::drive` runs its connect attempts inside
/// `futures_util::stream::FuturesUnordered`, which polls each future with a
/// waker of its own making, and embassy's *integrated* timer queue refuses
/// any waker that is not one of its task wakers. Measured, as a panic from
/// inside the reclaim path, with this backtrace:
///
/// ```text
/// panicked at embassy-executor-0.9.1/src/raw/waker.rs:38:
///   Found waker not created by the Embassy executor.
///   `embassy_time::Timer` only works with the Embassy executor.
///   ...
///   <FuturesUnordered<..connect..> as Stream>::poll_next
///   http_ng_native::connect::drive::{closure#0}
/// ```
///
/// `embassy_time::Timer` is fine everywhere else in this crate — including
/// [`crate::Embassy`]'s own `Timer::sleep`, which `Native` polls with the
/// ambient waker in `with_connect_timeout` and in the Happy Eyeballs
/// stagger, both hand-rolled `poll_fn`s that pass `cx` straight through
/// (`connect_timeout_is_enforced_over_this_runtimes_clock` is the test that
/// this stays true). Only the inside of `connect` is off limits, and
/// `Socket::set_timeout` is smoltcp's own answer for exactly this: the
/// deadline is enforced by the stack while it dispatches, needing no waker
/// at all (`smoltcp-0.13.1/src/socket/tcp.rs`, `timed_out` →
/// `set_state(Closed)`).
const CLOSING_TIMEOUT: Duration = Duration::from_secs(5);

/// A bounded pool of sockets, owned by the application and shared with
/// [`crate::Embassy`] by `&'static`.
pub struct SocketPool<const N: usize, const TX: usize, const RX: usize> {
    inner: RefCell<Inner>,
}

/// The two lists are `Vec`, but neither is ever grown past `N`: every
/// socket in them was created in [`SocketPool::new`] and no code path
/// creates another, so both are allocated once with `with_capacity(N)` and
/// only ever move sockets between themselves.
struct Inner {
    /// Sockets in `Closed`/`TimeWait`: `connect` may be called on them.
    free: Vec<TcpSocket<'static>>,
    /// Sockets whose exchange is over and whose `close()` has already been
    /// called — the FIN is queued or already on the wire. They stay
    /// registered with smoltcp until someone needs the slot.
    closing: Vec<TcpSocket<'static>>,
    /// `acquire` calls parked because every slot is in use. Woken by
    /// [`PooledSocket::drop`].
    waiters: Vec<Waker>,
}

impl Inner {
    /// Move every closing socket that has finished — `Closed` or
    /// `TimeWait`, the two states `connect` accepts — onto the free list,
    /// **at the end**, where [`SocketPool::acquire`]'s `pop` will take it
    /// first.
    ///
    /// Cheap (no `await`, no packet), and doing it eagerly is not
    /// housekeeping for its own sake — it is what stops one abandoned
    /// connect from starving the whole interface. A socket that was
    /// abandoned before the peer ever answered ends up `Closed` with its
    /// four-tuple still set, and smoltcp then tries to emit one RST for it
    /// — retrying **once a second, for ever**, if the peer's hardware
    /// address cannot be resolved, because the tuple is only cleared once
    /// the RST is actually sent (`smoltcp-0.13.1/src/socket/tcp.rs`,
    /// "forget about it after sending a single RST packet"). Each of those
    /// attempts consumes the neighbour cache's **global** ARP rate limit,
    /// which is one request per second for the entire interface — so a
    /// second socket trying to reach a different, perfectly reachable host
    /// never gets an ARP request out at all. Measured, with
    /// `embassy-net`'s `log` feature on, against a black-hole address:
    ///
    /// ```text
    /// #1: neighbor 192.168.69.9 silence timer expired, rediscovering
    /// outgoing segment will abort connection / sending RST|ACK
    /// address 192.168.69.9 not in neighbor cache, sending ARP request
    /// #3: neighbor 192.168.69.1 silence timer expired, rediscovering
    /// outgoing segment will send data or flags / sending SYN
    /// #3: neighbor 192.168.69.1 missing, silencing until t+1.000s
    /// ```
    ///
    /// — repeating until the watchdog fired, with no ARP request for
    /// `.1` ever sent. The way out is `connect`'s own `reset()`, which
    /// clears the tuple; taking the finished sockets first is what makes
    /// that happen at the next request instead of never.
    /// `connect_timeout_is_enforced_over_this_runtimes_clock` is that
    /// scenario, and it hung for the full 30s watchdog before this.
    fn reclaim_finished(&mut self) {
        let mut i = 0;
        while i < self.closing.len() {
            if matches!(self.closing[i].state(), State::Closed | State::TimeWait) {
                self.free.push(self.closing.remove(i));
            } else {
                i += 1;
            }
        }
    }
}

impl<const N: usize, const TX: usize, const RX: usize> core::fmt::Debug for SocketPool<N, TX, RX> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `try_borrow`: `Debug` must not panic just because it was called
        // from inside a scope that holds the borrow.
        match self.inner.try_borrow() {
            Ok(i) => f
                .debug_struct("SocketPool")
                .field("slots", &N)
                .field("free", &i.free.len())
                .field("closing", &i.closing.len())
                .field("waiting", &i.waiters.len())
                .finish(),
            Err(_) => f.debug_struct("SocketPool").field("slots", &N).finish(),
        }
    }
}

impl<const N: usize, const TX: usize, const RX: usize> SocketPool<N, TX, RX> {
    /// Build the pool's `N` sockets out of buffers the application owns.
    ///
    /// The `&'static mut` is the whole point: `TcpSocket::new` moves the
    /// two slices into smoltcp's `SocketSet`, and the lifetime is what
    /// keeps them alive for as long as the handle is. On a device those
    /// buffers come from a `StaticCell`; on a host, [`SocketPool::leak`] is
    /// the same thing with one `Box::leak` instead.
    pub fn new(stack: Stack<'static>, buffers: &'static mut [SocketBuffers<TX, RX>; N]) -> Self {
        let mut free = Vec::with_capacity(N);
        for b in buffers.iter_mut() {
            // Split one `&'static mut SocketBuffers` into two disjoint
            // `&'static mut [u8]`. Both coercions consume the reference
            // rather than reborrowing it, which is what preserves
            // `'static` — a `&mut b.rx[..]` here would not.
            let SocketBuffers { rx, tx } = b;
            let rx: &'static mut [u8] = rx;
            let tx: &'static mut [u8] = tx;
            free.push(TcpSocket::new(stack, rx, tx));
        }
        Self {
            inner: RefCell::new(Inner {
                free,
                closing: Vec::with_capacity(N),
                waiters: Vec::new(),
            }),
        }
    }

    /// [`SocketPool::new`] for a target with an allocator, leaking the
    /// buffers and the pool **once** — `N * (TX + RX)` bytes for the life
    /// of the process, not per connection.
    ///
    /// The per-connection `Box::leak` this replaces is the obvious bug the
    /// W7 research called out by name: 2 KiB gone for every request, for
    /// ever, on a part with 256 KiB of RAM.
    pub fn leak(stack: Stack<'static>) -> &'static Self {
        let buffers: &'static mut [SocketBuffers<TX, RX>; N] =
            Box::leak(Box::new([const { SocketBuffers::new() }; N]));
        Box::leak(Box::new(Self::new(stack, buffers)))
    }

    /// A socket that can be connected, waiting if every slot is busy.
    ///
    /// Reclaiming a closing socket is the only part of a connection's life
    /// that has to wait for the network, and it happens here rather than in
    /// a background task — see the module doc.
    pub(crate) async fn acquire(&'static self) -> PooledSocket<N, TX, RX> {
        loop {
            {
                let mut inner = self.inner.borrow_mut();
                inner.reclaim_finished();
                if let Some(sock) = inner.free.pop() {
                    return PooledSocket {
                        pool: self,
                        sock: Some(sock),
                    };
                }
            }
            // Nothing finished by itself; take the oldest socket that is
            // still closing and wait for it. Out of the list before
            // awaiting: a `RefCell` borrow must never be held across an
            // `await`.
            let closing = self.inner.borrow_mut().closing.pop();
            if let Some(mut sock) = closing {
                finish_closing(&mut sock).await;
                return PooledSocket {
                    pool: self,
                    sock: Some(sock),
                };
            }
            // Every slot is in flight. `PooledSocket::drop` is the only
            // thing that can change that, and it wakes us.
            core::future::poll_fn(|cx| {
                let mut inner = self.inner.borrow_mut();
                if inner.free.is_empty() && inner.closing.is_empty() {
                    inner.waiters.push(cx.waker().clone());
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            })
            .await;
        }
    }

    /// Test-only view of the bookkeeping: how many sockets are ready to
    /// connect, and how many are still closing.
    #[doc(hidden)]
    pub fn counts(&self) -> (usize, usize) {
        let inner = self.inner.borrow();
        (inner.free.len(), inner.closing.len())
    }
}

/// Wait until a socket that has been `close()`d is safe to connect again.
///
/// `flush()` is the public API that answers "has the FIN actually gone
/// out?": it resolves only once nothing is left to send, which for a closed
/// socket means the FIN was transmitted **and acknowledged**
/// (`embassy-net-0.9.1/src/tcp.rs`, `TcpIo::flush` waits on `fin_pending`
/// for `FinWait1 | Closing | LastAck` and on `rst_pending` for a `Closed`
/// socket that still has a peer).
///
/// After that the socket is usually in `FinWait2` — our side is done, the
/// peer has not closed its own half yet. We do not wait for it: the peer
/// has already seen the close, which is everything the cancellation
/// contract promises, and holding the slot hostage to a peer that may never
/// close would turn a pool of `N` into a pool of nothing. `abort()` moves
/// the socket to `Closed` so `connect` will take it.
///
/// The wait is bounded by [`CLOSING_TIMEOUT`], which was set on the socket
/// before its `close()` — see that constant for why the bound is smoltcp's
/// and not `embassy_time`'s.
async fn finish_closing(sock: &mut TcpSocket<'static>) {
    // Already finished — usually because the connection was abandoned
    // before the peer ever answered. There is no FIN for anyone to see,
    // and any RST smoltcp still wants to emit is best-effort: waiting for
    // one that cannot be dispatched is exactly the interface-wide stall
    // `Inner::reclaim_finished` describes.
    if matches!(sock.state(), State::Closed | State::TimeWait) {
        return;
    }
    // The FIN is the whole point of the closing list, so this one is
    // waited for. The result is deliberately ignored: `ConnectionReset`
    // means the peer tore the connection down first, which is a perfectly
    // good way for a socket we are closing anyway to end up reusable.
    let _ = sock.flush().await;
    // The peer never closed its own half (`FinWait2`) or never
    // acknowledged ours before `CLOSING_TIMEOUT`. Ours is done and the far
    // end has seen it; take the slot back rather than hold it hostage.
    if !matches!(sock.state(), State::Closed | State::TimeWait) {
        sock.abort();
    }
}

/// A socket checked out of a [`SocketPool`], which returns itself on drop
/// **without leaving the stack** — see the module doc.
pub struct PooledSocket<const N: usize, const TX: usize, const RX: usize> {
    pool: &'static SocketPool<N, TX, RX>,
    /// `None` only between [`PooledSocket::drop`] taking the socket and the
    /// value itself going away.
    sock: Option<TcpSocket<'static>>,
}

impl<const N: usize, const TX: usize, const RX: usize> core::fmt::Debug
    for PooledSocket<N, TX, RX>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PooledSocket")
            .field("state", &self.get().state())
            .finish()
    }
}

impl<const N: usize, const TX: usize, const RX: usize> PooledSocket<N, TX, RX> {
    pub(crate) fn get(&self) -> &TcpSocket<'static> {
        self.sock.as_ref().expect("socket is taken only in Drop")
    }

    pub(crate) fn get_mut(&mut self) -> &mut TcpSocket<'static> {
        self.sock.as_mut().expect("socket is taken only in Drop")
    }
}

impl<const N: usize, const TX: usize, const RX: usize> Drop for PooledSocket<N, TX, RX> {
    fn drop(&mut self) {
        let Some(mut sock) = self.sock.take() else {
            return;
        };
        // A peer that stops answering must not hold this slot for ever,
        // and the bound has to be set here, synchronously, because the
        // reclaim cannot use a timer (see `CLOSING_TIMEOUT`).
        sock.set_timeout(Some(CLOSING_TIMEOUT));
        // Queue the FIN *and* wake the stack task, both inside this one
        // call: `TcpSocket::close` goes through `TcpIo::with_mut`, whose
        // last act is `i.waker.wake()`. Nothing else in this function can
        // put a packet on the wire, and nothing else needs to.
        sock.close();
        let mut inner = self.pool.inner.borrow_mut();
        inner.closing.push(sock);
        // A slot became reclaimable; whoever is parked in `acquire` is the
        // one who will do the reclaiming.
        for w in inner.waiters.drain(..) {
            w.wake();
        }
    }
}
