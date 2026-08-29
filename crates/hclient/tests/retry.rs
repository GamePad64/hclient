//! `ClientBuilder::retry`, wired.
//!
//! The rules themselves are pure and tested in
//! `hclient-proto/tests/retry_policy.rs`. What only this level can say is
//! that the loop **sends again** — the request reaches the transport a
//! second time, carrying the same body, inside the same operation.

use std::time::Duration;

use hclient::retry::RetryVerdict;
use hclient::retry::{Backoff, RetryPolicy, RetryStatuses};
use hclient_core::{Error, ErrorKind, RequestBody};
use hclient_mock::MockTransport;

/// A backoff short enough that a retry costs a test nothing.
fn brisk() -> Backoff {
    Backoff {
        base: Duration::from_millis(1),
        max: Duration::from_millis(2),
        max_attempts: Some(3),
    }
}

fn unavailable(retry_after: Option<&str>) -> http::Response<&'static str> {
    let mut b = http::Response::builder().status(503);
    if let Some(v) = retry_after {
        b = b.header("retry-after", v);
    }
    b.body("busy").unwrap()
}

fn client(mock: &MockTransport, policy: Option<RetryPolicy>) -> hclient::Client {
    let mut b = hclient::Client::builder(mock.clone());
    if let Some(p) = policy {
        b = b.retry(Immediate, p);
    }
    b.build().expect("supported")
}

/// A clock whose sleeps are over before they start.
///
/// **The retry needs a clock and these tests need no wall time**, and the
/// pair of those facts is why the timer is a parameter of
/// `ClientBuilder::retry` rather than the client's default. Without one,
/// a build lacking `default-transport` gets `NoClock`, whose `Sleep` is
/// `std::future::Pending` — four of these tests hung for 300 s under
/// `--no-default-features` before the signature took a timer, which is a
/// worse failure than any of them could have produced by being wrong.
///
/// It also means every assertion below is about the *decision* and never
/// about a delay: an implementation that retried instantly and one that
/// waited an hour are indistinguishable here, deliberately, because the
/// waiting is `Backoff`'s and is tested where `Backoff` is.
#[derive(Debug, Clone, Copy)]
struct Immediate;

impl hclient_core::unversioned::Timer for Immediate {
    type Instant = std::time::Instant;
    type Sleep = std::future::Ready<()>;

    fn sleep(&self, _: Duration) -> Self::Sleep {
        std::future::ready(())
    }
    fn now(&self) -> Self::Instant {
        std::time::Instant::now()
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        earlier.elapsed()
    }
}

fn get(c: &hclient::Client) -> Result<hclient::Collected, Error> {
    futures_executor::block_on(async {
        c.get("https://example.com/x").send().await?.collect().await
    })
}

/// **The default retries nothing a server answered.** A `503` is an
/// answer, and repeating it can duplicate work only the caller knows the
/// cost of.
#[test]
fn the_default_policy_does_not_repeat_a_status() {
    let m = MockTransport::new();
    m.push_response(unavailable(None));
    m.push_response(http::Response::new("second"));
    let c = client(&m, Some(RetryPolicy::default()));

    let got = get(&c).expect("a 503 is an answer, not a failure");
    assert_eq!(got.status(), 503);
    assert_eq!(m.requests().len(), 1, "sent once");
}

/// With the statuses asked for, the hop is sent again and the second
/// answer is the one the caller gets.
#[test]
fn a_transient_status_is_sent_again_within_the_same_operation() {
    let m = MockTransport::new();
    m.push_response(unavailable(None));
    m.push_response(unavailable(None));
    m.push_response(http::Response::new("third"));
    let c = client(
        &m,
        Some(RetryPolicy {
            backoff: brisk(),
            statuses: RetryStatuses::Transient,
            ..RetryPolicy::default()
        }),
    );

    let got = get(&c).expect("the third answer");
    assert_eq!(got.status(), 200);
    assert_eq!(got.text().unwrap(), "third");
    assert_eq!(m.requests().len(), 3, "two retries after the first send");
}

