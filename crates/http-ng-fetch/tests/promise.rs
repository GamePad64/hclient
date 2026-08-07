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
