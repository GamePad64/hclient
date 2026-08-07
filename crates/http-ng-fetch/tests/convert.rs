#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use bytes::Bytes;
use http_ng_core::RequestBody;
use std::pin::Pin;
use std::task::{Context, Poll};

// ---------------------------------------------------------------------
// Brief's own three tests, verbatim (task-3-brief.md, Step 1).
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn rejects_a_forbidden_header_instead_of_dropping_it() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("host", "evil.example")
        .body(RequestBody::Empty)
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("host"), "{err}");
}

#[wasm_bindgen_test]
fn ordinary_headers_pass_through() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("x-custom", "v")
        .body(RequestBody::Empty)
        .unwrap();
    assert!(http_ng_fetch::testing::to_web_request(&f, req).is_ok());
}

#[wasm_bindgen_test]
fn streaming_body_is_rejected_where_duplex_is_absent() {
    let f = http_ng_fetch::Fetch::new();
    if f.capabilities_for_test().streaming_request_body {
        return; // supported in Chrome — nothing to check
    }
    let req = http::Request::builder()
        .uri("https://example.com/")
        .body(RequestBody::rewindable(|| RequestBody::Empty))
        .unwrap();
    // Rewindable is bufferable, so it passes; Streaming does not.
    assert!(http_ng_fetch::testing::to_web_request(&f, req).is_ok());
}

// ---------------------------------------------------------------------
// Test fixtures: minimal `http_body::Body` impls, the same shape as
// `http-ng-core/src/body.rs`'s own `EmptyStream` test fixture.
// ---------------------------------------------------------------------

/// Never produces a frame — enough to exist as `RequestBody::Streaming`'s
/// payload for tests that never actually poll it (this task's conversion
/// rejects `Streaming` before ever touching the inner stream).
struct NeverPolled;
impl http_body::Body for NeverPolled {
    type Data = Bytes;
    type Error = http_ng_core::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        Poll::Ready(None)
    }
}

fn streaming_body() -> RequestBody {
    RequestBody::Streaming(Box::new(NeverPolled))
}

// ---------------------------------------------------------------------
// `RequestBody::Streaming` is always a typed error in this task, and
// this crate never trusts the `duplex` probe enough to try anyway (module
// doc comment on `src/convert.rs`, point 3). Tested against a caller-
// supplied `Capabilities` — `to_web_request_with_caps` — rather than
// `Fetch::new()`'s real probe, because in THIS environment (headless
// Chrome) the real probe only ever produces `true`: a test gated on the
// real probe (like the brief's own `streaming_body_is_rejected_where_
// duplex_is_absent` above) would silently skip the one browser this suite
// actually runs in.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn streaming_body_is_rejected_even_when_duplex_is_claimed_supported() {
    let mut caps = http_ng_core::Capabilities::none();
    caps.streaming_request_body = true;
    caps.forbidden_request_headers = &http_ng_fetch::FORBIDDEN_HEADERS;
    let req = http::Request::builder()
        .uri("https://example.com/")
        .body(streaming_body())
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request_with_caps(req, &caps).unwrap_err();
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("streaming_request_body"), "{err}");
}

#[wasm_bindgen_test]
fn streaming_body_is_rejected_when_duplex_is_claimed_unsupported() {
    let mut caps = http_ng_core::Capabilities::none();
    caps.streaming_request_body = false;
    caps.forbidden_request_headers = &http_ng_fetch::FORBIDDEN_HEADERS;
    let req = http::Request::builder()
        .uri("https://example.com/")
        .body(streaming_body())
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request_with_caps(req, &caps).unwrap_err();
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
}

// ---------------------------------------------------------------------
// `RequestBody::Rewindable` is unwrapped through the SAME path as any
// other body, recursively — not a partial match that only understands
// `Full` and silently drops everything else (vertical 2's native `body.rs`
// defect, named explicitly in this task's brief).
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn rewindable_wrapping_full_is_bufferable() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(|| {
            RequestBody::Full(Bytes::from_static(b"hello"))
        }))
        .unwrap();
    assert!(http_ng_fetch::testing::to_web_request(&f, req).is_ok());
}

/// The mutation this test exists to catch: reverting `resolve_body`'s
/// `RequestBody::Rewindable(f) => body = f()` arm plus its `Streaming`
/// arm back to the brief's own reference shape —
/// `RequestBody::Rewindable(f) => match f() { RequestBody::Full(b) => ..,
/// _ => {} }` — makes a `Rewindable` wrapping `Streaming` silently resolve
/// to an empty, successfully-sent body instead of a typed error. That is
/// exactly the defect vertical 2's native `body.rs` shipped and review
/// caught; this test fails (`Ok` where it must be `Err`) under that
/// reversion and only that reversion — verified in the report's mutation
/// section.
#[wasm_bindgen_test]
fn rewindable_wrapping_streaming_is_rejected_not_silently_emptied() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(streaming_body))
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
}

