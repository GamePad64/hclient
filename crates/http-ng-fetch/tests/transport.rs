//! `Fetch: Transport` — this crate's integration tests against
//! `http_ng::Client`, the same discipline `http-ng-native`'s and
//! `http-ng-wasi`'s own `tests/transport.rs`/`to_error` tests follow: what's
//! checked here (error category fidelity, `Capabilities` honesty against
//! actual behavior, timeout rejection at `build()`, the promise adapter's
//! `Send`-ness surviving all the way to `Transport::execute`) are properties
//! of the SEAM between `Client::execute` and `Transport`, not of one
//! function in `convert.rs`/`body.rs` alone — so they're checked through a
//! real `Client`, not only through `testing::*` directly.
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use http_ng::Client;
use http_ng_core::ErrorKind;
use http_ng_fetch::Fetch;

// ---------------------------------------------------------------------
// Brief's own two tests (task-5-brief.md, Step 1) — the second verbatim,
// the first NOT verbatim. A defect in the brief's own reference code (see
// the task report): `c.get("data:text/plain,ok")` can never reach `Fetch`
// at all through `http_ng::Client`, for two independent reasons, each
// verified directly rather than assumed —
//
// 1. `http::Uri` cannot parse `"data:text/plain,ok"` in the first place
//    (`InvalidUri(InvalidFormat)`, confirmed with a throwaway crate against
//    `http` 1.5.0): the `http` crate's `Uri` grammar only covers HTTP's own
//    request-target forms (origin/absolute/authority/asterisk), not
//    RFC 3986 in general — an opaque scheme's content isn't universally
//    accepted (`"mailto:a@b.com"` and `"about:blank"` DO parse; the comma
//    in `text/plain,ok` does not). `RequestBuilder::new` (`http-ng/src/
//    request.rs`) parses the URL string into `http::Uri` before this crate
//    ever sees it, so this failure happens one whole crate away from
//    `http-ng-fetch`.
// 2. Even granting a working parse, `convert::checked_url` (Task 3) rejects
//    every scheme except `http`/`https` as `ErrorKind::Unsupported` —
//    deliberately, and documented as such. `Body::from_response`'s own
//    Task 4 helper (`testing::fetch_body`, `lib.rs`) already flags this
//    exact incompatibility for ITS OWN use of a `data:` URL: it "deliberately
//    does NOT route through `convert::to_web_request`... which would reject
//    exactly the `data:` URL." The brief's Step 1 test for THIS task does
//    route through the ordinary `Client` path, so it inherits the rejection
//    Task 4 specifically built around.
//
// Fixed by using a real `http://` URL instead — the test harness's own
// already-loaded page (`location.href`), so the exchange stays fully
// offline and deterministic (no real Internet access needed, same as the
// brief's `data:` URL was going for) while actually exercising the network
// path `data:` URLs skip entirely in a browser (real request/response
// headers, a real `web_sys::Response`, not an internally-synthesized one).
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
async fn end_to_end_through_the_client() {
    let url = web_sys::window()
        .expect("wasm_bindgen_test_configure!(run_in_browser) guarantees a window")
        .location()
        .href()
        .expect("the currently loaded page always has an href");
    let c = Client::builder(Fetch::new()).build().unwrap();
    let resp = c.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.collect().await.unwrap().text().unwrap();
    assert!(
        !body.is_empty(),
        "the harness's own already-loaded page must come back non-empty"
    );
}

#[wasm_bindgen_test]
async fn build_rejects_timeouts_fetch_cannot_express() {
    let err = Client::builder(Fetch::new())
        .timeouts(http_ng::Timeouts {
            connect: Some(std::time::Duration::from_secs(1)),
            ..Default::default()
        })
        .build()
        .unwrap_err();
    assert_eq!(err.what, "connect_timeout");
    assert!(err.backend.contains("Fetch"), "{}", err.backend);
}

// ---------------------------------------------------------------------
// `ErrorKind` fidelity through the REAL `Client`, not just `to_error`'s
// default in isolation (the default is checked generically by
// `http-ng-core/tests/shape.rs`; this proves what a caller of THIS backend
// actually observes) — the same discipline as native's
// `transport_error_kind_survives_the_client_instead_of_flattening_to_other`
// and wasi's identity `to_error`.
// ---------------------------------------------------------------------

