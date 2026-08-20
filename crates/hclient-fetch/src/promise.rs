use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use wasm_bindgen::prelude::*;

/// The underlying REASONING mirrors what wasm-bindgen does for `JsValue`
/// itself — but not the same SCOPE, and that distinction is load-bearing
/// for an `unsafe impl`, so it's spelled out precisely rather than
/// gestured at.
///
/// **What's actually shipped, and verified against the real source.**
/// `JsValue` is an index into a table owned by the generated JS glue.
/// Without `target_feature = "atomics"` there's one instance, one table,
/// no threads, and upstream itself declares this unconditionally, in
/// every released version this crate depends on:
///
/// ```text
/// // wasm-bindgen-0.2.126/src/lib.rs:173-176
/// #[cfg(not(target_feature = "atomics"))]
/// unsafe impl Send for JsValue {}
/// #[cfg(not(target_feature = "atomics"))]
/// unsafe impl Sync for JsValue {}
/// ```
///
/// **What is NOT shipped, checked directly against upstream rather than
/// inherited from a review comment.** No released `wasm-bindgen` gives
/// `Closure<T>` or `JsFuture` a `Send`/`Sync` impl under any `cfg`. The
/// only place upstream does this at all is an UNMERGED branch,
/// `unsafe-send-sync` on `wasm-bindgen/wasm-bindgen`
/// (commits `a7e0c944`, 2025-10-30, and `0c0a8a8e`, 2025-10-31 — fetched
/// and read directly, not taken on faith), which adds exactly:
///
/// ```text
/// #[cfg(unsafe_single_threaded_traits)]
/// unsafe impl Send for JsValue {}      // + Sync
/// #[cfg(unsafe_single_threaded_traits)]
/// unsafe impl<T: ?Sized> Send for Closure<T> {}  // + Sync
/// #[cfg(unsafe_single_threaded_traits)]
/// unsafe impl Send for JsFuture {}     // + Sync
/// ```
///
/// gated behind an explicit OPT-IN cfg, `unsafe_single_threaded_traits`
/// (`RUSTFLAGS="--cfg unsafe_single_threaded_traits"`) — deliberately
/// **not** automatic under `!atomics` the way the shipped `JsValue` impl
/// is, and the branch adds its own `compile_error!` if that cfg is
/// combined with `target_feature = "atomics"`, i.e. upstream converged on
/// the same underlying argument this type makes (no atomics implies no
/// threads implies one table) without ever shipping it unconditionally
/// for `Closure`/`JsFuture` the way it does for `JsValue`.
///
/// **What this type actually is, stated plainly.** `SingleThreaded<T>`
/// applies that same argument — `!atomics` implies one table, no threads —
/// to our own pair of `Closure`s, unconditionally under
/// `#[cfg(not(target_feature = "atomics"))]`, exactly like the shipped
/// `JsValue` impl and unlike anything shipped for `Closure` specifically.
/// This is a claim we are making ourselves, on the same evidence upstream
/// already accepted for `JsValue` and an unmerged branch accepted for
/// `Closure`/`JsFuture` too (under a stricter opt-in) — not something
/// copied from a released, audited surface. The compiler is still the
/// backstop: with `+atomics`, the `cfg` strips the impl and rejects
/// `SendJsFuture: Send` on its own (verified:
/// `RUSTFLAGS="-Ctarget-feature=+atomics,+bulk-memory" cargo +nightly
/// check -p hclient-fetch --target wasm32-unknown-unknown
/// -Zbuild-std=std,panic_abort --tests` fails — `--tests` is required to
/// see it at all, since the lib target alone never demands `Send`; see
/// spec amendment-C7 for the exact diagnostic and the full argument).
#[repr(transparent)]
pub(crate) struct SingleThreaded<T>(pub(crate) T);

