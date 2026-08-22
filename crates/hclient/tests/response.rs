//! Tests for `Response`/`Collected`/`RequestBuilder` at the `Client` level.

// `hclient::mock` lives behind the `test-util` feature (see `mock.rs`);
// without this line `cargo test -p hclient` with no flags used to fail
// with E0432 instead of compiling down to nothing — the same fix already
// made for `shape.rs` in Task 12. Task 13 fix round 2, Residual 3.
#![cfg(feature = "test-util")]

use hclient::mock::MockTransport;
use hclient::{Client, RequestBody};

#[test]
fn collected_keeps_status_and_headers_after_reading_the_body() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(201)
            .header("x-trace", "abc")
            .body("hello")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    let collected = futures_executor::block_on(resp.collect()).unwrap();
    assert_eq!(collected.text().unwrap(), "hello");
    // The key difference from reqwest, where `.text()` takes self by
    // value: status/headers/url must stay readable AFTER the body is read.
    assert_eq!(collected.status(), 201);
    assert_eq!(collected.headers().get("x-trace").unwrap(), "abc");
    assert_eq!(
        collected.url(),
        &"https://a/x".parse::<http::Uri>().unwrap()
    );
}

/// Queues a two-frame response and checks that `chunk()` hands back the
/// frames separately, in the original order — not one concatenated block.
/// A single-frame version of this test would also pass with an
/// implementation that reads the whole body in the first `poll_frame`
/// call, which wouldn't actually prove streaming: see
/// `MockTransport::push_response_frames` in mock.rs.
#[test]
fn chunk_streams_the_body_frame_by_frame_not_concatenated() {
    let m = MockTransport::new();
    m.push_response_frames(
        http::Response::builder()
            .status(200)
            .body(vec!["stream ", "me"])
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let mut resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    let first = futures_executor::block_on(resp.chunk())
        .expect("first frame must be present")
        .unwrap();
    assert_eq!(
        &first[..],
        b"stream ",
        "first chunk() call must yield only the first frame, not the whole body"
    );

    let second = futures_executor::block_on(resp.chunk())
        .expect("second frame must be present")
        .unwrap();
    assert_eq!(&second[..], b"me");

    assert!(
        futures_executor::block_on(resp.chunk()).is_none(),
        "no third frame was queued"
    );
}

#[test]
fn request_builder_sets_method_and_headers() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let _ = futures_executor::block_on(
        c.post("https://a/x")
            .header("x-k", "v")
            .body(RequestBody::Full(bytes::Bytes::from_static(b"p")))
            .send(),
    )
    .unwrap();

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(seen[0].method, http::Method::POST);
    assert_eq!(seen[0].headers.get("x-k").unwrap(), "v");
}

/// `request_builder_sets_method_and_headers` only proves POST; every verb
/// on the client must set its own specific method, rather than reusing the
/// same request-building path that just happens to be correct only for
/// POST.
#[test]
fn get_sends_the_get_method() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let _ = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()[0]
            .method,
        http::Method::GET
    );
}

#[test]
fn delete_sends_the_delete_method() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let _ = futures_executor::block_on(c.delete("https://a/x").send()).unwrap();

    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()[0]
            .method,
        http::Method::DELETE
    );
}

