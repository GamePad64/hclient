#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use http_body::Body as _;
use js_sys::{Object, Reflect, Uint8Array};
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

// ---------------------------------------------------------------------
// Brief's own two tests, verbatim (task-4-brief.md, Step 1).
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
async fn streams_a_response_body_in_chunks() {
    // A data: URL gives a deterministic response with no network.
    let f = http_ng_fetch::Fetch::new();
    let body = http_ng_fetch::testing::fetch_body(&f, "data:text/plain,hello%20world")
        .await
        .unwrap();
    assert_eq!(&body[..], b"hello world");
}

#[wasm_bindgen_test]
fn empty_body_is_end_stream() {
    let b = http_ng_fetch::Body::empty();
    assert!(http_body::Body::is_end_stream(&b));
}

// ---------------------------------------------------------------------
// Test fixtures: a `ReadableStream` whose `pull`/`cancel` behavior is
// driven entirely from Rust — deterministic multi-chunk delivery,
// mid-stream errors, non-byte chunks, and cancel-on-drop, none of which a
// real network `fetch()` can be made to produce on demand. No global
// listeners anywhere below (the harness trap flagged for this task: every
// `wasm_bindgen_test` in this file runs in the same JS realm) — every
// closure here is scoped to the one `web_sys::ReadableStream` its own test
// builds, and is dropped (or deliberately leaked, exactly like
// `wasm-streams`'s own internal `on_rejected` closure) without touching
// anything another test could observe.
// ---------------------------------------------------------------------

/// A `ReadableStream` whose `pull` callback is Rust code. `pull` is called
/// once, then again each time the stream's internal queue needs more data
/// (i.e. once per chunk actually consumed) — never again once `close()` or
/// `error()` has been called on the controller from inside it.
fn stream_with_pull(
    pull: impl FnMut(web_sys::ReadableStreamDefaultController) + 'static,
) -> web_sys::ReadableStream {
    let source = Object::new();
    let cb: Closure<dyn FnMut(web_sys::ReadableStreamDefaultController)> =
        Closure::wrap(Box::new(pull));
    Reflect::set(
        &source,
        &JsValue::from_str("pull"),
        cb.as_ref().unchecked_ref(),
    )
    .expect("setting a plain property on a fresh object never throws");
    // Leaked deliberately — the same trade-off `wasm-streams`'s own
    // `IntoStream::drop` makes for its `on_rejected` closure (see the
    // module doc comment on `src/body.rs`): the callback must outlive this
    // function and is invoked for as long as the stream itself is alive.
    // Bounded by the test that owns the stream, not unbounded.
    cb.forget();
    web_sys::ReadableStream::new_with_underlying_source(&source)
        .expect("constructing a ReadableStream from a plain pull-only source never throws")
}

/// Same as [`stream_with_pull`], plus a `cancel` callback that flips
/// `cancelled` to `true` — used by the cancellation test to observe,
/// deterministically, that dropping a `Body` mid-stream actually reaches
/// the underlying source's own `cancel()` and not merely that
/// `wasm-streams` claims to call it.
fn stream_with_pull_and_cancel(
    pull: impl FnMut(web_sys::ReadableStreamDefaultController) + 'static,
    cancelled: Rc<Cell<bool>>,
) -> web_sys::ReadableStream {
    let source = Object::new();
    let pull_cb: Closure<dyn FnMut(web_sys::ReadableStreamDefaultController)> =
        Closure::wrap(Box::new(pull));
    Reflect::set(
        &source,
        &JsValue::from_str("pull"),
        pull_cb.as_ref().unchecked_ref(),
    )
    .expect("setting pull never throws");
    pull_cb.forget();

    let cancel_cb: Closure<dyn FnMut(JsValue)> =
        Closure::wrap(Box::new(move |_reason: JsValue| {
            cancelled.set(true);
        }));
    Reflect::set(
        &source,
        &JsValue::from_str("cancel"),
        cancel_cb.as_ref().unchecked_ref(),
    )
    .expect("setting cancel never throws");
    cancel_cb.forget();

    web_sys::ReadableStream::new_with_underlying_source(&source)
        .expect("constructing a ReadableStream from a plain pull+cancel source never throws")
}