/// **The attempt count binds.** A server wedged on `503` gets exactly as
/// many requests as the backoff allows and the caller gets its answer —
/// not an error of ours, which would hide a status they can act on.
#[test]
fn a_wedged_server_gets_the_attempts_and_no_more() {
    let m = MockTransport::new();
    for _ in 0..8 {
        m.push_response(unavailable(None));
    }
    let c = client(
        &m,
        Some(RetryPolicy {
            backoff: brisk(),
            statuses: RetryStatuses::Transient,
            ..RetryPolicy::default()
        }),
    );

    let got = get(&c).expect("the server's own answer stands");
    assert_eq!(got.status(), 503);
    assert_eq!(m.requests().len(), 3, "three attempts, then the answer");
}

/// **A streaming body is never sent twice**, whatever the policy says.
/// The condition is the body's type and is known before the first
/// attempt — no policy overrides it and no server can ask past it.
#[test]
fn a_streaming_body_is_not_repeated_even_when_the_policy_says_retry() {
    let m = MockTransport::new();
    m.push_response(unavailable(None));
    m.push_response(http::Response::new("would have been the retry"));
    let c = client(
        &m,
        Some(RetryPolicy {
            backoff: brisk(),
            statuses: RetryStatuses::Transient,
            ..RetryPolicy::default()
        }),
    );

    let body = RequestBody::Streaming(Box::new(OneShot(Some(bytes::Bytes::from_static(b"once")))));
    let got = futures_executor::block_on(async {
        c.post("https://example.com/x")
            .body(body)
            .send()
            .await?
            .collect()
            .await
    })
    .expect("the 503 is the answer");

    assert_eq!(got.status(), 503);
    assert_eq!(m.requests().len(), 1, "sent once and never again");
}

/// **A failure the transport marks as unsent is retried by the default
/// policy**, and that is the whole differentiator: nothing reached a
/// server, so there is nothing to duplicate and no promise to ask for.
#[test]
fn an_unsent_failure_is_retried_by_the_default_policy() {
    let m = MockTransport::new();
    m.push_transport_error(Error::new(ErrorKind::Connect, Refused).unsent());
    m.push_response(http::Response::new("after the refusal"));
    let c = client(
        &m,
        Some(RetryPolicy {
            backoff: brisk(),
            ..RetryPolicy::default()
        }),
    );

    let got = get(&c).expect("the retry succeeded");
    assert_eq!(got.text().unwrap(), "after the refusal");
    assert_eq!(m.requests().len(), 2);
}

/// **And the same failure unmarked is not.** `ErrorKind` is identical in
/// both tests; only the transport's claim differs — which is the pair
/// that says the decision reads `is_unsent` and not the category. A
/// category-based retry would repeat a request that a response head over
/// `H1Opts::max_headers` had already delivered to a server.
#[test]
fn the_same_failure_without_the_mark_is_not_retried() {
    let m = MockTransport::new();
    m.push_transport_error(Error::new(ErrorKind::Connect, Refused));
    m.push_response(http::Response::new("never reached"));
    let c = client(
        &m,
        Some(RetryPolicy {
            backoff: brisk(),
            ..RetryPolicy::default()
        }),
    );

    let err = get(&c).expect_err("not retried, so the failure stands");
    assert_eq!(*err.kind(), ErrorKind::Connect);
    assert_eq!(m.requests().len(), 1);
}

/// A `Retry-After` this client cannot read stops the retry rather than
/// letting the backoff decide — a client that retried on its own schedule
/// would be doing the one thing the header forbids.
#[test]
fn an_http_date_retry_after_stops_the_retry() {
    let m = MockTransport::new();
    m.push_response(unavailable(Some("Wed, 21 Oct 2015 07:28:00 GMT")));
    m.push_response(http::Response::new("never reached"));
    let c = client(
        &m,
        Some(RetryPolicy {
            backoff: brisk(),
            statuses: RetryStatuses::Transient,
            ..RetryPolicy::default()
        }),
    );

    let got = get(&c).expect("the 503 stands");
    assert_eq!(got.status(), 503);
    assert_eq!(m.requests().len(), 1);
}

