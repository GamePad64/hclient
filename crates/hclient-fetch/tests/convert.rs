#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use bytes::Bytes;
use hclient_core::RequestBody;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

// ---------------------------------------------------------------------
// Brief's own three tests, verbatim (task-3-brief.md, Step 1).
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn rejects_a_forbidden_header_instead_of_dropping_it() {
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("host", "evil.example")
        .body(RequestBody::Empty)
        .unwrap();
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("host"), "{err}");
}

#[wasm_bindgen_test]
fn ordinary_headers_pass_through() {
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("x-custom", "v")
        .body(RequestBody::Empty)
        .unwrap();
    assert!(hclient_fetch::testing::to_web_request(&f, req).is_ok());
}

/// The brief's original guard, now that the field it reads is real. It used
/// to bail out early on `streaming_request_body == true`, a branch the
/// hardcoded `false` made unreachable in every browser; since v0.2 W6 that
/// branch is the one Chrome takes, so both sides are asserted instead of one
/// being skipped.
///
/// This is the only streaming test that runs against the REAL probe rather
/// than a caller-supplied `Capabilities`, which is what makes it worth
/// having: it checks that `Fetch::new()`'s own capabilities and
/// `to_web_request`'s own branch agree in whichever browser is running. The
/// two arms below then exercise each side deterministically, in both.
#[wasm_bindgen_test]
fn conversion_of_a_streaming_body_follows_this_browsers_real_capability() {
    let f = hclient_fetch::Fetch::new();
    let streams = f.capabilities_for_test().streaming_request_body;
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(streaming_body())
        .unwrap();
    let outcome = hclient_fetch::testing::to_web_request(&f, req);
    assert_eq!(
        outcome.is_ok(),
        streams,
        "a browser whose probe says it streams must get a built Request, and one whose probe \
         says it would corrupt the body must get a typed error — never the other way round"
    );
    if let Err(e) = outcome {
        assert!(
            matches!(e.kind(), hclient_core::ErrorKind::Unsupported),
            "{e}"
        );
    }
}

// ---------------------------------------------------------------------
// Test fixtures: minimal `http_body::Body` impls, the same shape as
// `hclient-core/src/body.rs`'s own `EmptyStream` test fixture.
// ---------------------------------------------------------------------

/// Never produces a frame — enough to exist as `RequestBody::Streaming`'s
/// payload for tests that only care whether the conversion accepted or
/// refused it, not what came out the other end.
struct NeverPolled;
impl http_body::Body for NeverPolled {
    type Data = Bytes;
    type Error = hclient_core::Error;
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

/// Yields its chunks one frame per poll, and counts the polls.
///
/// The counter is what distinguishes a stream from a copy: it is `Arc<
/// AtomicUsize>` rather than `Rc<Cell<..>>` because `RequestBody::Streaming`
/// is bounded `+ Send` (amendment C2), not because anything here is
/// concurrent — this crate runs on a wasm32 target without
/// `target_feature = "atomics"`, so there is exactly one thread.
struct Counted {
    chunks: Vec<&'static [u8]>,
    next: usize,
    polls: Arc<AtomicUsize>,
}
impl http_body::Body for Counted {
    type Data = Bytes;
    type Error = hclient_core::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        let me = self.get_mut();
        me.polls.fetch_add(1, Ordering::Relaxed);
        match me.chunks.get(me.next) {
            Some(c) => {
                me.next += 1;
                Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from_static(c)))))
            }
            None => Poll::Ready(None),
        }
    }
}

fn counted_body(chunks: Vec<&'static [u8]>) -> (RequestBody, Arc<AtomicUsize>) {
    let polls = Arc::new(AtomicUsize::new(0));
    let body = Counted {
        chunks,
        next: 0,
        polls: Arc::clone(&polls),
    };
    (RequestBody::Streaming(Box::new(body)), polls)
}

/// One data frame, then a trailers frame — the shape fetch has nowhere to
/// put (`Capabilities::request_trailers == false`).
struct WithTrailers(bool);
impl http_body::Body for WithTrailers {
    type Data = Bytes;
    type Error = hclient_core::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        let me = self.get_mut();
        if me.0 {
            return Poll::Ready(None);
        }
        me.0 = true;
        let mut t = http::HeaderMap::new();
        t.insert("x-checksum", http::HeaderValue::from_static("deadbeef"));
        Poll::Ready(Some(Ok(http_body::Frame::trailers(t))))
    }
}

