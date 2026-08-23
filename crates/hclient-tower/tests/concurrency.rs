//! A bound on how many requests are in flight at once — `tower`'s
//! `ConcurrencyLimit`, driven through both adapters.
//!
//! Nothing here is reimplemented: the point of this crate is that the
//! `tower` stack applies, so the deliverable is the layer actually
//! limiting when it sits between `TransportService` and `ServiceTransport`,
//! and a written-down account of the one thing it does not cover.
//!
//! # Why these tests poll by hand
//!
//! Concurrency claims need several futures alive at once and a say in when
//! each is polled. A `block_on` per future would run them one at a time
//! and could not tell "the third request is waiting for a permit" from
//! "the third request has not been started yet". So each test builds its
//! futures, pins them, and polls them with `Waker::noop()` — no executor,
//! no runtime, no sleeping, and no timing assumptions. `tokio::sync::
//! Semaphore`, which is what `ConcurrencyLimit` holds, needs no reactor,
//! so this is not cheating around a missing runtime.

use bytes::Bytes;
use hclient_core::unversioned::Transport;
use hclient_core::{Capabilities, Error, RequestBody};
use hclient_tower::{ServiceTransport, TransportService};
use http_body::Body as _;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use tower::limit::ConcurrencyLimit;

/// A body that never ends, so "this response's body is still open" is an
/// unambiguous state rather than a race with a buffered `Full`.
struct NeverEnds;

impl http_body::Body for NeverEnds {
    type Data = Bytes;
    type Error = Error;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        Poll::Pending
    }
}

/// A transport that reports when an exchange has actually STARTED — when
/// its future is first polled, not when the future was constructed — and
/// finishes only when told to.
///
/// Counting at first poll is what makes the limit observable: a
/// concurrency limit that failed to limit would still construct the same
/// three futures.
#[derive(Clone)]
struct Gated {
    started: Arc<AtomicUsize>,
    open: Arc<AtomicBool>,
    caps: Capabilities,
}

impl Gated {
    fn new() -> Self {
        Self {
            started: Arc::new(AtomicUsize::new(0)),
            open: Arc::new(AtomicBool::new(false)),
            caps: Capabilities::default(),
        }
    }
    fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }
    fn open_the_gate(&self) {
        self.open.store(true, Ordering::SeqCst);
    }
}

impl Transport for Gated {
    type Body = NeverEnds;
    type Error = Error;

