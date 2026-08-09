//! Negative control 3: **the wall that `Spawn<F>`'s shape actually hits**,
//! and it is not `Send`.
//!
//! `Spawn<F>` is generic over the future, so the bound has to *name* the
//! future. Generic library code that wants to spawn an `async` block of
//! its own cannot: an async block's type has no name. hyper never notices
//! because everything it hands its `Executor` is a concrete struct
//! (`hyper::client::conn::http1::Connection<I, B>`). A pool reaper is not.
//!
//! `cargo build --bin unnameable --features must-fail`

use http_ng_rt::{Spawn, Timer};
use std::time::Duration;

/// The natural way to write "start a background task that sleeps and then
/// does something", generic over the runtime.
fn start_reaper<R, F>(rt: R)
where
    R: Timer + Clone + 'static + Spawn<F>,
    F: Future<Output = ()>,
{
    let r = rt.clone();
    rt.spawn(async move {
        r.sleep(Duration::from_millis(1)).await;
    });
}

fn main() {
    start_reaper::<_, _>(http_ng_rt_tokio::Tokio);
}