/// Deliberately checks the actual bytes, not just `.is_ok()`: an `is_ok()`-
/// only assertion would still pass under the exact mutation this test
/// exists to catch (a partial match on the `Rewindable` arm that silently
/// resolves anything but `Full` to an empty body) — an empty `POST` body is
/// itself a legal, successful conversion, so a weaker assertion here would
/// be vacuous against precisely the defect the module doc comment names.
#[wasm_bindgen_test]
async fn nested_rewindable_resolves_through_every_level() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(|| {
            RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"deep")))
        }))
        .unwrap();
    let (request, _controller) = http_ng_fetch::testing::to_web_request(&f, req).unwrap();
    let text_promise = request.text().expect("text() must not throw");
    let text = http_ng_fetch::testing::send_js_future(text_promise)
        .await
        .expect("reading the body back must not reject");
    assert_eq!(text.as_string().as_deref(), Some("deep"));
}

#[wasm_bindgen_test]
fn a_factory_that_never_bottoms_out_is_a_bounded_error_not_a_hang() {
    let f = http_ng_fetch::Fetch::new();
    fn infinite() -> RequestBody {
        RequestBody::rewindable(infinite)
    }
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(infinite))
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    // Not `Unsupported`: this isn't fetch declining the request, it's this
    // conversion refusing to keep unwrapping — the same category `http-ng-
    // wasi`'s analogous `RewindTooDeep` uses.
    assert!(
        !matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
}

/// Builds a `RequestBody::Rewindable` chain exactly `depth` `Rewindable`
/// layers deep, terminating in a `Full` carrying `payload`. `depth == 0`
/// is `payload` itself, with no `Rewindable` wrapper at all.
fn nested_rewindable(depth: u8, payload: &'static [u8]) -> RequestBody {
    if depth == 0 {
        RequestBody::Full(Bytes::from_static(payload))
    } else {
        RequestBody::rewindable(move || nested_rewindable(depth - 1, payload))
    }
}

/// The exact boundary `MAX_REWIND_DEPTH`'s doc comment names: 15 levels of
/// `Rewindable` nesting is the practical ceiling (one below the constant
/// itself, since the constant counts loop iterations, not successfully
/// unwrapped layers — see `MAX_REWIND_DEPTH`'s doc comment). Checks actual
/// bytes, not `.is_ok()`, for the same reason `nested_rewindable_resolves_
/// through_every_level` does: an empty body would also be a "successful"
/// `POST`, so a weak assertion here wouldn't prove the 15th layer was
/// actually reached rather than silently given up on.
#[wasm_bindgen_test]
async fn rewindable_nested_at_the_practical_ceiling_still_resolves() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(nested_rewindable(15, b"fifteen-deep"))
        .unwrap();
    let (request, _controller) = http_ng_fetch::testing::to_web_request(&f, req).unwrap();
    let text_promise = request.text().expect("text() must not throw");
    let text = http_ng_fetch::testing::send_js_future(text_promise)
        .await
        .expect("reading the body back must not reject");
    assert_eq!(text.as_string().as_deref(), Some("fifteen-deep"));
}

/// One layer past the ceiling above: `RewindTooDeep`, not a hang and not a
/// silently emptied body.
#[wasm_bindgen_test]
fn rewindable_nested_one_past_the_ceiling_is_a_typed_error() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(nested_rewindable(16, b"sixteen-deep"))
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        !matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
}

// ---------------------------------------------------------------------
// GET/HEAD cannot carry a body — fetch throws a TypeError for this;
// caught ahead of time as a typed error instead of an opaque one.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn empty_body_is_fine_on_get() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("GET")
        .uri("https://example.com/")
        .body(RequestBody::Empty)
        .unwrap();
    assert!(http_ng_fetch::testing::to_web_request(&f, req).is_ok());
}

#[wasm_bindgen_test]
fn nonempty_body_on_get_is_rejected_not_silently_sent() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("GET")
        .uri("https://example.com/")
        .body(RequestBody::Full(Bytes::from_static(b"nope")))
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("GET"), "{err}");
}