/// A `Retry-After` beyond the ceiling stops it too, and the control below
/// it is the same header inside the ceiling.
#[test]
fn a_retry_after_beyond_the_ceiling_stops_and_one_inside_it_does_not() {
    for (value, expected_sends) in [("3600", 1), ("0", 2)] {
        let m = MockTransport::new();
        m.push_response(unavailable(Some(value)));
        m.push_response(http::Response::new("retried"));
        let c = client(
            &m,
            Some(RetryPolicy {
                backoff: brisk(),
                statuses: RetryStatuses::Transient,
                max_retry_after: Duration::from_secs(60),
                ..RetryPolicy::default()
            }),
        );
        let _ = get(&c).expect("an answer either way");
        assert_eq!(
            m.requests().len(),
            expected_sends,
            "Retry-After: {value} should give {expected_sends} send(s)"
        );
    }
}

#[derive(Debug, thiserror::Error)]
#[error("connection refused")]
struct Refused;

/// A body that yields once and cannot be rewound — `RetryKind::Impossible`.
struct OneShot(Option<bytes::Bytes>);

impl http_body::Body for OneShot {
    type Data = bytes::Bytes;
    type Error = Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, Self::Error>>> {
        std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
    }
}

/// **The question this client refuses to answer for the caller.** A
/// `POST` with a buffered body is trivially replayable and is exactly the
/// request that must not be repeated — `RetryKind` says *can*, and
/// nothing here says *may*. The predicate is where *may* lives.
#[test]
fn a_predicate_can_refuse_a_retry_the_policy_approved() {
    for (method, expected_sends) in [("GET", 2), ("POST", 1)] {
        let m = MockTransport::new();
        m.push_response(unavailable(None));
        m.push_response(http::Response::new("retried"));
        let c = hclient::Client::builder(m.clone())
            .retry(
                Immediate,
                RetryPolicy {
                    backoff: brisk(),
                    statuses: RetryStatuses::Transient,
                    ..RetryPolicy::default()
                },
            )
            .retry_predicate(|r| {
                if r.method() == http::Method::GET {
                    RetryVerdict::Retry
                } else {
                    RetryVerdict::Stop
                }
            })
            .build()
            .expect("supported");

        let req = http::Request::builder()
            .method(method)
            .uri("https://example.com/x")
            .body(RequestBody::Empty)
            .unwrap();
        let _ = futures_executor::block_on(c.execute(req)).expect("an answer either way");

        assert_eq!(
            m.requests().len(),
            expected_sends,
            "{method} should have been sent {expected_sends} time(s)"
        );
    }
}

/// **It can only narrow.** A predicate saying `Retry` about a retry the
/// policy already refused changes nothing — it is never asked, because it
/// is consulted after the decision rather than as part of it.
#[test]
fn a_predicate_cannot_widen_what_the_policy_refused() {
    let m = MockTransport::new();
    m.push_response(unavailable(None));
    m.push_response(http::Response::new("never reached"));
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = asked.clone();

    // The default policy does not retry statuses at all.
    let c = hclient::Client::builder(m.clone())
        .retry(Immediate, RetryPolicy::default())
        .retry_predicate(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            RetryVerdict::Retry
        })
        .build()
        .expect("supported");

    let got = get(&c).expect("the 503 stands");
    assert_eq!(got.status(), 503);
    assert_eq!(m.requests().len(), 1, "not retried");
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "and the predicate was never even asked"
    );
}

/// What the predicate is handed is the policy's **output**, including the
/// delay it settled on — `Retry-After` included — so a caller can refuse
/// a wait that is too long for this particular target.
#[test]
fn the_predicate_sees_the_decision_the_policy_reached() {
    let m = MockTransport::new();
    m.push_response(unavailable(Some("30")));
    m.push_response(http::Response::new("retried"));
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();

    let c = hclient::Client::builder(m.clone())
        .retry(
            Immediate,
            RetryPolicy {
                backoff: brisk(),
                statuses: RetryStatuses::Transient,
                ..RetryPolicy::default()
            },
        )
        .retry_predicate(move |r| {
            sink.lock().unwrap().push((
                r.attempt(),
                r.delay(),
                r.uri().path().to_owned(),
                r.method().clone(),
            ));
            RetryVerdict::Retry
        })
        .build()
        .expect("supported");

    let _ = get(&c).expect("an answer");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "asked once, about the one approved retry");
    let (attempt, delay, path, method) = &seen[0];
    assert_eq!(*attempt, 1, "attempts already made");
    assert_eq!(
        *delay,
        Duration::from_secs(30),
        "the server's Retry-After, not the raw backoff"
    );
    assert_eq!(path, "/x");
    assert_eq!(method, http::Method::GET);
}
