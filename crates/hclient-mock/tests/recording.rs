//! What the mock records, and what it deliberately does not.
//!
//! The workspace's own suites exercise this double heavily and none of
//! them reads a request body — they are about redirects, retries and
//! capability gates. So the surface an outside test author reaches for
//! first had no test at all, which is how it went four verticals without
//! one.

use bytes::Bytes;
use hclient_core::{RequestBody, unversioned::Transport};
use hclient_mock::{MockTransport, RecordedBody};

fn post(body: RequestBody) -> http::Request<RequestBody> {
    http::Request::builder()
        .method("POST")
        .uri("https://a/x")
        .body(body)
        .unwrap()
}

fn send(m: &MockTransport, req: http::Request<RequestBody>) {
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let _ = futures_executor::block_on(m.execute(req));
}

#[test]
fn a_full_body_is_recorded_and_an_empty_one_is_distinguishable_from_it() {
    let m = MockTransport::new();
    send(&m, post(RequestBody::Full(Bytes::from_static(b"payload"))));
    send(&m, post(RequestBody::Empty));

    let seen = m.requests();
    assert_eq!(seen[0].body.text(), Some("payload"));
    assert_eq!(
        seen[0].body,
        RecordedBody::Bytes(Bytes::from_static(b"payload"))
    );
    // `Empty` answers `Some("")` from `text` and `None` from `bytes`,
    // which is the pair that keeps "no body" from reading as "some bytes I
    // could not get".
    assert_eq!(seen[1].body, RecordedBody::Empty);
    assert_eq!(seen[1].body.text(), Some(""));
    assert_eq!(seen[1].body.bytes(), None);
}

/// The rule the whole design turns on: recording must not call anything.
///
/// `hclient`'s `too_early.rs` counts factory calls to pin *one snapshot
/// per hop, not one per attempt* — a claim about the client. A mock that
/// called the factory to fill a field made that count 2 and broke the
/// test, which is how this was found.
#[test]
fn recording_a_rewindable_body_does_not_call_its_factory() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&calls);
    let m = MockTransport::new();
    send(
        &m,
        post(RequestBody::rewindable(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            RequestBody::Full(Bytes::from_static(b"payload"))
        })),
    );

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the mock must not be an extra caller of the thing under test"
    );

    // And the opt-in is one call, made by the test rather than by the mock.
    let seen = m.requests();
    assert_eq!(
        seen[0].body.snapshot(),
        Some(Bytes::from_static(b"payload"))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_rewindable_body_answers_none_to_the_accessors_that_promise_not_to_call() {
    let m = MockTransport::new();
    send(
        &m,
        post(RequestBody::rewindable(|| {
            RequestBody::Full(Bytes::from_static(b"x"))
        })),
    );
    let seen = m.requests();
    // Neither collapses into empty bytes: a test comparing against
    // `Some("")` must not pass here.
    assert_eq!(seen[0].body.bytes(), None);
    assert_eq!(seen[0].body.text(), None);
}

/// A closure has no equality, so `Rewindable` is equal to nothing —
/// itself included, which is why this is `PartialEq` and not `Eq`.
#[test]
fn a_rewindable_body_is_equal_to_nothing_including_itself() {
    let m = MockTransport::new();
    send(&m, post(RequestBody::rewindable(|| RequestBody::Empty)));
    let seen = m.requests();
    assert_ne!(seen[0].body, seen[0].body.clone());
    assert_ne!(seen[0].body, RecordedBody::Empty);
    assert_ne!(seen[0].body, RecordedBody::NotRecorded);
}

#[test]
fn a_streaming_body_is_refused_rather_than_recorded_as_empty() {
    use http_body_util::BodyExt as _;
    let m = MockTransport::new();
    let stream = http_body_util::Full::new(Bytes::from_static(b"streamed"))
        .map_err(|e: std::convert::Infallible| match e {});
    send(&m, post(RequestBody::Streaming(Box::new(stream))));
    let seen = m.requests();
    // Draining it would consume the caller's stream and would hang on an
    // endless one. An honest refusal fails a test that expected bytes;
    // a silent `Empty` would have passed one.
    assert_eq!(seen[0].body, RecordedBody::NotRecorded);
    assert_eq!(seen[0].body.text(), None);
    assert_eq!(seen[0].body.snapshot(), None);
}

/// `Client::builder` takes its transport by value, so without this a test
/// has to hand the mock over and reach back through
/// `Client::transport_as::<MockTransport>()` to say what it wanted.
#[test]
fn clone_shares_the_queue_and_the_log() {
    let a = MockTransport::new();
    let b = a.clone();
    b.push_response(http::Response::builder().status(204).body("").unwrap());
    assert_eq!(a.queued(), 1, "the queue is one queue");

    let _ = futures_executor::block_on(a.execute(post(RequestBody::Empty)));
    assert_eq!(b.requests().len(), 1, "the log is one log");
    assert_eq!(b.queued(), 0);
}

/// `queued()` answers a question `requests().len()` cannot: an extra
/// scripted response nothing asked for is invisible to a length assertion
/// on the log.
#[test]
fn queued_counts_what_is_left_rather_than_what_was_used() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("a").unwrap());
    m.push_response(http::Response::builder().status(200).body("b").unwrap());
    assert_eq!(m.queued(), 2);
    let _ = futures_executor::block_on(m.execute(post(RequestBody::Empty)));
    assert_eq!((m.requests().len(), m.queued()), (1, 1));
}

/// The body accepts anything that becomes `Bytes`, which is what a payload
/// built at run time is. It was `&'static str` for four verticals, so a
/// test author's first attempt did not compile.
#[test]
fn a_response_body_can_be_built_at_run_time() {
    let m = MockTransport::new();
    let owned: String = format!("{{\"n\":{}}}", 1 + 1);
    m.push_response(http::Response::builder().status(200).body(owned).unwrap());
    m.push_response(
        http::Response::builder()
            .status(200)
            .body(vec![b'x'])
            .unwrap(),
    );
    m.push_response(
        http::Response::builder()
            .status(200)
            .body("literal")
            .unwrap(),
    );
    m.push_response(
        http::Response::builder()
            .status(200)
            .body(Bytes::from_static(b"b"))
            .unwrap(),
    );
    assert_eq!(m.queued(), 4);
}
