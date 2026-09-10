//! A scheme this crate does not implement, written from outside it.
//!
//! Digest's own tests pin digest. What only this file can say is that the
//! seam carries a scheme with **more than two legs** — which is the shape
//! NTLM and Negotiate have, and the reason the seam exists rather than a
//! third built-in scheme.

use std::sync::{Arc, Mutex};

use hclient::auth::{Auth, AuthFlow, AuthRequest, AuthStep, BoxFlow};
use hclient_core::body::RequestBody;
use hclient_mock::MockTransport;

fn challenge(token: &str) -> http::Response<&'static str> {
    let mut r = http::Response::builder().status(401);
    r = r.header("www-authenticate", format!("Fake {token}"));
    r.body("").unwrap()
}

/// Three legs, the shape NTLM has: an unauthenticated probe, a
/// negotiate, then an authenticate carrying the server's token back.
#[derive(Debug, Clone, Default)]
struct ThreeLeg {
    /// Every `Authorization` this flow produced, for the test to read.
    sent: Arc<Mutex<Vec<String>>>,
}

impl Auth for ThreeLeg {
    fn start(&self) -> BoxFlow {
        Box::new(ThreeLegFlow {
            leg: 0,
            server_token: None,
            sent: self.sent.clone(),
        })
    }
}

struct ThreeLegFlow {
    leg: u8,
    server_token: Option<String>,
    sent: Arc<Mutex<Vec<String>>>,
}

impl AuthFlow for ThreeLegFlow {
    fn authorize(&mut self, _req: &AuthRequest<'_>, headers: &mut http::HeaderMap) {
        let value = match self.leg {
            // Leg 0 is the pre-emptive one: nothing to send yet.
            0 => return,
            1 => "Fake negotiate".to_owned(),
            _ => format!(
                "Fake authenticate:{}",
                self.server_token.as_deref().unwrap_or("")
            ),
        };
        self.sent.lock().unwrap().push(value.clone());
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&value).unwrap(),
        );
    }

    fn on_response(&mut self, status: http::StatusCode, headers: &http::HeaderMap) -> AuthStep {
        if status != http::StatusCode::UNAUTHORIZED || self.leg >= 2 {
            return AuthStep::Done;
        }
        self.server_token = headers
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Fake "))
            .map(ToOwned::to_owned);
        self.leg += 1;
        AuthStep::Again
    }
}

/// **The point of the seam.** Three requests, each carrying what the
/// previous response asked for, and the third answered.
#[test]
fn a_three_leg_scheme_written_outside_this_crate_completes() {
    let m = MockTransport::new();
    m.push_response(challenge("start"));
    m.push_response(challenge("server-nonce"));
    m.push_response(http::Response::new("in"));

    let scheme = ThreeLeg::default();
    let c = hclient::Client::builder(m.clone()).build().expect("client");
    let got = futures_executor::block_on(async {
        c.get("https://a.test/x")
            .auth(scheme.clone())
            .send()
            .await?
            .collect()
            .await
    })
    .expect("the third answer");

    assert_eq!(got.status(), 200);
    assert_eq!(got.text().unwrap(), "in");
    assert_eq!(m.requests().len(), 3, "three legs reached the transport");
    assert_eq!(
        *scheme.sent.lock().unwrap(),
        vec![
            "Fake negotiate".to_owned(),
            "Fake authenticate:server-nonce".to_owned()
        ],
        "and the second leg carried the token the server sent"
    );
}

/// A flow that never finishes is bounded rather than looping for ever.
#[derive(Debug, Clone, Copy, Default)]
struct NeverDone;

impl Auth for NeverDone {
    fn start(&self) -> BoxFlow {
        Box::new(NeverDone)
    }
}

