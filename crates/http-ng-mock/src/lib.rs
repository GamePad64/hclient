//! Mock transport and a controllable timer: let you test a `Transport`
//! implementation, or a client built on one, on the host — with no network
//! and no wasm runtime.
//!
//! Depends on `http-ng-core` alone. It was carved out of the `http-ng`
//! facade for that reason: transports live *below* the facade, so reaching
//! these doubles through it meant a `Transport` author depending upward on
//! the whole client. Re-exported as `http_ng::mock` behind the facade's
//! `test-util` feature, so existing callers see no change.
//!
//! The response queue and request log sit behind a `std::sync::Mutex`, not
//! a `RefCell`. This isn't a style choice: `RefCell` would make
//! `MockTransport` `!Sync`, which would make `&MockTransport` `!Send`, and
//! therefore the future `Client::execute` returns (it borrows the
//! transport) would also be `!Send` — `tokio::spawn(client.get(u).send())`
//! wouldn't compile in a single test built on this mock. The property
//! "the client's future is Send when the transport is" is central to the
//! crate's design, and a test double shouldn't be what stops it from being
//! checked.

use bytes::Bytes;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, RetryKind};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

/// One recorded request: everything the mock saw before the body was
/// dropped.
///
/// `extensions` is stored whole, rather than unpacked down to the specific
/// types known today: `Timeouts` (Task 10) already travels through
/// `http::Extensions`, and it won't be the last type to start doing that —
/// a narrow field would have to be widened again for every new case.
/// `retry_kind` and `body_size_hint` are the little that can be said about
/// a body without reading it: the mock doesn't collect the actual bytes,
/// or `Streaming` bodies would have to be read to completion, which tests
/// don't need.
///
/// No `PartialEq`/`Eq`: `http::Extensions` doesn't implement them (it's a
/// `TypeId -> Box<dyn Any>` map, which has nothing to compare it with in
/// the general case), so `RecordedRequest` can't be compared as a whole —
/// only field by field.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: http::Method,
    pub uri: http::Uri,
    pub headers: http::HeaderMap,
    pub extensions: http::Extensions,
    pub retry_kind: RetryKind,
    pub body_size_hint: Option<u64>,
}

/// A mock body frame: data, trailers, or a break with an error. Exists so
/// `push_response_with_trailers` can queue a trailer frame, and
/// `push_response_frames_then_error` can break the body with an error
/// mid-stream. Without the first, the asymmetry between `Response::chunk()`
/// (skips trailers) and `into_parts()` with direct polling (hands them
/// back) would be documented but never verified — `push_response` and
/// `push_response_frames` produce data only. Without the second, the
/// `Some(Err(_))` path of `Response::chunk()` in `SseStream::next`
/// (Task 14, review round 2, Finding 2) would stay structurally similar to
/// the tested limit-exceeded path but unverified in its own right:
/// `MockBody` could only hand back `Ok` frames before this round, no
/// `poll_frame` call could ever return `Err`.
#[derive(Debug, Clone)]
enum MockFrame {
    Data(Bytes),
    Trailers(http::HeaderMap),
    Error(Error),
    /// An error the body hands back on EVERY poll, not just once.
    ///
    /// Exists for m6 of the branch's final review: `Response::chunk` used
    /// to not seal itself after `Some(Err(_))` and would poll the
    /// underlying body again. A one-shot `Error` doesn't show this — after
    /// it, frames simply run out, and a repeated `chunk()` returns `None`
    /// by coincidence, not because the body is sealed.
    RepeatingError(Error),
}

/// One entry in the mock's queue: a response, or the transport itself
/// failing.
///
/// `Err` isn't the same thing as `MockFrame::Error`: that one breaks a
/// body that has already started, this one fails `Transport::execute`
/// entirely, before any response at all. Without it, the mock couldn't
/// produce a SINGLE transport error other than `QueueEmpty` with its fixed
/// `ErrorKind::Other` — which is exactly why 165 tests on the branch never
/// noticed that `Client::execute` flattened the category of every
/// transport error into `Other` (B2 of the final review).
type Queued = Result<http::Response<VecDeque<MockFrame>>, Error>;