/// `Collected::json` wasn't part of step 3's code in the brief, but is
/// declared in this task's Interfaces section ("`Collected::json<T>()`, and
/// preserves status/headers/url"). Implemented per the Interfaces
/// contract; see the task report.
///
/// Behind the `json` feature: the method itself is `#[cfg(feature =
/// "json")]`-gated, so this test must be gated the same way — otherwise
/// `cargo test -p hclient --features test-util` (without `json`) won't
/// compile.
#[cfg(feature = "json")]
#[test]
fn collected_json_decodes_the_body_and_still_keeps_status() {
    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Payload {
        ok: bool,
        n: u32,
    }

    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(200)
            .body(r#"{"ok":true,"n":7}"#)
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();
    let collected = futures_executor::block_on(resp.collect()).unwrap();

    let payload: Payload = collected.json().unwrap();
    assert_eq!(payload, Payload { ok: true, n: 7 });
    assert_eq!(collected.status(), 200, "json() must not consume status");
}

/// `RequestBuilder::timeouts` must put `Timeouts` into the request's
/// `Extensions`, where the transport reads them from. Without
/// writing to `extensions`, this setter would be a silent no-op — exactly
/// the class of defect the crate's design tries to avoid.
///
/// This test does NOT check the "request-first, client-fallback" lookup
/// itself (spec §4.5) — an earlier version of this comment claimed it did,
/// but couldn't have: the client here sets no timeouts at all, so there's
/// nothing to override. The composition of client and request lives in
/// `tests/timeouts.rs` (B1 of the branch's final review — before it, this
/// didn't exist in the code either).
///
/// `with_capabilities` isn't decoration: since M3, `Client::execute`
/// checks the merged timeouts against `Capabilities`, and a mock with
/// `Capabilities::none()` now honestly rejects this request.
#[test]
fn timeouts_are_placed_in_extensions_where_the_transport_reads_them() {
    use hclient_core::Timeouts;
    use std::time::Duration;

    let mut caps = hclient::caps::Capabilities::none();
    caps.timeouts = hclient::caps::TimeoutSupport {
        resolve: false,
        connect: true,
        first_byte: true,
        between_bytes: true,
    };
    let m = MockTransport::new().with_capabilities(caps);
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let _ = futures_executor::block_on(
        c.get("https://a/x")
            .timeouts(Timeouts {
                resolve: None,
                connect: Some(Duration::from_secs(3)),
                ..Default::default()
            })
            .send(),
    )
    .unwrap();

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    let t = seen[0]
        .extensions
        .get::<Timeouts>()
        .expect("Timeouts set via RequestBuilder::timeouts must reach the transport");
    assert_eq!(t.connect, Some(Duration::from_secs(3)));
}

/// The brief's `header()` dropped an invalid pair silently (`if let
/// (Ok(n), Ok(v)) = .. { .. }`, no `else`) — the exact silent no-op
/// `ClientBuilder::build` was built against (Task 13 fix round 1,
/// Finding 4). A valid response is queued: if the bug comes back,
/// `send()` will silently reach the transport and return `Ok`, not `Err`.
#[test]
fn invalid_header_name_fails_send_instead_of_silently_dropping_it() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m).build().unwrap();

    let result = futures_executor::block_on(c.get("https://a/x").header("bad header", "v").send());
    assert!(
        result.is_err(),
        "an invalid header name must fail send(), not silently proceed: {result:?}"
    );
    assert!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .is_empty(),
        "the request must never reach the transport once header() recorded an error"
    );
}

/// Carried finding from Task 13's review (progress.md, "Task 13: minor
/// (deferred)"): the "first error wins" contract of `header()` is guaranteed
/// structurally (`header()` short-circuits before parsing once the error
/// slot is filled), but had no test pinning it. Calls an invalid *name*
/// first, then an invalid *value*: the error `send()` reports must be the
/// name error, not the value error that came second.
#[test]
fn header_first_error_wins_name_over_later_value_error() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m).build().unwrap();

    let result = futures_executor::block_on(
        c.get("https://a/x")
            .header("bad header", "v") // invalid name — recorded first
            .header("x-ok", "bad\nvalue") // invalid value — must not overwrite it
            .send(),
    );
    let err = result.expect_err("both header() calls are invalid; send() must fail");
    let src = std::error::Error::source(&err).expect("Error::new always sets a source");
    assert!(
        src.downcast_ref::<http::header::InvalidHeaderName>()
            .is_some(),
        "the first error (invalid name) must win over the later invalid value: {err}"
    );
}