    fn execute(
        &self,
        _req: http::Request<RequestBody>,
    ) -> impl Future<Output = Result<http::Response<NeverEnds>, Error>> {
        let started = Arc::clone(&self.started);
        let open = Arc::clone(&self.open);
        let mut counted = false;
        std::future::poll_fn(move |_| {
            if !counted {
                counted = true;
                started.fetch_add(1, Ordering::SeqCst);
            }
            if open.load(Ordering::SeqCst) {
                Poll::Ready(Ok(http::Response::new(NeverEnds)))
            } else {
                Poll::Pending
            }
        })
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

fn req() -> http::Request<RequestBody> {
    http::Request::builder()
        .uri("https://example.com/")
        .body(RequestBody::Empty)
        .unwrap()
}

fn limited(g: Gated, max: usize) -> impl Transport<Body = NeverEnds, Error = Error> {
    let caps = g.capabilities().clone();
    ServiceTransport::new(ConcurrencyLimit::new(TransportService::new(g), max), caps)
}

/// The property the limit exists for: with a limit of two, a third request
/// does not touch the transport until one of the first two has finished.
///
/// Both halves matter. Without the first assertion a limit of zero would
/// pass; without the second, a limit that never let anything through would.
#[test]
fn a_third_request_does_not_reach_the_transport_until_a_permit_frees_up() {
    let g = Gated::new();
    let t = limited(g.clone(), 2);

    let mut cx = Context::from_waker(Waker::noop());
    let mut a = pin!(t.execute(req()));
    let mut b = pin!(t.execute(req()));
    let mut c = pin!(t.execute(req()));

    assert!(a.as_mut().poll(&mut cx).is_pending());
    assert!(b.as_mut().poll(&mut cx).is_pending());
    assert!(c.as_mut().poll(&mut cx).is_pending());
    assert_eq!(
        g.started(),
        2,
        "the limit is two, so exactly two exchanges may have started"
    );

    // Let the first two finish; their permits are released with them.
    g.open_the_gate();
    assert!(a.as_mut().poll(&mut cx).is_ready());
    assert!(b.as_mut().poll(&mut cx).is_ready());
    assert_eq!(
        g.started(),
        2,
        "still nothing new before the third is polled"
    );

    assert!(c.as_mut().poll(&mut cx).is_ready());
    assert_eq!(
        g.started(),
        3,
        "the third must go through once a permit is free — a limit that \
         never releases would be indistinguishable from one that works, in \
         a test that stopped at the assertion above"
    );
}

/// The limit is enforced because `ServiceTransport` drives `poll_ready` to
/// completion before `call`, on the clone it is about to call — the
/// contract `tests/round_trip.rs` already pins with a service that asserts
/// on it.
///
/// Here the same contract is checked from the other side, against the real
/// layer rather than a hand-written double: `ConcurrencyLimit::call`
/// panics with "max requests in-flight; poll_ready must be called first"
/// when its permit was not reserved, so deleting the readiness drive from
/// the adapter turns this test into a panic rather than a silent
/// overshoot. `round_trip.rs`'s double proves readiness is driven;
/// this proves the drive is the thing that makes a real limiter limit.
#[test]
fn the_permit_is_reserved_in_poll_ready_not_in_call() {
    let g = Gated::new();
    let t = limited(g.clone(), 1);
    g.open_the_gate();

    let mut cx = Context::from_waker(Waker::noop());
    // Sequentially: each of these acquires, calls, and releases. A `call`
    // without a reserved permit would panic on the very first one.
    for expected in 1..=3 {
        let mut f = pin!(t.execute(req()));
        assert!(f.as_mut().poll(&mut cx).is_ready());
        assert_eq!(g.started(), expected);
    }
}

/// **The limit bounds exchanges, not sockets — measured, not assumed.**
///
/// `tower`'s permit lives in `ConcurrencyLimit`'s `ResponseFuture` and is
/// dropped when that future completes, which is when the response HEAD
/// arrives (`tower-0.5.3/src/limit/concurrency/future.rs`: the permit is
/// held only "so that it is dropped when the future completes"). The body
/// streams on afterwards, holding its connection, outside the limit.
///
/// So with a limit of one, a second exchange starts while the first
/// response's body is still open — which this test shows. The design
/// document's "without one, in-flight requests are unbounded and so are
/// sockets" is therefore only half closed by this layer: requests are
/// bounded, sockets are not. Recorded here rather than in prose alone,
/// because it is exactly the kind of claim that would otherwise be
/// inherited as true by whoever builds the connection pool on top of it.
#[test]
fn the_permit_is_released_at_the_response_head_so_bodies_are_not_bounded() {
    let g = Gated::new();
    let t = limited(g.clone(), 1);
    g.open_the_gate();

    let mut cx = Context::from_waker(Waker::noop());
    let mut first = pin!(t.execute(req()));
    let Poll::Ready(Ok(resp)) = first.as_mut().poll(&mut cx) else {
        panic!("the first exchange completes its head");
    };
    let (_, mut body) = resp.into_parts();
    assert!(
        std::pin::Pin::new(&mut body)
            .poll_frame(&mut cx)
            .is_pending(),
        "the first response's body must still be open for this test to mean \
         anything"
    );

    let mut second = pin!(t.execute(req()));
    assert!(second.as_mut().poll(&mut cx).is_ready());
    assert_eq!(
        g.started(),
        2,
        "a second exchange started while the first body was still streaming: \
         the limit does not cover response bodies"
    );
    // Held to here deliberately, and polled once more: the first body was
    // still unfinished for the whole of the second exchange, not merely at
    // the moment it started.
    assert!(
        std::pin::Pin::new(&mut body)
            .poll_frame(&mut cx)
            .is_pending()
    );
}