#[derive(Debug)]
pub struct MockTransport {
    queue: Mutex<VecDeque<Queued>>,
    seen: Mutex<Vec<RecordedRequest>>,
    caps: Capabilities,
}

/// Returned instead of a response when the mock's queue is empty.
///
/// `pub`, not a private type: a test must be able to tell this apart from
/// any other `ErrorKind::Other` error via
/// `Error::source().downcast_ref::<QueueEmpty>()`, rather than relying on
/// this being the mock's only path to an error today.
#[derive(Debug)]
pub struct QueueEmpty;
impl std::fmt::Display for QueueEmpty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MockTransport: response queue is empty")
    }
}
impl std::error::Error for QueueEmpty {}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
            caps: Capabilities::none(),
        }
    }

    pub fn with_capabilities(mut self, caps: Capabilities) -> Self {
        self.caps = caps;
        self
    }

    /// Queues a response made of a single frame — the common case when a
    /// test doesn't care about chunk boundaries.
    pub fn push_response(&self, resp: http::Response<&'static str>) {
        let (parts, body) = resp.into_parts();
        let mut frames = VecDeque::new();
        frames.push_back(MockFrame::Data(Bytes::from_static(body.as_bytes())));
        self.queue
            .lock()
            .expect("mock lock poisoned")
            .push_back(Ok(http::Response::from_parts(parts, frames)));
    }

    /// Queues a response made of several frames — for example, to
    /// reproduce an SSE stream split across a chunk boundary (Task 14).
    /// Frames are handed back by `poll_frame` one at a time, in the order
    /// passed in.
    pub fn push_response_frames(&self, resp: http::Response<Vec<&'static str>>) {
        let (parts, body) = resp.into_parts();
        let frames: VecDeque<MockFrame> = body
            .into_iter()
            .map(|s| MockFrame::Data(Bytes::from_static(s.as_bytes())))
            .collect();
        self.queue
            .lock()
            .expect("mock lock poisoned")
            .push_back(Ok(http::Response::from_parts(parts, frames)));
    }

    /// Like `push_response_frames`, but adds a trailers frame last —
    /// demonstrates the asymmetry between `Response::chunk()` (skips
    /// trailers) and `into_parts()` + direct body polling (hands them
    /// back), see `response.rs`.
    pub fn push_response_with_trailers(
        &self,
        resp: http::Response<Vec<&'static str>>,
        trailers: http::HeaderMap,
    ) {
        let (parts, body) = resp.into_parts();
        let mut frames: VecDeque<MockFrame> = body
            .into_iter()
            .map(|s| MockFrame::Data(Bytes::from_static(s.as_bytes())))
            .collect();
        frames.push_back(MockFrame::Trailers(trailers));
        self.queue
            .lock()
            .expect("mock lock poisoned")
            .push_back(Ok(http::Response::from_parts(parts, frames)));
    }

    /// Like `push_response_with_trailers`, but the trailers frame sits
    /// BETWEEN two groups of data, rather than last.
    ///
    /// Exists for m5 of the branch's final review. `Response::chunk`
    /// documents that trailer frames are SKIPPED and reading continues,
    /// but with a trailer at the end, "skipped it and hit EOF" and
    /// "stopped on it" are the same observation, and mutating
    /// `Err(_) => continue` into `Err(_) => return None` left the whole
    /// suite green. A frame in the middle tells these two hypotheses
    /// apart.
    pub fn push_response_with_trailers_between_data(
        &self,
        resp: http::Response<Vec<&'static str>>,
        trailers: http::HeaderMap,
        after: Vec<&'static str>,
    ) {
        let (parts, body) = resp.into_parts();
        let mut frames: VecDeque<MockFrame> = body
            .into_iter()
            .map(|s| MockFrame::Data(Bytes::from_static(s.as_bytes())))
            .collect();
        frames.push_back(MockFrame::Trailers(trailers));
        frames.extend(
            after
                .into_iter()
                .map(|s| MockFrame::Data(Bytes::from_static(s.as_bytes()))),
        );
        self.queue
            .lock()
            .expect("mock lock poisoned")
            .push_back(Ok(http::Response::from_parts(parts, frames)));
    }

    /// Like `push_response_frames_then_error`, but the error isn't
    /// one-shot: the body hands it back on every subsequent poll, the way
    /// a genuinely broken connection would. See `MockFrame::RepeatingError`.
    pub fn push_response_frames_then_repeating_error(
        &self,
        resp: http::Response<Vec<&'static str>>,
        err: Error,
    ) {
        let (parts, body) = resp.into_parts();
        let mut frames: VecDeque<MockFrame> = body
            .into_iter()
            .map(|s| MockFrame::Data(Bytes::from_static(s.as_bytes())))
            .collect();
        frames.push_back(MockFrame::RepeatingError(err));
        self.queue
            .lock()
            .expect("mock lock poisoned")
            .push_back(Ok(http::Response::from_parts(parts, frames)));
    }

    /// Like `push_response_frames`, but breaks the body with an error
    /// after `frames`, instead of a clean EOF — reproduces a connection
    /// dropping mid-stream (for example, for `SseStream::next`, whose
    /// `Some(Err(_))` path out of `Response::chunk()` would otherwise stay
    /// structurally similar to the tested decoder-limit-exceeded path but
    /// unverified in its own right; Task 14, review round 2, Finding 2).
    /// `err` reaches `Response::chunk()` unchanged: since vertical 2's
    /// final review (finding F2), `chunk()` passes `err` through
    /// unmodified when it's already an `http_ng_core::Error`
    /// (`MockBody::Error` — exactly that), instead of relabeling its
    /// category as `Body` — the same trick `Transport::to_error`'s default
    /// uses. It only wraps in `ErrorKind::Body` a foreign error type,
    /// which isn't the case here.
    pub fn push_response_frames_then_error(
        &self,
        resp: http::Response<Vec<&'static str>>,
        err: Error,
    ) {
        let (parts, body) = resp.into_parts();
        let mut frames: VecDeque<MockFrame> = body
            .into_iter()
            .map(|s| MockFrame::Data(Bytes::from_static(s.as_bytes())))
            .collect();
        frames.push_back(MockFrame::Error(err));
        self.queue
            .lock()
            .expect("mock lock poisoned")
            .push_back(Ok(http::Response::from_parts(parts, frames)));
    }

    /// Queues a failure of the transport ITSELF — `Transport::execute`
    /// will return `Err(err)`, there will be no response at all.
    ///
    /// Exists for B2 of the branch's final review: `Client::execute` used
    /// to flatten the category of every transport error into
    /// `ErrorKind::Other`, and no test saw it, because the mock could only
    /// fail `execute` by exhausting its queue — and that one's category is
    /// `Other` anyway, correctly. Here the caller sets the category, so
    /// "did it reach the consumer" becomes an observable property.
    pub fn push_transport_error(&self, err: Error) {
        self.queue
            .lock()
            .expect("mock lock poisoned")
            .push_back(Err(err));
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.seen.lock().expect("mock lock poisoned").clone()
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for MockTransport {
    type Body = MockBody;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        let (parts, body) = req.into_parts();
        // Read the little that's known about the body without reading it,
        // before it's dropped along with the rest of `parts`.
        let retry_kind = body.retry_kind();
        let body_size_hint = body.size_hint();
        self.seen
            .lock()
            .expect("mock lock poisoned")
            .push(RecordedRequest {
                method: parts.method,
                uri: parts.uri,
                headers: parts.headers,
                extensions: parts.extensions,
                retry_kind,
                body_size_hint,
            });
        match self.queue.lock().expect("mock lock poisoned").pop_front() {
            Some(Ok(r)) => {
                let (p, frames) = r.into_parts();
                Ok(http::Response::from_parts(p, MockBody { frames }))
            }
            Some(Err(e)) => Err(e),
            None => Err(Error::new(ErrorKind::Other, QueueEmpty)),
        }
    }

    /// Identity, not wrapping — `Self::Error` is already
    /// `http_ng_core::Error`.
    ///
    /// The same reason `http-ng-wasi` overrides it: without the override,
    /// `Client::execute` would wrap an already-classified error in another
    /// one with `ErrorKind::Other`, and `push_transport_error` would stop
    /// proving anything — the mock must be a faithful model of a backend,
    /// not something that masks the defect under test.
    fn to_error(&self, e: Self::Error) -> Error {
        e
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// The mock's response body: a sequence of frames, so the mock can
/// reproduce a split at a chunk boundary. With a single-frame body, the
/// SSE stream in Task 14 would get the whole stream as one piece, and the
/// stitching-together path at the stream level would go unverified.
#[derive(Debug)]
pub struct MockBody {
    frames: VecDeque<MockFrame>,
}

impl http_body::Body for MockBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        let Some(f) = self.frames.pop_front() else {
            return Poll::Ready(None);
        };
        Poll::Ready(Some(match f {
            MockFrame::Data(b) => Ok(http_body::Frame::data(b)),
            MockFrame::Trailers(h) => Ok(http_body::Frame::trailers(h)),
            MockFrame::Error(e) => Err(e),
            MockFrame::RepeatingError(e) => {
                let out = e.clone();
                self.frames.push_front(MockFrame::RepeatingError(e));
                Err(out)
            }
        }))
    }

    fn is_end_stream(&self) -> bool {
        self.frames.is_empty()
    }
}

