//! `quinn::Runtime` over this workspace's runtime seam.
//!
//! `rt` is any `R` a runtime crate here supplies — `Tokio`, `Smol`, ... —
//! and the bounds below are [`endpoint`]'s own, spelled out because a
//! doctest cannot infer them from a sketch.
//!
//! ```no_run
//! # use std::fmt;
//! # use hclient_rt::{Spawn, Timer, UdpAdoptStd};
//! # use hclient_native::QuinnTask;
//! # async fn example<R>(
//! #     rt: &R,
//! #     client_cfg: quinn::ClientConfig,
//! #     addr: std::net::SocketAddr,
//! # ) -> Result<(), Box<dyn std::error::Error>>
//! # where
//! #     R: Timer + UdpAdoptStd + Spawn<QuinnTask> + Clone + Send + Sync + 'static,
//! #     R::Sleep: Send + 'static,
//! #     R::Socket: fmt::Debug + Send + Sync + 'static,
//! # {
//! let endpoint = hclient_native::endpoint(rt, "0.0.0.0:0".parse()?)?;
//! let conn = endpoint.connect_with(client_cfg, addr, "example.com")?.await?;
//! # let _ = conn;
//! # Ok(())
//! # }
//! ```
//!
//! This crate is the whole reason QUIC is reachable here at all. quinn
//! needs a spawner, a timer and a UDP socket, and it takes all three
//! through **unsealed public traits** — unlike `hyper`'s HTTP/2 client,
//! whose `Http2ClientConnExec` has a private supertrait, so an executor of
//! ours could not be written at any price and v0.2 had to reach past hyper
//! to the `h2` crate. Nothing here reaches past anything: `quinn::{Runtime,
//! AsyncTimer, AsyncUdpSocket, UdpPoller}` are all implemented from
//! outside quinn, in this file.
//!
//! # Which way round this crate points
//!
//! The rest of the `hclient-rt-*` family implements **this workspace's**
//! seams for one runtime — `hclient-rt-tokio` is `TcpConnect`, `Timer`,
//! `UdpBind` on tokio. This one points the other way: it implements
//! **quinn's** runtime traits over whatever `R` a caller already has, so
//! the arrow is `seam -> quinn` rather than `runtime -> seam`. It is in the
//! family because its subject is the seam and it names no runtime; it is
//! not one of them because it adds no backend.
//!
//! # The three bounds quinn adds, and where they live
//!
//! `Runtime`, `AsyncTimer` and `AsyncUdpSocket` are each declared `Send +
//! Sync + Debug + 'static` (`quinn-0.11.11/src/runtime.rs:16`, `:33`,
//! `:41`). `hclient_rt`'s own [`Timer`], [`Spawn`] and [`UdpBind`] promise
//! none of that, deliberately — `Send`ness in this workspace is inferred by
//! auto-traits, not declared.
//!
//! Those bounds are therefore paid **here and in [`crate::H3Runtime`]**,
//! by the crates that want QUIC, and not by the seam. A runtime that cannot
//! satisfy them can still implement [`UdpBind`] honestly and be used for
//! everything else; it just cannot be handed to this crate. That is a
//! compile error where the caller wrote it, which is the shape this project
//! already uses for `DefaultTransport` on unsupported targets.
//!
//! # The `Timer` change that turned out not to be needed
//!
//! `Timer` looked as if it must gain an absolute-deadline sleep, because
//! `AsyncTimer::reset(i)` re-arms a timer to an absolute instant while our
//! seam has `sleep(Duration)` over an opaque `Instant` with no conversion
//! in either direction.
//!
//! **Measured, and the seam change is unnecessary.** quinn's `Instant` *is*
//! `std::time::Instant` on every non-wasm target (`quinn-0.11.11/src/lib.rs:56`,
//! `pub(crate) use std::time::{Duration, Instant}`), and `Runtime::now`
//! defaults to `std::time::Instant::now()`. So the deadline quinn hands
//! over and the clock this module subtracts from are the same clock, and
//! `reset(i)` is exactly `sleep(i - now)` with no conversion and no
//! guesswork. `SeamTimer` (private, below) does that, and `Timer` is
//! untouched.
//!
//! [`Timer`]: hclient_rt::Timer
//! [`Spawn`]: hclient_rt::Spawn
//! [`UdpBind`]: hclient_rt::UdpBind
#![forbid(unsafe_code)]

use hclient_rt::{Timer, UdpAdoptStd, UdpBind, UdpDatagrams};
use std::fmt;
use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Instant;

