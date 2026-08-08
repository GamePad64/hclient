#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn resolves_a_promise() {
    let p = js_sys::Promise::resolve(&wasm_bindgen::JsValue::from_str("ok"));
    let v = http_ng_fetch::testing::send_js_future(p).await.unwrap();
    assert_eq!(v.as_string().as_deref(), Some("ok"));
}

#[wasm_bindgen_test]
async fn propagates_rejection() {
    let p = js_sys::Promise::reject(&wasm_bindgen::JsValue::from_str("nope"));
    let e = http_ng_fetch::testing::send_js_future(p).await.unwrap_err();
    assert_eq!(e.as_string().as_deref(), Some("nope"));
}

#[wasm_bindgen_test]
fn future_is_send_on_the_default_target() {
    // The main claim: `!Send` is a property of building with wasm threads,
    // not of the browser. Without `+atomics`, everything is Send.
    fn assert_send<T: Send>() {}
    assert_send::<http_ng_fetch::testing::SendJsFutureAlias>();
}

/// Fix round 1, finding 2. Before this fix, `SendJsFuture` held its two
/// `Closure`s as a sibling field, dropped as soon as the future itself was
/// dropped — including while the underlying promise was still pending. A
/// `Closure` invoked by JS after being dropped throws
/// (`ScopedClosure::drop` invalidates the JS-side function first), and
/// since `SendJsFuture::new` discards the promise `.then2()` returns
/// (`let _ = promise.then2(...)`), nothing observed that throw — it
/// surfaced only as a browser-level unhandled promise rejection, later,
/// whenever the already-pending promise finally settled.
///
/// A first version of this test tried to observe that directly, via a
/// real global `unhandledrejection` listener. Abandoned after it produced
/// a demonstrated false negative AND a false positive in the same run:
/// `wasm-bindgen-test`'s browser runner executes every `#[wasm_bindgen_test]`
/// in one shared page/JS realm, back to back, and `unhandledrejection`'s
/// dispatch turned out not to be reliably bounded within one test's own
/// microtask-plus-macrotask window — a deliberately-unhandled rejection
/// created in one test to sanity-check the listener was still unobserved
/// at that test's own check, then bled into a LATER test's listener
/// window and was misattributed there. Real, but not deterministic enough
/// to trust as a regression test.
///
/// This version instead tests the actual retention mechanism directly:
/// `SendJsFuture::downgrade_state` (exposed for exactly this) gives a
/// `Weak` handle to the same `Arc` the callbacks hold their own clones of.
/// If dropping the future leaves the callbacks (and hence the `Arc`) still
/// alive — provable with `Weak::upgrade`, synchronously, no promise
/// scheduling or browser events involved — then whichever callback later
/// fires is guaranteed to still be a live, callable `Closure` when it
/// does.
#[wasm_bindgen_test]
fn dropping_a_pending_future_does_not_drop_its_still_needed_callbacks() {
    // Never settled during this test — the callbacks stay registered
    // throughout, which is exactly the condition the historical bug needed.
    let promise = js_sys::Promise::new(&mut |_resolve, _reject| {});
    let survived =
        http_ng_fetch::testing::callbacks_survive_dropping_the_future_while_pending(promise);
    assert!(
        survived,
        "dropping a pending SendJsFuture dropped its still-registered callbacks with it"
    );
}

/// Fix round 1, finding 3. Before this fix, polling `SendJsFuture` again
/// after it had already returned `Poll::Ready` silently returned
/// `Poll::Pending` forever — `result` had already been `take()`n by the
/// first `Ready` and nothing ever refilled it. That's a silent hang, out
/// of `Future`'s own contract; this test drives the future to completion
/// once (the normal way), then polls it a second time and requires a loud
/// panic instead.
#[wasm_bindgen_test]
#[should_panic]
async fn polling_after_ready_panics_loudly_instead_of_hanging_silently() {
    use std::future::Future;

    let p = js_sys::Promise::resolve(&wasm_bindgen::JsValue::from_str("ok"));
    let mut fut = Box::pin(http_ng_fetch::testing::send_js_future(p));
    fut.as_mut().await.unwrap();
    let _ = std::future::poll_fn(|cx| fut.as_mut().poll(cx)).await;
}
