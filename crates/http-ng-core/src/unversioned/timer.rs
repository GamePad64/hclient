use core::time::Duration;
use std::future::Future;

/// The one runtime capability the portable core needs: timeouts and
/// backoff. Networking and spawning live in the transports.
///
/// Not `hyper::rt::Timer`: that one has `Sleep: Send + Sync` unconditionally,
/// `sleep()` returns `Pin<Box<dyn Sleep>>` (an allocation per sleep), and
/// `now()` is typed on `std::time::Instant`, which panics on
/// `wasm32-unknown-unknown`.
pub trait Timer {
    type Instant: Copy + PartialOrd;

    fn sleep(&self, d: Duration) -> impl Future<Output = ()>;
    fn now(&self) -> Self::Instant;
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration;
}