/// `Capabilities` shaped like a browser that streams, for the arm the local
/// browser may not be. Mirrors what `caps::probe()` sets, minus the fields
/// no conversion here reads.
fn caps_that_stream() -> hclient_core::Capabilities {
    let mut caps = hclient_core::Capabilities::none();
    caps.streaming_request_body = true;
    caps.forbidden_request_headers = &hclient_fetch::FORBIDDEN_HEADERS;
    caps
}

/// Whether the browser running this suite genuinely sends a stream, per the
/// crate's own probe.
///
/// **The test seam can force this crate's code path; it cannot force the
/// browser to behave.** `to_web_request_with_caps` makes `to_web_request`
/// take the streaming branch in any engine, and that is enough to test
/// every decision this crate makes. It is not enough to test what comes out
/// the far side, because the far side is the browser: hand Firefox 153 a
/// `ReadableStream` and it stringifies it whatever we passed in `caps`.
///
/// Measured here, live, in both engines (the same two facts the server-side
/// harness in `docs/measurements/w6-request-streams/` recorded from
/// outside):
///
/// | | `Request.body` for a Full body | for a stream body | invented `Content-Type` |
/// |---|---|---|---|
/// | Chrome 151 | `Some` | `Some` | no |
/// | Firefox 153 | `None` | `None` | `text/plain;charset=UTF-8` |
///
/// Note Firefox's first column: `Request.prototype.body` is simply absent
/// there, for **any** body. So `body().is_none()` on Firefox is not evidence
/// of the corruption — it is evidence of nothing — which is why the
/// Firefox-side assertion below reads the invented `Content-Type` and the
/// stringified text instead, and why the drain-based tests gate on this
/// function rather than trying to assert something weaker everywhere.
///
/// Reads the raw probe rather than `Capabilities::streaming_request_body`,
/// deliberately. This is a question about the ENGINE — "can these
/// assertions be made here at all" — and it must stay true even while the
/// capability derived from it is what a mutation moves. Gating on the
/// capability would let `probe()` be frozen to `true` and quietly skip the
/// corruption test on Firefox, which is exactly the mutation the suite has
/// to catch.
fn browser_streams() -> bool {
    hclient_fetch::testing::supports_streaming_request_body_for_test()
}

/// The corruption itself, reproduced in whichever browser has it — the
/// in-browser counterpart of the server-side measurement, and the reason
/// `streaming_request_body` may not simply be hardcoded `true`.
///
/// Forces `caps.streaming_request_body = true` on a browser whose own probe
/// says `false`, i.e. deliberately does the wrong thing, and shows what the
/// wrong thing costs: the caller's `AAAA` never appears, the request carries
/// the 23-byte string `[object ReadableStream]`, and the browser stamps
/// `Content-Type: text/plain;charset=UTF-8` on it because that is now a text
/// body. No error is raised anywhere — which is the whole problem.
///
/// This is the one place in the file that reads a body back with
/// `Request::text()` rather than the raw stream, and it is the one place
/// where that is the right instrument: the question is what the BROWSER did
/// to the value, not what this crate's adapter produced. The bytes it
/// reports match `results/report-firefox-h2.json`'s `bytesHex`
/// (`5b6f626a656374205265616461626c6553747265616d5d`) recorded by a Node
/// server on the other end of a real socket.
///
/// On a browser that streams, there is no corruption to demonstrate and the
/// test returns early — its counterpart
/// `the_callers_frames_reach_the_browser_as_separate_chunks` is what runs
/// there instead. Exactly one of the two does real work in any given engine,
/// by construction, and both count toward the `browser` job's minimum in
/// either.
#[wasm_bindgen_test]
async fn a_browser_that_stringifies_replaces_the_body_with_no_error_at_all() {
    if browser_streams() {
        return;
    }
    let (body, _polls) = counted_body(vec![b"AAAA"]);
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(body)
        .unwrap();
    // No error, in either the conversion or the browser — that is the point.
    let (request, _controller) =
        hclient_fetch::testing::to_web_request_with_caps(req, &caps_that_stream()).unwrap();

    assert_eq!(
        request
            .headers()
            .get("Content-Type")
            .ok()
            .flatten()
            .as_deref(),
        Some("text/plain;charset=UTF-8"),
        "a browser that stringifies the stream stamps the content type of the string it just \
         invented — this is the fingerprint the probe keys on"
    );
    let text = hclient_fetch::testing::send_js_future(request.text().unwrap())
        .await
        .expect("reading the body back must not reject")
        .as_string()
        .unwrap_or_default();
    assert_eq!(
        text, "[object ReadableStream]",
        "the caller's bytes are gone and USVString conversion has taken their place; sending \
         this would be a silently corrupted upload, which is why the capability must be false \
         here"
    );
}

