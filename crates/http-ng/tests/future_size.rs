//! How big `Client::execute`'s future is, asserted rather than discovered.
//!
//! **This test exists because the previous detector was a stack overflow
//! in another crate.** `http-ng-native`'s
//! `checkout_walks_past_a_dead_connection_to_a_live_one` holds *two*
//! client futures in one frame — `tokio::join!` of two requests — which
//! made it the first thing to notice when the cache wiring grew the
//! future, and it noticed by aborting with `SIGABRT` rather than by
//! failing an assertion. Measured on that tree: the test needs between
//! 1 MiB and 2 MiB of stack without the cache and between 2 MiB and
//! 2.5 MiB with it, against a 2 MiB default. The margin was always thin;
//! the cache ate what was left.
//!
//! So the bound is stated here, where a reader can see it, and the number
//! is a **ceiling with room** rather than the current value: pinning the
//! exact size would fail on every unrelated edit and be relaxed without
//! thought, which is how a guard becomes a rubber stamp.
#![cfg(all(feature = "test-util", not(target_family = "wasm")))]

use http_ng::Client;
use http_ng::mock::MockTransport;

/// Measured 4,232 bytes without the `cache` feature and 4,344 with it, on
/// x86-64 Linux, debug.
///
/// **The ceiling is 6 KiB, and 8 KiB was wrong.** It was chosen as
/// "roughly double" before anyone measured what a wrapper actually costs;
/// `http-ng-native`'s twin of this test then measured it — one extra
/// `async fn` layer around an exchange grew that future by **1.81×** — so
/// a ceiling at 2× is a guard that cannot fire for the defect it names.
/// 6 KiB is 1.38× today's, which is room for an ordinary field and none
/// for a layer.
const CEILING: usize = 6 * 1024;

#[test]
fn the_execute_future_stays_under_its_ceiling() {
    let c = Client::builder(MockTransport::new())
        .build()
        .expect("build");
    let fut = c.execute(http::Request::new(http_ng_core::RequestBody::Empty));
    let size = std::mem::size_of_val(&fut);
    assert!(
        size <= CEILING,
        "`Client::execute`'s future is {size} bytes, over the {CEILING}-byte \
         ceiling. Something joined it by value that should be boxed — see \
         `cached::Cached::recorder`, which is boxed for exactly this reason \
         and records the measurement that forced it. A caller holding two \
         of these in one frame is not hypothetical: \
         `http-ng-native`'s `checkout_walks_past_a_dead_connection_to_a_live_one` \
         does, and overflowed its stack when this last grew."
    );
}
