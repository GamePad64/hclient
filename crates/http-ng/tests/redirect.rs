//! Tests for the redirect stage at the `Client` level:
//! `http-ng-proto::redirect::decide` is already tested as a pure function
//! (Task 5) — this checks that the `client.rs`/`stages/redirect.rs` plumbing
//! doesn't distort its decision while shuffling data between hops.

// `http_ng::mock` lives behind the `test-util` feature (see `mock.rs`);
// without this line `cargo test -p http-ng` with no flags used to fail
// with E0432 instead of compiling down to nothing — the same fix already
// made for `shape.rs` in Task 12. Task 13 fix round 2, Residual 3.
#![cfg(feature = "test-util")]

use http_ng::mock::MockTransport;
use http_ng::{Client, RedirectPolicy, RequestBody};

fn redirect_to(loc: &'static str) -> http::Response<&'static str> {
    http::Response::builder()
        .status(302)
        .header("location", loc)
        .body("")
        .unwrap()
}

#[test]
fn follows_a_redirect_and_records_both_hops() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://a/second"));
    m.push_response(http::Response::builder().status(200).body("done").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .uri("https://a/first")
        .body(RequestBody::Empty)
        .unwrap();
    let resp = futures_executor::block_on(c.execute(req)).unwrap();

    assert_eq!(resp.status(), 200);
    let seen = c.transport().requests();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[1].uri,
        "https://a/second".parse::<http::Uri>().unwrap()
    );
}

#[test]
fn strips_authorization_when_the_host_changes() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://evil/steal"));
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .uri("https://a/first")
        .header("authorization", "Bearer secret")
        .header("x-safe", "keep")
        .body(RequestBody::Empty)
        .unwrap();
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c.transport().requests();
    assert!(
        seen[0].headers.contains_key("authorization"),
        "the first hop keeps it"
    );
    assert!(
        !seen[1].headers.contains_key("authorization"),
        "the second hop strips it"
    );
    assert!(
        seen[1].headers.contains_key("x-safe"),
        "non-secret headers remain"
    );
}

#[test]
fn does_not_follow_304() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(304)
            .header("location", "https://a/nope")
            .body("")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    let resp = futures_executor::block_on(c.execute(req)).unwrap();

    assert_eq!(resp.status(), 304);
    assert_eq!(c.transport().requests().len(), 1);
}

#[test]
fn enforces_the_hop_limit() {
    let m = MockTransport::new();
    for _ in 0..5 {
        m.push_response(redirect_to("https://a/loop"));
    }

    let c = Client::builder(m)
        .redirect(RedirectPolicy { limit: 2 })
        .build()
        .unwrap();
    let req = http::Request::builder()
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    let err = futures_executor::block_on(c.execute(req)).unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert_eq!(
        c.transport().requests().len(),
        3,
        "the original request plus two hops"
    );
}

/// `hops >= policy.limit` — off-by-one bait: `limit: 0` must reject the very
/// first redirect without ever incrementing `hops`, sending only the
/// original request.
#[test]
fn redirect_limit_of_zero_sends_only_the_original_request() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://a/loop"));

    let c = Client::builder(m)
        .redirect(RedirectPolicy { limit: 0 })
        .build()
        .unwrap();
    let req = http::Request::builder()
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    let err = futures_executor::block_on(c.execute(req)).unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert_eq!(
        c.transport().requests().len(),
        1,
        "only the original request, not a single hop"
    );
}

#[test]
fn post_becomes_get_and_drops_body_on_302() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://a/second"));
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://a/first")
        .body(RequestBody::Full(bytes::Bytes::from_static(b"payload")))
        .unwrap();
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c.transport().requests();
    assert_eq!(seen[1].method, http::Method::GET);
    // Found by review: the test's name promises the body is dropped, but
    // the original version only checked the method. Check the body's
    // shape on both hops — it was present on the first one (7 bytes of
    // "payload"), and must become empty on the second, rather than riding
    // along to a cross-origin destination.
    assert_eq!(seen[1].body_size_hint, Some(0), "the body must be dropped");
    assert_eq!(
        seen[0].body_size_hint,
        Some(7),
        "it was there on the first hop"
    );
}

