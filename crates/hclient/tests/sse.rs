//! `SseStream`: a decoded stream of SSE events over `Response::chunk`.
//!
//! `#![cfg(feature = "test-util")]` — this file pulls in `hclient::mock`,
//! which is gated behind `test-util` (see `mock.rs`); without the gate, a
//! bare `cargo test -p hclient` here wouldn't compile (the same pattern as
//! in `shape.rs` and `response.rs`).
#![cfg(feature = "test-util")]

use hclient::Client;
use hclient::mock::MockTransport;
use hclient::sse::DEFAULT_MAX_EVENT_SIZE;
use hclient::sse::SseEvent;
use hclient::sse::SseStream;

fn sse_response(body: &'static str) -> http::Response<&'static str> {
    http::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}

#[test]
fn parses_events_from_a_response() {
    let m = MockTransport::new();
    m.push_response(sse_response("data: one\n\ndata: two\n\n"));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();

    let mut got = Vec::new();
    while let Some(e) = futures_executor::block_on(s.next()) {
        got.push(e.unwrap())
    }

    assert_eq!(
        got,
        vec![
            SseEvent::Message {
                event: None,
                data: "one".into(),
                id: None
            },
            SseEvent::Message {
                event: None,
                data: "two".into(),
                id: None
            },
        ]
    );
}

#[test]
fn rejects_wrong_content_type() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body("{}")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_err());
}

// ── Content-Type: token boundaries, not a prefix ───────────────────────
//
// `starts_with(MIME)` accepted `"text/event-streamfoo"` and rejected
// `"Text/Event-Stream"`. The four forms below cover both sides of the
// defect: the exact match is already checked by `parses_events_from_a_response`
// above.

fn sse_response_with_content_type(
    body: &'static str,
    content_type: &'static str,
) -> http::Response<&'static str> {
    http::Response::builder()
        .status(200)
        .header("content-type", content_type)
        .body(body)
        .unwrap()
}

#[test]
fn accepts_content_type_with_charset_parameter() {
    let m = MockTransport::new();
    m.push_response(sse_response_with_content_type(
        "data: x\n\n",
        "text/event-stream; charset=utf-8",
    ));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_ok());
}

#[test]
fn accepts_content_type_regardless_of_case() {
    let m = MockTransport::new();
    m.push_response(sse_response_with_content_type(
        "data: x\n\n",
        "Text/Event-Stream",
    ));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(
        SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_ok(),
        "HTTP media types are case-insensitive (RFC 9110 §5.5)"
    );
}

#[test]
fn rejects_content_type_that_merely_starts_with_the_mime_type() {
    let m = MockTransport::new();
    m.push_response(sse_response_with_content_type(
        "data: x\n\n",
        "text/event-streamfoo",
    ));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(
        SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_err(),
        "a prefix match is not a media-type token match"
    );
}

#[test]
fn rejects_non_200_status() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(204)
            .header("content-type", "text/event-stream")
            .body("")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(
        SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_err(),
        "204 means \"stop forever\", not \"empty stream\""
    );
}

// ── Ordering of a fatal error ──────────────────────────────────────────
//
// `next()` used to return `Err` on a limit violation IMMEDIATELY, before an
// already-parsed valid event from the same `push` could reach the caller
// through a separate `next()` call — and on top of that the stream wasn't
// truly over: the next call silently handed back that event as `Ok`, and
// only AFTER it, `None`. The observed sequence was `Err, Ok("a"), None` —
// losing both "`Err` before `Ok`" and "fatality" itself.
#[test]
fn oversized_event_is_fatal_but_does_not_lose_events_decoded_before_it() {
    let m = MockTransport::new();
    // A valid event under the limit, then the same oversized event the
    // decoder's unit test `oversized_event_is_a_fatal_error` (sse/decode.rs)
    // uses, in a single frame: both are parsed in one `push`, so the decoder
    // gets to dispatch the first one BEFORE the second fails the limit
    // check.
    m.push_response(sse_response("data: a\n\ndata: 0123456789abcdefghij\n\n"));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, 16).unwrap();

    let first = futures_executor::block_on(s.next())
        .expect("the event decoded before the limit tripped must not be lost")
        .expect("the first event is valid, not the error");
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "a".into(),
            id: None
        },
        "the valid event must survive, and survive intact"
    );

    let second = futures_executor::block_on(s.next())
        .expect("the limit violation must surface as an item, not be swallowed");
    assert!(
        second.is_err(),
        "the oversized event must be reported as an error, not accepted"
    );

    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "the stream must be over immediately after the fatal error"
    );
    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "\"fatal\" must mean forever, not a one-shot glitch the stream recovers from"
    );
}

