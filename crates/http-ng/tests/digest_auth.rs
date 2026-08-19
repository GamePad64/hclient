//! Digest authentication through `Client`, watched from the server's side.
//!
//! `tests/digest_vectors.rs` checks the arithmetic against RFC 7616's own
//! printed answers. Nothing here re-checks it: what is asserted here is
//! *when* the client answers, *what the server received*, and the two
//! places it refuses to — because a digest implementation that computes
//! perfectly and answers the wrong `401` is the failure that costs a
//! password.
#![cfg(all(
    feature = "digest-auth",
    feature = "test-util",
    not(target_family = "wasm")
))]

use http_ng::Client;
use http_ng::mock::MockTransport;

const CHALLENGE: &str =
    "Digest realm=\"r\", nonce=\"abc123\", algorithm=SHA-256, qop=\"auth\", opaque=\"op\"";

fn challenge_then(second: http::Response<&'static str>) -> MockTransport {
    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(401)
            .header("www-authenticate", CHALLENGE)
            .body("")
            .unwrap(),
    );
    t.push_response(second);
    t
}

fn ok() -> http::Response<&'static str> {
    http::Response::builder().status(200).body("in").unwrap()
}

fn sent(t: &MockTransport, nth: usize) -> Option<String> {
    t.requests()
        .get(nth)?
        .headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// **The whole exchange**: nothing on the first request, an answer on the
/// second, and the pieces of that answer that a server actually
/// recomputes.
///
/// The first assertion is the one that separates digest from basic: a
/// digest response is computed from a nonce the server chooses, so the
/// first request *cannot* carry one, and a client that sent something
/// there would be sending something meaningless.
#[test]
fn the_challenge_is_answered_on_the_second_request_and_not_the_first() {
    let c = Client::builder(challenge_then(ok()))
        .build()
        .expect("build");
    let body = futures_executor::block_on(async {
        c.get("https://a.test/dir/index.html?q=1")
            .digest_auth("Mufasa", "Circle of Life")
            .send()
            .await?
            .collect()
            .await
    })
    .expect("the second request succeeds");
    assert_eq!(body.status(), 200);
    assert_eq!(body.bytes(), &b"in"[..]);

    assert_eq!(
        c.transport().requests().len(),
        2,
        "one challenge, one answer"
    );
    assert_eq!(
        sent(c.transport(), 0),
        None,
        "the nonce had not arrived yet, so there was nothing to compute"
    );
    let auth = sent(c.transport(), 1).expect("the answer");
    assert!(auth.starts_with("Digest "), "{auth}");
    for want in [
        "username=\"Mufasa\"",
        "realm=\"r\"",
        "nonce=\"abc123\"",
        "algorithm=SHA-256",
        "qop=auth",
        "nc=00000001",
        // The server's own state, echoed back untouched.
        "opaque=\"op\"",
        // **The request-target and not the URL.** §3.4.2 hashes what goes
        // on the request line, so a full URL here would give the server a
        // different `A2` and a second `401` nobody could explain.
        "uri=\"/dir/index.html?q=1\"",
    ] {
        assert!(auth.contains(want), "missing {want} in {auth}");
    }
    assert!(
        !auth.contains("Circle of Life"),
        "the password is hashed, never sent: {auth}"
    );
}

/// **Only once per hop.** A server wedged on `401` gets two requests and
/// the caller gets the second `401` — the `425` branch's rule, and for the
/// same reason: an unbounded retry against exactly that server is an
/// infinite one.
#[test]
fn a_server_that_keeps_challenging_gets_two_requests_and_the_401_stands() {
    let t = MockTransport::new();
    for _ in 0..4 {
        t.push_response(
            http::Response::builder()
                .status(401)
                .header("www-authenticate", CHALLENGE)
                .body("")
                .unwrap(),
        );
    }
    let c = Client::builder(t).build().expect("build");
    let got = futures_executor::block_on(async {
        c.get("https://a.test/x")
            .digest_auth("u", "p")
            .send()
            .await?
            .collect()
            .await
    })
    .expect("a 401 is an answer, not a failure");
    assert_eq!(got.status(), 401, "the server's answer reaches the caller");
    assert_eq!(c.transport().requests().len(), 2);
}

/// **No credentials, no answer** — the control that says the branch is
/// gated on the caller's ask rather than on the status code.
#[test]
fn without_credentials_the_401_is_simply_the_answer() {
    let c = Client::builder(challenge_then(ok()))
        .build()
        .expect("build");
    let got = futures_executor::block_on(async {
        c.get("https://a.test/x").send().await?.collect().await
    })
    .expect("a 401 is an answer");
    assert_eq!(got.status(), 401);
    assert_eq!(c.transport().requests().len(), 1, "nothing was retried");
}

/// **A `401` carrying no digest challenge is not answered**, so a `Basic`
/// or `Bearer` challenge does not draw a digest header at a server that
/// never asked for one.
#[test]
fn a_401_without_a_digest_challenge_is_left_alone() {
    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(401)
            .header("www-authenticate", "Basic realm=\"r\"")
            .body("")
            .unwrap(),
    );
    t.push_response(ok());
    let c = Client::builder(t).build().expect("build");
    let got = futures_executor::block_on(async {
        c.get("https://a.test/x")
            .digest_auth("u", "p")
            .send()
            .await?
            .collect()
            .await
    })
    .expect("a 401 is an answer");
    assert_eq!(got.status(), 401);
    assert_eq!(c.transport().requests().len(), 1);
}