/// Drains a built `web_sys::Request`'s body stream chunk by chunk.
///
/// **Reads the raw `ReadableStream`, deliberately not `Request::text()`.**
/// `text()` would answer a different question — it concatenates, so it
/// cannot tell three chunks from one buffer, which is the only thing these
/// tests are for. It is also the wrong instrument here for a second reason:
/// it consumes the body through the browser's own machinery and reports its
/// own `TypeError` when that machinery objects, so a failure in the adapter
/// under test and a failure in the observer look identical. The raw reader
/// puts the observer outside the thing being observed.
async fn drain(request: &web_sys::Request) -> Vec<Vec<u8>> {
    use futures_util::StreamExt;
    let raw = request
        .body()
        .expect("a streamed request must expose a body");
    let mut stream = wasm_streams::ReadableStream::from_raw(raw).into_stream();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("the request body stream must not error");
        out.push(js_sys::Uint8Array::new(&chunk).to_vec());
    }
    out
}

// ---------------------------------------------------------------------
// Both arms of `caps.streaming_request_body`, exercised deterministically
// through `to_web_request_with_caps` so that each runs in BOTH browsers
// rather than only in the one whose real probe happens to produce it.
// Chrome's real probe says `true` and Firefox's says `false`; without this
// seam, half of this file's coverage would be dark on each engine.
//
// Until v0.2 W6 both arms ended in the same typed error, because nothing
// here could build a `ReadableStream`. Now the `true` arm builds one, and
// what makes that safe is that the field it reads is whatwg/fetch#1470's
// behavioural probe rather than the `duplex` presence check — see the
// module doc comment on `src/convert.rs`, point 3.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn streaming_body_is_rejected_when_the_browser_would_corrupt_it() {
    let mut caps = hclient_core::Capabilities::none();
    caps.streaming_request_body = false;
    caps.forbidden_request_headers = &hclient_fetch::FORBIDDEN_HEADERS;
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(streaming_body())
        .unwrap();
    let err = hclient_fetch::testing::to_web_request_with_caps(req, &caps).unwrap_err();
    assert!(
        matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
        "{err}"
    );
    // Names the capability, so a caller reading the message learns which
    // one to consult rather than only that something was unsupported.
    assert!(err.to_string().contains("streaming_request_body"), "{err}");
}

/// The mirror of the test above, and the one that would have been
/// impossible to write before W6: with the capability on, the conversion
/// must **succeed** and the built `Request` must actually carry a body.
///
/// `body().is_some()` rather than `is_ok()` alone: a conversion that
/// quietly dropped the stream and built a bodyless `Request` would satisfy
/// `is_ok()`, and that is precisely the silent-empty-body defect this
/// file's neighbours were written against.
#[wasm_bindgen_test]
fn streaming_body_is_built_when_the_browser_genuinely_streams() {
    if !browser_streams() {
        return; // see `browser_streams` — `Request.body` does not exist here
    }
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(streaming_body())
        .unwrap();
    let (request, _controller) =
        hclient_fetch::testing::to_web_request_with_caps(req, &caps_that_stream())
            .expect("a browser that streams must get a built Request, not a typed refusal");
    assert!(
        request.body().is_some(),
        "the built Request must carry the stream — an Ok() with no body is the silent \
         empty-body defect, not a success"
    );
}

// ---------------------------------------------------------------------
// The two properties that separate a stream from a copy. A test that only
// checked "the conversion returned Ok" would pass for an implementation
// that drained the caller's body into a buffer at construction time and
// handed fetch a `Uint8Array` — which is exactly what W6 was asked NOT to
// build.
//
// The observer that settles this end to end is a SERVER, and it lives in
// `docs/measurements/w6-request-streams/`: Chrome 151 over h2 delivered
// the three 4-byte chunks in three separate DATA frames at t =
// 294/594/894 ms. That harness cannot run in CI — wasm-bindgen's test
// server speaks HTTP/1.1, and a stream body over HTTP/1.1 is exactly the
// case Chrome refuses in 3 ms with nothing on the wire. So the in-CI
// proof is taken one layer below the socket, at the `ReadableStream` the
// browser would be pulling from, which is the last place this crate's
// code is still responsible for the answer.
// ---------------------------------------------------------------------