/// A network failure is the one failure `execute` can produce with no
/// server listening and no DNS involved: a loopback port nothing is bound
/// to refuses (or, for a handful of browser-blocked ports, is rejected
/// outright) near-instantly — bounded by construction, not by a timeout.
/// The Fetch Standard gives no richer signal than a single `TypeError`
/// here (`Capabilities` has no separate "resolve failed" concept for this
/// backend, unlike native/wasi) — the brief's own mapping puts this at
/// `ErrorKind::Connect`, and that category must survive `Client::execute`'s
/// `self.transport.to_error(e)` step unflattened.
#[wasm_bindgen_test]
async fn network_failure_reaches_the_caller_as_connect_not_other() {
    let c = Client::builder(Fetch::new()).build().unwrap();
    let err = c.get("http://127.0.0.1:59999/").send().await.unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Connect, "{err}");
    assert!(
        !err.to_string().starts_with("Other:"),
        "the category must be printed once, and it must be the real one: {err}"
    );
}

/// A forbidden header (Task 3's `check_headers`) is `ErrorKind::Unsupported`
/// at the `convert` layer; this proves the category is still `Unsupported`,
/// not re-wrapped into `Other`, once it has actually gone through
/// `Fetch::execute` and `Client::execute`'s own error step.
#[wasm_bindgen_test]
async fn forbidden_header_reaches_the_caller_as_unsupported_not_other() {
    let c = Client::builder(Fetch::new()).build().unwrap();
    let err = c
        .get("https://example.com/")
        .header("host", "evil.example")
        .send()
        .await
        .unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Unsupported, "{err}");
    assert!(
        !err.to_string().starts_with("Other:"),
        "the category must be printed once, and it must be the real one: {err}"
    );
    assert!(err.to_string().contains("host"), "{err}");
}

#[derive(Debug)]
struct DummySource;
impl std::fmt::Display for DummySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dummy source")
    }
}
impl std::error::Error for DummySource {}

/// `to_error` must be the identity: `Fetch::Error` is already
/// `http_ng_core::Error`, so wrapping it again (the default's fallback for
/// a foreign error type) would double the category
/// (`Other: Unsupported: ...`) and break every `is_*` predicate. This is
/// the direct, structural half of the guarantee; the two tests above are
/// the behavioral half, exercised through a real `Client`.
#[wasm_bindgen_test]
fn to_error_is_the_identity_so_the_classification_survives_unwrapped() {
    use http_ng_core::unversioned::Transport;
    let t = Fetch::new();
    let original = http_ng_core::Error::new(ErrorKind::Tls, DummySource);
    let out = t.to_error(original.clone());
    assert_eq!(out.kind(), original.kind());
    assert_eq!(out.to_string(), original.to_string());
}

// ---------------------------------------------------------------------
// Capability fidelity: the one property this task is explicitly obligated
// to re-verify, not just inherit. `caps.streaming_request_body` in THIS
// environment (headless Chrome) probes `true` — `tests/convert.rs` already
// proves `convert::to_web_request` rejects `Streaming` unconditionally
// against a CALLER-SUPPLIED `Capabilities` claiming `true`
// (`streaming_body_is_rejected_even_when_duplex_is_claimed_supported`).
// That's the conversion layer in isolation. This closes the loop through
// the ACTUAL `Transport` impl, the REAL probed `Capabilities`, and a REAL
// `Client`: probe -> declaration -> behavior, nothing stood in.
// ---------------------------------------------------------------------

struct NeverPolled;
impl http_body::Body for NeverPolled {
    type Data = bytes::Bytes;
    type Error = http_ng_core::Error;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, Self::Error>>> {
        std::task::Poll::Ready(None)
    }
}

#[wasm_bindgen_test]
async fn streaming_request_body_is_rejected_through_the_real_client_and_real_probe() {
    let f = Fetch::new();
    // Not gated on the probe's value (unlike `tests/convert.rs`'s
    // `streaming_body_is_rejected_where_duplex_is_absent`, which mirrors
    // the brief and skips in Chrome): the rejection must hold regardless of
    // what the real probe says, and in THIS environment it says `true` —
    // exactly the case a probe-gated test would never exercise.
    let c = Client::builder(f).build().unwrap();
    let err = c
        .post("https://example.com/")
        .body(http_ng_core::RequestBody::Streaming(Box::new(NeverPolled)))
        .send()
        .await
        .unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Unsupported, "{err}");
    assert!(err.to_string().contains("streaming_request_body"), "{err}");
}

