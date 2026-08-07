//! Where SSE reconnect's clock and randomness come from.
//!
//! **Why this exists at all.** `http_ng_core::unversioned::Timer` is the
//! project's answer to "the one runtime capability the portable core
//! needs" — but it's a QUARANTINED, backend/runtime-author-facing trait
//! (see the doc comment on `http-ng-core/src/unversioned/mod.rs`), never
//! re-exported from `http-ng`'s own facade (`lib.rs`), and `Client<T>` has
//! exactly one generic parameter (`T: Transport`) with nowhere to plug a
//! `Timer` in. An ordinary consumer calling `client.sse(url).connect()`
//! has no `Timer` to hand over even if asked, and asking them to write one
//! (an 8-line adapter over `tokio::time::sleep` or `async_io::Timer`, the
//! same shape `http-ng-rt-tokio`/`http-ng-rt-smol` already carry) just to
//! get automatic reconnect would defeat the point of "automatic". So this
//! module supplies an ambient `Timer` impl, `AmbientClock`, sourced
//! per-target, entirely inside this crate — the same reasoning
//! `crates/http-ng-proto/src/backoff.rs` already applies to jitter ("the
//! backoff takes jitter as a parameter... the randomness comes from the
//! caller" — here, `http-ng` IS that caller, for both the delay and the
//! jitter that scales it), extended to the clock, since a sleep needs
//! exactly the same kind of ambient sourcing a jitter draw does.
//!
//! **Two backends, chosen by `#[cfg]`, never by a runtime feature probe.**
//! Neither one assumes a specific async executor is already running —
//! `MockTransport`-based tests in this crate deliberately never enter a
//! tokio/smol runtime at all (see `mock.rs`'s own doc comment on why), so
//! `tokio::time::sleep`/`async_io::Timer` (which either panic outside their
//! own runtime, or silently spin up a second one — see
//! `http-ng-rt-smol/src/lib.rs`'s own rejection of `async-compat` for
//! exactly that reason) are both wrong tools here.
//!
//! - `not(all(target_family = "wasm", not(target_os = "wasi")))` — native
//!   and WASI: a hand-rolled thread-per-sleep future. `std` only, no new
//!   dependency, no `unsafe`. One OS thread per in-flight sleep is
//!   wasteful for a general-purpose timer, but reconnect backoff sleeps
//!   happen at most once per failed connection attempt — not a hot path.
//!   `std::thread::spawn` compiles on `wasm32-wasip2` (the `wasip2` CI job
//!   builds `http-ng` under that target via `http-ng-wasi`'s
//!   `examples/fetch.rs`, a dev-dependency) even though it has no working
//!   OS thread to hand out under stock `wasmtime` — the same class of
//!   "compiles everywhere, documented to require a real environment at
//!   runtime" already established for `Client::new()`'s tokio requirement
//!   (`client.rs`'s own doc comment). Nothing in this repository's CI
//!   exercises SSE reconnect on `wasm32-wasip2`.
//! - `all(target_family = "wasm", not(target_os = "wasi"))` — the browser
//!   specifically (excludes WASI, which is also `target_family = "wasm"`):
//!   `setTimeout`, found through `js_sys::global()` reflection rather than
//!   `web_sys::window()` — the same reason `http-ng-fetch`'s `execute`
//!   avoids `window()` (Task 5 of this vertical): a page's dedicated
//!   Worker has no `window` at all, only `self`/`global`.

use core::time::Duration;
use http_ng_core::unversioned::Timer;

