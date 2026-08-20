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
/// It used to be `fn sleep(&self, d: Duration) -> impl Future<Output = ()>`
/// — an RPITIT. That is more comfortable to write and it cost two real
/// things, both of which had been recorded as unfixable before anyone
/// measured them:
///
/// - **A struct cannot hold a sleep.** `hclient::deadline::Deadline`
///   therefore checked elapsed time on each `poll_frame` rather than
///   racing a sleep, and so could not cut a response body that goes
///   *completely* silent after the head: nothing wakes the wrapper, so
///   nothing ever looks at the clock again. Measured with a counting waker
///   and no executor running: that shape registers **zero** wakes and can
///   never fire; a stored sleep registers one. **Since collected**:
///   `Deadline` holds a `Pin<Box<Tm::Sleep>>` and polls it whenever the
///   wrapped body answers `Pending`, and `hclient`'s `tests/deadline.rs`
///   cuts a server that sends the head and then nothing for ever. This
///   bullet is the reason the associated type exists, not a cost still
///   being paid.
/// - **Generic code cannot spawn a background task.**
///   `hclient_rt::Spawn<F>` takes the future as a type parameter, so a
///   bound has to name it — and an anonymous future has no name. That is
///   what actually stood behind "a pool driven by a spawned task does not
///   compile on this seam", a sentence three pieces of work built on and
///   which was wrong about the reason. See `hclient-native`'s `pool`
///   module doc.
///
/// The RPITIT hid a third thing, smaller but the most likely to be
/// mistaken for a bug: a backend whose native timer resolves to something
/// other than `()`. `async { t.await; }` discarded that value silently.
/// Naming the type makes it visible, and [`Discard`] is the adapter for
/// it — see its doc comment.
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