/// `chunk()` skips trailer frames — documented in `response.rs` but, before
/// this fix round, untested: `push_response`/`push_response_frames` only ever
/// produce data frames. `push_response_with_trailers` closes that gap
/// (Task 13 fix round 1, Finding 5).
#[test]
fn chunk_skips_trailer_frames() {
    let mut trailers = http::HeaderMap::new();
    trailers.insert("x-trailer", "v".parse().unwrap());

    let m = MockTransport::new();
    m.push_response_with_trailers(
        http::Response::builder()
            .status(200)
            .body(vec!["data"])
            .unwrap(),
        trailers,
    );

    let c = Client::builder(m).build().unwrap();
    let mut resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    let first = futures_executor::block_on(resp.chunk())
        .expect("the data frame must be present")
        .unwrap();
    assert_eq!(&first[..], b"data");
    assert!(
        futures_executor::block_on(resp.chunk()).is_none(),
        "chunk() must skip the trailer frame and report end of stream, not surface it as data"
    );
}

/// The other half of the asymmetry `chunk_skips_trailer_frames` proves one
/// side of: `into_parts()` hands back the raw body, and polling it directly
/// (as `SseStream`/any caller with `Task 14`-style needs would) DOES see the
/// trailer frame that `chunk()` swallows.
#[test]
fn into_parts_lets_you_poll_the_trailer_frame_directly() {
    use http_body::Body as _;
    use std::task::{Context, Poll};

    let mut trailers = http::HeaderMap::new();
    trailers.insert("x-trailer", "v".parse().unwrap());

    let m = MockTransport::new();
    m.push_response_with_trailers(
        http::Response::builder()
            .status(200)
            .body(vec!["data"])
            .unwrap(),
        trailers,
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();
    let (_, body) = resp.into_parts();

    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut pinned = std::pin::pin!(body);

    match pinned.as_mut().poll_frame(&mut cx) {
        Poll::Ready(Some(Ok(f))) => {
            assert_eq!(f.into_data().unwrap(), bytes::Bytes::from_static(b"data"))
        }
        other => panic!("expected the data frame, got {other:?}"),
    }
    match pinned.as_mut().poll_frame(&mut cx) {
        Poll::Ready(Some(Ok(f))) => {
            let t = f
                .into_trailers()
                .expect("second frame must be trailers, not data");
            assert_eq!(t.get("x-trailer").unwrap(), "v");
        }
        other => panic!("expected the trailers frame, got {other:?}"),
    }
    match pinned.as_mut().poll_frame(&mut cx) {
        Poll::Ready(None) => {}
        other => panic!("expected end of stream after the trailer frame, got {other:?}"),
    }
}

/// `Response::version()` and `into_parts()` had no direct test (Task 13 fix
/// round 1, Finding 6): `into_parts()` was only exercised indirectly through
/// the trailer tests above, and `version()` not at all.
#[test]
fn version_and_into_parts_expose_the_full_response_head() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(201)
            .version(http::Version::HTTP_11)
            .header("x-k", "v")
            .body("body")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();
    assert_eq!(resp.version(), http::Version::HTTP_11);

    let (parts, mut body) = resp.into_parts();
    assert_eq!(parts.status, 201);
    assert_eq!(parts.headers.get("x-k").unwrap(), "v");

    // The body handed back by into_parts() is the real, unread one.
    use http_body::Body as _;
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    match std::pin::Pin::new(&mut body).poll_frame(&mut cx) {
        std::task::Poll::Ready(Some(Ok(f))) => {
            assert_eq!(f.into_data().unwrap(), bytes::Bytes::from_static(b"body"))
        }
        other => panic!("expected the data frame via the raw body, got {other:?}"),
    }
}

// ── resolution of the branch's final review: minors m4/m5/m6 ─────────────

