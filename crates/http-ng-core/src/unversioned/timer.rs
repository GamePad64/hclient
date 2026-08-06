use core::time::Duration;
use std::future::Future;

/// Единственная способность рантайма, нужная портативному ядру: таймауты и
/// backoff. Сеть и spawn живут в транспортах.
///
/// Не `hyper::rt::Timer`: у того `Sleep: Send + Sync` безусловно, `sleep()`
/// возвращает `Pin<Box<dyn Sleep>>` (аллокация на каждый sleep), а `now()`
/// типизирован на `std::time::Instant`, который паникует на
/// `wasm32-unknown-unknown`.
pub trait Timer {
    type Instant: Copy + PartialOrd;

    fn sleep(&self, d: Duration) -> impl Future<Output = ()>;
    fn now(&self) -> Self::Instant;
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration;
}