/// The future type `quinn::Runtime::spawn` hands over.
///
/// Named, `Send` and `'static` — which is the whole reason `Spawn` is
/// usable here when generic library code cannot use it. `Spawn<F>` makes
/// the future a type parameter of the *trait*, so a bound has to name it,
/// and an `async` block has no name (`E0308: expected type parameter F,
/// found async block`) — the wall v0.2 W2 hit and recorded. quinn does not
/// hand over an `async` block; it hands over exactly this type, which
/// `impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for Tokio`
/// accepts verbatim.
pub type QuinnTask = Pin<Box<dyn Future<Output = ()> + Send>>; // send-bound-exception: amendment-C10

/// The runtime seam, dressed as a `quinn::Runtime`.
pub struct SeamRuntime<R> {
    rt: R,
}

impl<R> SeamRuntime<R> {
    pub fn new(rt: R) -> Self {
        Self { rt }
    }
}

// Hand-written rather than derived: `R` is not required to be `Debug`, and
// requiring it would be one more bound on the runtime for the benefit of a
// trait bound that only wants *something* printable.
impl<R> fmt::Debug for SeamRuntime<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SeamRuntime")
    }
}

impl<R> quinn::Runtime for SeamRuntime<R>
where
    R: Timer + UdpAdoptStd + hclient_rt::Spawn<QuinnTask> + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C10
    R::Sleep: Send + 'static, // send-bound-exception: amendment-C10
    R::Socket: fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
{
    fn new_timer(&self, i: Instant) -> Pin<Box<dyn quinn::AsyncTimer>> {
        Box::pin(SeamTimer::new(self.rt.clone(), i))
    }

    fn spawn(&self, future: QuinnTask) {
        hclient_rt::Spawn::spawn(&self.rt, future);
    }

    fn wrap_udp_socket(
        &self,
        t: std::net::UdpSocket,
    ) -> io::Result<Arc<dyn quinn::AsyncUdpSocket>> {
        Ok(Arc::new(SeamSocket::new(self.rt.adopt(t)?)))
    }
}

pin_project_lite::pin_project! {
    /// A `quinn::AsyncTimer` built from [`Timer::sleep`].
    ///
    /// Holds the clock as well as the sleep, because `reset` has to build a
    /// new one — `Timer` offers no way to re-arm an existing future, and it
    /// does not need one: dropping a sleep and making another is what every
    /// runtime here does underneath anyway.
    ///
    /// Pin-projected rather than boxed: `tokio::time::Sleep` is `!Unpin`, so
    /// the alternative is `Pin<Box<R::Sleep>>` and an allocation on every
    /// `reset` — and quinn resets a connection's timer on essentially every
    /// packet.
    struct SeamTimer<R: Timer> {
        rt: R,
        #[pin]
        sleep: R::Sleep,
    }
}

impl<R: Timer> SeamTimer<R> {
    fn new(rt: R, deadline: Instant) -> Self {
        let sleep = rt.sleep(until(deadline));
        Self { rt, sleep }
    }
}

/// How long from now until `deadline`.
///
/// `saturating_duration_since` rather than a subtraction: a deadline
/// already in the past is ordinary (a timer armed for a moment that elapsed
/// while the loop was busy) and must become a zero-length sleep, not a
/// panic.
///
/// **The sentence above used to imply that `deadline - Instant::now()`
/// panics, and on 1.97 it does not** — a mutation swapping one for the
/// other survived the whole suite, and `past - Instant::now()` measured
/// `0ns` rather than an abort. `impl Sub<Instant> for Instant` calls
/// `duration_since`, which has saturated since 1.60. The call stays as it
/// is for the reason std's own doc gives beside that change — *"future
/// versions may reintroduce the panic in some circumstances"* — so this is
/// a choice about a guarantee rather than about today's behaviour, and the
/// mutation is recorded as a second control rather than as a gap.
fn until(deadline: Instant) -> std::time::Duration {
    deadline.saturating_duration_since(Instant::now())
}

impl<R: Timer> fmt::Debug for SeamTimer<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SeamTimer")
    }
}

impl<R> quinn::AsyncTimer for SeamTimer<R>
where
    R: Timer + Send + 'static, // send-bound-exception: amendment-C10
    R::Sleep: Send + 'static,  // send-bound-exception: amendment-C10
{
    fn reset(self: Pin<&mut Self>, i: Instant) {
        let mut this = self.project();
        let fresh = this.rt.sleep(until(i));
        this.sleep.set(fresh);
    }

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.project().sleep.poll(cx)
    }
}

