//! Why `Connected` has no emitter here, measured rather than argued.
//!
//! `PerformanceResourceTiming` is the one place a browser exposes anything
//! that looks like `ConnectTiming`: `domainLookupStart`/`End`,
//! `connectStart`/`End`, `secureConnectionStart`, and `nextHopProtocol`,
//! which is very nearly `Connected::version` as well. A reviewer's first
//! question about `src/hooks.rs` is therefore "why not read that", and a
//! paragraph of prose is not an answer this workspace accepts.
//!
//! So this file is the answer: **two measurements and one thing this
//! harness cannot measure**, said as such. Each of the two is sufficient
//! on its own.
//!
//! 1. The entry does not exist when `execute` returns the head, which is
//!    the only moment a `Connected` could be emitted. **Measured.**
//! 2. Nothing on the entry identifies which request it belongs to, so a
//!    `ConnectionId` minted from one would be a guess. **Measured.**
//! 3. Cross-origin without `Timing-Allow-Origin`, every phase is zero and
//!    the protocol is blank. **Read from the specification, not measured
//!    here** — see the last test for the two attempts that failed and why.
#![cfg(target_arch = "wasm32")]
use std::future::poll_fn;
use std::pin::Pin;
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_fetch::Fetch;
use wasm_bindgen::{JsCast, JsValue};

fn perf() -> web_sys::Performance {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("performance"))
        .expect("global scope exposes performance")
        .unchecked_into::<web_sys::Performance>()
}

/// Every `PerformanceResourceTiming` whose `name` is `url`, as raw
/// `JsValue`s — `web_sys::PerformanceResourceTiming` is not among this
/// crate's enabled features and reading three fields through `Reflect` is
/// cheaper than enabling it for one test.
fn entries_for(url: &str) -> Vec<JsValue> {
    let f: js_sys::Function =
        js_sys::Reflect::get(perf().as_ref(), &JsValue::from_str("getEntriesByName"))
            .expect("Performance has getEntriesByName")
            .dyn_into()
            .expect("and it is a function");
    let list: js_sys::Array = f
        .call1(perf().as_ref(), &JsValue::from_str(url))
        .expect("getEntriesByName does not throw")
        .dyn_into()
        .expect("it returns an array");
    list.iter().collect()
}

fn field(entry: &JsValue, name: &str) -> JsValue {
    js_sys::Reflect::get(entry, &JsValue::from_str(name)).expect("an ordinary property read")
}

fn number(entry: &JsValue, name: &str) -> f64 {
    field(entry, name)
        .as_f64()
        .unwrap_or_else(|| panic!("{name} is a number on a resource timing entry"))
}

fn origin() -> String {
    web_sys::window()
        .expect("run_in_browser gives a window")
        .location()
        .origin()
        .expect("a loaded page has an origin")
}

/// A URL nothing else in this file has fetched, so its entries are ours.
fn fresh(tag: &str) -> String {
    format!("{}/?timing={tag}", origin())
}

fn get(uri: &str) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("a well-formed GET")
}

/// Reads a response body to its end by hand-polling `poll_frame`, rather
/// than through `http_body_util::BodyExt` — this crate deliberately has no
/// `http-body-util` dependency, dev or otherwise, and `lib.rs`'s own
/// `testing::collect` drains the same way for the same reason.
async fn drain(resp: http::Response<hclient_fetch::Body>) {
    use http_body::Body as _;
    let mut body = resp.into_body();
    loop {
        match poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
            Some(Ok(_)) => {}
            Some(Err(e)) => panic!("the harness page has a readable body: {e}"),
            None => break,
        }
    }
}

/// **The surface really is there**, which is what makes the other two
/// measurements findings rather than excuses.
///
/// Same-origin, body drained: an entry exists, it carries a
/// `nextHopProtocol`, and it carries connect timestamps. If this test ever
/// fails, the reason `Connected` is missing has changed and the rest of
/// this file is measuring the wrong thing.
#[wasm_bindgen_test]
async fn same_origin_the_browser_does_expose_connect_timings_after_the_fact() {
    let t = Fetch::new();
    let url = fresh("exists");

    let resp = t.execute(get(&url)).await.expect("the harness page");
    drain(resp).await;

    let entries = entries_for(&url);
    assert_eq!(
        entries.len(),
        1,
        "one fetch of a URL nothing else asked for is one entry"
    );
    let e = &entries[0];
    assert!(
        field(e, "nextHopProtocol").as_string().is_some(),
        "same-origin, the protocol the browser actually spoke IS readable — \
         just not from anywhere `execute` can reach"
    );
    assert!(
        number(e, "responseEnd") > 0.0,
        "and the entry is complete once the body has been read"
    );
}