/// A `web_sys::Response` wrapping `stream`, with caller-chosen headers —
/// lets tests set `content-length`/`content-encoding` freely, independent
/// of whatever bytes the stream actually produces (this is testing THIS
/// crate's own caution around those headers, not real decompression).
fn response_from_stream(
    stream: &web_sys::ReadableStream,
    headers: &[(&str, &str)],
) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    if !headers.is_empty() {
        let h = web_sys::Headers::new().expect("Headers::new never throws");
        for (name, value) in headers {
            h.append(name, value)
                .expect("Headers::append never throws here");
        }
        init.set_headers_headers(&h);
    }
    web_sys::Response::new_with_opt_readable_stream_and_init(Some(stream), &init)
        .expect("constructing a Response around a ReadableStream never throws")
}

/// Awaits exactly one `poll_frame`, unwrapping a data frame into its bytes.
/// `Frame::into_data` failing (i.e. a trailers frame) is impossible for this
/// `Body` — see its module doc comment — so mapping that case to empty
/// bytes rather than handling it is a documented non-issue, not a silent
/// gap.
async fn next_data(
    body: &mut http_ng_fetch::Body,
) -> Option<Result<bytes::Bytes, http_ng_core::Error>> {
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *body).poll_frame(cx))
        .await
        .map(|r| r.map(|f| f.into_data().unwrap_or_else(|_| bytes::Bytes::new())))
}

/// One microtask tick, via this crate's own promise adapter — not
/// `wasm_bindgen_futures::JsFuture` directly, matching how every other test
/// in this crate awaits a `Promise` (Task 1's `SendJsFuture`, exposed at
/// `testing::send_js_future`).
async fn microtask_tick() {
    let p = js_sys::Promise::resolve(&JsValue::undefined());
    http_ng_fetch::testing::send_js_future(p)
        .await
        .expect("Promise::resolve never rejects");
}

// ---------------------------------------------------------------------
// `Body::from_response` on a bodyless `Response` — the OTHER path to an
// empty body, distinct from `Body::empty()` above (which the brief's own
// test already covers, but never exercises `from_response`'s `resp.body()
// == None` branch at all).
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn empty_response_body_is_end_stream_and_zero_sized() {
    let init = web_sys::ResponseInit::new();
    let resp = web_sys::Response::new_with_opt_readable_stream_and_init(None, &init)
        .expect("a bodyless Response is always constructible");
    let body = http_ng_fetch::testing::body_from_response(&resp).unwrap();
    assert!(http_body::Body::is_end_stream(&body));
    assert_eq!(http_body::Body::size_hint(&body).exact(), Some(0));
}

// ---------------------------------------------------------------------
// Multiple chunks arrive as separate frames, not merged into one — proves
// this is genuinely a streaming body, not a buffer with a stream-shaped API
// wrapped around one big read.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
async fn multiple_chunks_are_delivered_as_separate_frames_not_merged() {
    let calls = Rc::new(Cell::new(0u32));
    let calls_in_pull = calls.clone();
    let stream = stream_with_pull(move |controller| {
        let n = calls_in_pull.get();
        calls_in_pull.set(n + 1);
        match n {
            0 => {
                let _ = controller.enqueue_with_chunk(&Uint8Array::from(&b"ab"[..]));
            }
            1 => {
                let _ = controller.enqueue_with_chunk(&Uint8Array::from(&b"cd"[..]));
            }
            _ => {
                let _ = controller.close();
            }
        }
    });
    let resp = response_from_stream(&stream, &[]);
    let mut body = http_ng_fetch::testing::body_from_response(&resp).unwrap();

    assert!(
        !http_body::Body::is_end_stream(&body),
        "a stream body with data still coming must not report end-of-stream up front"
    );

    let first = next_data(&mut body).await.unwrap().unwrap();
    assert_eq!(
        &first[..],
        b"ab",
        "the first chunk must arrive on its own, not merged with the second"
    );
    let second = next_data(&mut body).await.unwrap().unwrap();
    assert_eq!(&second[..], b"cd");
    assert!(
        next_data(&mut body).await.is_none(),
        "the stream must end cleanly after both chunks"
    );
    assert!(http_body::Body::is_end_stream(&body));
}

