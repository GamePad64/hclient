//! The two body views, asserted directly rather than through a client.
//!
//! `OutgoingBody` and `IncomingBody` are the app transport's two halves of
//! the body boundary, and every request in `tests/app_transport.rs` and
//! `tests/axum_router.rs` goes through both — yet nothing there reads
//! `is_end_stream`, `size_hint` or the `Debug`. A client collects a body by
//! polling frames until `None`, so a body that lied about every one of
//! those three answered every test in this suite correctly.
//!
//! Measured, on each of the ten mutants this crate's sweep produced: the
//! four on `OutgoingBody` and the three on `IncomingBody` all survived the
//! whole suite before this file existed. They are hints rather than
//! decoration — `http_body_util::BodyExt::collect` sizes its buffer from
//! `size_hint`, and `hyper` skips framing a body that says it has ended —
//! so a wrong answer is a wrong number of allocations at best and a
//! truncated body at worst.
//!
//! **Each arm is asserted separately because the enum is what the
//! mutations collapse.** A single `Full` case cannot tell
//! `is_end_stream -> false` from the real thing, and a single `Empty` case
//! cannot tell `-> true`; only the pair discriminates, which is why both
//! constants appear in the survivor list and why both are pinned here.

use bytes::Bytes;
use hclient_core::body::RequestBody;
use hclient_core::transport::Transport as _;
use hclient_tower::app::{AppTransport, IncomingBody, OutgoingBody};
use http_body::Body as _;
use http_body_util::BodyExt as _;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

/// `RequestBody::reduce` collapses an empty `Full` to `Empty`, so the only
/// way to reach `Inner::Full` is with bytes — which is what makes
/// `size_hint` discriminating across the three arms rather than answering
/// zero twice.
fn outgoing(body: RequestBody) -> OutgoingBody {
    OutgoingBody::new(body).expect("no rewind depth is exceeded here")
}

fn streaming(s: &'static str) -> RequestBody {
    RequestBody::Streaming(Box::new(
        http_body_util::Full::new(Bytes::from_static(s.as_bytes()))
            .map_err(|e: std::convert::Infallible| match e {}),
    ))
}

/// A body whose hints are deliberately unlike the defaults, so a forwarder
/// that answered `Default::default()` or a constant is distinguishable from
/// one that asks the body underneath.
///
/// `is_end_stream` is `true` while `size_hint` is a non-zero exact — an
/// inconsistent pair no honest body would report, and the point: it means
/// each of the two answers can only have come from here.
#[derive(Clone)]
struct Peculiar;

impl http_body::Body for Peculiar {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, std::io::Error>>> {
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        true
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(4242)
    }
}

/// A body that fails, so `IncomingBody`'s error conversion has a subject.
#[derive(Clone)]
struct Failing;

impl http_body::Body for Failing {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, std::io::Error>>> {
        Poll::Ready(Some(Err(std::io::Error::other("the app's body gave up"))))
    }
}

// ---------------------------------------------------------------- Outgoing

/// The `Debug` is written rather than derived — a streaming body has none —
/// so what it prints is this crate's own statement and nothing else checks
/// it. All three arms, because the mutation that survived replaced the
/// whole `match` with `Ok(())`, which is indistinguishable from the truth
/// for any single arm that happens to print nothing.
#[test]
fn the_outgoing_bodys_debug_names_the_arm_and_the_length() {
    assert_eq!(
        format!("{:?}", outgoing(RequestBody::Empty)),
        "OutgoingBody::Empty"
    );
    assert_eq!(
        format!(
            "{:?}",
            outgoing(RequestBody::Full(Bytes::from_static(b"seven!!")))
        ),
        "OutgoingBody::Full(7 bytes)",
        "the length is the part a reader is looking for"
    );
    assert_eq!(
        format!("{:?}", outgoing(streaming("streamed"))),
        "OutgoingBody::Streaming(..)",
        "a streaming body has no Debug of its own, so this reports the shape"
    );
}