/// m4. `RequestBuilder::headers()` used to ASSIGN (`self.headers =
/// headers`) instead of extending — so `.header("x-a","1").headers(map)`
/// lost `x-a` with no diagnostic at all. The same class of defect as the
/// brief's `header()`, which Task 13 fixed and covered with a test: a
/// value the caller passed in disappears silently.
#[test]
fn headers_extends_what_header_already_set_instead_of_discarding_it() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m).build().unwrap();

    let mut extra = http::HeaderMap::new();
    extra.insert("x-b", "2".parse().unwrap());

    futures_executor::block_on(
        c.get("https://a/x")
            .header("x-a", "1")
            .headers(extra)
            .send(),
    )
    .unwrap();

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(
        seen[0].headers.get("x-a").map(|v| v.to_str().unwrap()),
        Some("1"),
        "headers() must not discard what header() set"
    );
    assert_eq!(
        seen[0].headers.get("x-b").map(|v| v.to_str().unwrap()),
        Some("2"),
    );
}

/// The flip side of m4: "extending" doesn't mean "accumulating
/// duplicates". A same-named header from `headers()` OVERRIDES one set
/// earlier — otherwise `.header("accept","a").headers({accept:b})` would
/// go out on the wire as two `accept`s, which for most headers means a
/// different request.
#[test]
fn headers_overrides_a_same_named_header_rather_than_duplicating_it() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m).build().unwrap();

    let mut extra = http::HeaderMap::new();
    extra.insert("x-a", "second".parse().unwrap());

    futures_executor::block_on(
        c.get("https://a/x")
            .header("x-a", "first")
            .headers(extra)
            .send(),
    )
    .unwrap();

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(seen[0].headers.get_all("x-a").iter().count(), 1);
    assert_eq!(
        seen[0].headers.get("x-a").map(|v| v.to_str().unwrap()),
        Some("second"),
    );
}

/// m5. `chunk_skips_trailer_frames` puts the trailer LAST, where "skipped
/// it and hit EOF" and "stopped on it" are the same observation: mutating
/// `Err(_) => continue` into `Err(_) => return None` left the whole
/// `hclient` suite green. Here the trailer sits BETWEEN data frames, and
/// the two hypotheses diverge.
#[test]
fn chunk_continues_reading_data_that_follows_a_trailer_frame() {
    let mut trailers = http::HeaderMap::new();
    trailers.insert("x-trailer", "v".parse().unwrap());

    let m = MockTransport::new();
    m.push_response_with_trailers_between_data(
        http::Response::builder()
            .status(200)
            .body(vec!["before"])
            .unwrap(),
        trailers,
        vec!["after"],
    );

    let c = Client::builder(m).build().unwrap();
    let mut resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    let first = futures_executor::block_on(resp.chunk()).unwrap().unwrap();
    assert_eq!(&first[..], b"before");
    let second = futures_executor::block_on(resp.chunk())
        .expect("data following a trailer frame must not be swallowed")
        .unwrap();
    assert_eq!(&second[..], b"after");
    assert!(futures_executor::block_on(resp.chunk()).is_none());
}

/// m6. After `Some(Err(_))`, the body used not to be sealed: the next
/// `chunk()` would poll the underlying `Body` again. A caller using
/// `Response::chunk` directly (without `SseStream`, which has its own
/// `done` flag) could spin in a loop over a body that returns an error on
/// every poll. The error is terminal: handed back exactly once, then it's
/// the end of the stream.
#[test]
fn chunk_is_terminal_after_an_error_and_does_not_poll_the_body_again() {
    let m = MockTransport::new();
    // The error is REPEATING: with a one-shot error the test would be
    // vacuous — frames simply run out after it, and a second `chunk()`
    // would return `None` by coincidence, not because the body is sealed
    // (verified: with `push_response_frames_then_error` this test stays
    // green even without the fix).
    m.push_response_frames_then_repeating_error(
        http::Response::builder()
            .status(200)
            .body(vec!["data"])
            .unwrap(),
        hclient::Error::new(hclient::ErrorKind::Other, std::io::Error::other("boom")),
    );

    let c = Client::builder(m).build().unwrap();
    let mut resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    assert_eq!(
        &futures_executor::block_on(resp.chunk()).unwrap().unwrap()[..],
        b"data"
    );
    let err = futures_executor::block_on(resp.chunk())
        .expect("error frame")
        .unwrap_err();
    // Vertical 2's final review, finding F2: `chunk()` no longer relabels
    // an already-classified error as `ErrorKind::Body` — `MockBody::Error`
    // is already `hclient_core::Error`, and its `kind()` (`Other`, set one
    // line above) must survive unchanged. `chunk_survives_a_
    // non_body_error_kind_instead_of_relabeling_it_body` (below) checks the
    // same property in a targeted way; this test does it in passing,
    // together with terminality.
    assert_eq!(*err.kind(), hclient::ErrorKind::Other);
    assert!(
        futures_executor::block_on(resp.chunk()).is_none(),
        "after an error the body is sealed — it's not handed back a second time and the body isn't polled again"
    );
}