/// Every task waiting for one socket to become writable.
///
/// # Why this exists, and why it is not over-engineering
///
/// [`UdpDatagrams::poll_writable`] takes `&self` and a `Context`, so an
/// implementation over `tokio::net::UdpSocket::poll_send_ready` or
/// `async_io::Async::poll_writable` stores **one** waker: the last caller
/// to register wins and every earlier one sleeps for ever. quinn creates a
/// `UdpPoller` per connection *and* one for the endpoint driver
/// (`runtime.rs:88-93` says exactly this: *"Each `UdpPoller` is responsible
/// for notifying at most one task"*), and they can all be blocked on
/// writability at once when a send buffer fills.
///
/// So this adapter keeps the list quinn assumes, without asking the seam to
/// keep it: the wakers of every waiting poller live here, the inner socket
/// is polled with a `Waker` that fans out to all of them, and a socket that
/// stores one waker is enough. The cost is one `Mutex<Vec<Waker>>` per
/// socket and a spurious wake or two on the way out; the alternative is a
/// stall under exactly the load h3 is chosen for.
#[derive(Debug, Default)]
struct WakeAll(Mutex<Vec<Waker>>);

impl WakeAll {
    fn register(&self, w: &Waker) {
        let mut waiters = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // `will_wake` keeps the list from growing without bound when the
        // same poller re-registers on every poll, which is the normal case.
        if !waiters.iter().any(|existing| existing.will_wake(w)) {
            waiters.push(w.clone());
        }
    }

    fn wake_all(&self) {
        let taken = std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()));
        for w in taken {
            w.wake();
        }
    }
}

impl Wake for WakeAll {
    fn wake(self: Arc<Self>) {
        self.wake_all();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_all();
    }
}

/// A [`UdpDatagrams`] socket, dressed as a `quinn::AsyncUdpSocket`.
#[derive(Debug)]
struct SeamSocket<S> {
    inner: S,
    writable: Arc<WakeAll>,
    caps: hclient_rt::UdpCaps,
}

impl<S: UdpDatagrams> SeamSocket<S> {
    fn new(inner: S) -> Self {
        // Read once, at construction. `caps()` is allowed to be a real
        // query of the descriptor, and quinn asks `max_transmit_segments`
        // on a hot path.
        let caps = inner.caps();
        Self {
            inner,
            writable: Arc::new(WakeAll::default()),
            caps,
        }
    }

    fn poll_writable_shared(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.writable.register(cx.waker());
        let fanout = Waker::from(Arc::clone(&self.writable));
        let mut inner_cx = Context::from_waker(&fanout);
        match self.inner.poll_writable(&mut inner_cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(r) => {
                // Ready consumed the inner registration, so anyone else on
                // the list would now wait on nothing. Wake them to re-poll
                // and re-register instead.
                self.writable.wake_all();
                Poll::Ready(r)
            }
        }
    }
}