// ---------------------------------------------------------------------
// A body that stops producing bytes without an error is the worst outcome
// available (this vertical's own dispatch, referencing vertical 2's
// close-delimited-body fix). A rejected `read()` mid-stream must become a
// typed `ErrorKind::Body`, never a quiet `Ready(None)`.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
async fn mid_stream_error_is_a_typed_body_error_not_a_quiet_end() {
    let calls = Rc::new(Cell::new(0u32));
    let calls_in_pull = calls.clone();
    let stream = stream_with_pull(move |controller| {
        let n = calls_in_pull.get();
        calls_in_pull.set(n + 1);
        if n == 0 {
            let _ = controller.enqueue_with_chunk(&Uint8Array::from(&b"partial"[..]));
        } else {
            let err: JsValue = js_sys::Error::new("simulated network failure").into();
            controller.error_with_e(&err);
        }
    });
    let resp = response_from_stream(&stream, &[]);
    let mut body = http_ng_fetch::testing::body_from_response(&resp).unwrap();

    let first = next_data(&mut body).await.unwrap().unwrap();
    assert_eq!(&first[..], b"partial");

    let err = next_data(&mut body)
        .await
        .expect("a mid-stream failure must produce a frame — Err, not a silent None")
        .unwrap_err();
    assert_eq!(
        err.kind(),
        &http_ng_core::ErrorKind::Body,
        "a stream read failure is ErrorKind::Body, not Decode and not the opaque Other: {err}"
    );

    // Honest afterward, too: a terminal state, not "one more None to be
    // safe" — and size_hint stops promising bytes that already failed to
    // arrive.
    assert!(http_body::Body::is_end_stream(&body));
    assert_eq!(http_body::Body::size_hint(&body).exact(), Some(0));
    assert!(
        next_data(&mut body).await.is_none(),
        "polling again after the error must stay a clean end, not error a second time or panic"
    );
}

// ---------------------------------------------------------------------
// Fix round 1, review finding 2: "what happens when the underlying
// response is aborted" was never tested — `Body::from_response` only ever
// sees a `web_sys::Response`, never an `AbortController`, so the abort
// path can't be reached from inside this crate's own construction API.
// The reviewer's probe (`.superpowers/sdd/2026-08-05-v01-fetch-and-
// acceptance/review-task-4-abort-probe.rs`) drove this with a REAL
// `fetch()` + `AbortController` against `httpbin.org/drip` and confirmed
// by execution (PASS, 2/2 runs) that aborting mid-stream rejects the
// response body's `ReadableStream` and lands as `ErrorKind::Body` — never
// a quiet `None`. That test is not committed here: this project does not
// ship tests that depend on a public host being reachable (the same
// reason the TEST-NET-address tests were removed earlier in this project,
// after turning out to be routable through this container's tunnel) — an
// offline build, a slow or gone host, or a flaky network would all fail
// this test for reasons that have nothing to do with a defect in our code.
//
// Per the WHATWG Fetch Standard's "abort a fetch request" algorithm
// (`https://fetch.spec.whatwg.org/#abort-fetch` — a fetch is aborted by
// erroring the response's `ReadableStream` with a `DOMException` named
// `"AbortError"`), the OBSERVABLE effect on `Body::poll_frame` is a
// rejected `read()` — exactly the same shape `stream_with_pull`'s
// `controller.error_with_e` already produces for
// `mid_stream_error_is_a_typed_body_error_not_a_quiet_end`, above.
// `src/body.rs` confirms this architecturally, not just by claim: its
// `Err(e) => Err(Error::new(ErrorKind::Body, StreamRead(js_message(&e))))`
// arm has no branch on WHY the read was rejected — an `AbortError` and a
// generic network failure take the identical path. This test reproduces
// the exact `DOMException` shape a real abort produces (name
// `"AbortError"`, the message Chrome actually uses) rather than the
// generic `js_sys::Error` the sibling test uses, so it specifically pins
// "an abort looks like this to us" rather than merely restating the
// sibling test under a new name.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
async fn aborting_is_a_typed_body_error_not_a_quiet_end() {
    let calls = Rc::new(Cell::new(0u32));
    let calls_in_pull = calls.clone();
    let stream = stream_with_pull(move |controller| {
        let n = calls_in_pull.get();
        calls_in_pull.set(n + 1);
        if n == 0 {
            let _ = controller.enqueue_with_chunk(&Uint8Array::from(&b"partial"[..]));
        } else {
            let abort_err = web_sys::DomException::new_with_message_and_name(
                "The user aborted a request.",
                "AbortError",
            )
            .expect("DomException::new_with_message_and_name never throws");
            let abort_err: JsValue = abort_err.into();
            controller.error_with_e(&abort_err);
        }
    });
    let resp = response_from_stream(&stream, &[]);
    let mut body = http_ng_fetch::testing::body_from_response(&resp).unwrap();

    let first = next_data(&mut body).await.unwrap().unwrap();
    assert_eq!(
        &first[..],
        b"partial",
        "the stream must be genuinely live before the abort, not already ended"
    );

    let err = next_data(&mut body)
        .await
        .expect("an abort mid-stream must produce a frame — Err, not a silent None")
        .unwrap_err();
    assert_eq!(
        err.kind(),
        &http_ng_core::ErrorKind::Body,
        "an aborted request is a transport failure like any other stream rejection — \
         ErrorKind::Body, not a quiet end and not a separate, uncategorized case: {err}"
    );
    assert!(http_body::Body::is_end_stream(&body));
    assert_eq!(http_body::Body::size_hint(&body).exact(), Some(0));
}

