use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use wasm_bindgen::prelude::*;

/// The same technique wasm-bindgen applies to `JsValue` itself.
///
/// `JsValue` is an index into a table owned by the generated JS glue. As
/// long as the module is built **without** `target_feature = "atomics"`,
/// there's one instance, one table, no threads — and upstream itself
/// declares `unsafe impl Send for JsValue` under the same `cfg`
/// (`wasm-bindgen-0.2.126/src/lib.rs:173-176`). With `+atomics` each worker
/// gets its own table, and the compiler correctly rejects us (verified:
/// `RUSTFLAGS="-Ctarget-feature=+atomics,+bulk-memory" cargo +nightly check
/// -p http-ng-fetch --target wasm32-unknown-unknown
/// -Zbuild-std=std,panic_abort --tests` fails (see spec amendment-C7 for
/// the exact diagnostic and why `--tests` is required to see it at all —
/// the lib target alone compiles under `+atomics` too, since nothing in it
/// alone demands `Send`).
#[repr(transparent)]
pub(crate) struct SingleThreaded<T>(pub(crate) T);

/// The one exception to this project's `#![forbid(unsafe_code)]` default —
/// see `docs/superpowers/specs/2026-08-05-http-ng-design.md` amendment C7
/// for the full argument. Both lines the CI `no-unsafe-code` job would
/// otherwise flag carry their own `unsafe-code-exception` marker, per-line,
/// the same convention `no-declared-send` uses for `send-bound-exception`.
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C7
    reason = "mirrors wasm_bindgen::JsValue: without wasm threads the process is single-threaded by construction"
)]
#[cfg(not(target_feature = "atomics"))]
unsafe impl<T> Send for SingleThreaded<T> {} // unsafe-code-exception: amendment-C7

#[derive(Default)]
pub(crate) struct State {
    result: Option<Result<JsValue, JsValue>>,
    waker: Option<Waker>,
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
    _keepalive: SingleThreaded<ClosurePair>,
}

/// Factored out per `clippy::type_complexity` (checks are fixed, not
/// silenced — a named alias, not an `#[allow]`).
type ClosurePair = (Closure<dyn FnMut(JsValue)>, Closure<dyn FnMut(JsValue)>);

// The closures inside `_keepalive` don't implement `Debug` (verified:
// `wasm_bindgen::closure::Closure<T>` has no `Debug` impl), so this can't
// be derived — same reason `NativeBody` (`http-ng-native/src/h1.rs`) writes
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
            .finish()
    }
}

impl SendJsFuture {
    pub(crate) fn new(promise: js_sys::Promise) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let make = |ok: bool| {
            let state = state.clone();
            Closure::wrap(Box::new(move |v: JsValue| {
                let mut s = state.lock().expect("promise state poisoned");
                s.result = Some(if ok { Ok(v) } else { Err(v) });
                if let Some(w) = s.waker.take() {
                    w.wake();
                }
            }) as Box<dyn FnMut(JsValue)>)
        };
        let (on_ok, on_err) = (make(true), make(false));
        let _ = promise.then2(&on_ok, &on_err);
        Self {
            state,
            _keepalive: SingleThreaded((on_ok, on_err)),
        }
    }

    /// A weak handle to the shared state, for `tests/promise.rs` to prove
    /// that dropping `SendJsFuture` while its promise is still pending does
    /// NOT drop the callbacks registered against it: if `Weak::upgrade`
    /// still succeeds after the future itself is gone, only the callbacks'
    /// own clones of the `Arc` can be keeping it alive.
    ///
    /// Not `#[cfg(test)]`: `tests/promise.rs` is an integration test, a
    /// separate crate linking the ordinary build of this library, so
    /// `cfg(test)` would remove it for exactly the caller that needs it.
    pub(crate) fn downgrade_state(&self) -> std::sync::Weak<Mutex<State>> {
        Arc::downgrade(&self.state)
    }
}

impl Future for SendJsFuture {
    type Output = Result<JsValue, JsValue>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut s = self.state.lock().expect("promise state poisoned");
        match s.result.take() {
            Some(r) => Poll::Ready(r),
            None => {
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