impl<S> quinn::AsyncUdpSocket for SeamSocket<S>
where
    S: UdpDatagrams + fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
{
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn quinn::UdpPoller>> {
        Box::pin(SeamPoller(self))
    }

    fn try_send(&self, transmit: &quinn::udp::Transmit) -> io::Result<()> {
        let d = hclient_rt::Datagrams {
            destination: transmit.destination,
            src_ip: transmit.src_ip,
            ecn: transmit.ecn.map(from_quinn_ecn),
            segment_size: transmit.segment_size,
            contents: transmit.contents,
        };
        // The guard, not decoration: a socket handed a GSO batch larger
        // than it declared refuses by name rather than putting one
        // oversized datagram on the wire. quinn reads
        // `max_transmit_segments` before it batches, so this can only fire
        // on a bug — which is exactly why it is worth firing.
        d.reject_unsupported(self.caps)?;
        self.inner.try_send(&d)
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [quinn::udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let mut ours = vec![hclient_rt::RecvMeta::default(); meta.len()];
        let n = std::task::ready!(self.inner.poll_recv(cx, bufs, &mut ours))?;
        for (dst, src) in meta.iter_mut().zip(ours.iter().take(n)) {
            *dst = quinn::udp::RecvMeta {
                addr: src.addr,
                len: src.len,
                stride: if src.stride == 0 { src.len } else { src.stride },
                ecn: src.ecn.map(to_quinn_ecn),
                dst_ip: src.dst_ip,
            };
        }
        Poll::Ready(Ok(n))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.caps.max_send_segments
    }

    fn max_receive_segments(&self) -> usize {
        self.caps.max_recv_segments
    }

    fn may_fragment(&self) -> bool {
        self.caps.may_fragment
    }
}

/// The conversion the seam's independence costs, in both directions.
///
/// `hclient_rt::EcnCodepoint` is declared rather than re-exported so that
/// `hclient-rt` carries no QUIC dependency, and this `match` is the whole
/// price of that. Exhaustive both ways: adding a variant on either side is
/// a compile error here rather than a silently unmapped codepoint.
fn to_quinn_ecn(c: hclient_rt::EcnCodepoint) -> quinn::udp::EcnCodepoint {
    match c {
        hclient_rt::EcnCodepoint::Ect0 => quinn::udp::EcnCodepoint::Ect0,
        hclient_rt::EcnCodepoint::Ect1 => quinn::udp::EcnCodepoint::Ect1,
        hclient_rt::EcnCodepoint::Ce => quinn::udp::EcnCodepoint::Ce,
    }
}

fn from_quinn_ecn(c: quinn::udp::EcnCodepoint) -> hclient_rt::EcnCodepoint {
    match c {
        quinn::udp::EcnCodepoint::Ect0 => hclient_rt::EcnCodepoint::Ect0,
        quinn::udp::EcnCodepoint::Ect1 => hclient_rt::EcnCodepoint::Ect1,
        quinn::udp::EcnCodepoint::Ce => hclient_rt::EcnCodepoint::Ce,
    }
}

/// One task's view of a socket's writability. See [`WakeAll`].
#[derive(Debug)]
struct SeamPoller<S>(Arc<SeamSocket<S>>);

impl<S> quinn::UdpPoller for SeamPoller<S>
where
    S: UdpDatagrams + fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
{
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.poll_writable_shared(cx)
    }
}