// ---------------------------------------------------------------------
// A chunk that isn't bytes is a DIFFERENT failure from a stream read
// failing — `ErrorKind::Decode`, not `ErrorKind::Body`. Mutation check
// (see the task report): collapsing both cases through one generic
// `js_err`/`ErrorKind::Other` mapping — the brief's own reference shape —
// makes this test and `mid_stream_error_is_a_typed_body_error_not_a_quiet_
// end` both fail their `kind()` assertion, and makes them indistinguishable
// from each other by `kind()` alone.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
async fn a_non_byte_chunk_is_a_typed_decode_error_distinct_from_a_stream_error() {
    let stream = stream_with_pull(|controller| {
        // Legal for a plain (non-"bytes"-typed) ReadableStream to enqueue
        // any JS value at all; a real fetch() response stream never does
        // this — deliberately hostile input exercising our own defensive
        // check, not a scenario ordinary network traffic reaches.
        let _ = controller.enqueue_with_chunk(&JsValue::from_str("not bytes"));
    });
    let resp = response_from_stream(&stream, &[]);
    let mut body = http_ng_fetch::testing::body_from_response(&resp).unwrap();

    let err = next_data(&mut body).await.unwrap().unwrap_err();
    assert_eq!(
        err.kind(),
        &http_ng_core::ErrorKind::Decode,
        "a non-byte chunk is ErrorKind::Decode, not ErrorKind::Body: {err}"
    );
    assert_ne!(
        err.kind(),
        &http_ng_core::ErrorKind::Body,
        "must be distinguishable from a stream read failure by kind() alone"
    );
}

// ---------------------------------------------------------------------
// `size_hint` — honest at construction, honest under a `Content-Encoding`
// that invalidates `Content-Length`, and honest once the stream has ended,
// even when it started with no promise to begin with (the exact defect
// class the wasi body shipped: a stale, over-promising hint that outlives
// the stream).
// ---------------------------------------------------------------------

/// Fix round 1, review finding 1: the ORIGINAL version of this test set
/// only `content-length`, never `content-encoding` — so the `"identity"`
/// match arm inside `content_length_hint` (`v.eq_ignore_ascii_case
/// ("identity")`) was covered by nothing, even though the test's own name
/// claimed it was. The reviewer proved this by making that arm never match
/// (e.g. comparing against a string other than `"identity"`) and finding
/// all ten tests still green. A server that explicitly sends
/// `Content-Encoding: identity` is legal and does happen (RFC 9110 §8.4.1
/// lists it as one of the registered content codings); this test now
/// actually sends it. `size_hint_reflects_content_length_when_encoding_
/// header_is_absent`, below, keeps the header genuinely omitted — a
/// distinct branch (`Ok(None)` vs `Ok(Some("identity"))` in
/// `content_length_hint`) worth its own name and its own coverage, not a
/// stand-in for this one.
#[wasm_bindgen_test]
fn size_hint_reflects_content_length_when_encoding_is_identity() {
    let stream = stream_with_pull(|controller| {
        let _ = controller.close();
    });
    let resp = response_from_stream(
        &stream,
        &[("content-length", "1234"), ("content-encoding", "identity")],
    );
    let body = http_ng_fetch::testing::body_from_response(&resp).unwrap();
    assert_eq!(http_body::Body::size_hint(&body).exact(), Some(1234));
}