/// The `Full` arm's `Debug` reads the *remaining* bytes rather than the
/// original length, which is what `b.as_ref().map_or(0, Bytes::len)` says
/// and what a reader debugging a half-sent body needs.
///
/// Without this, the `map_or(0, ..)` fallback is unreachable: every fresh
/// `Full` is `Some`, so nothing else in this suite ever prints a taken one.
#[test]
fn a_full_outgoing_body_that_has_been_taken_debugs_as_zero_bytes() {
    let mut b = outgoing(RequestBody::Full(Bytes::from_static(b"seven!!")));
    assert_eq!(format!("{b:?}"), "OutgoingBody::Full(7 bytes)");

    let mut cx = Context::from_waker(Waker::noop());
    let Poll::Ready(Some(Ok(_))) = Pin::new(&mut b).poll_frame(&mut cx) else {
        panic!("the one frame")
    };
    assert_eq!(
        format!("{b:?}"),
        "OutgoingBody::Full(0 bytes)",
        "the frame is gone, and the Debug says so rather than repeating the original length"
    );
}

/// `is_end_stream` in both directions and on every arm.
///
/// The pair is the assertion: `-> true` and `-> false` are two separate
/// survivors, and a test carrying only one arm kills only one of them.
/// `Full` moves from `false` to `true` as its single frame is taken, which
/// is the transition neither constant can imitate.
#[test]
fn the_outgoing_bodys_end_of_stream_is_the_arm_it_is_in() {
    assert!(
        outgoing(RequestBody::Empty).is_end_stream(),
        "no body at all has ended before it began"
    );

    let mut full = outgoing(RequestBody::Full(Bytes::from_static(b"payload")));
    assert!(
        !full.is_end_stream(),
        "a frame is still owed — a `true` here would let a server frame nothing"
    );
    let mut cx = Context::from_waker(Waker::noop());
    let Poll::Ready(Some(Ok(_))) = Pin::new(&mut full).poll_frame(&mut cx) else {
        panic!("the one frame")
    };
    assert!(
        full.is_end_stream(),
        "and once it is taken, it has: `Full(None)` is the end"
    );

    assert!(
        !outgoing(streaming("bytes")).is_end_stream(),
        "a streaming arm answers from the body underneath, which has a frame left"
    );
}

/// `size_hint` per arm, and the three answers are genuinely three.
///
/// `RequestBody::reduce` collapses an empty `Full` to `Empty`, so an
/// `Inner::Full` always holds bytes — which is what makes the exact
/// non-zero hint unreachable for any other arm and kills the
/// `Default::default()` mutant, whose hint is `0..` with no exact.
#[test]
fn the_outgoing_bodys_size_hint_is_exact_where_the_length_is_known() {
    assert_eq!(
        outgoing(RequestBody::Empty).size_hint().exact(),
        Some(0),
        "no body is exactly zero bytes, not merely unknown"
    );

    let mut full = outgoing(RequestBody::Full(Bytes::from_static(b"payload")));
    assert_eq!(
        full.size_hint().exact(),
        Some(7),
        "the whole point of the exact hint: a server sizes its buffer from it"
    );
    let mut cx = Context::from_waker(Waker::noop());
    let Poll::Ready(Some(Ok(_))) = Pin::new(&mut full).poll_frame(&mut cx) else {
        panic!("the one frame")
    };
    assert_eq!(
        full.size_hint().exact(),
        Some(0),
        "an already-taken Full has no bytes left, which is the same fact as Empty"
    );

    assert_eq!(
        outgoing(streaming("12345")).size_hint().exact(),
        Some(5),
        "and a streaming arm forwards whatever the body underneath says"
    );
}

/// The bytes themselves still arrive — the control for the three tests
/// above, which all read hints rather than content. A body reporting
/// perfect hints and yielding nothing would pass every one of them.
#[test]
fn an_outgoing_body_yields_the_bytes_it_was_built_from() {
    let collected = futures_executor::block_on(
        outgoing(RequestBody::Full(Bytes::from_static(b"payload"))).collect(),
    )
    .expect("collects")
    .to_bytes();
    assert_eq!(&collected[..], b"payload");

    let streamed = futures_executor::block_on(outgoing(streaming("streamed")).collect())
        .expect("collects")
        .to_bytes();
    assert_eq!(&streamed[..], b"streamed");

    let empty = futures_executor::block_on(outgoing(RequestBody::Empty).collect())
        .expect("collects")
        .to_bytes();
    assert!(empty.is_empty());
}