/// A controllable [`http_ng_core::unversioned::Timer`] for tests: `sleep`
/// never actually waits — it records the requested `Duration` and resolves
/// immediately — so a reconnect test (`ReconnectingSseStream`, `sse.rs`)
/// stays on the bare `futures_executor` executor this crate's test suite
/// uses everywhere, with no real sleeping and no real runtime, and can
/// still assert exactly what the backoff computed rather than only how
/// many times it eventually reconnected.
///
/// `Clone`, cheaply — `Arc` inside — so a test can hand one copy to
/// `SseBuilder::with_timer` and keep another to call `sleeps()` on
/// afterward.
#[derive(Debug, Clone, Default)]
pub struct TestTimer {
    sleeps: std::sync::Arc<Mutex<Vec<std::time::Duration>>>,
}

impl TestTimer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every `Duration` `sleep` was called with, in call order.
    pub fn sleeps(&self) -> Vec<std::time::Duration> {
        self.sleeps.lock().expect("TestTimer lock poisoned").clone()
    }
}

impl http_ng_core::unversioned::Timer for TestTimer {
    /// The sum of every recorded sleep so far — a simple virtual clock,
    /// sufficient for the one thing `Timer::Instant` needs to support
    /// (`Copy + PartialOrd`), without claiming any relationship to real
    /// wall-clock time.
    type Instant = std::time::Duration;