/// Laziness: building the `Request` must not consume the body.
///
/// `wasm_streams::ReadableStream::from_stream` sets the queuing strategy's
/// high-water mark to 0, so nothing is pulled until the consumer asks. If
/// that ever changed — or if someone "simplified" the `Streaming` arm by
/// collecting the body first — this counter would be non-zero and the
/// request would no longer be streamed, however green everything else
/// stayed.
#[wasm_bindgen_test]
fn a_streaming_body_is_not_drained_when_the_request_is_built() {
    if !browser_streams() {
        return; // see `browser_streams` — `Request.body` does not exist here
    }
    let (body, polls) = counted_body(vec![b"AAAA", b"BBBB", b"CCCC"]);
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(body)
        .unwrap();
    let (request, _controller) =
        hclient_fetch::testing::to_web_request_with_caps(req, &caps_that_stream()).unwrap();
    assert!(request.body().is_some());
    assert_eq!(
        polls.load(Ordering::Relaxed),
        0,
        "the caller's body was polled while the Request was merely being CONSTRUCTED — it is \
         being buffered, not streamed"
    );
}

/// Chunk boundaries: the caller's frames must reach the browser as separate
/// chunks, and unchanged.
///
/// This is the assertion `Request::text()` could not make — it concatenates,
/// so `AAAABBBBCCCC` would read back identically from one buffered chunk and
/// from three streamed ones. Comparing the vector of chunks pins both the
/// bytes and the framing.
#[wasm_bindgen_test]
async fn the_callers_frames_reach_the_browser_as_separate_chunks() {
    if !browser_streams() {
        return; // see `browser_streams` — `Request.body` does not exist here
    }
    let (body, polls) = counted_body(vec![b"AAAA", b"BBBB", b"CCCC"]);
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(body)
        .unwrap();
    let (request, _controller) =
        hclient_fetch::testing::to_web_request_with_caps(req, &caps_that_stream()).unwrap();

    let chunks = drain(&request).await;
    assert_eq!(
        chunks,
        vec![b"AAAA".to_vec(), b"BBBB".to_vec(), b"CCCC".to_vec()],
        "three frames in must be three chunks out, byte for byte — one 12-byte chunk would mean \
         the adapter concatenated them and the body is not streamed"
    );
    // Polled on demand and not more than the frames required: three data
    // frames plus the one poll that returns `None` to end the stream.
    assert_eq!(polls.load(Ordering::Relaxed), 4);
}

/// A body that fails mid-stream errors the `ReadableStream` rather than
/// ending it — a truncated-but-successful upload is the one outcome that
/// must not be possible.
///
/// The caller's typed error cannot cross into JS as a value, so it crosses
/// as its `Display` text; the assertion is that the stream errors at all
/// and that the text survives, not that the category does (module doc
/// comment on `src/convert.rs`, point 4).
#[wasm_bindgen_test]
async fn a_body_that_fails_mid_stream_errors_the_stream_instead_of_ending_it() {
    use futures_util::StreamExt;

    if !browser_streams() {
        return; // see `browser_streams` — `Request.body` does not exist here
    }

    struct FailsAfterOne(bool);
    impl http_body::Body for FailsAfterOne {
        type Data = Bytes;
        type Error = hclient_core::Error;
        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
            let me = self.get_mut();
            if me.0 {
                return Poll::Ready(Some(Err(hclient_core::Error::new(
                    hclient_core::ErrorKind::Other,
                    std::io::Error::other("the-producer-gave-up"),
                ))));
            }
            me.0 = true;
            Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from_static(
                b"AAAA",
            )))))
        }
    }

    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::Streaming(Box::new(FailsAfterOne(false))))
        .unwrap();
    let (request, _controller) =
        hclient_fetch::testing::to_web_request_with_caps(req, &caps_that_stream()).unwrap();

    let raw = request.body().unwrap();
    let mut stream = wasm_streams::ReadableStream::from_raw(raw).into_stream();
    let first = stream.next().await.expect("the first frame must arrive");
    assert_eq!(
        js_sys::Uint8Array::new(&first.expect("the first frame is not the failing one")).to_vec(),
        b"AAAA".to_vec()
    );
    let second = stream
        .next()
        .await
        .expect("the stream must yield an error, not simply end");
    let err = second.expect_err("a failed body must error the stream, never close it cleanly");
    let text = js_sys::Reflect::get(&err, &wasm_bindgen::JsValue::from_str("message"))
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_default();
    assert!(
        text.contains("the-producer-gave-up"),
        "the caller's own message must survive into the JS error a human will read: {text:?}"
    );
}