// ---------------------------------------------------------------- Incoming

/// `IncomingBody`'s field is private and it has no constructor, so the only
/// way to hold one is the way a consumer does: through
/// `AppTransport::execute`. That is the honest instrument rather than a
/// concession — it means every assertion below is about the path an
/// `axum::Router`'s response really takes.
fn response_body<B>(app_body: B) -> IncomingBody<B>
where
    B: http_body::Body<Data = Bytes> + Unpin + Clone + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send + Sync + 'static,
{
    #[derive(Clone)]
    struct Answers<B>(B);

    impl<B: Clone> tower_service::Service<http::Request<OutgoingBody>> for Answers<B> {
        type Response = http::Response<B>;
        type Error = std::convert::Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: http::Request<OutgoingBody>) -> Self::Future {
            std::future::ready(Ok(http::Response::new(self.0.clone())))
        }
    }

    let t = AppTransport::new("testserver", Answers(app_body));
    let resp = futures_executor::block_on(
        t.execute(
            http::Request::builder()
                .uri("http://testserver/")
                .body(RequestBody::Empty)
                .expect("a well-formed request"),
        ),
    )
    .expect("the app answers");
    resp.into_body()
}

/// `IncomingBody` forwards both hints to the body underneath rather than
/// answering for it.
///
/// `Peculiar` reports a pair no real body would — ended, and 4242 bytes
/// exact — so each answer can only have come from there. Both mutants are
/// killed by one fixture: `-> true`/`-> false` on `is_end_stream` and
/// `Default::default()` on `size_hint`, the last of which has no exact at
/// all.
#[test]
fn the_incoming_body_forwards_both_hints_rather_than_answering_for_the_app() {
    let b = response_body(Peculiar);
    assert!(
        b.is_end_stream(),
        "forwarded, not decided here: `Peculiar` says it has ended"
    );
    assert_eq!(
        b.size_hint().exact(),
        Some(4242),
        "and the size is the app's own, which a `Default::default()` would lose"
    );
}

/// The other direction of `is_end_stream`, against a body that has not
/// ended — without which `-> true` is indistinguishable from forwarding.
#[test]
fn an_incoming_body_that_has_not_ended_says_so() {
    let b = response_body(http_body_util::Full::new(Bytes::from_static(b"still here")));
    assert!(
        !b.is_end_stream(),
        "a `Full` with its frame still owed has not ended"
    );
    assert_eq!(b.size_hint().exact(), Some(10));
}

/// **The conversion the type exists for.** `DynTransport`'s blanket impl
/// wants a body error that converts into this workspace's; a server-side
/// body's does not. So the app's error arrives as `ErrorKind::Body`
/// carrying the app's own message, and nothing in the rest of this suite
/// reaches it — every fixture elsewhere answers `Infallible`.
#[test]
fn an_app_body_failure_arrives_as_a_body_error_carrying_the_apps_message() {
    let err = futures_executor::block_on(response_body(Failing).collect())
        .expect_err("the app's body fails");
    assert_eq!(*err.kind(), hclient_core::error::ErrorKind::Body);
    assert!(
        format!("{err:#}").contains("the app's body gave up"),
        "the app's own text must survive the boxing: {err:#}"
    );
}

/// The frames themselves are forwarded — the control for the hint tests,
/// which would all pass over a body that yielded nothing.
#[test]
fn an_incoming_body_yields_the_apps_frames() {
    let collected = futures_executor::block_on(
        response_body(http_body_util::Full::new(Bytes::from_static(
            b"from the app",
        )))
        .collect(),
    )
    .expect("collects")
    .to_bytes();
    assert_eq!(&collected[..], b"from the app");
}