/// A fresh, uniform `[0.0, 1.0)` jitter draw for `Backoff::delay`.
///
/// `getrandom` failing is a documented but exceedingly rare condition (the
/// OS's entropy source is unavailable). Silently discarding the error
/// (`.unwrap_or(())`, the shape this was originally sketched with) would
/// leave the buffer at all-zeros with no record that anything went wrong —
/// this project's "no silent no-ops" rule, so the fallback is written out
/// instead: an all-zeros draw maps to `jitter = 0.0`, `Backoff`'s OWN
/// documented resolution for an out-of-domain jitter (see
/// `backoff.rs`'s `nan_jitter_does_not_panic_and_is_treated_as_no_reduction`)
/// — no reduction, the conservative (slower, not faster) direction, so a
/// starved entropy source degrades reconnect into un-jittered exponential
/// backoff rather than into hammering the server.
pub(crate) fn jitter() -> f64 {
    let mut buf = [0u8; 8];
    if getrandom::fill(&mut buf).is_err() {
        return 0.0;
    }
    (u64::from_le_bytes(buf) as f64) / (u64::MAX as f64)
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AmbientClock;

#[cfg(not(all(target_family = "wasm", not(target_os = "wasi"))))]
mod imp {
    use super::*;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    #[derive(Debug)]
    struct Shared {
        done: bool,
        waker: Option<Waker>,
    }

    /// One-shot future, backed by one OS thread that really sleeps and then
    /// wakes whichever waker last polled this future. `Mutex`, not an
    /// atomic bool plus a separate waker slot: the "check done, else store
    /// waker" sequence in `poll` and the "set done, then wake" sequence in
    /// the sleeping thread must not interleave (a poll landing between the
    /// two would drop the wake and hang forever) — the same shared-lock
    /// discipline `MockTransport` uses for its own reasons (see its doc
    /// comment).
    #[derive(Debug)]
    pub(crate) struct Sleep {
        shared: Arc<Mutex<Shared>>,
    }

    impl Future for Sleep {
        type Output = ();
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            let mut s = self.shared.lock().expect("ambient clock lock poisoned");
            if s.done {
                Poll::Ready(())
            } else {
                // Overwritten on every pending poll, not just the first:
                // the executor is free to move this future between tasks
                // (rare for a leaf `.await`, but the contract doesn't
                // forbid it), and only the LATEST waker is the one that
                // will actually be polled again.
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    impl Timer for AmbientClock {
        type Instant = std::time::Instant;

        fn sleep(&self, d: Duration) -> impl Future<Output = ()> {
            let shared = Arc::new(Mutex::new(Shared {
                done: d.is_zero(),
                waker: None,
            }));
            if !d.is_zero() {
                let shared2 = Arc::clone(&shared);
                std::thread::spawn(move || {
                    std::thread::sleep(d);
                    let mut s = shared2.lock().expect("ambient clock lock poisoned");
                    s.done = true;
                    if let Some(w) = s.waker.take() {
                        w.wake();
                    }
                });
            }
            Sleep { shared }
        }

        fn now(&self) -> Self::Instant {
            std::time::Instant::now()
        }

        fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
            std::time::Instant::now().saturating_duration_since(earlier)
        }
    }
}

#[cfg(all(target_family = "wasm", not(target_os = "wasi")))]
mod imp {
    use super::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;

    impl Timer for AmbientClock {
        // `f64`, not `std::time::Instant`: that type's `now()` panics on
        // `wasm32-unknown-unknown` (see this trait's own doc comment in
        // `http-ng-core`) — `Smol`/`Tokio` each pick their own native
        // instant type for the identical reason. Milliseconds since epoch,
        // matching `js_sys::Date::now()`'s own unit.
        type Instant = f64;

        fn sleep(&self, d: Duration) -> impl Future<Output = ()> {
            // Saturates to `f64::INFINITY` for a pathological `Duration`
            // (e.g. `Duration::MAX`, reachable from an extreme `Backoff`
            // config) rather than panicking; the browser's own `setTimeout`
            // clamps an out-of-range delay to its spec-defined maximum
            // (~24.8 days) instead of erroring, so an absurd input degrades
            // to "wait a very long but bounded time", not a crash.
            let ms = d.as_secs_f64() * 1000.0;
            async move {
                let promise = js_sys::Promise::new(&mut |resolve, _reject| {
                    // `js_sys::global()`, not `web_sys::window()`: this
                    // must also work from a dedicated Worker, which has no
                    // `window` — the same reasoning `http-ng-fetch`'s
                    // `execute` already documents for finding `fetch`
                    // itself (Task 5 of this vertical).
                    let global = js_sys::global();
                    let set_timeout = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout"))
                        .expect("global scope exposes setTimeout — true of Window and every WorkerGlobalScope")
                        .unchecked_into::<js_sys::Function>();
                    // Errors here mean the host lied about having
                    // `setTimeout`, which the `expect` above already ruled
                    // out — nothing left to recover from, and `call2`'s
                    // `Result` exists for exactly the case just excluded.
                    set_timeout
                        .call2(&global, &resolve, &JsValue::from_f64(ms))
                        .expect("setTimeout, once found, does not reject a numeric delay");
                });
                // A rejected timer promise isn't a real failure mode of
                // `setTimeout` (it never rejects) — discarding the `Result`
                // here isn't a silent no-op over a real error channel, it's
                // acknowledging one that structurally cannot fire.
                let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
            }
        }

        fn now(&self) -> Self::Instant {
            js_sys::Date::now()
        }

        fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
            Duration::from_secs_f64((js_sys::Date::now() - earlier).max(0.0) / 1000.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_core::unversioned::Timer;

    #[test]
    fn jitter_is_in_the_documented_unit_range() {
        for _ in 0..64 {
            let j = jitter();
            assert!((0.0..1.0).contains(&j), "jitter {j} escaped [0.0, 1.0)");
        }
    }

    #[test]
    fn jitter_draws_are_not_all_identical() {
        // A stuck/degenerate RNG source (e.g. accidentally always zeroing
        // the buffer) would still pass the range check above — this test
        // exists to catch exactly that.
        let draws: std::collections::HashSet<_> = (0..16).map(|_| jitter().to_bits()).collect();
        assert!(
            draws.len() > 1,
            "16 draws produced the same jitter value — RNG source looks stuck"
        );
    }

    #[test]
    fn zero_duration_sleep_resolves_without_blocking_the_test() {
        futures_executor::block_on(AmbientClock.sleep(Duration::ZERO));
    }

    #[test]
    fn a_short_sleep_actually_waits_at_least_that_long() {
        let want = Duration::from_millis(15);
        let start = std::time::Instant::now();
        futures_executor::block_on(AmbientClock.sleep(want));
        let elapsed = std::time::Instant::now().duration_since(start);
        assert!(
            elapsed >= want,
            "sleep({want:?}) returned after only {elapsed:?} — woke up too early"
        );
    }
}