/// The other trustworthy branch: no `Content-Encoding` header at all (the
/// common case for ordinary, uncompressed responses) — `content_length_
/// hint`'s `Ok(None) => true` arm, distinct from the `Ok(Some("identity"))`
/// arm the test above now actually exercises.
#[wasm_bindgen_test]
fn size_hint_reflects_content_length_when_encoding_header_is_absent() {
    let stream = stream_with_pull(|controller| {
        let _ = controller.close();
    });
    let resp = response_from_stream(&stream, &[("content-length", "1234")]);
    let body = http_ng_fetch::testing::body_from_response(&resp).unwrap();
    assert_eq!(http_body::Body::size_hint(&body).exact(), Some(1234));
}

#[wasm_bindgen_test]
fn size_hint_does_not_trust_content_length_under_content_encoding() {
    let stream = stream_with_pull(|controller| {
        let _ = controller.close();
    });
    let resp = response_from_stream(
        &stream,
        &[("content-length", "1234"), ("content-encoding", "gzip")],
    );
    let body = http_ng_fetch::testing::body_from_response(&resp).unwrap();
    assert_eq!(
        http_body::Body::size_hint(&body).exact(),
        None,
        "Content-Length describes the WIRE size under compression, not what this stream \
         actually yields once decoded — reporting it as exact here would be exactly the kind \
         of promise this project forbids a capability from making"
    );
}

#[wasm_bindgen_test]
async fn size_hint_becomes_honest_zero_once_the_stream_ends_even_without_a_content_length() {
    let stream = stream_with_pull(|controller| {
        let _ = controller.close();
    });
    let resp = response_from_stream(&stream, &[]);
    let mut body = http_ng_fetch::testing::body_from_response(&resp).unwrap();
    assert_eq!(
        http_body::Body::size_hint(&body).exact(),
        None,
        "no Content-Length was given — no promise should be made up front"
    );
    assert!(next_data(&mut body).await.is_none());
    assert_eq!(
        http_body::Body::size_hint(&body).exact(),
        Some(0),
        "honest at the end too, not just at the start"
    );
}

// ---------------------------------------------------------------------
// Cancellation: dropping a `Body` before it's exhausted must actually
// reach the underlying source's own `cancel()` callback, not merely rely
// on `wasm-streams` claiming to call it.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
async fn dropping_a_pending_body_cancels_the_underlying_reader() {
    let cancelled = Rc::new(Cell::new(false));
    let stream = stream_with_pull_and_cancel(
        |controller| {
            let _ = controller.enqueue_with_chunk(&Uint8Array::from(&b"x"[..]));
        },
        cancelled.clone(),
    );
    let resp = response_from_stream(&stream, &[]);
    let mut body = http_ng_fetch::testing::body_from_response(&resp).unwrap();

    // Consume the one available chunk, leaving the reader locked but idle —
    // the same shape as a caller that stops reading a still-open response
    // body early (an SSE consumer losing interest, say).
    let _ = next_data(&mut body).await;
    assert!(
        !cancelled.get(),
        "must not be cancelled before the Body is even dropped"
    );

    drop(body);

    // `cancel()` on the reader is promise-based (Streams §4.6), so it may
    // take more than one microtask to actually invoke the underlying
    // source's own `cancel` callback. Bounded polling, not a fixed sleep —
    // and bounded, not unbounded, per this project's "every wait bounded"
    // rule.
    for _ in 0..20 {
        if cancelled.get() {
            break;
        }
        microtask_tick().await;
    }
    assert!(
        cancelled.get(),
        "dropping a Body mid-stream must release interest in it — the underlying source's own \
         cancel() must actually run, not just wasm-streams's internal reader lock"
    );
}