impl AuthFlow for NeverDone {
    fn authorize(&mut self, _: &AuthRequest<'_>, _: &mut http::HeaderMap) {}
    fn on_response(&mut self, _: http::StatusCode, _: &http::HeaderMap) -> AuthStep {
        AuthStep::Again
    }
}

#[test]
fn a_flow_that_never_finishes_is_bounded_and_named() {
    let m = MockTransport::new();
    for _ in 0..10 {
        m.push_response(challenge("again"));
    }
    let c = hclient::Client::builder(m.clone()).build().expect("client");
    let err = futures_executor::block_on(async {
        c.get("https://a.test/x").auth(NeverDone).send().await
    })
    .expect_err("must not loop for ever");

    assert!(format!("{err:#}").contains("did not finish"), "{err:#}");
    assert_eq!(
        m.requests().len(),
        usize::from(hclient::auth::MAX_LEGS),
        "exactly the bound, then the refusal"
    );
}

/// **A body that cannot be sent twice ends it**, and the challenge stands
/// as the answer — the server's own, which is more use than an error of
/// ours. Same gate as the `425` replay and the retry.
#[test]
fn a_streaming_body_gets_one_leg_and_the_challenge_stands() {
    let m = MockTransport::new();
    m.push_response(challenge("start"));
    m.push_response(http::Response::new("never reached"));

    let c = hclient::Client::builder(m.clone()).build().expect("client");
    let got = futures_executor::block_on(async {
        c.post("https://a.test/x")
            .auth(ThreeLeg::default())
            .body(RequestBody::Streaming(Box::new(OneShot(Some(
                bytes::Bytes::from_static(b"once"),
            )))))
            .send()
            .await?
            .collect()
            .await
    })
    .expect("the 401 is the answer");

    assert_eq!(got.status(), 401);
    assert_eq!(m.requests().len(), 1, "sent once and never again");
}

/// **Credentials do not cross an origin.** The scheme is dropped on a hop
/// that changed host, so a `401` from the new one is not answered — the
/// rule digest already followed, now true of every scheme.
#[test]
fn a_scheme_is_not_carried_across_an_origin() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://b.test/x")
            .body("")
            .unwrap(),
    );
    m.push_response(challenge("from-b"));
    m.push_response(http::Response::new("never reached"));

    let scheme = ThreeLeg::default();
    let c = hclient::Client::builder(m.clone()).build().expect("client");
    let got = futures_executor::block_on(async {
        c.get("https://a.test/x")
            .auth(scheme.clone())
            .send()
            .await?
            .collect()
            .await
    })
    .expect("b's 401 is the answer");

    assert_eq!(got.status(), 401);
    assert_eq!(m.requests().len(), 2, "the hop, and no answer to b");
    assert!(
        scheme.sent.lock().unwrap().is_empty(),
        "nothing was computed for the other origin"
    );
}

/// A body that yields once and cannot be rewound.
struct OneShot(Option<bytes::Bytes>);

impl http_body::Body for OneShot {
    type Data = bytes::Bytes;
    type Error = hclient::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, Self::Error>>> {
        std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
    }
}

// ── What a scheme can see of the body it is signing ─────────────────────

/// A flow that records what [`AuthRequest::body`] showed it, per leg.
#[derive(Clone, Default, Debug)]
struct Watching {
    seen: Arc<Mutex<Vec<String>>>,
}

/// `BodyView` is `Copy` and borrows, so what a test can keep is a
/// description rather than the value.
fn describe(v: hclient_core::body::BodyView<'_>) -> String {
    match v {
        hclient_core::body::BodyView::Empty => "empty".to_owned(),
        hclient_core::body::BodyView::Bytes(b) => {
            format!("bytes:{}", String::from_utf8_lossy(b))
        }
        hclient_core::body::BodyView::Opaque => "opaque".to_owned(),
    }
}

impl Auth for Watching {
    fn start(&self) -> BoxFlow {
        Box::new(self.clone())
    }
}

impl AuthFlow for Watching {
    fn authorize(&mut self, req: &AuthRequest<'_>, _: &mut http::HeaderMap) {
        self.seen.lock().unwrap().push(describe(req.body()));
    }
    fn on_response(&mut self, _: http::StatusCode, _: &http::HeaderMap) -> AuthStep {
        AuthStep::Done
    }
}

fn body_seen_by_a_flow(body: RequestBody) -> Vec<String> {
    let m = MockTransport::new();
    m.push_response(http::Response::new("ok"));
    let w = Watching::default();
    let c = hclient::Client::builder(m).build().expect("client");
    futures_executor::block_on(async {
        c.post("https://a.test/x")
            .auth(w.clone())
            .body(body)
            .send()
            .await
    })
    .expect("a response");
    w.seen.lock().unwrap().clone()
}

