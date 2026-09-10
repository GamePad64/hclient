//! A runtime's clock, as this workspace's seams need it.
//!
//! [`Timer`] is three methods over two associated types, and the shape is
//! decided by one fact: [`Timer::Instant`] is a **stopwatch, not a
//! calendar** — `Copy + PartialOrd` with an `elapsed_since`, and no
//! epoch. Anything that needs a wall clock (cookie `Expires`, an HTTP
//! cache's `Date`) reaches for one itself and says so, because anchoring a
//! calendar to this would freeze outright under a clock whose
//! `elapsed_since` is always zero.
//!
//! [`Timer::Sleep`] is an associated type rather than an RPITIT, for the
//! reason every seam here names its futures: naming is not requiring, so
//! each runtime answers for its own auto traits and a `Send`-boxing
//! consumer can still write the bound. [`Discard`] is the wrapper for a
//! sleep whose output is not `()`.
use core::time::Duration;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// The one runtime capability the portable core needs: timeouts and
/// backoff. Networking and spawning live in the transports.
///
/// Not `hyper::rt::Timer`: that one has `Sleep: Send + Sync` unconditionally,
/// `sleep()` returns `Pin<Box<dyn Sleep>>` (an allocation per sleep), and
/// `now()` is typed on `std::time::Instant`, which panics on
/// `wasm32-unknown-unknown`.
///
/// # Why [`Timer::Sleep`] is an associated type and not `impl Future`
///
/// An RPITIT — `fn sleep(&self, d: Duration) -> impl Future<Output = ()>` —
/// is more comfortable to write and costs two things:
///
/// - **A struct cannot hold a sleep.** It has no name to store, so a body
///   wrapper can only check elapsed time on each `poll_frame`, which
///   structurally cannot cut a response body that goes *completely* silent
///   after the head: nothing wakes the wrapper, so nothing ever looks at
///   the clock again. Measured with a counting waker and no executor
///   running, that shape registers **zero** wakes; a stored sleep
///   registers one. `hclient::body::Deadline` holds a
///   `Pin<Box<Tm::Sleep>>` for exactly this reason.
/// - **Generic code cannot spawn a background task.**
///   `hclient_rt::Spawn<F>` takes the future as a type parameter, so a
///   bound has to name it, and an anonymous future has no name. See
///   `hclient-native`'s `pool` module doc.
///
/// It also hides a third thing, the one most likely to be mistaken for a
/// bug: a backend whose native timer resolves to something other than
/// `()`, which `async { t.await; }` discards silently. Naming the type
/// makes that visible, and [`Discard`] is the adapter for it.
///
/// [`TcpConnect::Stream`](https://docs.rs/hclient-rt) is the same idea
/// applied to a socket; this is not a new shape in the seam.
pub trait Timer {
    type Instant: Copy + PartialOrd;

    /// The future [`Timer::sleep`] returns, **named**.
    ///
    /// `Send`ness is deliberately not required here, exactly as it is not
    /// required of `Timer` itself: a caller that needs a `Send` sleep gets
    /// it because its own clock's `Sleep` happens to be `Send`, inferred
    /// rather than declared.
    type Sleep: Future<Output = ()>;

    fn sleep(&self, d: Duration) -> Self::Sleep;
    fn now(&self) -> Self::Instant;
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration;
}

/// Adapts a future that resolves to *something* into one that resolves to
/// `()`, for use as a [`Timer::Sleep`].
///
/// **This is not redundant, and it is not a mistake.** Two of this
/// project's clocks have a native timer whose `Output` is not `()`:
/// `async_io::Timer` resolves to the `std::time::Instant` at which it
/// fired, and `hclient-fetch`'s `SendJsFuture` resolves to
/// `Result<JsValue, JsValue>`. While [`Timer::sleep`] was an RPITIT both
/// were discarded invisibly inside an `async` block; with a named
/// associated type the discard has to be written down, and this is where
/// it is written down once instead of twice.
///
/// `F: Unpin` rather than a pin projection: every timer this wraps is
/// `Unpin` already, and this workspace forbids `unsafe`, so the safe
/// projection is the only one available and the bound is honest about it.
#[derive(Debug, Clone, Copy)]
pub struct Discard<F>(pub F);

impl<F: Future + Unpin> Future for Discard<F> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.0).poll(cx).map(|_| ())
    }
}

// ── erasure ─────────────────────────────────────────────────────────────
//
// The boxed form of the trait above, beside it for `BoxFuture`'s reason.
// A runtime writes none of it: the blanket impl below is over every
// `Timer`, and the instant is erased as a *question* — how long ago was
// this — because `Copy` on a trait object is not a thing.

/// An erased sleep, as [`BoxTimer`] hands one back.
///
/// `Send`, for [`crate::transport::BoxBody`]'s reason and inseparably from
/// it: a response body holds a sleep — that is how a total timeout cuts a
/// silent body — so the two answer the same question.
pub type BoxSleep = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>; // send-bound-exception: amendment-C14

/// A moment a [`BoxTimer`] recorded, which can be asked how long ago it
/// was and nothing else.
///
/// One method on purpose: it is what lets an erased clock exist at all.
/// See this module's own doc.
pub trait BoxInstantOf {
    /// How long since this stamp was taken, on the clock that took it.
    fn elapsed(&self) -> Duration;
}

/// A stamp a [`BoxTimer`] took, erased.
///
/// Not `Send`, for [`BoxSleep`]'s reason: the same body holds the stamp the
/// sleep was computed from.
pub type BoxInstant = Box<dyn BoxInstantOf + Send>; // send-bound-exception: amendment-C14

/// [`Timer`], with the sleep boxed and the instant behind [`BoxInstantOf`].
pub trait BoxTimer {
    /// [`Timer::now`], as a stamp that outlives the borrow.
    fn now_boxed(&self) -> BoxInstant;

    /// [`Timer::sleep`], boxed.
    fn sleep_boxed(&self, d: Duration) -> BoxSleep;
}

/// A clock a facade can share between threads, erased.
///
/// [`crate::transport::SharedTransport`]'s reasoning, for the other seam.
pub type SharedTimer = dyn BoxTimer + Send + Sync; // send-bound-exception: amendment-C12

/// The stamp the blanket [`BoxTimer`] hands out: the clock and the moment
/// together, so `elapsed` is answered by the clock that took it.
struct Stamp<Tm: Timer> {
    timer: Tm,
    at: Tm::Instant,
}

impl<Tm: Timer> BoxInstantOf for Stamp<Tm> {
    fn elapsed(&self) -> Duration {
        self.timer.elapsed_since(self.at)
    }
}

impl<Tm> BoxTimer for Tm
where
    Tm: Timer + Clone + Send + 'static, // send-bound-exception: amendment-C14
    Tm::Instant: Send,                  // send-bound-exception: amendment-C14
    Tm::Sleep: Send + 'static,          // send-bound-exception: amendment-C14
{
    fn now_boxed(&self) -> BoxInstant {
        Box::new(Stamp {
            timer: self.clone(),
            at: self.now(),
        })
    }

    fn sleep_boxed(&self, d: Duration) -> BoxSleep {
        Box::pin(self.sleep(d))
    }
}
