//! A scheme this crate does not implement, written from outside it.
//!
//! Digest's own tests pin digest. What only this file can say is that the
//! seam carries a scheme with **more than two legs** — which is the shape
//! NTLM and Negotiate have, and the reason the seam exists rather than a
//! third built-in scheme.

use std::sync::{Arc, Mutex};

use hclient::auth::{Auth, AuthFlow, AuthStep, BoxedFlow};
use hclient_core::RequestBody;
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
    fn start(&self) -> BoxedFlow {
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
    fn authorize(&mut self, _m: &http::Method, _u: &http::Uri, headers: &mut http::HeaderMap) {
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
    fn start(&self) -> BoxedFlow {
        Box::new(NeverDone)
    }
}

impl AuthFlow for NeverDone {
    fn authorize(&mut self, _: &http::Method, _: &http::Uri, _: &mut http::HeaderMap) {}
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