/// **A scheme that signs what it sends can see it, which is the whole of
/// why this parameter exists.**
///
/// AWS SigV4 — 31.5M downloads a quarter, more than every auth crate this
/// seam was built for put together — hashes the payload into
/// `x-amz-content-sha256`. Written against `authorize(&method, &uri,
/// &mut headers)` it could not be written at all, and the bytes were
/// three lines away in `Client::run` the whole time.
#[test]
fn a_buffered_body_reaches_the_flow_as_its_bytes() {
    assert_eq!(
        body_seen_by_a_flow(RequestBody::Full("hello".into())),
        vec!["bytes:hello".to_owned()]
    );
}

/// **`Empty` is a value and not an absence**, which is the distinction the
/// three-state answer exists for: SigV4 hashes the empty string here, and
/// a scheme told `Opaque` instead would write `UNSIGNED-PAYLOAD` for a
/// request that has nothing to hide.
#[test]
fn no_body_is_empty_rather_than_opaque() {
    assert_eq!(
        body_seen_by_a_flow(RequestBody::Empty),
        vec!["empty".to_owned()]
    );
}

/// **A streaming body is `Opaque`, and that is honest rather than
/// unhelpful.** Showing it means buffering it, which is a cost every
/// caller would pay for a scheme most do not use — so the scheme is told
/// it cannot have the bytes and decides for itself.
#[test]
fn a_streaming_body_is_opaque() {
    let body = RequestBody::Streaming(Box::new(OneShot(Some(bytes::Bytes::from_static(b"x")))));
    assert_eq!(body_seen_by_a_flow(body), vec!["opaque".to_owned()]);
}

/// **A `Rewindable` body is shown as its bytes, and the factory is still
/// called exactly once** — which is the opposite of what this test was
/// written to assert.
///
/// The reasoning that produced the wrong prediction: showing a
/// `Rewindable`'s bytes means running its factory, this client makes
/// exactly one snapshot per hop, so a flow must be told `Opaque`. Every
/// step is true and the conclusion is not, because the snapshot is taken
/// **before** `authorize` runs — `Client::run` calls `body.rewind()` and
/// then builds the flow — so the bytes are in hand already and the flow
/// is shown those. Nothing is called a second time, which is what the
/// counter here is for rather than the assertion above it.
///
/// So the rule is *the flow sees what the snapshot sees*, and `Opaque`
/// is left to a snapshot that has no bytes either: a `Streaming` body,
/// and a `Rewindable` whose factory hands back another one.
#[test]
fn a_rewindable_body_is_shown_as_bytes_without_a_second_factory_call() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c2 = Arc::clone(&calls);
    let body = RequestBody::rewindable(move || {
        c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        RequestBody::Full("hello".into())
    });
    assert_eq!(body_seen_by_a_flow(body), vec!["bytes:hello".to_owned()]);
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the snapshot the client already takes, and no second call for the flow"
    );
}

/// **The nested case is the one that stays `Opaque`, and it is why the
/// answer is three-state rather than an `Option<&Bytes>`.**
///
/// A `Rewindable` whose factory returns another `Rewindable` is legal —
/// `RequestBody`'s own doc says so, and `MAX_REWIND_DEPTH` exists for it.
/// Its bytes are behind a **second** call, so the flow is told it cannot
/// have them; a scheme that signs payloads writes `UNSIGNED-PAYLOAD`
/// rather than a signature over the wrong thing.
#[test]
fn a_nested_rewindable_body_is_opaque() {
    let body =
        RequestBody::rewindable(|| RequestBody::rewindable(|| RequestBody::Full("hello".into())));
    assert_eq!(body_seen_by_a_flow(body), vec!["opaque".to_owned()]);
}

/// A two-leg flow that records the body it was shown on **each** leg.
#[derive(Clone, Default, Debug)]
struct WatchingTwice {
    seen: Arc<Mutex<Vec<String>>>,
    legs: Arc<std::sync::atomic::AtomicU8>,
}

impl Auth for WatchingTwice {
    fn start(&self) -> BoxFlow {
        Box::new(self.clone())
    }
}

impl AuthFlow for WatchingTwice {
    fn authorize(&mut self, req: &AuthRequest<'_>, _: &mut http::HeaderMap) {
        self.seen.lock().unwrap().push(describe(req.body()));
    }
    fn on_response(&mut self, _: http::StatusCode, _: &http::HeaderMap) -> AuthStep {
        if self.legs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            AuthStep::Again
        } else {
            AuthStep::Done
        }
    }
}