/// The other fatal path: a body-level error from `Response::chunk()` (e.g. a
/// dropped connection mid-stream), not a decoder size-limit violation. Same
/// ordering contract as the test above — "structurally identical" is not
/// "actually exercised", so this is tested independently rather than assumed
/// to hold by resemblance.
/// `MockTransport::push_response_frames_then_error` (`mock.rs`) makes
/// `MockBody::poll_frame` return `Err` after its data frames.
/// `Response::chunk()` passes an already-classified
/// `hclient_core::Error` through unchanged rather than
/// re-wrapping it as `ErrorKind::Body` — this test pushes `ErrorKind::Other`
/// specifically and only checks `is_err()`, so it doesn't pin which kind
/// survives; `chunk_survives_a_non_body_error_kind_instead_of_relabeling_it_
/// body` in `hclient/tests/response.rs` is the test that pins that.
#[test]
fn body_error_is_fatal_but_does_not_lose_events_decoded_before_it() {
    let m = MockTransport::new();
    m.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: a\n\n"])
            .unwrap(),
        hclient_core::Error::new(
            hclient_core::ErrorKind::Other,
            std::io::Error::other("boom"),
        ),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();

    let first = futures_executor::block_on(s.next())
        .expect("the event decoded before the body error must not be lost")
        .expect("the first event is valid, not the error");
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "a".into(),
            id: None
        },
        "the valid event must survive, and survive intact"
    );

    let second = futures_executor::block_on(s.next())
        .expect("the body error must surface as an item, not be swallowed");
    assert!(
        second.is_err(),
        "the body error must be reported as an error, not accepted as a valid end of stream"
    );

    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "the stream must be over immediately after the fatal error"
    );
    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "\"fatal\" must mean forever, not a one-shot glitch the stream recovers from"
    );
}

#[test]
fn tracks_last_event_id_for_future_reconnects() {
    let m = MockTransport::new();
    m.push_response(sse_response("id: 99\ndata: x\n\n"));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();
    while futures_executor::block_on(s.next()).is_some() {}
    assert_eq!(s.last_event_id(), Some("99"));
}

// ── Split at a chunk boundary ───────────────────────────────────────────
//
// No test above crosses a transport chunk boundary: `MockTransport::
// push_response` hands back the whole body as one frame, so the
// stitching-together at the `SseStream` level (not just inside
// `SseDecoder`/`LineSplitter`, already covered in hclient-proto) stays
// unverified. `push_response_frames` exists exactly for this.

/// An event split mid-field (`"on" | "e\n\n..."`) — the most common real
/// case: a TCP chunk almost never lines up with a line boundary.
#[test]
fn event_split_mid_field_across_frames_still_yields_two_events() {
    let m = MockTransport::new();
    m.push_response_frames(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: on", "e\n\ndata: two\n", "\n"])
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();

    let mut got = Vec::new();
    while let Some(e) = futures_executor::block_on(s.next()) {
        got.push(e.unwrap())
    }

    assert_eq!(
        got,
        vec![
            SseEvent::Message {
                event: None,
                data: "one".into(),
                id: None
            },
            SseEvent::Message {
                event: None,
                data: "two".into(),
                id: None
            },
        ]
    );
}

/// A CRLF terminator split exactly between CR and LF at a transport frame
/// boundary, on a `data:` line — not at an empty-line boundary. (A split
/// at an *empty*-line boundary is not diagnostic: a mutation disabling
/// `carried_terminator` still gives the right result, because the phantom
/// empty line lands on an already-drained data buffer and `dispatch()` is
/// a no-op on it. Here, though,
/// the unconsumed LF triggers a PREMATURE dispatch between two `data:`
/// lines of the same event: a broken `carried_terminator` produces two
/// events `"ab"`/`"cd"` instead of one `"ab\ncd"`.) `carried_terminator`
/// itself is already covered by `sse/lines.rs`'s unit tests; here it's
/// checked for the first time across the whole stack — transport ->
/// `Response::chunk` -> `SseDecoder` -> `SseStream`.
#[test]
fn crlf_terminator_split_mid_event_across_frame_boundary_joins_the_data_lines() {
    let m = MockTransport::new();
    m.push_response_frames(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: ab\r", "\ndata: cd\n\n"])
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();

    let mut got = Vec::new();
    while let Some(e) = futures_executor::block_on(s.next()) {
        got.push(e.unwrap())
    }

    assert_eq!(
        got,
        vec![SseEvent::Message {
            event: None,
            data: "ab\ncd".into(),
            id: None
        }]
    );
}
