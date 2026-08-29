//! The octet counter: what it reports, when, and what it refuses to say.
//!
//! Every claim here is about the seam rather than about a backend, which
//! is what makes it testable with no socket at all: [`Meter`] is arithmetic
//! and [`Counting`] is an `http_body::Body` wrapper, so the whole of
//! *"cumulative, only when it moved, and an absent denominator rather than
//! a guessed one"* can be driven by hand.

use bytes::Bytes;
use hclient_core::unversioned::{
    Counting, Direction, Event, Hooks, Meter, Metered, NoHooks, Progress, meter,
};
use http_body::{Frame, SizeHint};
use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};

/// What a hook saw, flattened to the three fields these tests assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    direction: Direction,
    transferred: u64,
    expected: Option<u64>,
}

#[derive(Clone, Default)]
struct Recorder {
    seen: Rc<RefCell<Vec<Seen>>>,
}

impl Recorder {
    fn seen(&self) -> Vec<Seen> {
        self.seen.borrow().clone()
    }
    fn one_way(&self, d: Direction) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.direction == d)
            .collect()
    }
}

impl Hooks for Recorder {
    fn on(&self, event: &Event<'_>) {
        if let Event::Progress(p) = event {
            self.seen.borrow_mut().push(Seen {
                direction: p.direction,
                transferred: p.transferred,
                expected: p.expected,
            });
        }
    }
}

/// A body of fixed chunks whose `size_hint` is whatever the test says, so
/// the *known length* and *unknown length* cases are one type apart.
struct Chunks {
    frames: Vec<Frame<Bytes>>,
    hint: SizeHint,
}

impl Chunks {
    fn data(parts: &[&str], hint: SizeHint) -> Self {
        Self {
            frames: parts
                .iter()
                .map(|p| Frame::data(Bytes::from_static(p.as_bytes().to_vec().leak())))
                .collect(),
            hint,
        }
    }
}

impl http_body::Body for Chunks {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.frames.is_empty() {
            return Poll::Ready(None);
        }
        Poll::Ready(Some(Ok(self.frames.remove(0))))
    }

    fn size_hint(&self) -> SizeHint {
        self.hint
    }
}

/// Drains a body with a noop waker, since nothing here ever answers
/// `Pending`.
fn drain<B: http_body::Body + Unpin>(mut b: B) -> usize {
    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut n = 0;
    while let Poll::Ready(Some(_)) = Pin::new(&mut b).poll_frame(&mut cx) {
        n += 1;
    }
    n
}

fn uri() -> http::Uri {
    "https://example.invalid/thing".parse().expect("literal")
}

/// **The total is cumulative and the events are one per frame that moved.**
///
/// Three chunks of 1, 2 and 3 octets give `1, 3, 6` rather than
/// `1, 2, 3` — which is the property every other claim in this file rests
/// on, because it is what lets a hook drop events and still be right.
#[test]
fn a_receiving_total_accumulates_rather_than_reporting_deltas() {
    let rec = Recorder::default();
    let body = Chunks::data(&["a", "bc", "def"], SizeHint::with_exact(6));
    let counted = Counting::new(
        body,
        rec.clone(),
        hclient_core::unversioned::ConnectionId::UNWATCHED,
        Some(&uri()),
        None,
    );
    drain(counted);

    assert_eq!(
        rec.one_way(Direction::Receiving)
            .iter()
            .map(|s| s.transferred)
            .collect::<Vec<_>>(),
        vec![1, 3, 6],
    );
}

/// **A poll that moved nothing reports nothing.**
///
/// The end of the body is a poll like any other: it yields `None`, the
/// count has not changed, and no event is emitted for it. Without this
/// rule every body would end with a duplicate of its last event, and a
/// hook counting *events* rather than octets would be wrong by one.
#[test]
fn the_poll_that_ends_a_body_adds_no_event() {
    let rec = Recorder::default();
    let body = Chunks::data(&["ab"], SizeHint::with_exact(2));
    let counted = Counting::new(
        body,
        rec.clone(),
        hclient_core::unversioned::ConnectionId::UNWATCHED,
        Some(&uri()),
        None,
    );
    // Two polls: one frame, then the end.
    assert_eq!(drain(counted), 1);
    assert_eq!(rec.one_way(Direction::Receiving).len(), 1);
}

/// **An unknown length is `None`, never a zero and never a lower bound.**
///
/// A chunked response states no length, so the denominator is absent — the
/// distinction `Progress::expected` exists for. The control is the test
/// above, where the same body with an exact hint reports `Some(6)`.
#[test]
fn an_unstated_length_is_none_rather_than_a_guess() {
    let rec = Recorder::default();
    let body = Chunks::data(&["abc"], SizeHint::default());
    let counted = Counting::new(
        body,
        rec.clone(),
        hclient_core::unversioned::ConnectionId::UNWATCHED,
        Some(&uri()),
        None,
    );
    drain(counted);
    assert_eq!(
        rec.one_way(Direction::Receiving),
        vec![Seen {
            direction: Direction::Receiving,
            transferred: 3,
            expected: None,
        }],
    );
}

/// **A stated length is carried verbatim.** The pair with the test above
/// is the assertion: an `Option` that was always `None` would pass that
/// one alone.
#[test]
fn a_stated_length_is_reported_as_the_denominator() {
    let rec = Recorder::default();
    let body = Chunks::data(&["abc"], SizeHint::with_exact(3));
    let counted = Counting::new(
        body,
        rec.clone(),
        hclient_core::unversioned::ConnectionId::UNWATCHED,
        Some(&uri()),
        None,
    );
    drain(counted);
    assert_eq!(rec.one_way(Direction::Receiving)[0].expected, Some(3));
}