/// **The second leg is shown the body *it* will send, not the one the
/// first leg sent** — and the two differ, which is what makes this a test
/// rather than a control.
///
/// A `Rewindable` whose factory returns another `Rewindable` is `Opaque`
/// on the first leg: its bytes are one call further down than the
/// snapshot went. The replay for the second leg rewinds that snapshot
/// once more, so by then the bytes *are* in hand and the flow is shown
/// them. Handing the second leg the first leg's view would report
/// `Opaque` for a request whose bytes are known — a signing scheme would
/// write `UNSIGNED-PAYLOAD` over a payload it could have signed.
///
/// This is the pair the obvious mutation needs: passing `replay` instead
/// of the leg's own body leaves every other test in this file green.
#[test]
fn each_leg_is_shown_the_body_that_leg_will_send() {
    let m = MockTransport::new();
    m.push_response(challenge("one"));
    m.push_response(http::Response::new("ok"));

    let w = WatchingTwice::default();
    let c = hclient::Client::builder(m.clone()).build().expect("client");
    futures_executor::block_on(async {
        c.post("https://a.test/x")
            .auth(w.clone())
            .body(RequestBody::rewindable(|| {
                RequestBody::rewindable(|| RequestBody::Full("hello".into()))
            }))
            .send()
            .await
    })
    .expect("a response");

    assert_eq!(m.requests().len(), 2, "two legs reached the transport");
    assert_eq!(
        *w.seen.lock().unwrap(),
        vec!["opaque".to_owned(), "bytes:hello".to_owned()],
    );
}

/// A scheme that authorises **pre-emptively**, which is the only shape
/// that can collide with a header setter on the first request.
///
/// `ThreeLeg` above returns early on leg 0 — the NTLM shape — so it
/// cannot reach the collision at all: a test written on it passes whether
/// or not the client guards the header, which is how the first version of
/// the test below scored a survived mutation.
#[derive(Debug, Clone, Default)]
struct Preemptive;

impl Auth for Preemptive {
    fn start(&self) -> BoxFlow {
        Box::new(PreemptiveFlow)
    }
}

struct PreemptiveFlow;

impl AuthFlow for PreemptiveFlow {
    fn authorize(&mut self, _req: &AuthRequest<'_>, headers: &mut http::HeaderMap) {
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Fake preemptive"),
        );
    }

    fn on_response(&mut self, _status: http::StatusCode, _headers: &http::HeaderMap) -> AuthStep {
        AuthStep::Done
    }
}

/// A header setter and a scheme are two mechanisms, and both can be set.
///
/// `RequestBuilder` has four auth setters whose names do not say that two
/// of them (`basic_auth`, `bearer_auth`) write an `Authorization` header
/// immediately while two (`auth`, `digest_auth`) install a scheme that
/// answers a challenge later. So a caller can set both, and nothing said
/// what happens — `auth`'s doc carries the table now, and this is what
/// keeps it true.
///
/// The precedence is the only one that works: a scheme **replaces** the
/// header, because an answer that could not override a pre-set header
/// would leave `basic_auth(..).auth(..)` sending `Basic` for ever at a
/// server asking for something else.
///
/// **It must be a pre-emptive scheme**, and that is what the first
/// version of this test got wrong: written on `ThreeLeg`, whose leg 0
/// sends nothing, it passed with the precedence mutated either way —
/// the `Basic` on leg 0 was the flow declining to write rather than the
/// client preferring the header. A test that cannot fail is the thing
/// this workspace is about, and the way to find one is still to break
/// its subject on purpose.
#[test]
fn a_scheme_replaces_a_preset_header_rather_than_deferring_to_it() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = hclient::Client::builder(m.clone()).build().unwrap();
    let got = futures_executor::block_on(
        c.get("https://a.test/x")
            .basic_auth("u", "p")
            .auth(Preemptive)
            .send(),
    )
    .expect("the exchange completes");
    assert_eq!(got.status(), 200);

    let sent: Vec<Option<String>> = m
        .requests()
        .iter()
        .map(|r| {
            r.headers
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_owned())
        })
        .collect();

    assert_eq!(
        sent,
        vec![Some("Fake preemptive".to_owned())],
        "the scheme's value, not the `Basic` the header setter wrote"
    );
}