/// Bind a socket through the seam and hand quinn an endpoint driven by it.
///
/// `new_with_abstract_socket`, not `Endpoint::client`: the latter binds a
/// `std::net::UdpSocket` itself and calls
/// [`quinn::Runtime::wrap_udp_socket`], which would make [`UdpAdoptStd`]
/// the required capability rather than [`UdpBind`]. Going through the
/// abstract socket keeps [`UdpBind`] — the trait every runtime can
/// implement — as the one this path actually needs, with `UdpAdoptStd`
/// present only because the `quinn::Runtime` trait has a method that
/// demands it.
pub fn endpoint<R>(rt: &R, local: SocketAddr) -> io::Result<quinn::Endpoint>
where
    R: Timer + UdpAdoptStd + hclient_rt::Spawn<QuinnTask> + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C10
    R::Sleep: Send + 'static, // send-bound-exception: amendment-C10
    R::Socket: fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
{
    let socket = Arc::new(SeamSocket::new(UdpBind::bind(rt, local)?));
    quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        None,
        socket,
        Arc::new(SeamRuntime::new(rt.clone())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two pollers on one socket, of which the inner socket can remember
    /// only the most recent — the exact shape `tokio::net::UdpSocket` and
    /// `async_io::Async` have, and the reason [`WakeAll`] exists.
    #[derive(Debug, Default)]
    struct OneWakerSocket {
        last: Mutex<Option<Waker>>,
    }

    impl UdpDatagrams for OneWakerSocket {
        fn try_send(&self, _: &hclient_rt::Datagrams<'_>) -> io::Result<()> {
            Err(io::ErrorKind::WouldBlock.into())
        }
        fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            *self.last.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }
        fn poll_recv(
            &self,
            _: &mut Context<'_>,
            _: &mut [IoSliceMut<'_>],
            _: &mut [hclient_rt::RecvMeta],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
        fn local_addr(&self) -> io::Result<SocketAddr> {
            Ok(SocketAddr::from(([127, 0, 0, 1], 0)))
        }
    }

    #[derive(Default)]
    struct Counter(std::sync::atomic::AtomicUsize);
    impl Wake for Counter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn a_socket_with_one_waker_slot_still_wakes_every_waiting_poller() {
        // Without `WakeAll` the second poller's registration overwrites the
        // first's, the socket wakes one task, and the other waits for ever.
        // This is the failure quinn's own doc describes and the reason it
        // hands out a `UdpPoller` per interested task rather than exposing
        // one `poll_send`.
        let sock = Arc::new(SeamSocket::new(OneWakerSocket::default()));

        let a = Arc::new(Counter::default());
        let b = Arc::new(Counter::default());
        let (wa, wb) = (Waker::from(a.clone()), Waker::from(b.clone()));

        assert!(
            sock.poll_writable_shared(&mut Context::from_waker(&wa))
                .is_pending()
        );
        assert!(
            sock.poll_writable_shared(&mut Context::from_waker(&wb))
                .is_pending()
        );

        // The socket remembered one waker — ours, the fan-out one — and
        // fires it once.
        let inner = sock.inner.last.lock().unwrap().clone().expect("registered");
        inner.wake();

        let n = |c: &Arc<Counter>| c.0.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(n(&a), 1, "the first poller must not be forgotten");
        assert_eq!(n(&b), 1, "the second poller must be woken too");
    }

    /// A socket that is Pending once and writable ever after — the shape a
    /// real one has the moment its send buffer drains.
    #[derive(Debug, Default)]
    struct ReadyOnSecondPoll {
        polls: Mutex<usize>,
    }

    impl UdpDatagrams for ReadyOnSecondPoll {
        fn try_send(&self, _: &hclient_rt::Datagrams<'_>) -> io::Result<()> {
            Ok(())
        }
        fn poll_writable(&self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            let mut n = self.polls.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        }
        fn poll_recv(
            &self,
            _: &mut Context<'_>,
            _: &mut [IoSliceMut<'_>],
            _: &mut [hclient_rt::RecvMeta],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
        fn local_addr(&self) -> io::Result<SocketAddr> {
            Ok(SocketAddr::from(([127, 0, 0, 1], 0)))
        }
    }

    #[test]
    fn a_ready_answer_wakes_the_pollers_it_did_not_answer() {
        // The other half of the fan-out, and the half that had no test until
        // a mutation deleting `wake_all()` from the `Ready` arm survived the
        // whole suite.
        //
        // `poll_writable` returning `Ready` CONSUMES the inner socket's one
        // registration — the fan-out waker it was given is spent, and the
        // socket is holding nothing. Every other poller on the list is then
        // waiting on a wake-up that can no longer come from anywhere, which
        // is the same stall `WakeAll` exists to prevent, arriving through
        // the success path instead of the failure one.
        let sock = Arc::new(SeamSocket::new(ReadyOnSecondPoll::default()));

        let stranded = Arc::new(Counter::default());
        let w = Waker::from(stranded.clone());
        assert!(
            sock.poll_writable_shared(&mut Context::from_waker(&w))
                .is_pending(),
            "the first poll must register and wait"
        );

        // A second poller gets the socket's `Ready`. The first is not its
        // business and is not told anything by the socket.
        let lucky = Waker::from(Arc::new(Counter::default()));
        assert!(
            sock.poll_writable_shared(&mut Context::from_waker(&lucky))
                .is_ready()
        );

        assert_eq!(
            stranded.0.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a poller left on the list after someone else took the readiness \
             must be woken to re-poll and re-register"
        );
    }

    #[test]
    fn re_registering_the_same_poller_does_not_grow_the_list() {
        // A poller that returns `Pending` is polled again with the same
        // waker on the next wake-up. Without `will_wake` the list grows by
        // one clone per poll for the life of the connection.
        let sock = SeamSocket::new(OneWakerSocket::default());
        let w = Waker::from(Arc::new(Counter::default()));
        for _ in 0..8 {
            assert!(
                sock.poll_writable_shared(&mut Context::from_waker(&w))
                    .is_pending()
            );
        }
        assert_eq!(sock.writable.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn ecn_survives_the_round_trip_through_both_conversions() {
        // The price of `hclient-rt` not depending on a QUIC crate is these
        // two `match`es, and the thing that could go wrong with them is a
        // silent swap of `Ect0` and `Ect1` — two codepoints that differ by
        // one bit and mean different things to a congestion controller.
        for c in [
            hclient_rt::EcnCodepoint::Ect0,
            hclient_rt::EcnCodepoint::Ect1,
            hclient_rt::EcnCodepoint::Ce,
        ] {
            assert_eq!(from_quinn_ecn(to_quinn_ecn(c)), c);
            // And against the wire value, so a consistent double-swap in
            // both directions cannot pass either.
            assert_eq!(to_quinn_ecn(c) as u8, c as u8);
        }
    }

    #[test]
    fn a_deadline_already_past_is_a_zero_sleep_not_a_panic() {
        let past = Instant::now() - std::time::Duration::from_secs(60);
        assert_eq!(until(past), std::time::Duration::ZERO);
        // And a future one is positive, or the subtraction is backwards and
        // every timer fires immediately.
        let soon = Instant::now() + std::time::Duration::from_secs(30);
        assert!(until(soon) > std::time::Duration::from_secs(29));
    }
}
