//! [`BrowserClock`]: the browser's `setTimeout`, as an
//! `hclient_core::unversioned::Timer`.
//!
//! **Why this lives here, and why that's a fact about this vertical, not a
//! pattern.** Every other runtime capability in this project is implemented
//! by the crate that actually knows the runtime — `hclient-rt-tokio` and
//! `hclient-rt-smol` each provide `Timer` for their own executor. For SSE
//! reconnect (`hclient`'s `SseBuilder::with_timer`) to work in a browser at
//! all, SOME crate has to be the browser's runtime crate — and there is no
//! separate `hclient-rt-browser`: the runtime *is* the browser, and
//! `hclient-fetch` is the only crate that already talks to it, already
//! carries `wasm-bindgen`/`js-sys`, and already has the one `Send`-adapter
//! (`promise::SendJsFuture`) a browser-native async primitive needs. This
//! makes `hclient-fetch` both a `Transport` AND a provider of a runtime
//! capability, which no other crate in the project is — that duality is a
//! *consequence* of the browser having no separate runtime crate to put this
//! in, not a precedent for `hclient-native` (which has `hclient-rt-tokio`/
//! `hclient-rt-smol` to do this properly) to start growing its own `Timer`
//! impls too.
//!
//! **History.** An earlier attempt put an ambient, per-target clock inside
//! `hclient` itself (ambient meaning: no explicit `Timer` argument anywhere,
//! `hclient` picked one per `#[cfg]`). That was rejected on review, for two
//! reasons that both apply regardless of which crate the clock lives in
//! except that here they don't: (1) it put per-target runtime code in the
//! *facade* crate — the same thing `crates/hclient-rt-pair-check` exists to
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
use futures_channel::oneshot;
use hclient_core::unversioned::Timer;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

/// SSE reconnect's clock for the browser — see the module doc comment for
/// why it's here rather than in `hclient` or a separate runtime crate.
///
/// A zero-sized marker type, the same shape as `hclient_rt_tokio::Tokio`/
/// `hclient_rt_smol::Smol`: `Client::builder(Fetch::new())` for the
/// transport, `.with_timer(BrowserClock)` for reconnect — two independent
/// values, not one type serving double duty, so a caller who only wants the
/// transport never has to think about the timer at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct BrowserClock;

impl Timer for BrowserClock {
    // `f64`, not `std::time::Instant`: that type's `now()` panics on
    // `wasm32-unknown-unknown` (see `Timer`'s own doc comment in
    // `hclient-core`) — `Tokio`/`Smol` each pick their own native instant
    // type for the identical reason. Milliseconds since epoch, matching
    // `js_sys::Date::now()`'s own unit.
    type Instant = f64;

    /// **The adapter is not redundant.** [`SendJsFuture`] resolves to
    /// `Result<JsValue, JsValue>`, not `()`, so a named `Timer::Sleep`
    /// has to say what happens to that value. A `let _ =` inside an
    /// `async` block discards it invisibly; here the discard is in the
    /// type, and the reasoning that justifies it — a
    /// `setTimeout` promise structurally cannot reject — is the comment
    /// on that `let _ =`, kept below on the constructor.
    ///
    /// `hclient-rt-smol` needs the same adapter for the same reason
    /// (`async_io::Timer` resolves to an `Instant`), which is why
    /// `Discard` lives in `hclient-core` rather than in either backend.
    ///
    /// Nothing in the old `async` block ran *after* the await, so moving
    /// the promise construction out of it is a pure re-association: the
    /// `setTimeout` call now happens when `sleep` is called rather than
    /// on the first poll. For a timer that is the more correct of the
    /// two — the delay starts when the caller asked for it, not whenever
    /// somebody first polls.
    type Sleep = Elapsed;

    fn sleep(&self, d: Duration) -> Self::Sleep {
        // The multiply does NOT saturate to `f64::INFINITY` for a
        // pathological `Duration`, and the browser does not then clamp to a
        // ~24.8-day maximum. Measured directly:
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
        // `setTimeout` (it never rejects) — discarding the `Result` here
        // isn't a silent no-op over a real error channel, it's
        // acknowledging one that structurally cannot fire. That discard is
        // now `Discard`, in the return type, rather than a `let _ =`
        // buried in an async block. `SendJsFuture`, not
        // `wasm_bindgen_futures::JsFuture`: see the module doc comment on
        // why this is what keeps this file free of a second `unsafe`.
        // The promise is built above, so `setTimeout` is already running
        // by the time this returns — the delay starts when the caller
        // asked for it, which is the property the previous shape had and
        // this one keeps. What is spawned only *waits* on it.
        let (tx, rx) = oneshot::channel();
        wasm_bindgen_futures::spawn_local(async move {
            // A `setTimeout` promise structurally cannot reject, which is
            // the reasoning the discarded `Result` has always rested on.
            let _ = SendJsFuture::new(promise).await;
            let _ = tx.send(());
        });
        Elapsed(rx)
    }

    fn now(&self) -> Self::Instant {
        js_sys::Date::now()
    }

    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        Duration::from_secs_f64((js_sys::Date::now() - earlier).max(0.0) / 1000.0)
    }
}

/// What [`BrowserClock::sleep`] hands back: a `oneshot` the browser's
/// timer fires, and **nothing JS-shaped**.
///
/// # Why this is not `Discard<SendJsFuture>` any more
///
/// It was, and that was fewer moving parts. `SendJsFuture`'s `Send` is an
/// `unsafe impl` whose argument is that `wasm32-unknown-unknown` has one
/// thread, so it is stripped under `target_feature = "atomics"` by the
/// same `cfg` that strips wasm-bindgen's own impl for `JsValue`. A
/// `Timer::Sleep` that is `Send` only without wasm threads is enough for
/// this crate on its own — but not for `hclient::Client`, whose erased
/// timer boxes the sleep as `Send`, so the browser would have lost
/// `Client` entirely the moment anybody built with threads.
///
/// `oneshot::Receiver<()>` is `Send` because `()` is, with no claim about
/// threads anywhere in it. This is the same trade `body::pump` makes one
/// module over and for the same reason: keep the JS on the thread that
/// owns it and let a plain value cross.
///
/// The cost is one `spawn_local` per sleep. It is bounded by the sleep —
/// the task awaits one promise and sends one `()` — and the timer was
/// already running before it started, so nothing about *when* the delay
/// begins changed.
#[derive(Debug)]
pub struct Elapsed(oneshot::Receiver<()>);

impl Future for Elapsed {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // `Err` is the sender dropped without sending, which means the
        // spawned task was destroyed — only possible if the whole wasm
        // instance is going away, at which point nothing is polling this
        // either. Resolving rather than hanging is the answer that cannot
        // wedge a caller.
        Pin::new(&mut self.0).poll(cx).map(|_| ())
    }
}