/// Trailers have nowhere to go — fetch has no request trailers in any form
/// — so they error the stream rather than being dropped and the rest sent.
///
/// This is the one guard that cannot run before `to_web_request` returns:
/// a trailers frame arrives after the last data frame, by which point the
/// browser owns the stream. Same shape as the trailers guard `hclient-wasi`
/// moved into its `Body`.
#[wasm_bindgen_test]
async fn trailers_error_the_stream_rather_than_being_silently_dropped() {
    use futures_util::StreamExt;

    if !browser_streams() {
        return; // see `browser_streams` — `Request.body` does not exist here
    }
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::Streaming(Box::new(WithTrailers(false))))
        .unwrap();
    let (request, _controller) =
        hclient_fetch::testing::to_web_request_with_caps(req, &caps_that_stream()).unwrap();

    let raw = request.body().unwrap();
    let mut stream = wasm_streams::ReadableStream::from_raw(raw).into_stream();
    let first = stream
        .next()
        .await
        .expect("the trailers frame must surface as an error, not as end-of-stream");
    let err = first.expect_err("a trailers frame must error the stream");
    let text = js_sys::Reflect::get(&err, &wasm_bindgen::JsValue::from_str("message"))
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_default();
    assert!(
        text.contains("trailers"),
        "the error must name trailers as the cause: {text:?}"
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
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(|| {
            RequestBody::Full(Bytes::from_static(b"hello"))
        }))
        .unwrap();
    assert!(hclient_fetch::testing::to_web_request(&f, req).is_ok());
}

/// The mutation this test exists to catch: reverting `resolve_body`'s
/// `RequestBody::Rewindable(f) => body = f()` arm plus its `Streaming`
/// arm back to the brief's own reference shape —
/// `RequestBody::Rewindable(f) => match f() { RequestBody::Full(b) => ..,
/// _ => {} }` — makes a `Rewindable` wrapping `Streaming` silently resolve
/// to an empty, successfully-sent body instead of reaching the `Streaming`
/// arm at all. That is exactly the defect vertical 2's native `body.rs`
/// shipped and review caught.
///
/// Pinned against `streaming_request_body = false` since v0.2 W6, which is
/// what keeps it sensitive to that mutation in **both** browsers: the
/// silently-emptied body is `Ok` and the correctly-unwrapped one is a typed
/// `Unsupported`, so the two are still distinguishable. Under the `true`
/// capability they both come back `Ok` and only the drained bytes tell them
/// apart — which is
/// `a_rewindable_wrapping_streaming_is_unwrapped_all_the_way_to_the_stream`
/// below, the same mutation caught from the other side.
#[wasm_bindgen_test]
fn rewindable_wrapping_streaming_is_rejected_not_silently_emptied() {
    let mut caps = hclient_core::Capabilities::none();
    caps.streaming_request_body = false;
    caps.forbidden_request_headers = &hclient_fetch::FORBIDDEN_HEADERS;
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(streaming_body))
        .unwrap();
    let err = hclient_fetch::testing::to_web_request_with_caps(req, &caps).unwrap_err();
    assert!(
        matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
        "{err}"
    );
}

/// The same defect from the capability-on side: a `Rewindable` whose
/// factory returns a `Streaming` must be unwrapped all the way down and the
/// resulting stream must carry the caller's bytes.
///
/// Under the partial-match mutation this conversion still returns `Ok` —
/// an empty `POST` body is a legal request — so only reading the chunks
/// back distinguishes correct from silently-emptied here.
#[wasm_bindgen_test]
async fn a_rewindable_wrapping_streaming_is_unwrapped_all_the_way_to_the_stream() {
    if !browser_streams() {
        return; // see `browser_streams` — `Request.body` does not exist here
    }
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(|| {
            RequestBody::Streaming(Box::new(Counted {
                chunks: vec![b"XX", b"YY"],
                next: 0,
                polls: Arc::new(AtomicUsize::new(0)),
            }))
        }))
        .unwrap();
    let (request, _controller) =
        hclient_fetch::testing::to_web_request_with_caps(req, &caps_that_stream()).unwrap();
    assert_eq!(
        drain(&request).await,
        vec![b"XX".to_vec(), b"YY".to_vec()],
        "the rewind factory's Streaming body must reach the browser intact, not be flattened \
         to an empty body by a partial match"
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
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(|| {
            RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"deep")))
        }))
        .unwrap();
    let (request, _controller) = hclient_fetch::testing::to_web_request(&f, req).unwrap();
    let text_promise = request.text().expect("text() must not throw");
    let text = hclient_fetch::testing::send_js_future(text_promise)
        .await
        .expect("reading the body back must not reject");
    assert_eq!(text.as_string().as_deref(), Some("deep"));
}