/// **Measurement 1: the material for a `Connected` does not exist when the
/// head arrives.**
///
/// A resource timing entry is queued when the *resource* finishes, and
/// `Transport::execute` returns when the **head** does, with the body
/// still a `ReadableStream` nobody has read. A `Connected` must precede
/// the `Head` it explains — so at the only moment this transport could
/// emit one, there is nothing to build it from.
///
/// The response body is deliberately left unread across the assertion:
/// that is the ordinary shape of a streaming client, and it is the shape
/// in which the entry is provably absent.
#[wasm_bindgen_test]
async fn the_entry_does_not_exist_yet_when_execute_returns_the_head() {
    let t = Fetch::new();
    let url = fresh("head-first");

    let resp = t.execute(get(&url)).await.expect("the harness page");
    let at_head = entries_for(&url).len();

    drain(resp).await;
    let after_body = entries_for(&url).len();

    assert_eq!(
        at_head, 0,
        "no entry exists at the moment a `Connected` would have to be \
         emitted — the browser queues it when the resource finishes, and \
         the resource has not finished"
    );
    assert_eq!(
        after_body, 1,
        "and it appears once the body has been read, which is after every \
         event this transport could attach it to"
    );
}

/// **Measurement 2: an entry cannot be attributed to a request.**
///
/// The only handle a `PerformanceResourceTiming` offers is `name`, the
/// URL. Two fetches of one URL produce two entries that agree on `name`,
/// on `entryType` and on `initiatorType` — there is no request identity
/// anywhere on the object, so a transport with two requests in flight to
/// one origin cannot say which timing is whose.
///
/// That is fatal for `ConnectionId` specifically: an id is only worth
/// having because a later event can be matched to it, and an id minted
/// against the wrong entry is worse than no id at all.
#[wasm_bindgen_test]
async fn two_requests_to_one_url_produce_two_entries_nothing_can_tell_apart() {
    let t = Fetch::new();
    let url = fresh("indistinguishable");

    for _ in 0..2 {
        let resp = t.execute(get(&url)).await.expect("the harness page");
        drain(resp).await;
    }

    let entries = entries_for(&url);
    assert_eq!(entries.len(), 2, "two fetches, two entries");
    for key in ["name", "entryType", "initiatorType"] {
        assert_eq!(
            field(&entries[0], key).as_string(),
            field(&entries[1], key).as_string(),
            "`{key}` is the same on both entries, so it cannot separate them"
        );
    }
    for key in ["requestId", "id", "connectionId", "transferId"] {
        assert!(
            field(&entries[0], key).is_undefined(),
            "there is no `{key}` either — the object carries no request or \
             connection identity of any kind"
        );
    }
}

/// **The third reason is read rather than measured, and this is the
/// attempt that failed.**
///
/// Resource Timing Level 2 §4.2 zeroes every phase timestamp
/// (`domainLookupStart`/`End`, `connectStart`/`End`,
/// `secureConnectionStart`, `requestStart`, `responseStart`) and returns
/// the empty string for `nextHopProtocol` on a cross-origin resource whose
/// server did not send `Timing-Allow-Origin`. That is the general case for
/// an HTTP client — an origin the page does not control — so the surface
/// that looked like `ConnectTiming` has, exactly where a client needs it,
/// no timing in it.
///
/// **It is not measured here, and pretending otherwise would be the defect
/// this file exists to avoid.** A `wasm-pack` harness serves exactly one
/// origin and has no network, so there is no second origin to fetch. Two
/// ways of reaching the same socket under a different host string were
/// tried and both fail with `TypeError: Failed to fetch` — `localhost`
/// (which resolves to `::1`, where the harness does not listen) and
/// `[::ffff:127.0.0.1]` (rejected before a socket, measured on Chrome
/// 151). What would close it is a second listener in the harness, which is
/// a change to `wasm-bindgen-test-runner` rather than to this crate.
///
/// The test below asserts the *shape* of the gap rather than the fact: the
/// same-origin entry this file can see is the one case the specification
/// does **not** zero, so a reader who takes it for the general case is
/// reading the wrong row. If a cross-origin origin ever becomes reachable
/// here, this test is where the real measurement goes.
#[wasm_bindgen_test]
async fn the_only_origin_this_harness_has_is_the_one_the_spec_does_not_zero() {
    let here = origin();
    assert!(
        here.contains("127.0.0.1"),
        "the harness serves one origin and it is a loopback literal: {here}"
    );

    let t = Fetch::new();
    let url = fresh("same-origin-only");
    let resp = t.execute(get(&url)).await.expect("the harness page");
    drain(resp).await;

    let entries = entries_for(&url);
    assert_eq!(entries.len(), 1);
    assert!(
        number(&entries[0], "requestStart") > 0.0,
        "same-origin, `requestStart` is a real timestamp — which is exactly \
         the row the specification exempts, and exactly the row an HTTP \
         client's origins are not in"
    );
}