/// Final review, F2 (major): `Response::chunk()` used to wrap EVERY body
/// error in `Error::new(ErrorKind::Body, e)` unconditionally — the exact
/// pattern vertical 1's final review named finding B2 and fixed with
/// `Transport::to_error`'s "already ours? pass it through" default, except
/// this half of the request (the response body) never got the same fix.
/// `MockBody::Error` is `hclient_core::Error` already, so a body that fails
/// with a real classification (`Cancelled`, chosen because it exists
/// precisely so a caller can tell "the runtime is shutting down" apart from
/// a genuine body failure without downcasting — see `ErrorKind::Cancelled`'s
/// doc comment) must reach the caller as `Cancelled`, not get relabeled.
///
/// Checks every symptom the review measured, not just `kind()`: the
/// predicate (`is_cancelled()`), the source chain depth (double-wrap would
/// insert an extra `hclient::Error` layer a caller would have to downcast
/// through), and `Display` (double-wrap prints the category twice).
///
/// Mutation-checked (see this task's report): reverting `classify_body_error`
/// to the old unconditional `Error::new(ErrorKind::Body, e)` turns this test
/// red with `kind() == Body`, confirming it is the test that would have
/// caught finding F2 — the same mutation the review's own probe (B8) found
/// zero tests catching before this fix round.
#[test]
fn chunk_survives_a_non_body_error_kind_instead_of_relabeling_it_body() {
    let m = MockTransport::new();
    let empty: Vec<&'static str> = Vec::new();
    m.push_response_frames_then_error(
        http::Response::builder().status(200).body(empty).unwrap(),
        hclient::Error::new(
            hclient::ErrorKind::Cancelled,
            std::io::Error::other("pool gone"),
        ),
    );

    let c = Client::builder(m).build().unwrap();
    let mut resp = futures_executor::block_on(c.get("https://a/").send()).unwrap();
    let err = futures_executor::block_on(resp.chunk())
        .expect("error frame")
        .unwrap_err();

    assert_eq!(
        *err.kind(),
        hclient::ErrorKind::Cancelled,
        "the backend's own classification must survive chunk(), not become Body: {err}"
    );
    assert!(
        err.is_cancelled(),
        "the is_cancelled() predicate must agree with kind(): {err}"
    );
    assert!(
        !err.to_string().starts_with("Body:"),
        "the category must print once, not Body: Cancelled: ..: {err}"
    );
    // Exactly one level of source(), not two: a double-wrap would insert an
    // extra `hclient::Error` the caller has to downcast through before
    // reaching the original `std::io::Error`.
    let src = std::error::Error::source(&err).expect("Error::new always sets a source");
    assert!(
        src.downcast_ref::<hclient::Error>().is_none(),
        "source() must be the original std::io::Error directly, not another hclient::Error \
         wrapping it: {src}"
    );
    assert!(src.downcast_ref::<std::io::Error>().is_some(), "{src}");
}
