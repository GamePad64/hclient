//! `Fetch: Transport` — this crate's integration tests against
//! `hclient::Client`, the same discipline `hclient-native`'s and
//! `hclient-wasi`'s own `tests/transport.rs`/`to_error` tests follow: what's
//! checked here (error category fidelity, `Capabilities` honesty against
//! actual behavior, timeout rejection at `build()`, the promise adapter's
//! `Send`-ness surviving all the way to `Transport::execute`) are properties
//! of the SEAM between `Client::execute` and `Transport`, not of one
//! function in `convert.rs`/`body.rs` alone — so they're checked through a
//! real `Client`, not only through `testing::*` directly.
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use hclient::Client;
use hclient_core::ErrorKind;
use hclient_fetch::Fetch;
use wasm_bindgen::{JsCast, JsValue};

// ---------------------------------------------------------------------
// Brief's own two tests (task-5-brief.md, Step 1) — the second verbatim,
// the first NOT verbatim. A defect in the brief's own reference code (see
// the task report): `c.get("data:text/plain,ok")` can never reach `Fetch`
// at all through `hclient::Client`, for two independent reasons, each
// verified directly rather than assumed —
//
// 1. `http::Uri` cannot parse `"data:text/plain,ok"` in the first place
//    (`InvalidUri(InvalidFormat)`, confirmed with a throwaway crate against
//    `http` 1.5.0): the `http` crate's `Uri` grammar only covers HTTP's own
//    request-target forms (origin/absolute/authority/asterisk), not
//    RFC 3986 in general — an opaque scheme's content isn't universally
//    accepted (`"mailto:a@b.com"` and `"about:blank"` DO parse; the comma
//    in `text/plain,ok` does not). `RequestBuilder::new` (`hclient/src/
//    request.rs`) parses the URL string into `http::Uri` before this crate
//    ever sees it, so this failure happens one whole crate away from
//    `hclient-fetch`.
// 2. Even granting a working parse, `convert::checked_url` rejects
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
        .timeouts(hclient::Timeouts {
            resolve: None,
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
// `hclient-core/tests/shape.rs`; this proves what a caller of THIS backend
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
/// `hclient_core::Error`, so wrapping it again (the default's fallback for
/// a foreign error type) would double the category
/// (`Other: Unsupported: ...`) and break every `is_*` predicate. This is
/// the direct, structural half of the guarantee; the two tests above are
/// the behavioral half, exercised through a real `Client`.
#[wasm_bindgen_test]
fn to_error_is_the_identity_so_the_classification_survives_unwrapped() {
    use hclient_core::unversioned::Transport;
    let t = Fetch::new();
    let original = hclient_core::Error::new(ErrorKind::Tls, DummySource);
    let out = t.to_error(original.clone());
    assert_eq!(out.kind(), original.kind());
    assert_eq!(out.to_string(), original.to_string());
}

// ---------------------------------------------------------------------
// Capability fidelity: the one property this task is explicitly obligated
// to re-verify, not just inherit. `tests/convert.rs` already proves
// `convert::to_web_request` rejects `Streaming` unconditionally against a
// CALLER-SUPPLIED `Capabilities` claiming `true`
// (`streaming_body_is_rejected_even_when_duplex_is_claimed_supported`).
// That's the conversion layer in isolation. This closes the loop through
// the ACTUAL `Transport` impl, the REAL probed `Capabilities`, and a REAL
// `Client`: probe -> declaration -> behavior, nothing stood in.
//
// (`caps.streaming_request_body` varies by browser again as of v0.2 W6 —
// Chrome 151 probes `true`, Firefox 153 `false`. This comment has now said
// three different things across three tasks: Task 5 wrote "probes `true` in
// headless Chrome", the reopening made it a hardcoded `false` everywhere,
// and W6 made it the browser's own answer. The test below is written so
// that it needs no fourth revision: it asserts BOTH outcomes and picks by
// the probe, rather than encoding whichever one is true this month.)
// ---------------------------------------------------------------------

struct NeverPolled;
impl http_body::Body for NeverPolled {
    type Data = bytes::Bytes;
    type Error = hclient_core::Error;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, Self::Error>>> {
        std::task::Poll::Ready(None)
    }
}

/// The whole W6 story through the real `Transport`, the real probed
/// `Capabilities` and a real `Client` — probe -> declaration -> behaviour,
/// nothing stood in. Both outcomes are asserted, because both are correct:
/// which one you get is the browser's answer, not ours.
///
/// **Where the probe says the browser would corrupt the body** (Firefox
/// 153): a typed `Unsupported` naming the capability, raised before
/// anything is sent. That is the outcome the whole capability exists to
/// produce.
///
/// **Where the probe says the browser streams** (Chrome 151): the request
/// is attempted — and here it must fail anyway, for the *other* reason W6
/// measured. The URL is the harness's own already-loaded page, so this is
/// same-origin (no CORS in the way) and, decisively, **HTTP/1.1** —
/// wasm-bindgen's test server speaks nothing else. A `ReadableStream`
/// request body needs HTTP/2, so Chrome refuses it in milliseconds with a
/// bare `TypeError: Failed to fetch`, before pulling the stream and with
/// nothing reaching the server.
///
/// So this test is also the live, in-CI confirmation of the measured
/// HTTP/1.1 finding, and of the one thing this crate adds on top of it: the
/// browser's `TypeError` is indistinguishable from a refused connection,
/// and `convert::StreamingBodyFetchFailed` is what names the cause the
/// browser structurally cannot report. `ErrorKind` stays `Connect` — that
/// is still what happened — so the assertion is on the text, which is where
/// the added information lives.
#[wasm_bindgen_test]
async fn a_streaming_request_body_through_the_real_client_fails_for_the_measured_reason() {
    let f = Fetch::new();
    let streams = f.capabilities_for_test().streaming_request_body;
    let url = web_sys::window()
        .expect("wasm_bindgen_test_configure!(run_in_browser) guarantees a window")
        .location()
        .href()
        .expect("the currently loaded page always has an href");
    let c = Client::builder(f).build().unwrap();
    let err = c
        .post(&url)
        .body(hclient_core::RequestBody::Streaming(Box::new(NeverPolled)))
        .send()
        .await
        .unwrap_err();

    if streams {
        assert_eq!(
            *err.kind(),
            ErrorKind::Connect,
            "a browser that streams gets as far as the network, where this HTTP/1.1 test \
             server refuses the stream body: {err}"
        );
        assert!(
            err.to_string().contains("HTTP/2"),
            "the opaque `TypeError: Failed to fetch` must be labelled with the cause the \
             browser cannot report — otherwise it is indistinguishable from a refused \
             connection: {err}"
        );
    } else {
        assert_eq!(*err.kind(), ErrorKind::Unsupported, "{err}");
        assert!(err.to_string().contains("streaming_request_body"), "{err}");
    }
}

// ---------------------------------------------------------------------
// `Transport::capabilities()` must forward the SAME probed value the crate
// itself already trusts internally (`Fetch::caps`, read by
// `to_web_request`) — not a fresh `Capabilities::none()` or a
// re-probe that could disagree with what `execute` actually consulted.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn capabilities_forwards_the_same_probe_execute_itself_consults() {
    use hclient_core::unversioned::Transport;
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
// `hclient-wasi/tests/shape.rs`'s `execute_future_is_send_for_an_empty_
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
    use hclient_core::RequestBody;
    use hclient_core::unversioned::Transport;
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
    use hclient_core::RequestBody;
    use hclient_core::unversioned::Transport;
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
// early. `convert::to_web_request`'s own doc comment says the
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
//
// **v0.2 W1 amends that paragraph.** It is still true that this suite
// cannot make a real fetch hang on demand, but that turned out not to be
// what the measurement needs. A fetch cannot settle without a microtask
// turn, so a request issued and abandoned inside ONE synchronous stretch
// of Rust is necessarily still in flight when it is abandoned — no waiting
// required, and nothing to bound. What was missing was therefore never the
// hang; it was an observer. The two tests below add one, and they measure
// what the two guard tests above cannot: that the guard is wired into the
// real `execute` path, and that the browser acts on it.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn dropping_the_guard_before_defuse_aborts_the_signal() {
    assert!(
        hclient_fetch::testing::abort_guard_behavior(false),
        "Drop must call AbortController::abort() when the guard was never defused"
    );
}

#[wasm_bindgen_test]
fn defusing_the_guard_prevents_the_abort() {
    assert!(
        !hclient_fetch::testing::abort_guard_behavior(true),
        "defuse() must stop Drop from calling abort() — a settled promise (success or \
         failure) has nothing left to cancel, and aborting a successful exchange after the \
         fact would corrupt a Body the caller hasn't read yet"
    );
}

// ---------------------------------------------------------------------
// v0.2 W1: the same guard, measured through the real `execute` path, with
// the browser as the observer.
//
// # Why an observer was needed, and what counts as one
//
// "The future stopped being polled" and "the transfer stopped" are
// different claims. The two guard tests above establish the first half of
// the mechanism — `AbortOnDrop` really does abort a controller — but they
// build that controller themselves and never let `execute` near it, so
// they would stay green if `execute` stopped arming the guard, stopped
// attaching the signal, or stopped calling `fetch` at all.
//
// The observer here is the browser itself. `execute` looks `fetch` up on
// the global object (see its own comment on why: it must work in a Worker,
// where `window` does not exist), so a wrapper installed there sits
// exactly between this crate and the network, delegates to the real
// `fetch`, and keeps the promise the browser hands back. What the browser
// then says about that promise is the measurement: a `DOMException` named
// `AbortError` is produced by the "abort a fetch" algorithm terminating an
// ongoing fetch, and it is not a value this side can synthesize. Compare
// `tests/body.rs`'s note on the same name arriving from a real
// `httpbin.org/drip` abort during review.
//
// # Why no timing, and why the fetch is real
//
// The URL is the harness's own page (`location.href`) — a real HTTP
// request over a real connection to the test server, not a `data:` URL the
// browser answers internally. The request is issued by the single
// `Future::poll` below and abandoned in the same synchronous stretch, with
// no `.await` in between, so no microtask has run and the promise cannot
// have settled: the exchange is in flight by construction rather than by
// hoping a network round trip is slow.
//
// "By construction" is still an assumption about `execute`'s internals,
// though — an `.await` added ahead of the `fetch` call would silently turn
// this into a test that aborts nothing. So the wrapper records that it was
// called, and the test asserts that record BEFORE dropping anything. The
// in-flight exchange is measured, not assumed.
// ---------------------------------------------------------------------

/// Names on the global object shared by the wrapper below and the two
/// tests. Prefixed because the global object here is the test page's, and
/// `wasm-bindgen-test` runs every test in this file against that same one.
const CALLED: &str = "__httpNgFetchWasCalled";
const OUTCOME: &str = "__httpNgFetchOutcome";
const REAL: &str = "__httpNgRealFetch";

/// Replaces `globalThis.fetch` with a wrapper that delegates to the real
/// one, records that it ran, and records what the browser eventually made
/// of the request.
///
/// The outcome is stored as a promise that ALWAYS fulfils — `p.then(ok,
/// err)` rather than `p` itself — for two reasons. It gives the test one
/// shape to read in both directions, and it attaches a rejection handler
/// in the same turn the promise is created, so an aborted fetch never
/// reaches the browser as an unhandled rejection (which some runners
/// treat as a test failure in its own right).
fn install_fetch_observer() {
    let global = js_sys::global();
    let real = js_sys::Reflect::get(&global, &JsValue::from_str("fetch"))
        .expect("every browser global has `fetch`");
    js_sys::Reflect::set(&global, &JsValue::from_str(REAL), &real).expect("global is writable");
    js_sys::Reflect::set(&global, &JsValue::from_str(CALLED), &JsValue::FALSE)
        .expect("global is writable");
    let wrapper = js_sys::Function::new_with_args(
        "req",
        &format!(
            "globalThis.{CALLED} = true;\
             const p = globalThis.{REAL}.call(globalThis, req);\
             globalThis.{OUTCOME} = p.then(\
                 (r) => ({{ aborted: false, status: r.status, name: null }}),\
                 (e) => ({{ aborted: true, status: null, name: e && e.name }}),\
             );\
             return p;"
        ),
    );
    js_sys::Reflect::set(&global, &JsValue::from_str("fetch"), &wrapper)
        .expect("global is writable");
}

/// Puts the browser's own `fetch` back. Called as soon as the request has
/// been issued — the wrapper's whole job is done by then, and leaving it
/// installed would leak into every test that runs after this one on the
/// same page.
fn restore_fetch() {
    let global = js_sys::global();
    let real = js_sys::Reflect::get(&global, &JsValue::from_str(REAL)).expect("stashed above");
    js_sys::Reflect::set(&global, &JsValue::from_str("fetch"), &real).expect("global is writable");
}

/// Reads back what the browser made of the request: `(aborted, name)`.
async fn observed_outcome() -> (bool, Option<String>) {
    let global = js_sys::global();
    let outcome: js_sys::Promise = js_sys::Reflect::get(&global, &JsValue::from_str(OUTCOME))
        .expect("the wrapper stores an outcome promise before returning")
        .dyn_into()
        .expect("the wrapper stores a promise");
    let record = hclient_fetch::testing::send_js_future(outcome)
        .await
        .expect("the outcome promise is built to always fulfil, never reject");
    let aborted = js_sys::Reflect::get(&record, &JsValue::from_str("aborted"))
        .expect("record field")
        .as_bool()
        .expect("`aborted` is a boolean");
    let name = js_sys::Reflect::get(&record, &JsValue::from_str("name"))
        .expect("record field")
        .as_string();
    (aborted, name)
}

fn page_url() -> String {
    web_sys::window()
        .expect("wasm_bindgen_test_configure!(run_in_browser) guarantees a window")
        .location()
        .href()
        .expect("the currently loaded page always has an href")
}

fn get(url: &str) -> http::Request<hclient_core::RequestBody> {
    http::Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .body(hclient_core::RequestBody::Empty)
        .expect("request")
}

/// Issues a real request through `Fetch::execute`, polls it exactly once,
/// and returns the still-pending future — plus the proof that the request
/// actually went out.
///
/// Panics if the single poll completed the future: that would mean the
/// exchange was over before the caller could act on it, and everything
/// downstream would be measuring nothing.
fn issue_and_poll_once(
    t: &Fetch,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<http::Response<hclient_fetch::Body>, hclient_core::Error>,
            > + '_,
    >,
> {
    use hclient_core::unversioned::Transport;
    use std::task::{Context, Poll, Waker};

    install_fetch_observer();
    let mut fut = Box::pin(t.execute(get(&page_url())));
    let mut cx = Context::from_waker(Waker::noop());
    match fut.as_mut().poll(&mut cx) {
        Poll::Pending => {}
        Poll::Ready(_) => panic!(
            "a real network fetch cannot settle before the first microtask checkpoint — if it \
             did, there was no in-flight exchange here to cancel"
        ),
    }
    restore_fetch();

    let called = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str(CALLED))
        .expect("installed above")
        .as_bool()
        .unwrap_or(false);
    assert!(
        called,
        "`execute` must have called `fetch` by the time its future first returns Pending — \
         if it now awaits something else first, the exchange is not yet in flight and \
         everything this test goes on to assert about aborting it is vacuous"
    );
    fut
}

/// The claim: dropping an in-flight `execute` future makes the browser
/// abort the fetch it is running.
///
/// Mutation that turns this red: delete `impl Drop for AbortOnDrop`
/// (`src/lib.rs`), or stop arming the guard in `execute`. The browser then
/// carries the request to completion and reports a 200 instead of an
/// `AbortError`.
#[wasm_bindgen_test]
async fn dropping_the_execute_future_makes_the_browser_report_an_aborted_fetch() {
    let t = Fetch::new();
    let fut = issue_and_poll_once(&t);
    drop(fut);

    let (aborted, name) = observed_outcome().await;
    assert!(
        aborted,
        "dropping the execute future must abort the browser's fetch, not merely stop polling \
         it: the browser carried the request through to a response"
    );
    assert_eq!(
        name.as_deref(),
        Some("AbortError"),
        "the rejection must be the browser's own `AbortError` — any other name means the \
         fetch failed for some unrelated reason and this test proved nothing about \
         cancellation"
    );
}

/// The control, and the half that gives the test above meaning: the same
/// request to the same URL, not dropped, must be carried to a real
/// response.
///
/// Without it, `aborted == true` could mean the harness never had a
/// working request in the first place.
#[wasm_bindgen_test]
async fn an_execute_future_that_is_kept_completes_the_very_same_fetch() {
    let t = Fetch::new();
    let fut = issue_and_poll_once(&t);
    let resp = fut
        .await
        .expect("the harness's own page must fetch cleanly");
    assert_eq!(resp.status(), 200);

    let (aborted, name) = observed_outcome().await;
    assert!(
        !aborted,
        "a kept execute future must leave its fetch alone: {name:?}"
    );
}

/// The declaration and the measurement, in one file on purpose.
///
/// `CancelSupport::None` is a legitimate answer for a backend that cannot
/// cancel — and it is therefore also the quiet way to make the two tests
/// above stop applying here. Asserting the declared value closes that
/// exit.
#[wasm_bindgen_test]
fn fetch_declares_the_cancellation_it_performs() {
    assert_eq!(
        Fetch::new().capabilities_for_test().cancel_on_drop,
        hclient_core::CancelSupport::Supported,
        "the tests in this file measure a cancellation that `Fetch` must also declare — a \
         backend is free to declare `None`, but not to declare `None` while behaving \
         otherwise, nor to quietly stop being covered by the measurement"
    );
}