#[wasm_bindgen_test]
fn a_factory_that_never_bottoms_out_is_a_bounded_error_not_a_hang() {
    let f = hclient_fetch::Fetch::new();
    fn infinite() -> RequestBody {
        RequestBody::rewindable(infinite)
    }
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(infinite))
        .unwrap();
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
    // Not `Unsupported`: this isn't fetch declining the request, it's this
    // conversion refusing to keep unwrapping — the same category `hclient-
    // wasi`'s analogous `RewindTooDeep` uses.
    assert!(
        !matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
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
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(nested_rewindable(15, b"fifteen-deep"))
        .unwrap();
    let (request, _controller) = hclient_fetch::testing::to_web_request(&f, req).unwrap();
    let text_promise = request.text().expect("text() must not throw");
    let text = hclient_fetch::testing::send_js_future(text_promise)
        .await
        .expect("reading the body back must not reject");
    assert_eq!(text.as_string().as_deref(), Some("fifteen-deep"));
}

/// One layer past the ceiling above: `RewindTooDeep`, not a hang and not a
/// silently emptied body.
#[wasm_bindgen_test]
fn rewindable_nested_one_past_the_ceiling_is_a_typed_error() {
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(nested_rewindable(16, b"sixteen-deep"))
        .unwrap();
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        !matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
        "{err}"
    );
}

// ---------------------------------------------------------------------
// GET/HEAD cannot carry a body — fetch throws a TypeError for this;
// caught ahead of time as a typed error instead of an opaque one.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn empty_body_is_fine_on_get() {
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("GET")
        .uri("https://example.com/")
        .body(RequestBody::Empty)
        .unwrap();
    assert!(hclient_fetch::testing::to_web_request(&f, req).is_ok());
}

#[wasm_bindgen_test]
fn nonempty_body_on_get_is_rejected_not_silently_sent() {
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("GET")
        .uri("https://example.com/")
        .body(RequestBody::Full(Bytes::from_static(b"nope")))
        .unwrap();
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("GET"), "{err}");
}

/// The same rule for a body whose length nobody knows yet.
///
/// W6 widened this guard from `matches!(resolved, ResolvedBody::Full(_))`
/// to "any arm that is not `None`", and this test is what holds the widened
/// form in place: a `RequestBody::Streaming` on a `GET` is still a body as
/// far as the `Request` constructor is concerned, and fetch throws on it
/// however the body is expressed. Under the pre-W6 shape this conversion
/// would succeed and the browser would throw later, at a point where the
/// error is no longer ours to type.
///
/// Runs against `caps_that_stream()`, so it exercises the guard in both
/// engines rather than only where the real probe says `true` — otherwise
/// the streaming arm would exit at the capability check above and never
/// reach the method check at all.
#[wasm_bindgen_test]
fn a_streaming_body_on_get_is_rejected_just_as_a_buffered_one_is() {
    let req = http::Request::builder()
        .method("GET")
        .uri("https://example.com/")
        .body(streaming_body())
        .unwrap();
    let err =
        hclient_fetch::testing::to_web_request_with_caps(req, &caps_that_stream()).unwrap_err();
    assert!(
        matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("GET"), "{err}");
}