/// The one exception to this project's `#![forbid(unsafe_code)]` default —
/// see `docs/superpowers/specs/2026-08-05-hclient-design.md` amendment C7
/// for the full argument. Both lines the CI `no-unsafe-code` job would
/// otherwise flag carry their own `unsafe-code-exception` marker, per-line
/// AND scoped to this one file — the same convention `no-declared-send`
/// uses for `send-bound-exception`, narrowed further because, unlike that
/// marker, this one has exactly one legitimate location in the whole
/// project.
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C7
    reason = "mirrors wasm_bindgen::JsValue: without wasm threads the process is single-threaded by construction"
)]
#[cfg(not(target_feature = "atomics"))]
unsafe impl<T> Send for SingleThreaded<T> {} // unsafe-code-exception: amendment-C7

type ClosurePair = (Closure<dyn FnMut(JsValue)>, Closure<dyn FnMut(JsValue)>);

/// `pub(crate)`, not module-private: `SendJsFuture::downgrade_state`
/// (below) returns `Weak<Mutex<State>>`, and that function is `pub(crate)`
/// itself, called from `lib.rs`'s `testing` module — a sibling module
/// needs `State` at least `pub(crate)` to be able to hold a value of that
/// type at all. Diagnosed and applied by `v3-impl-task-3` mid fix-round-1,
/// not by this file's own author: `downgrade_state` was added here while
/// `State` was still module-private, which is `E0446` and broke every
/// other crate in `hclient-fetch` — including `caps.rs`/`convert.rs`,
/// which have nothing to do with this file. Fixed with the minimal
/// one-line widening, on the correct diagnosis, and folded into this
/// file's own history rather than left as a silent fix in someone else's
/// commit.
#[derive(Default)]
pub(crate) struct State {
    result: Option<Result<JsValue, JsValue>>,
    waker: Option<Waker>,
    /// Keeps both callbacks alive from `SendJsFuture::new` until the
    /// promise actually settles — mirrors js-sys 0.3.103's `JsFuture`
    /// (`futures/mod.rs:101-108` `Inner::callbacks`, `:159-211`
    /// `finish`/`From<Promise<T>>`), and for the identical reason: a
    /// `Closure` dropped BEFORE the promise it's registered on settles
    /// throws when JS later invokes it (`ScopedClosure::drop` invalidates
    /// the JS-side function first), and since `SendJsFuture::new` discards
    /// the promise `.then2()` returns, nothing here would observe that as
    /// anything but a silent, unhandled rejection in the browser console.
    ///
    /// Storing the callbacks here, alongside `result`/`waker`, behind the
    /// same `Arc`, and having whichever callback fires explicitly drop
    /// BOTH (`SendJsFuture::new`'s `finish`, below) makes "the callback
    /// that's about to run is still alive when it runs, and is dropped
    /// only after" true unconditionally — including when `SendJsFuture`
    /// itself was already dropped, since each callback holds its own
    /// clone of the `Arc` and therefore keeps this `State` (and hence
    /// itself) alive independently of the future handle. Dropping the
    /// SIBLING callback (the one that will now never fire) from the same
    /// call is sound because a JS `Promise` invokes at most one of
    /// resolve/reject, ever — the same guarantee upstream's own comment
    /// on `From<Promise<T>>` relies on for the identical trick.
    callbacks: Option<SingleThreaded<ClosurePair>>,
}

/// A `Send`-compatible replacement for `wasm_bindgen_futures::JsFuture`.
///
/// `JsFuture` holds an `Rc<RefCell<Inner<T>>>` inside
/// (`js-sys-0.3.103/src/futures/mod.rs:118-119` — as of `wasm-bindgen-futures`
/// 0.4.76 that crate is a thin re-export shim over `js_sys::futures`, so
/// `JsFuture` itself now lives in `js-sys`, which is already this crate's
/// dependency) and is therefore `!Send` — but that's an implementation
/// choice, not a platform property: `JsValue` itself, `js_sys::Promise`, and
/// `web_sys::{Request, Response, ReadableStream}` **are** `Send` on the
/// default target.
///
/// `pub`, not `pub(crate)`: the module `promise` stays private (see
/// `lib.rs`), and the only path to this type from outside the crate is the
/// explicit re-export at `testing::SendJsFutureAlias` — `pub(crate)` here
/// would make that re-export `E0365` (private type re-exported through a
/// public module), since `tests/promise.rs` is compiled as a separate,
/// external crate and can only see items that are actually `pub` all the
/// way through. The type is still not part of the advertised public API:
/// nothing outside `testing` names it, and `testing` itself is
/// `#[doc(hidden)]`.
pub struct SendJsFuture {
    state: Arc<Mutex<State>>,
    /// `Future::poll` must never be called again after it has returned
    /// `Ready` (the trait's own contract). Before this flag existed,
    /// polling again silently returned `Pending` forever — `result` had
    /// already been `take()`n by the first `Ready` and nothing ever
    /// refills it — exactly the "quiet hang, no test name, no message"
    /// failure mode this project has spent two verticals removing (most
    /// recently the F1 watchdog rewrite in vertical 2). This flag turns
    /// that into an immediate, loud panic naming the type instead.
    completed: bool,
}

