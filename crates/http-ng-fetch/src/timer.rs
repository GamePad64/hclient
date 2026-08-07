//! [`BrowserClock`]: the browser's `setTimeout`, as an
//! `http_ng_core::unversioned::Timer`.
//!
//! **Why this lives here, and why that's a fact about this vertical, not a
//! pattern.** Every other runtime capability in this project is implemented
//! by the crate that actually knows the runtime — `http-ng-rt-tokio` and
//! `http-ng-rt-smol` each provide `Timer` for their own executor. For SSE
//! reconnect (`http-ng`'s `SseBuilder::with_timer`) to work in a browser at
//! all, SOME crate has to be the browser's runtime crate — and there is no
//! separate `http-ng-rt-browser`: the runtime *is* the browser, and
//! `http-ng-fetch` is the only crate that already talks to it, already
//! carries `wasm-bindgen`/`js-sys`, and already has the one `Send`-adapter
//! (`promise::SendJsFuture`) a browser-native async primitive needs. This
//! makes `http-ng-fetch` both a `Transport` AND a provider of a runtime
//! capability, which no other crate in the project is — that duality is a
//! *consequence* of the browser having no separate runtime crate to put this
//! in, not a precedent for `http-ng-native` (which has `http-ng-rt-tokio`/
//! `http-ng-rt-smol` to do this properly) to start growing its own `Timer`
//! impls too.
//!
//! **History.** An earlier attempt put an ambient, per-target clock inside
//! `http-ng` itself (ambient meaning: no explicit `Timer` argument anywhere,
//! `http-ng` picked one per `#[cfg]`). That was rejected on review, for two
//! reasons that both apply regardless of which crate the clock lives in
//! except that here they don't: (1) it put per-target runtime code in the
//! *facade* crate — the same thing `crates/http-ng-rt-pair-check` exists to
//! catch, and which this crate, being a genuine per-target runtime provider,
//! is exactly where that code belongs; (2) the native half of that code
//! compiled on `wasm32-wasip2` while being unable to actually work there — a
//! problem specific to sharing one clock type across incompatible targets,
//! which doesn't arise here since this crate only ever builds for
//! `wasm32-unknown-unknown` in the first place.
//!
//! **No new `unsafe`.** `sleep` builds on [`crate::promise::SendJsFuture`],
//! the crate's existing `Send`-compatible bridge over a JS promise (see its
//! own doc comment) — not a second, independent wrapper. This crate's CI
//! `no-unsafe-code` check path-scopes the one legitimate
//! `unsafe-code-exception` marker in the whole project to `promise.rs`
//! specifically; reusing `SendJsFuture` rather than writing a fresh
//! `wasm_bindgen_futures::JsFuture`-based wrapper (which is `!Send`, and
//! would need its own `unsafe impl Send` to fix, exactly duplicating
//! `promise.rs`'s reasoning in a second file) is what keeps that true. No
//! `unsafe` appears anywhere in this file.

use crate::promise::SendJsFuture;
use core::time::Duration;
use http_ng_core::unversioned::Timer;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

/// SSE reconnect's clock for the browser — see the module doc comment for
/// why it's here rather than in `http-ng` or a separate runtime crate.
///
/// A zero-sized marker type, the same shape as `http_ng_rt_tokio::Tokio`/
/// `http_ng_rt_smol::Smol`: `Client::builder(Fetch::new())` for the
/// transport, `.with_timer(BrowserClock)` for reconnect — two independent
/// values, not one type serving double duty, so a caller who only wants the
/// transport never has to think about the timer at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct BrowserClock;

impl Timer for BrowserClock {
    // `f64`, not `std::time::Instant`: that type's `now()` panics on
    // `wasm32-unknown-unknown` (see `Timer`'s own doc comment in
    // `http-ng-core`) — `Tokio`/`Smol` each pick their own native instant
    // type for the identical reason. Milliseconds since epoch, matching
    // `js_sys::Date::now()`'s own unit.
    type Instant = f64;

    fn sleep(&self, d: Duration) -> impl std::future::Future<Output = ()> {
        // The previous version of this comment claimed the multiply
        // saturates to `f64::INFINITY` for a pathological `Duration` and
        // that the browser then clamps to a ~24.8-day maximum — both
        // false, caught in review round 1 (Minor-7). Measured directly:
        // `Duration::MAX.as_secs_f64() * 1000.0 ≈ 1.8447e22`, an ordinary
        // finite `f64`, nowhere near `f64::MAX` (≈1.7977e308) — no
        // saturation happens at all, and `setTimeout`'s `timeout`
        // parameter is a WebIDL `long` (32-bit signed), which coerces an
        // out-of-range double via `ToInt32` (modulo `2^32`, reinterpreted
        // signed) rather than clamping to any spec-defined maximum — the
        // real outcome for a value this large is an effectively arbitrary
        // short delay, most plausibly firing very soon, not a predictable
        // "long but bounded" wait. This path stays unreachable in
        // practice regardless: `Backoff::max` caps the delay before it
        // ever reaches `sleep`, so no config gets `d` anywhere near
        // `Duration::MAX` — but the comment shouldn't assert a precise
        // behavior nobody verified.
        let ms = d.as_secs_f64() * 1000.0;
        async move {
            let promise = js_sys::Promise::new(&mut |resolve, _reject| {
                // `js_sys::global()`, not `web_sys::window()`: this must
                // also work from a dedicated Worker, which has no `window`
                // — the same reasoning `Fetch::execute` already documents
                // for finding `fetch` itself.
                let global = js_sys::global();
                let set_timeout = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout"))
                    .expect(
                        "global scope exposes setTimeout — true of Window \
                             and every WorkerGlobalScope",
                    )
                    .unchecked_into::<js_sys::Function>();
                // Errors here mean the host lied about having `setTimeout`,
                // which the `expect` above already ruled out — nothing left
                // to recover from, and `call2`'s `Result` exists for
                // exactly the case just excluded.
                set_timeout
                    .call2(&global, &resolve, &JsValue::from_f64(ms))
                    .expect("setTimeout, once found, does not reject a numeric delay");
            });
            // A rejected timer promise isn't a real failure mode of
            // `setTimeout` (it never rejects) — discarding the `Result`
            // here isn't a silent no-op over a real error channel, it's
            // acknowledging one that structurally cannot fire. `SendJsFuture`,
            // not `wasm_bindgen_futures::JsFuture`: see the module doc
            // comment on why this is what keeps this file free of a second
            // `unsafe`.
            let _ = SendJsFuture::new(promise).await;
        }
    }

    fn now(&self) -> Self::Instant {
        js_sys::Date::now()
    }

    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        Duration::from_secs_f64((js_sys::Date::now() - earlier).max(0.0) / 1000.0)
    }
}