#[wasm_bindgen_test]
fn nonempty_body_on_head_is_rejected_not_silently_sent() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("HEAD")
        .uri("https://example.com/")
        .body(RequestBody::Full(Bytes::from_static(b"nope")))
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    // Not just `.is_err()`: the browser's own `Request` constructor throws
    // for GET/HEAD-with-body too, so a bare `.is_err()` check would still
    // pass with our own ahead-of-time check removed — it just wouldn't be
    // `Unsupported` anymore, only the opaque `Other` `js_err` produces.
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("HEAD"), "{err}");
}

// ---------------------------------------------------------------------
// The URL itself.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn non_http_scheme_is_a_typed_unsupported_error_not_opaque() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("ftp://example.com/")
        .body(RequestBody::Empty)
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("ftp"), "{err}");
}

/// `checked_url` only checks the scheme, not the authority separately —
/// `http::Uri` structurally can't represent an `http`/`https` scheme
/// without one (see `checked_url`'s doc comment) — so a relative-form URI
/// like this one is caught by the same "no scheme" branch `non_http_scheme_
/// is_a_typed_unsupported_error_not_opaque` exercises differently, not a
/// separate "no authority" branch. Named for what it actually checks:
/// a schemeless / relative URI can't reach the browser at all.
#[wasm_bindgen_test]
fn a_relative_uri_is_a_typed_unsupported_error_not_opaque() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("/relative")
        .body(RequestBody::Empty)
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
}

// ---------------------------------------------------------------------
// Header values that can't be represented as a JS string.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn non_ascii_header_value_is_a_typed_unsupported_error_not_opaque() {
    let f = http_ng_fetch::Fetch::new();
    let value = http::HeaderValue::from_bytes(&[0xff, 0xfe])
        .expect("obs-text is a legal byte range for HeaderValue");
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("x-binary", value)
        .body(RequestBody::Empty)
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("x-binary"), "{err}");
}

// ---------------------------------------------------------------------
// The forbidden-header message names the actual offending header, not a
// hardcoded one — catches a mutation where every rejection prints the
// same fixed string regardless of which header actually tripped it.
// ---------------------------------------------------------------------

/// Distinguishes the fast, ahead-of-time `check_headers` rejection from the
/// slower, after-construction `verify_headers_survived` safety net (see the
/// module doc comment on `src/convert.rs`) — both produce
/// `ErrorKind::Unsupported` mentioning `host`, since the browser silently
/// drops `Host` at construction time too regardless of whether our own
/// fixed list already caught it, so a test that only checks the error kind
/// or that the header's name appears in the message cannot tell whether
/// `check_headers` ever actually ran. This one can: the two paths produce
/// different `Display` text (`"fetch forbids setting"` vs `"silently
/// dropped"`), so asserting on the fast path's own wording is what proves
/// `check_headers` — not just its slower backstop — is really wired in.
#[wasm_bindgen_test]
fn forbidden_header_is_caught_by_the_fast_check_not_only_the_safety_net() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("host", "evil.example")
        .body(RequestBody::Empty)
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        err.to_string().contains("fetch forbids setting"),
        "expected the fast check_headers rejection wording, got: {err}"
    );
}

#[wasm_bindgen_test]
fn forbidden_header_rejection_names_the_actual_header() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("cookie", "a=b")
        .body(RequestBody::Empty)
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(err.to_string().contains("cookie"), "{err}");
    assert!(!err.to_string().contains("host"), "{err}");
}

// ---------------------------------------------------------------------
// `FORBIDDEN_HEADERS` is a verified subset, not the whole predicate
// (Task 2's own doc comment: it structurally cannot express fetch's
// `Sec-*`/`Proxy-*` prefix rule). A header outside that fixed list still
// gets silently dropped by the browser when building the `Request` — this
// is the exact "capability that lies" scenario this task's dispatch
// singled out, and it must not be allowed to succeed quietly.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn a_header_outside_the_fixed_list_is_still_caught_not_silently_dropped() {
    let f = http_ng_fetch::Fetch::new();
    // Not in `FORBIDDEN_HEADERS` (a fixed 14-entry array — see its doc
    // comment) — `check_headers` cannot catch this ahead of time. It's
    // still forbidden by the Fetch Standard's `Sec-*` prefix rule, which a
    // fixed array cannot express, so the browser drops it silently when
    // building the `Request` and `verify_headers_survived` must be what
    // catches it.
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("sec-test-header", "1")
        .body(RequestBody::Empty)
        .unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("sec-test-header"), "{err}");
}