// The closures inside `State::callbacks` don't implement `Debug` (verified:
// `wasm_bindgen::closure::Closure<T>` has no `Debug` impl), so this can't
// be derived — same reason `NativeBody` (`hclient-native/src/h1.rs`) writes
// its `Debug` by hand rather than deriving it.
impl std::fmt::Debug for SendJsFuture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ready = self
            .state
            .lock()
            .map(|s| s.result.is_some())
            .unwrap_or(false);
        f.debug_struct("SendJsFuture")
            .field("ready", &ready)
            .field("completed", &self.completed)
            .finish()
    }
}

impl SendJsFuture {
    pub(crate) fn new(promise: js_sys::Promise) -> Self {
        let state: Arc<Mutex<State>> = Arc::new(Mutex::new(State::default()));

        // Runs inside whichever of `on_ok`/`on_err` actually fires. Drops
        // BOTH callbacks from within the one that's executing: by this
        // point the JS-side glue has already extracted this Rust closure's
        // body to run it, so freeing the `Closure`'s own bookkeeping here
        // is the same trick js-sys 0.3.103's `JsFuture::from` uses in its
        // own `finish` (`futures/mod.rs:165-188`), verified against that
        // source directly.
        fn finish(state: &Mutex<State>, result: Result<JsValue, JsValue>) {
            let waker = {
                let mut s = state.lock().expect("promise state poisoned");
                s.callbacks = None;
                s.result = Some(result);
                s.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        }

        let make = |ok: bool| {
            let state = state.clone();
            Closure::once(move |v: JsValue| {
                finish(&state, if ok { Ok(v) } else { Err(v) });
            })
        };
        let (on_ok, on_err) = (make(true), make(false));
        let _ = promise.then2(&on_ok, &on_err);
        state.lock().expect("promise state poisoned").callbacks =
            Some(SingleThreaded((on_ok, on_err)));

        Self {
            state,
            completed: false,
        }
    }

    /// Exists solely for `tests/promise.rs` (fix round 1, finding 2), to
    /// prove — deterministically, with no dependence on real `Promise` or
    /// browser-event timing — that dropping `SendJsFuture` while its
    /// promise is still pending does NOT drop the callbacks still
    /// registered against it. A weak handle to the same shared state,
    /// checked for survival strictly AFTER the `SendJsFuture` itself
    /// (this handle's only other owner from the test's point of view) has
    /// been dropped: if `Weak::upgrade` still succeeds at that point, only
    /// the callbacks' own clones of the `Arc` can be keeping it alive,
    /// which is exactly the property being fixed for.
    ///
    /// Not `#[cfg(test)]`: `tests/promise.rs` is a separate, external
    /// crate (an integration test), which links against the ordinary,
    /// non-`cfg(test)` build of this library — `cfg(test)` here would
    /// vanish for exactly the target that needs to call it. Reachable
    /// only through `testing::downgrade_state`, same as `SendJsFuture`
    /// itself.
    pub(crate) fn downgrade_state(&self) -> std::sync::Weak<Mutex<State>> {
        Arc::downgrade(&self.state)
    }
}

impl Future for SendJsFuture {
    type Output = Result<JsValue, JsValue>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        assert!(
            !this.completed,
            "SendJsFuture polled again after already returning Poll::Ready — \
             violates the Future contract"
        );
        let mut s = this.state.lock().expect("promise state poisoned");
        match s.result.take() {
            Some(r) => {
                this.completed = true;
                Poll::Ready(r)
            }
            None => {
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