#[wasm_bindgen_test]
fn nonempty_body_on_head_is_rejected_not_silently_sent() {
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("HEAD")
        .uri("https://example.com/")
        .body(RequestBody::Full(Bytes::from_static(b"nope")))
        .unwrap();
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
    // Not just `.is_err()`: the browser's own `Request` constructor throws
    // for GET/HEAD-with-body too, so a bare `.is_err()` check would still
    // pass with our own ahead-of-time check removed — it just wouldn't be
    // `Unsupported` anymore, only the opaque `Other` `js_err` produces.
    assert!(
        matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(err.to_string().contains("HEAD"), "{err}");
}

// ---------------------------------------------------------------------
// The URL itself.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn non_http_scheme_is_a_typed_unsupported_error_not_opaque() {
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("ftp://example.com/")
        .body(RequestBody::Empty)
        .unwrap();
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
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
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("/relative")
        .body(RequestBody::Empty)
        .unwrap();
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
        "{err}"
    );
}

// ---------------------------------------------------------------------
// Header values that can't be represented as a JS string.
// ---------------------------------------------------------------------

#[wasm_bindgen_test]
fn non_ascii_header_value_is_a_typed_unsupported_error_not_opaque() {
    let f = hclient_fetch::Fetch::new();
    let value = http::HeaderValue::from_bytes(&[0xff, 0xfe])
        .expect("obs-text is a legal byte range for HeaderValue");
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("x-binary", value)
        .body(RequestBody::Empty)
        .unwrap();
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
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
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("host", "evil.example")
        .body(RequestBody::Empty)
        .unwrap();
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        err.to_string().contains("fetch forbids setting"),
        "expected the fast check_headers rejection wording, got: {err}"
    );
}

#[wasm_bindgen_test]
fn forbidden_header_rejection_names_the_actual_header() {
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("cookie", "a=b")
        .body(RequestBody::Empty)
        .unwrap();
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
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
    let f = hclient_fetch::Fetch::new();
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
    let err = hclient_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(
        matches!(err.kind(), hclient_core::ErrorKind::Unsupported),
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
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/path?q=1")
        .header("x-custom", "value")
        .body(RequestBody::Full(Bytes::from_static(b"payload")))
        .unwrap();
    let (request, _controller) = hclient_fetch::testing::to_web_request(&f, req).unwrap();

    assert_eq!(request.method(), "POST");
    assert_eq!(request.url(), "https://example.com/path?q=1");
    assert_eq!(
        request.headers().get("x-custom").unwrap().as_deref(),
        Some("value")
    );

    let text_promise = request
        .text()
        .expect("text() must not throw on a buffered body");
    let text = hclient_fetch::testing::send_js_future(text_promise)
        .await
        .expect("reading the body back must not reject");
    assert_eq!(text.as_string().as_deref(), Some("payload"));
}

#[wasm_bindgen_test]
async fn rewound_body_bytes_actually_reach_the_constructed_request() {
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://example.com/")
        .body(RequestBody::rewindable(|| {
            RequestBody::Full(Bytes::from_static(b"rewound-bytes"))
        }))
        .unwrap();
    let (request, _controller) = hclient_fetch::testing::to_web_request(&f, req).unwrap();
    // `body_used` must still be false — we constructed the request, we
    // haven't consumed it yet by reading `.text()` below.
    assert!(!request.body_used());
    let text_promise = request.text().expect("text() must not throw");
    let text = hclient_fetch::testing::send_js_future(text_promise)
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
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .body(RequestBody::Empty)
        .unwrap();
    let (request, controller) = hclient_fetch::testing::to_web_request(&f, req).unwrap();
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
    let mut caps = hclient_core::Capabilities::none();
    caps.forbidden_request_headers = &FORBIDDEN;

    let mut h = http::HeaderMap::new();
    h.insert(http::header::AUTHORIZATION, "secret".parse().unwrap());
    assert!(hclient_fetch::testing::check_headers(&h, &caps).is_err());

    let mut h = http::HeaderMap::new();
    h.insert("x-other", "fine".parse().unwrap());
    assert!(hclient_fetch::testing::check_headers(&h, &caps).is_ok());
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
    let f = hclient_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("x-padded", "  padded value  ")
        .body(RequestBody::Empty)
        .unwrap();
    // Must succeed: `verify_headers_survived` only checks the name
    // `x-padded` is present, which it is — the whitespace trim is not an
    // error condition this check is meant to catch.
    let (request, _controller) = hclient_fetch::testing::to_web_request(&f, req).unwrap();
    // And the trim genuinely happened — this isn't merely "didn't error",
    // the value on the wire really is different from what was set.
    assert_eq!(
        request.headers().get("x-padded").unwrap().as_deref(),
        Some("padded value"),
        "the browser must have trimmed the surrounding whitespace"
    );
}