#[test]
fn build_rejects_a_timeout_the_backend_cannot_honour() {
    use http_ng::Timeouts;
    let m = MockTransport::new(); // Capabilities::none() — timeouts unsupported
    let err = Client::builder(m)
        .timeouts(Timeouts {
            connect: Some(std::time::Duration::from_secs(1)),
            ..Default::default()
        })
        .build()
        .unwrap_err();
    assert_eq!(err.what, "connect_timeout");
}

/// Only `Location` is read from the response when building the next hop.
/// Nothing in `next_hop`/`decide()` today touches any other response
/// header, but before fix round 1 no test checked this behaviorally — a
/// mutation merging `resp.headers()` into the next request's headers
/// (`Set-Cookie` from the server, or anything else leaking into the chain)
/// would have slipped past all six tests from the brief unnoticed.
#[test]
fn response_headers_do_not_leak_into_the_next_hop() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a/second")
            .header("set-cookie", "sid=abc123")
            .header("x-injected", "should-not-cross")
            .body("")
            .unwrap(),
    );
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .uri("https://a/first")
        .body(RequestBody::Empty)
        .unwrap();
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c.transport().requests();
    assert!(
        !seen[1].headers.contains_key("set-cookie"),
        "response header set-cookie must not leak into the next request"
    );
    assert!(
        !seen[1].headers.contains_key("x-injected"),
        "response header x-injected must not leak into the next request"
    );
}

/// `Timeouts` (Task 10) travels to the transport via the request's
/// `http::Extensions` — the entire mechanism it lives there for relies on
/// `extensions` surviving to every hop, not just the first one.
#[test]
fn per_request_extensions_survive_a_hop_unchanged() {
    use http_ng_core::Timeouts;
    use std::time::Duration;

    // Capabilities aren't decorative anymore now that `Client::execute`
    // checks the merged timeouts (M3 of the branch's final review): a mock
    // with `Capabilities::none()` now honestly rejects the `connect`
    // timeout, and this test is about carrying `extensions` across hops,
    // not about the gate.
    let mut caps = http_ng::Capabilities::none();
    caps.timeouts = http_ng::TimeoutSupport {
        connect: true,
        first_byte: true,
        between_bytes: true,
    };
    let m = MockTransport::new().with_capabilities(caps);
    m.push_response(redirect_to("https://a/second"));
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let mut req = http::Request::builder()
        .uri("https://a/first")
        .body(RequestBody::Empty)
        .unwrap();
    req.extensions_mut().insert(Timeouts {
        connect: Some(Duration::from_secs(3)),
        ..Default::default()
    });
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c.transport().requests();
    let t0 = seen[0]
        .extensions
        .get::<Timeouts>()
        .expect("hop 0 carries the Timeouts inserted on the original request");
    let t1 = seen[1]
        .extensions
        .get::<Timeouts>()
        .expect("hop 1 must carry the same Timeouts, not drop it");
    assert_eq!(t0.connect, Some(Duration::from_secs(3)));
    assert_eq!(
        t1.connect,
        Some(Duration::from_secs(3)),
        "unchanged across the hop"
    );
}

/// A `Streaming` body isn't replayable: `RequestBody::rewind()` returns
/// `None`. The honest behavior is to return the 3xx as-is and not send a
/// second request with an empty body where the server expects the
/// original payload.
///
/// Status is 307, not 302: `decide()` only downgrades the method (and
/// deliberately drops the body along with it, `drop_body`) for POST on
/// 301/302/303. On 307/308 the method and body must survive unchanged —
/// this is the path where the body's non-replayability actually matters
/// in full, rather than being masked by the downgrade.
#[test]
fn unreplayable_streaming_body_stops_at_the_3xx_instead_of_a_second_empty_request() {
    struct OneShot(Option<bytes::Bytes>);
    impl http_body::Body for OneShot {
        type Data = bytes::Bytes;
        type Error = http_ng_core::Error;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, Self::Error>>> {
            std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
        }
    }

    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(307)
            .header("location", "https://a/second")
            .body("")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://a/first")
        .body(RequestBody::Streaming(Box::new(OneShot(Some(
            bytes::Bytes::from_static(b"payload"),
        )))))
        .unwrap();
    let resp = futures_executor::block_on(c.execute(req)).unwrap();

    // No second request was ever sent: the mock only saw the original hop.
    assert_eq!(resp.status(), 307, "the 3xx is returned as-is");
    let seen = c.transport().requests();
    assert_eq!(
        seen.len(),
        1,
        "no second request with an empty body is sent"
    );
}