// ---------------------------------------------------------------------
// A successful conversion actually carries the request through faithfully
// — method, URL, headers, and body bytes, not just "didn't error".
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
async fn successful_conversion_carries_method_url_headers_and_body() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/path?q=1")
        .header("x-custom", "value")
        .body(RequestBody::Full(Bytes::from_static(b"payload")))
        .unwrap();
    let (request, _controller) = http_ng_fetch::testing::to_web_request(&f, req).unwrap();

    assert_eq!(request.method(), "POST");
    assert_eq!(request.url(), "https://example.com/path?q=1");
    assert_eq!(
        request.headers().get("x-custom").unwrap().as_deref(),
        Some("value")
    );

    let text_promise = request
        .text()
        .expect("text() must not throw on a buffered body");
    let text = http_ng_fetch::testing::send_js_future(text_promise)
        .await
        .expect("reading the body back must not reject");
    assert_eq!(text.as_string().as_deref(), Some("payload"));
}

#[wasm_bindgen_test]
async fn rewound_body_bytes_actually_reach_the_constructed_request() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(|| {
            RequestBody::Full(Bytes::from_static(b"rewound-bytes"))
        }))
        .unwrap();
    let (request, _controller) = http_ng_fetch::testing::to_web_request(&f, req).unwrap();
    // `body_used` must still be false — we constructed the request, we
    // haven't consumed it yet by reading `.text()` below.
    assert!(!request.body_used());
    let text_promise = request.text().expect("text() must not throw");
    let text = http_ng_fetch::testing::send_js_future(text_promise)
        .await
        .expect("reading the body back must not reject");
    // If `resolve_body` had silently dropped the rewound bytes (the exact
    // defect this task's brief calls out), this would read back empty
    // instead of the bytes the factory actually produced.
    assert_eq!(text.as_string().as_deref(), Some("rewound-bytes"));
}

// ---------------------------------------------------------------------
// The `AbortController` returned alongside a successful conversion is
// wired to the built `Request`'s own signal, not a decoy object nobody
// listens to.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn abort_controller_signal_is_actually_the_requests_signal() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .body(RequestBody::Empty)
        .unwrap();
    let (request, controller) = http_ng_fetch::testing::to_web_request(&f, req).unwrap();
    let controller = controller.expect("AbortController::new() succeeds in any real browser");
    assert!(!request.signal().aborted());
    controller.abort();
    assert!(
        request.signal().aborted(),
        "the Request's own signal must observe the controller's abort() — \
         otherwise the returned controller controls nothing"
    );
}

// ---------------------------------------------------------------------
// `check_headers` in isolation, against a synthetic `Capabilities` — not
// tied to the real `FORBIDDEN_HEADERS` list, so this tests the mechanism
// itself rather than that one specific array.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn check_headers_rejects_exactly_what_capabilities_declares_forbidden() {
    static FORBIDDEN: [http::HeaderName; 1] = [http::header::AUTHORIZATION];
    let mut caps = http_ng_core::Capabilities::none();
    caps.forbidden_request_headers = &FORBIDDEN;

    let mut h = http::HeaderMap::new();
    h.insert(http::header::AUTHORIZATION, "secret".parse().unwrap());
    assert!(http_ng_fetch::testing::check_headers(&h, &caps).is_err());

    let mut h = http::HeaderMap::new();
    h.insert("x-other", "fine".parse().unwrap());
    assert!(http_ng_fetch::testing::check_headers(&h, &caps).is_ok());
}

// ---------------------------------------------------------------------
// `verify_headers_survived` checks NAME presence, not byte-exact value
// fidelity — documented precisely, not left for the next reader to
// discover. Confirmed directly (not assumed): `web_sys::Headers::append`
// trims leading/trailing HTTP whitespace from a value before storing it
// (RFC 7230 optional-whitespace normalization, applied by the Fetch
// Standard's own "normalize a byte sequence" step — not a Chrome quirk).
// A caller who sets `" padded "` gets back `"padded"`; that is a real,
// observable difference `verify_headers_survived` does not catch, because
// it never compares values, only checks that the name is still present.
// This is deliberately not treated as a silent drop: the value SURVIVED,
// merely normalized the same way any HTTP implementation is allowed to.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
async fn header_value_whitespace_is_trimmed_and_not_flagged_as_a_silent_drop() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("x-padded", "  padded value  ")
        .body(RequestBody::Empty)
        .unwrap();
    // Must succeed: `verify_headers_survived` only checks the name
    // `x-padded` is present, which it is — the whitespace trim is not an
    // error condition this check is meant to catch.
    let (request, _controller) = http_ng_fetch::testing::to_web_request(&f, req).unwrap();
    // And the trim genuinely happened — this isn't merely "didn't error",
    // the value on the wire really is different from what was set.
    assert_eq!(
        request.headers().get("x-padded").unwrap().as_deref(),
        Some("padded value"),
        "the browser must have trimmed the surrounding whitespace"
    );
}
