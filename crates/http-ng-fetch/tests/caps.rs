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

/// Not from the brief: added when this task was reopened over a finding in
/// `streaming_request_body`'s neighborhood (see the task report's "reopened"
/// section) — auditing all sixteen fields surfaced a second, independent
/// case of the same defect class. `convert::to_web_request` never calls
/// `RequestInit::set_redirect`; fetch's default (`redirect: "follow"`)
/// governs every request unconditionally, resolved entirely inside the
/// browser before `Client`'s own redirect stage — or this crate — ever sees
/// an intermediate 3xx. `RedirectSupport::Configurable` ("we set the
/// policy") was never true here; `Internal` is `RedirectSupport`'s own name,
/// in `http-ng-core`, for exactly this shape — its doc comment names a
/// browser's `fetch()` with the default `redirect: "follow"` as the
/// motivating example, written before this crate existed to be that example.
#[wasm_bindgen_test]
fn redirects_are_internal_not_configurable() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    assert_eq!(c.redirects, http_ng_core::RedirectSupport::Internal);
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

/// Replaces the original `duplex_support_is_probed_not_assumed`, which
/// asserted `streaming_request_body == full_duplex` — true, but no longer
/// meaningful: `probe()` now sets both fields to the SAME hardcoded literal
/// (see `caps::probe`'s doc comment), so that equality would hold even if
/// the literal were flipped to `true`, or if `probe()` forgot to set
/// `full_duplex` at all and it fell back to `Capabilities::none()`'s
/// default `false` by coincidence. This test pins the actual, current
/// value instead, which is the claim that can actually go wrong: this
/// crate does not send a streaming request body yet — `convert::
/// resolve_body` rejects `RequestBody::Streaming` unconditionally — so
/// reporting `true` here, on any browser, would be exactly the capability
/// that lies this whole registry exists to prevent.
/// `supports_duplex_reflects_the_prototype_not_a_hardcoded_constant` below
/// is what still exercises the genuine, browser-varying fact
/// (`supports_duplex()` itself); this test and that one are deliberately
/// no longer the same test.
#[wasm_bindgen_test]
fn streaming_request_body_and_full_duplex_are_false_until_the_send_path_exists() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    assert!(!c.streaming_request_body);
    assert!(!c.full_duplex);
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

/// **Reopened-task rewrite.** This test used to monkey-patch
/// `Request.prototype.duplex` and check that `Capabilities::
/// streaming_request_body` followed the flip. It can't do that any more —
/// `streaming_request_body` is now a hardcoded `false` (see `caps::probe`'s
/// doc comment and `streaming_request_body_and_full_duplex_are_false_until_
/// the_send_path_exists` above), decoupled from the browser fact on
/// purpose. What's still real and still browser-varying is
/// `supports_duplex()` ITSELF (exported for this test via
/// `testing::supports_duplex_for_test`, since the underlying function stays
/// `pub(crate)`) — so this test now targets that function directly, not the
/// `Capabilities` it no longer feeds. Same mechanism as before: flip
/// whichever way `Request.prototype` actually reads in this browser,
/// confirm the raw probe follows the flip in both directions (checking the
/// PRE-mutation baseline too, not only the flipped state — a probe
/// hardcoded to a constant can agree with an arbitrary single flipped
/// state by coincidence, but not with both the natural state and its
/// opposite; this exact gap was found by mutation-testing this test's own
/// first draft, see the task report), and restore the prototype before
/// returning, since `wasm-bindgen-test` runs every `#[wasm_bindgen_test]`
/// in this file against the same page and global object.
#[wasm_bindgen_test]
fn supports_duplex_reflects_the_prototype_not_a_hardcoded_constant() {
    use wasm_bindgen::{JsCast, JsValue};

    let global = js_sys::global();
    let ctor = js_sys::Reflect::get(&global, &JsValue::from_str("Request"))
        .expect("Request must exist in a browser test target");
    let proto = js_sys::Reflect::get(&ctor, &JsValue::from_str("prototype"))
        .expect("Request.prototype must exist");
    let key = JsValue::from_str("duplex");
    let had_duplex_before =
        js_sys::Reflect::has(&proto, &key).expect("`in` on an object never throws");
    // The ORIGINAL descriptor, not merely "was it there". In Chrome
    // `Request.prototype.duplex` is an accessor; deleting it and putting a
    // plain string back would satisfy `Reflect::has` while leaving the realm
    // subtly different for whichever test runs next in this shared page.
    let original_descriptor =
        js_sys::Object::get_own_property_descriptor(proto.unchecked_ref::<js_sys::Object>(), &key);

    // Baseline, taken before any mutation — see this test's own doc comment
    // for why the baseline check, not only the post-flip one, is what
    // actually rules out a hardcoded constant.
    assert_eq!(
        http_ng_fetch::testing::supports_duplex_for_test(),
        had_duplex_before,
        "before any mutation, supports_duplex() must match the browser's actual, unmodified \
         Request.prototype.duplex"
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

    let observed = http_ng_fetch::testing::supports_duplex_for_test();

    // Restore before asserting: a failed assertion below must not leave
    // `Request.prototype` mutated for whichever test in this page runs next.
    // Restoring the captured descriptor puts back an accessor as an accessor,
    // rather than substituting a data property that merely answers `in`.
    if had_duplex_before {
        js_sys::Object::define_property(
            proto.unchecked_ref::<js_sys::Object>(),
            &key,
            original_descriptor.unchecked_ref::<js_sys::Object>(),
        );
    } else {
        js_sys::Reflect::delete_property(proto.unchecked_ref::<js_sys::Object>(), &key)
            .expect("restore failed");
    }
    let restored = js_sys::Reflect::has(&proto, &key).expect("`in` never throws");
    assert_eq!(
        restored, had_duplex_before,
        "restore must put the prototype back as found"
    );
    if had_duplex_before {
        let now = js_sys::Object::get_own_property_descriptor(
            proto.unchecked_ref::<js_sys::Object>(),
            &key,
        );
        let was_accessor = !js_sys::Reflect::get(&original_descriptor, &JsValue::from_str("get"))
            .expect("descriptor lookup never throws")
            .is_undefined();
        let is_accessor = !js_sys::Reflect::get(&now, &JsValue::from_str("get"))
            .expect("descriptor lookup never throws")
            .is_undefined();
        assert_eq!(
            was_accessor, is_accessor,
            "restore must put back the same KIND of property — an accessor replaced by a data \
             property answers `in` identically and still leaves the realm changed"
        );
    }

    assert_eq!(
        observed, !had_duplex_before,
        "supports_duplex() must follow the flipped Request.prototype.duplex, not a cached or \
         hardcoded answer"
    );
}