/// **The credentials do not cross an origin, and the pair is the
/// assertion.** Same chain, same challenge, one host apart: within the
/// origin the client answers, across it the `401` stands. Either half
/// alone would pass for a client with the wrong rule.
#[test]
fn a_401_from_another_origin_is_not_answered_and_one_from_this_origin_is() {
    for (location, want_status, want_requests) in [
        ("https://a.test/second", 200u16, 3usize),
        ("https://b.test/second", 401, 2),
    ] {
        let t = MockTransport::new();
        t.push_response(
            http::Response::builder()
                .status(302)
                .header("location", location)
                .body("")
                .unwrap(),
        );
        t.push_response(
            http::Response::builder()
                .status(401)
                .header("www-authenticate", CHALLENGE)
                .body("")
                .unwrap(),
        );
        t.push_response(ok());
        let c = Client::builder(t).build().expect("build");
        let got = futures_executor::block_on(async {
            c.get("https://a.test/first")
                .digest_auth("u", "p")
                .send()
                .await?
                .collect()
                .await
        })
        .unwrap_or_else(|e| panic!("{location}: {e:?}"));
        assert_eq!(got.status(), want_status, "{location}");
        assert_eq!(
            c.transport().requests().len(),
            want_requests,
            "{location}: a password derived secret must not reach a server \
             the caller never named"
        );
    }
}

/// **A body that cannot be sent twice leaves the `401` standing**, the
/// `RequestBody::retry_kind()` gate the `425` replay is under. A streaming
/// body has already been consumed by the first attempt; answering the
/// challenge with an empty one would be a different request wearing the
/// same credentials.
#[test]
fn a_body_that_cannot_be_replayed_leaves_the_challenge_unanswered() {
    use http_body_util::BodyExt as _;
    let c = Client::builder(challenge_then(ok()))
        .build()
        .expect("build");
    let body = http_ng_core::RequestBody::Streaming(Box::new(
        http_body_util::Full::new(bytes::Bytes::from_static(b"once")).map_err(|e| match e {}),
    ));
    let got = futures_executor::block_on(async {
        c.post("https://a.test/x")
            .digest_auth("u", "p")
            .body(body)
            .send()
            .await?
            .collect()
            .await
    })
    .expect("a 401 is an answer");
    assert_eq!(got.status(), 401);
    assert_eq!(c.transport().requests().len(), 1);
}

/// A challenge offering only `auth-int` is refused rather than answered
/// wrongly — end to end, not just in the parser, because what a caller
/// sees is one request and their `401`.
#[test]
fn a_server_requiring_auth_int_gets_no_answer() {
    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(401)
            .header(
                "www-authenticate",
                "Digest realm=\"r\", nonce=\"n\", qop=\"auth-int\"",
            )
            .body("")
            .unwrap(),
    );
    t.push_response(ok());
    let c = Client::builder(t).build().expect("build");
    let got = futures_executor::block_on(async {
        c.get("https://a.test/x")
            .digest_auth("u", "p")
            .send()
            .await?
            .collect()
            .await
    })
    .expect("a 401 is an answer");
    assert_eq!(got.status(), 401);
    assert_eq!(c.transport().requests().len(), 1);
}

/// **The `Authorization` value is marked sensitive**, so a `Debug` of the
/// request does not print a secret derived from the password. Asserted
/// because a `Debug` is the only place it shows, and an observable
/// property with no observer is a gap rather than a control — the same
/// note `basic_auth`'s marking carries.
#[test]
fn the_answer_is_marked_sensitive() {
    let c = Client::builder(challenge_then(ok()))
        .build()
        .expect("build");
    futures_executor::block_on(async {
        c.get("https://a.test/x")
            .digest_auth("u", "p")
            .send()
            .await?
            .collect()
            .await
    })
    .expect("succeeds");
    let reqs = c.transport().requests();
    let v = reqs[1]
        .headers
        .get(http::header::AUTHORIZATION)
        .expect("the answer");
    assert!(v.is_sensitive(), "{v:?}");
}