    fn sleep(&self, d: std::time::Duration) -> impl std::future::Future<Output = ()> {
        self.sleeps.lock().expect("TestTimer lock poisoned").push(d);
        std::future::ready(())
    }

    fn now(&self) -> Self::Instant {
        self.sleeps().into_iter().sum()
    }

    fn elapsed_since(&self, earlier: Self::Instant) -> std::time::Duration {
        self.now().saturating_sub(earlier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_core::unversioned::Transport;

    #[test]
    fn records_requests_and_replays_queued_responses() {
        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(204).body("").unwrap());

        let fut = m.execute(
            http::Request::builder()
                .method("POST")
                .uri("https://a/x")
                .body(RequestBody::Empty)
                .unwrap(),
        );
        let resp = futures_executor::block_on(fut).unwrap();

        assert_eq!(resp.status(), 204);
        let rec = m.requests();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].method, http::Method::POST);
        assert_eq!(rec[0].uri, "https://a/x".parse::<http::Uri>().unwrap());
    }

    /// A continuation of `records_requests_and_replays_queued_responses`:
    /// that test covers the method and URI, but not headers. Here the
    /// request differs from the defaults in all three fields at once — the
    /// method isn't GET, the URI isn't empty, and a non-standard header is
    /// added — so `requests()` can't be accidentally "fixed" by copying
    /// only part of `parts`.
    #[test]
    fn records_headers_that_differ_from_defaults_too() {
        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(200).body("").unwrap());

        let fut = m.execute(
            http::Request::builder()
                .method("PATCH")
                .uri("https://a/y")
                .header("x-mock-test", "custom-value")
                .body(RequestBody::Empty)
                .unwrap(),
        );
        futures_executor::block_on(fut).unwrap();

        let rec = m.requests();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].method, http::Method::PATCH);
        assert_eq!(rec[0].uri, "https://a/y".parse::<http::Uri>().unwrap());
        assert_eq!(
            rec[0]
                .headers
                .get("x-mock-test")
                .map(|v| v.to_str().unwrap()),
            Some("custom-value"),
        );
    }

    #[test]
    fn errors_when_the_queue_is_empty() {
        let m = MockTransport::new();
        let fut = m.execute(http::Request::new(RequestBody::Empty));
        let err = futures_executor::block_on(fut).unwrap_err();
        assert_eq!(err.kind(), &ErrorKind::Other);
        // Not just any ErrorKind::Other will do: the test must
        // specifically distinguish the mock's queue exhaustion from some
        // hypothetical other Other-error.
        let src = std::error::Error::source(&err).expect("Error::new always sets a source");
        assert!(
            src.downcast_ref::<QueueEmpty>().is_some(),
            "the error's source must downcast specifically to QueueEmpty"
        );
    }

    #[test]
    fn with_capabilities_overrides_the_default_none() {
        let mut caps = Capabilities::none();
        caps.streaming_request_body = true;
        let m = MockTransport::new().with_capabilities(caps);
        assert!(m.capabilities().streaming_request_body);
    }

    /// `Empty` and `Full` are the only `RequestBody` variants whose size is
    /// known ahead of time (see
    /// `body::size_hint_is_known_for_empty_and_full_bodies` in
    /// `http-ng-core`). `Some(0)` for `Empty` isn't "size unknown", it's
    /// "there was no body"; telling the two apart is the whole point of
    /// recording this field for Task 12 (a redirect must not send a body
    /// where there wasn't one).
    #[test]
    fn body_size_hint_distinguishes_empty_from_populated() {
        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(200).body("").unwrap());
        m.push_response(http::Response::builder().status(200).body("").unwrap());

        futures_executor::block_on(m.execute(http::Request::new(RequestBody::Empty))).unwrap();
        futures_executor::block_on(m.execute(http::Request::new(RequestBody::Full(
            Bytes::from_static(b"payload"),
        ))))
        .unwrap();

        let rec = m.requests();
        assert_eq!(rec.len(), 2);
        assert_eq!(
            rec[0].body_size_hint,
            Some(0),
            "Empty body carries no bytes"
        );
        assert_eq!(rec[0].retry_kind, RetryKind::Free);
        assert_eq!(
            rec[1].body_size_hint,
            Some(7),
            "Full body's real size must survive into the recording"
        );
        assert_eq!(rec[1].retry_kind, RetryKind::Free);
    }

    /// `Timeouts` (Task 10) travels to the transport via `http::Extensions`
    /// — that's the entire mechanism it lives there for. Without recording
    /// `extensions` whole, no test could confirm that the client actually
    /// attaches per-request timeouts and that they survive to reach the
    /// transport.
    #[test]
    fn extensions_round_trip_through_the_recording_so_timeouts_survive() {
        use http_ng_core::Timeouts;
        use std::time::Duration;

        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(200).body("").unwrap());

        let mut req = http::Request::new(RequestBody::Empty);
        req.extensions_mut().insert(Timeouts {
            connect: Some(Duration::from_secs(3)),
            ..Default::default()
        });

        futures_executor::block_on(m.execute(req)).unwrap();

        let rec = m.requests();
        let recorded = rec[0]
            .extensions
            .get::<Timeouts>()
            .expect("Timeouts inserted into the request must survive into the recording");
        assert_eq!(recorded.connect, Some(Duration::from_secs(3)));
    }

    /// Checks that `poll_frame` hands back frames one at a time, not the
    /// whole body on the first call. Without this property, `SseStream`
    /// (Task 14), built on top of `Response::chunk()`, could never see an
    /// event split across a chunk boundary through the mock — only a
    /// concatenated stream.
    #[test]
    fn multi_frame_response_yields_frames_separately_not_concatenated() {
        use http_body::Body as _;

        let m = MockTransport::new();
        m.push_response_frames(
            http::Response::builder()
                .status(200)
                .body(vec!["first-chunk", "second-chunk"])
                .unwrap(),
        );

        let resp =
            futures_executor::block_on(m.execute(http::Request::new(RequestBody::Empty))).unwrap();

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut pinned = std::pin::pin!(resp.into_body());

        let first = match pinned.as_mut().poll_frame(&mut cx) {
            Poll::Ready(Some(Ok(f))) => f,
            other => panic!("expected the first frame ready, got {other:?}"),
        };
        assert_eq!(
            first.into_data().unwrap(),
            Bytes::from_static(b"first-chunk"),
            "first poll must yield only the first chunk, not the whole payload"
        );

        let second = match pinned.as_mut().poll_frame(&mut cx) {
            Poll::Ready(Some(Ok(f))) => f,
            other => panic!("expected the second frame ready, got {other:?}"),
        };
        assert_eq!(
            second.into_data().unwrap(),
            Bytes::from_static(b"second-chunk")
        );

        match pinned.as_mut().poll_frame(&mut cx) {
            Poll::Ready(None) => {}
            other => panic!("expected end of stream after both frames, got {other:?}"),
        }
    }

    /// No existing test before this round ever queued more than one
    /// response — the order could have matched what was expected by
    /// coincidence (if `push_back`/`pop_front` had been swapped for stack
    /// semantics somewhere). Here three responses, distinguishable by
    /// status, must come back in exactly the order they were queued.
    #[test]
    fn responses_replay_in_fifo_order() {
        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(200).body("").unwrap());
        m.push_response(http::Response::builder().status(201).body("").unwrap());
        m.push_response(http::Response::builder().status(202).body("").unwrap());

        for expected in [200u16, 201, 202] {
            let resp =
                futures_executor::block_on(m.execute(http::Request::new(RequestBody::Empty)))
                    .unwrap();
            assert_eq!(resp.status().as_u16(), expected);
        }
    }

    // ── TestTimer ─────────────────────────────────────────────────────

    #[test]
    fn test_timer_sleep_resolves_immediately_without_real_waiting() {
        use http_ng_core::unversioned::Timer;
        use std::time::{Duration, Instant};

        let t = TestTimer::new();
        let start = Instant::now();
        // A duration long enough that a real sleep would make this test
        // itself violate "every wait bounded" if `sleep` genuinely waited.
        futures_executor::block_on(t.sleep(Duration::from_secs(3600)));
        assert!(
            Instant::now().duration_since(start) < Duration::from_millis(100),
            "TestTimer::sleep must resolve immediately, not actually wait"
        );
    }

    #[test]
    fn test_timer_records_every_sleep_call_in_order() {
        use http_ng_core::unversioned::Timer;
        use std::time::Duration;

        let t = TestTimer::new();
        futures_executor::block_on(t.sleep(Duration::from_millis(1)));
        futures_executor::block_on(t.sleep(Duration::from_millis(2)));
        futures_executor::block_on(t.sleep(Duration::from_millis(3)));
        assert_eq!(
            t.sleeps(),
            vec![
                Duration::from_millis(1),
                Duration::from_millis(2),
                Duration::from_millis(3)
            ],
            "recorded in call order, not e.g. reversed or deduplicated"
        );
    }

    /// `Clone` shares the same recording — a test that hands one clone to
    /// `SseBuilder::with_timer` and keeps another to inspect afterward must
    /// see the SAME sleeps, not an independent, empty log.
    #[test]
    fn test_timer_clones_share_the_same_recording() {
        use http_ng_core::unversioned::Timer;
        use std::time::Duration;

        let original = TestTimer::new();
        let handed_to_caller = original.clone();
        futures_executor::block_on(handed_to_caller.sleep(Duration::from_millis(42)));
        assert_eq!(
            original.sleeps(),
            vec![Duration::from_millis(42)],
            "a clone's sleep must be visible through the original handle too"
        );
    }

    #[test]
    fn test_timer_now_and_elapsed_since_track_the_virtual_clock() {
        use http_ng_core::unversioned::Timer;
        use std::time::Duration;

        let t = TestTimer::new();
        let t0 = t.now();
        futures_executor::block_on(t.sleep(Duration::from_millis(10)));
        let t1 = t.now();
        assert_eq!(t.elapsed_since(t0), Duration::from_millis(10));
        assert_eq!(t1, Duration::from_millis(10));
    }
}
