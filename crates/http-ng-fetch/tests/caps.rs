#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn declares_what_fetch_genuinely_cannot_do() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    // No trailers, no 1xx, no version selection — whatwg/fetch#772 proposes
    // removing the trailers API altogether.
    assert!(!c.request_trailers);
    assert!(!c.response_trailers);
    assert!(!c.informational_1xx);
    assert!(!c.version_select);
    assert!(!c.version_reported);
    // No TLS, no client certificates, no proxy.
    assert_eq!(c.tls_config, http_ng_core::TlsSupport::None);
    assert!(!c.client_certs);
    assert!(!c.proxy);
    // Cookies and cache are ambient, owned by the browser.
    assert!(c.owns_cookie_jar);
    assert!(c.owns_cache);
    // Upgrade is unreachable: WebSocket in the browser is a separate global.
    assert_eq!(c.upgrade, http_ng_core::UpgradeSupport::None);
}

#[wasm_bindgen_test]
fn only_the_connect_deadline_exists_and_it_is_one_for_everything() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    // AbortSignal is one deadline for the whole exchange. Declaring three
    // separate timeouts would be a lie.
    assert!(!c.timeouts.connect);
    assert!(!c.timeouts.first_byte);
    assert!(!c.timeouts.between_bytes);
}

#[wasm_bindgen_test]
fn forbidden_headers_are_listed_not_silently_dropped() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    let names: Vec<_> = c
        .forbidden_request_headers
        .iter()
        .map(|h| h.as_str())
        .collect();
    for must in [
        "host",
        "connection",
        "content-length",
        "cookie",
        "origin",
        "transfer-encoding",
        "te",
        "upgrade",
    ] {
        assert!(names.contains(&must), "{must} must be in the list");
    }
}

#[wasm_bindgen_test]
fn duplex_support_is_probed_not_assumed() {
    // In Chrome 131+ — true, in Firefox and Safari — false. One binary.
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    assert_eq!(
        c.streaming_request_body, c.full_duplex,
        "in fetch, duplex and streaming request bodies are the same thing"
    );
}

/// Not from the brief: `Capabilities` is `#[non_exhaustive]`, so this crate
/// cannot write the completeness check `http-ng-core`'s own
/// `Capabilities::none_is_the_conservative_base` writes (amendment C6,
/// `docs/superpowers/specs/2026-08-05-http-ng-design.md`) — any
/// destructure of `Capabilities` from outside its defining crate needs
/// `..`, and `..` silently absorbs a field added later. What CAN be
/// checked from here: name every field `probe()` does not set explicitly
/// and assert each still holds `Capabilities::none()`'s conservative
/// default. Same technique, same name, as
/// `http-ng-native/tests/transport.rs`'s
/// `undeclared_capability_fields_match_their_conservative_defaults_today`.
#[wasm_bindgen_test]
fn undeclared_capability_fields_match_their_conservative_defaults_today() {
    let http_ng_core::Capabilities {
        request_trailers,
        response_trailers,
        client_certs,
        proxy,
        version_select,
        version_reported,
        informational_1xx,
        ..
    } = http_ng_fetch::Fetch::new().capabilities_for_test();
    assert!(!request_trailers);
    assert!(!response_trailers);
    assert!(!client_certs);
    assert!(!proxy);
    assert!(!version_select);
    assert!(!version_reported);
    assert!(!informational_1xx);
}

/// Not from the brief: `duplex_support_is_probed_not_assumed` above only
/// checks internal consistency (`streaming_request_body == full_duplex`)
/// — `probe()` sets both fields from the same local `bool`, so that
/// assertion passes even if `supports_duplex()` were hardcoded to a
/// constant `true` or `false` (verified by actually making that edit
/// locally — see the task report). This test mutation-checks
/// `supports_duplex()` itself: it flips whichever way `Request.prototype`
/// actually reads in this browser, confirms the probe follows the flip in
/// both directions, and puts the prototype back before returning, since
/// `wasm-bindgen-test` runs every `#[wasm_bindgen_test]` in this file
/// against the same page and global object.
#[wasm_bindgen_test]
fn duplex_reflects_the_prototype_not_a_hardcoded_constant() {
    use wasm_bindgen::{JsCast, JsValue};

    let global = js_sys::global();
    let ctor = js_sys::Reflect::get(&global, &JsValue::from_str("Request"))
        .expect("Request must exist in a browser test target");
    let proto = js_sys::Reflect::get(&ctor, &JsValue::from_str("prototype"))
        .expect("Request.prototype must exist");
    let key = JsValue::from_str("duplex");
    let had_duplex_before =
        js_sys::Reflect::has(&proto, &key).expect("`in` on an object never throws");

    // Baseline, taken before any mutation: a probe hardcoded to a constant
    // agrees with the flip check below whenever that constant happens to
    // equal `!had_duplex_before` — which, on a browser whose *natural*
    // state is already the opposite of the hardcoded constant, is exactly
    // never (the flip always lands on `!had_duplex_before`). What a
    // hardcoded constant CANNOT do is agree with both the natural state
    // *and* the flipped state, so this assertion is what actually forces
    // both directions to be checked (found by mutation-testing this test
    // itself: a `supports_duplex()` hardcoded to `false` passed every
    // other assertion here undetected, in a Chrome where the natural state
    // is already `true` — see the task report).
    assert_eq!(
        http_ng_fetch::Fetch::new()
            .capabilities_for_test()
            .streaming_request_body,
        had_duplex_before,
        "before any mutation, probe() must match the browser's actual, unmodified Request.prototype.duplex"
    );

    if had_duplex_before {
        js_sys::Reflect::delete_property(proto.unchecked_ref::<js_sys::Object>(), &key)
            .expect("Request.prototype.duplex is expected to be a configurable property");
    } else {
        js_sys::Reflect::set(&proto, &key, &JsValue::from_str("half"))
            .expect("adding an own property to Request.prototype must succeed");
    }
    let flipped = js_sys::Reflect::has(&proto, &key).expect("`in` never throws");
    assert_eq!(
        flipped, !had_duplex_before,
        "the mutation itself must take effect before it can be tested against"
    );

    let observed = http_ng_fetch::Fetch::new().capabilities_for_test();

    // Restore before asserting: a failed assertion below must not leave
    // `Request.prototype` mutated for whichever test in this page runs next.
    if had_duplex_before {
        js_sys::Reflect::set(&proto, &key, &JsValue::from_str("half")).expect("restore failed");
    } else {
        js_sys::Reflect::delete_property(proto.unchecked_ref::<js_sys::Object>(), &key)
            .expect("restore failed");
    }
    let restored = js_sys::Reflect::has(&proto, &key).expect("`in` never throws");
    assert_eq!(
        restored, had_duplex_before,
        "restore must put the prototype back exactly as found"
    );

    assert_eq!(
        observed.streaming_request_body, !had_duplex_before,
        "probe() must follow the flipped Request.prototype.duplex, not a cached or hardcoded answer"
    );
    assert_eq!(observed.full_duplex, !had_duplex_before);
}
