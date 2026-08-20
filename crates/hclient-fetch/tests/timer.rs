//! `BrowserClock` — the browser half of SSE reconnect's `Timer`.
//!
//! Same convention as this crate's other test files (`caps.rs`,
//! `transport.rs`, ...): `#[wasm_bindgen_test]`, run against a real browser
//! global. Unlike `TestTimer` (`hclient`'s controllable clock for its own
//! `MockTransport`-based unit tests), there is no way to fake `setTimeout`
//! deterministically here without reimplementing it — these tests exercise
//! the real thing, against real (short) delays.
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use hclient_core::unversioned::Timer;
use hclient_fetch::BrowserClock;
use std::time::Duration;

#[wasm_bindgen_test]
async fn sleep_does_not_resolve_before_the_requested_duration_elapses() {
    let t = BrowserClock;
    let start = t.now();
    t.sleep(Duration::from_millis(30)).await;
    let elapsed = t.elapsed_since(start);
    assert!(
        elapsed >= Duration::from_millis(30),
        "sleep(30ms) returned after only {elapsed:?} — resolved too early"
    );
}

/// A guard against a `sleep` that always resolves immediately regardless of
/// the requested `Duration` (the exact mutation `hclient`'s own
/// `honours_server_sent_retry_over_the_policy` test was strengthened to
/// catch after it initially missed a `sleep(Duration::ZERO)` substitution —
/// see that task's report). Two different, well-separated delays; a
/// hardcoded-zero (or any other constant) `sleep` would make both finish in
/// roughly the same, near-zero time instead of the longer one taking
/// visibly longer.
#[wasm_bindgen_test]
async fn a_longer_sleep_takes_measurably_longer_than_a_shorter_one() {
    let t = BrowserClock;

    let short_start = t.now();
    t.sleep(Duration::from_millis(5)).await;
    let short_elapsed = t.elapsed_since(short_start);

    let long_start = t.now();
    t.sleep(Duration::from_millis(80)).await;
    let long_elapsed = t.elapsed_since(long_start);

    assert!(
        long_elapsed > short_elapsed,
        "an 80ms sleep ({long_elapsed:?}) must take measurably longer than \
         a 5ms one ({short_elapsed:?}) — a fixed-duration sleep wouldn't"
    );
    assert!(long_elapsed >= Duration::from_millis(80));
    assert!(short_elapsed >= Duration::from_millis(5));
}

#[wasm_bindgen_test]
async fn sleeping_zero_resolves_without_hanging() {
    // Bounded by wasm-bindgen-test's own per-test timeout if this is
    // wrong — no additional guard needed, but worth stating why this test
    // is safe to write at all: an actually-hanging `sleep(ZERO)` would
    // fail the test suite loudly (a timeout), not silently.
    BrowserClock.sleep(Duration::ZERO).await;
}

#[wasm_bindgen_test]
fn now_does_not_go_backwards_across_two_immediate_calls() {
    let t = BrowserClock;
    let a = t.now();
    let b = t.now();
    assert!(b >= a, "now() went backwards: {a} then {b}");
}

#[wasm_bindgen_test]
fn elapsed_since_a_point_in_the_past_is_positive() {
    let t = BrowserClock;
    let earlier = t.now();
    // Busy-wait a handful of `now()` calls rather than sleep, to keep this
    // test synchronous and independent of `sleep` itself (which the tests
    // above already cover) — just needs SOME wall-clock time to pass.
    let mut last = earlier;
    for _ in 0..100_000 {
        last = t.now();
        if last > earlier {
            break;
        }
    }
    assert!(
        last > earlier,
        "no time appeared to pass across 100,000 now() calls"
    );
    assert!(t.elapsed_since(earlier) > Duration::ZERO);
}
