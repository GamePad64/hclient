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

/// Replaces `streaming_request_body_and_full_duplex_are_false_until_the_send_path_exists`,
/// which pinned the hardcoded `false` that v0.2 W6 removed. The claim worth
/// testing is no longer the value — that varies by browser, so any literal
/// asserted here would be wrong in one of the two engines CI runs — but the
/// *derivation*: `Capabilities::streaming_request_body` must be whatever
/// `caps::supports_streaming_request_body()` answered, and that function
/// must answer the browser rather than a constant.
///
/// Both halves are checked, because either alone is vacuous. Asserting only
/// the equality would hold if both sides were frozen to the same literal —
/// which is why the mutation test below exists; asserting only the probe
/// would not notice `probe()` dropping the assignment and falling back to
/// `Capabilities::none()`'s `false`.
#[wasm_bindgen_test]
fn streaming_request_body_is_the_behavioural_probes_answer_not_a_constant() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    assert_eq!(
        c.streaming_request_body,
        http_ng_fetch::testing::supports_streaming_request_body_for_test(),
        "Capabilities::streaming_request_body must be the probe's answer, not a value that \
         merely happens to match it in this browser"
    );
}

/// The mutation test for the probe itself, and the reason it can run in
/// **both** directions in **either** browser: `web_sys::Request::
/// new_with_str_and_init` compiles to `new Request(..)` in wasm-bindgen's
/// glue, where `Request` is a free variable resolved through the scope
/// chain to `globalThis` at call time. Replacing `globalThis.Request` with
/// a stand-in therefore changes what the probe actually constructs.
///
/// The two stand-ins are the two measured browsers, reduced to the
/// behaviour the probe keys on (`docs/measurements/w6-request-streams/`):
///
/// - **Firefox 153** — never reads `duplex`, and stamps
///   `Content-Type: text/plain;charset=UTF-8` because it stringified the
///   stream to `[object ReadableStream]`. The probe must answer `false`.
/// - **Chrome 151** — reads `duplex`, invents no `Content-Type`. The probe
///   must answer `true`.
///
/// A probe hardcoded either way fails one of the two assertions. So does
/// one that consulted only `duplex` presence (`supports_duplex`), which
/// these stand-ins deliberately do not affect at all — they share the real
/// `Request.prototype`, so the cheap check answers the same thing under
/// both, and a crate wired to it would be blind to the whole difference.
#[wasm_bindgen_test]
fn the_probe_follows_the_browsers_behaviour_in_both_directions() {
    use wasm_bindgen::JsCast;

    // Returns the real constructor so each arm can restore it. Written as
    // one `new Function` body rather than several `Reflect` calls because
    // what is being installed IS a JS constructor.
    let install = |body: &str| -> js_sys::Function {
        js_sys::Function::new_no_args(body)
            .call0(&wasm_bindgen::JsValue::NULL)
            .expect("installing the stand-in constructor must not throw")
            .unchecked_into::<js_sys::Function>()
    };
    let restore = |real: &js_sys::Function| {
        js_sys::Reflect::set(
            &js_sys::global(),
            &wasm_bindgen::JsValue::from_str("Request"),
            real,
        )
        .expect("restoring the real Request must not throw");
    };

    // --- the Firefox shape: stringifies, so it never looks at `duplex` ---
    let real = install(
        r#"
        const real = globalThis.Request;
        const fake = function (url, init) {
            // Deliberately does NOT touch init.duplex — that is the point.
            const r = new real(url, { method: (init && init.method) || 'GET' });
            r.headers.set('Content-Type', 'text/plain;charset=UTF-8');
            return r;
        };
        fake.prototype = real.prototype;
        globalThis.Request = fake;
        return real;
        "#,
    );
    let saw_firefox = http_ng_fetch::testing::supports_streaming_request_body_for_test();
    restore(&real);
    assert!(
        !saw_firefox,
        "against a constructor that stringifies the stream and invents a Content-Type — the \
         measured Firefox 153 behaviour — the probe must answer false; answering true here is \
         what would put `[object ReadableStream]` on the wire in place of the caller's bytes"
    );

    // --- the Chrome shape: reads `duplex`, invents no Content-Type ---
    let real = install(
        r#"
        const real = globalThis.Request;
        const fake = function (url, init) {
            if (init) { void init.duplex; }
            const r = new real(url, { method: (init && init.method) || 'GET' });
            r.headers.delete('Content-Type');
            return r;
        };
        fake.prototype = real.prototype;
        globalThis.Request = fake;
        return real;
        "#,
    );
    let saw_chrome = http_ng_fetch::testing::supports_streaming_request_body_for_test();
    restore(&real);
    assert!(
        saw_chrome,
        "against a constructor that reads `duplex` and invents no Content-Type — the measured \
         Chrome 151 behaviour — the probe must answer true; answering false here would mean the \
         probe is a hardcoded `false` and no browser could ever stream"
    );
}

/// `full_duplex` is a separate field with a separate answer, and this test
/// exists so the two cannot be collapsed back into one literal.
///
/// It is `false` on the merits, not for want of a probe. `duplex: "half"`
/// is half duplex by name and by measurement: in the Chrome/h2 run the
/// three body chunks went out at t = 294/594/894 ms and the `fetch` promise
/// resolved only at 1206 ms — the response was not readable until the
/// request body had finished. W3's floor rule applies to this field
/// unchanged, and over-claiming it costs a caller a deadlock rather than a
/// degradation.
#[wasm_bindgen_test]
fn full_duplex_stays_false_even_where_the_request_body_streams() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    assert!(
        !c.full_duplex,
        "streaming a request body is not duplex; `duplex: \"half\"` is in the name"
    );
}

/// Pins the cheap check against the deciding one, which is the whole reason
/// `supports_duplex` still exists (see its doc comment and
/// whatwg/fetch#1470).
///
/// They agree in Chrome 151 (`true`/`true`) and Firefox 153
/// (`false`/`false`), measured 2026-08-09. This test does not assert a
/// direction — it asserts they have not diverged. The day it fails is the
/// day a browser exposes `Request.prototype.duplex` and still refuses to
/// send the stream (#1470's exact scenario), and on that day the right
/// response is to update this test and the `caps.rs` comments that call the
/// presence check "not currently wrong" — not to make the crate follow the
/// cheap one.
#[wasm_bindgen_test]
fn the_cheap_presence_check_and_the_deciding_probe_still_agree() {
    assert_eq!(
        http_ng_fetch::testing::supports_duplex_for_test(),
        http_ng_fetch::testing::supports_streaming_request_body_for_test(),
        "`'duplex' in Request.prototype` and whatwg/fetch#1470's behavioural detection have \
         diverged in this browser — the presence check has started lying, and `caps.rs`'s \
         comments saying it is 'not currently wrong' need rewriting"
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