// ---------------------------------------------------------------------
// `Transport::capabilities()` must forward the SAME probed value the crate
// itself already trusts internally (`Fetch::caps`, read by
// `to_web_request`) — not a fresh `Capabilities::none()` or a
// re-probe that could disagree with what `execute` actually consulted.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn capabilities_forwards_the_same_probe_execute_itself_consults() {
    use http_ng_core::unversioned::Transport;
    let f = Fetch::new();
    let probed = f.capabilities_for_test();
    let via_trait = Transport::capabilities(&f);
    assert_eq!(via_trait.owns_cookie_jar, probed.owns_cookie_jar);
    assert_eq!(via_trait.owns_cache, probed.owns_cache);
    assert_eq!(via_trait.tls_config, probed.tls_config);
    assert_eq!(
        via_trait.streaming_request_body,
        probed.streaming_request_body
    );
    assert_eq!(via_trait.full_duplex, probed.full_duplex);
    assert_eq!(via_trait.timeouts, probed.timeouts);
}

// ---------------------------------------------------------------------
// `Send`-ness of the future `execute` returns — the entire payoff of
// Task 1's `promise::SendJsFuture` (a `Send`-compatible replacement for
// `wasm_bindgen_futures::JsFuture`, which is `!Send`). Mirrors
// `http-ng-wasi/tests/shape.rs`'s `execute_future_is_send_for_an_empty_
// request_body`/`..._even_for_a_streaming_request_body`, adapted to run as
// a `#[wasm_bindgen_test]` rather than a target-neutral `#[test]`: unlike
// `WasiHttp::new()`, `Fetch::new()` calls into real `js_sys`/browser globals
// during its capability probe, so — consistent with every other test file
// in this crate — this needs the real browser environment `wasm_bindgen_
// test_configure!(run_in_browser)` provides, not just a host build. The
// future is never polled (an `async fn`'s body doesn't run before the first
// poll), so no network call happens either way.
// ---------------------------------------------------------------------

fn assert_send<T: Send>(_: T) {}

#[wasm_bindgen_test]
fn execute_future_is_send_for_an_empty_request_body() {
    use http_ng_core::RequestBody;
    use http_ng_core::unversioned::Transport;
    let t = Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .body(RequestBody::Empty)
        .unwrap();
    let fut = t.execute(req);
    assert_send(fut);
}

#[wasm_bindgen_test]
fn execute_future_is_send_even_for_a_streaming_request_body() {
    use http_ng_core::RequestBody;
    use http_ng_core::unversioned::Transport;
    let t = Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .body(RequestBody::Streaming(Box::new(NeverPolled)))
        .unwrap();
    let fut = t.execute(req);
    assert_send(fut);
}

// ---------------------------------------------------------------------
// Cancelling an in-flight `fetch()` when the `execute()` future is dropped
// early. `convert::to_web_request`'s own doc comment (Task 3) says the
// `AbortController` it returns is for "a later task (the `Transport` impl,
// driving deadlines and cancellation)" to hold onto — the brief's own
// reference `execute` receives it as `_abort` and never touches it again,
// which honors none of that: a caller racing `c.get(url).send()` against a
// timer (`futures::select!` or similar; there's no `tokio::time::timeout`
// on wasm) and losing the race leaves the browser's `fetch()` running to
// completion unseen, for nothing. `lib.rs`'s `AbortOnDrop` closes this gap.
//
// Tested at the level of the guard itself (`testing::abort_guard_behavior`),
// not by racing a real `fetch()` against a timer: no controllable "still
// pending" fetch exists in this test environment (a `data:` URL resolves
// immediately; a real network request's timing is not something this suite
// can bound deterministically). `AbortController`/`AbortSignal` are plain,
// synchronous browser objects with no network involved, so this is fully
// deterministic and mirrors how `tests/body.rs` proves response-side
// cancel-on-drop against a hand-built `ReadableStream` rather than a live
// server.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn dropping_the_guard_before_defuse_aborts_the_signal() {
    assert!(
        http_ng_fetch::testing::abort_guard_behavior(false),
        "Drop must call AbortController::abort() when the guard was never defused"
    );
}

#[wasm_bindgen_test]
fn defusing_the_guard_prevents_the_abort() {
    assert!(
        !http_ng_fetch::testing::abort_guard_behavior(true),
        "defuse() must stop Drop from calling abort() — a settled promise (success or \
         failure) has nothing left to cancel, and aborting a successful exchange after the \
         fact would corrupt a Body the caller hasn't read yet"
    );
}