/// **A trailers frame is not octets.**
///
/// No `Content-Length` counts header fields, so counting them would put
/// the numerator past a denominator that is right. The body here yields
/// one 3-octet data frame and one trailers frame, and the total stays 3.
#[test]
fn trailers_are_not_counted_as_body_octets() {
    let rec = Recorder::default();
    let mut frames = vec![Frame::data(Bytes::from_static(b"abc"))];
    frames.push(Frame::trailers(http::HeaderMap::new()));
    let body = Chunks {
        frames,
        hint: SizeHint::with_exact(3),
    };
    let counted = Counting::new(
        body,
        rec.clone(),
        hclient_core::unversioned::ConnectionId::UNWATCHED,
        Some(&uri()),
        None,
    );
    drain(counted);
    assert_eq!(
        rec.one_way(Direction::Receiving)
            .iter()
            .map(|s| s.transferred)
            .collect::<Vec<_>>(),
        vec![3],
        "a trailers frame moved no body octets, so it is neither counted nor reported",
    );
}

/// **`uri: None` is *do not count here*.**
///
/// The QUIC arm's case: something below already wrapped this body, so this
/// wrapper must be a pass-through even though a hook is watching. The
/// control is every test above, which passes `Some`.
#[test]
fn a_wrapper_told_not_to_count_reports_nothing_and_still_yields_the_body() {
    let rec = Recorder::default();
    let body = Chunks::data(&["abc"], SizeHint::with_exact(3));
    let counted = Counting::new(
        body,
        rec.clone(),
        hclient_core::unversioned::ConnectionId::UNWATCHED,
        None,
        None,
    );
    assert_eq!(drain(counted), 1, "the body still passes through");
    assert!(rec.seen().is_empty());
}

/// **The two directions are separate counters and are labelled.**
///
/// The request body writes into a shared [`Meter`] through [`Metered`],
/// and the response body reports it alongside its own — which is the only
/// arrangement that can report an upload still running after the head.
#[test]
fn the_send_meter_is_reported_beside_the_receive_one_and_the_two_are_distinguishable() {
    let rec = Recorder::default();
    let sent: Arc<Meter> = Arc::new(meter::<Recorder>(Some(4)).expect("watching"));

    // The request body moves 4 octets before anything is read back.
    let outgoing = Metered::new(
        Chunks::data(&["ab", "cd"], SizeHint::with_exact(4)),
        Some(sent.clone()),
    );
    drain(outgoing);
    assert!(
        rec.seen().is_empty(),
        "`Metered` counts and reports nothing — reporting belongs where the hook is",
    );

    let counted = Counting::new(
        Chunks::data(&["xyz"], SizeHint::with_exact(3)),
        rec.clone(),
        hclient_core::unversioned::ConnectionId::UNWATCHED,
        Some(&uri()),
        Some(sent),
    );
    drain(counted);

    assert_eq!(
        rec.one_way(Direction::Sending),
        vec![Seen {
            direction: Direction::Sending,
            transferred: 4,
            expected: Some(4),
        }],
    );
    assert_eq!(
        rec.one_way(Direction::Receiving),
        vec![Seen {
            direction: Direction::Receiving,
            transferred: 3,
            expected: Some(3),
        }],
    );
}

/// **Nothing is measured when nobody is watching**, and [`meter`] is the
/// one place that decides it — the same gate `mark` is for clocks.
#[test]
fn an_unwatched_build_has_no_counter_at_all() {
    assert!(meter::<NoHooks>(Some(10)).is_none());
    assert!(meter::<Recorder>(Some(10)).is_some());
}

/// **A meter reports only what changed since the last look**, and the
/// mechanism is a second stored value rather than a caller remembering.
///
/// Called three times with one increment in the middle: one event.
#[test]
fn a_meter_asked_twice_with_nothing_in_between_reports_once() {
    let rec = Recorder::default();
    let m = meter::<Recorder>(None).expect("watching");
    let uri = uri();
    let id = hclient_core::unversioned::ConnectionId::UNWATCHED;

    m.report(&rec, id, &uri, Direction::Sending);
    m.add(5);
    m.report(&rec, id, &uri, Direction::Sending);
    m.report(&rec, id, &uri, Direction::Sending);

    assert_eq!(
        rec.one_way(Direction::Sending),
        vec![Seen {
            direction: Direction::Sending,
            transferred: 5,
            expected: None,
        }],
        "the first look had nothing to say and the third had nothing new",
    );
}

/// The payload's own accessors, so a hook that keeps the event rather than
/// the fields sees the same numbers. Cheap, and it is what stops
/// `Progress::new`'s argument order being silently wrong.
#[test]
fn a_progress_event_carries_what_it_was_built_with() {
    let uri = uri();
    let p = Progress::new(
        hclient_core::unversioned::ConnectionId::UNWATCHED,
        &uri,
        Direction::Receiving,
        7,
    )
    .expected(Some(9));
    assert_eq!(p.transferred, 7);
    assert_eq!(p.expected, Some(9));
    assert_eq!(p.direction, Direction::Receiving);
    assert_eq!(p.uri, &uri);
    assert_eq!(
        Progress::new(
            hclient_core::unversioned::ConnectionId::UNWATCHED,
            &uri,
            Direction::Sending,
            0,
        )
        .expected,
        None,
        "the setter's default is the absent denominator, not a zero one",
    );
}
