//! Tests for the redirect stage at the `Client` level:
//! `hclient-proto::redirect::decide` is already tested as a pure function
//! — this checks that the `client.rs`/`stages/redirect.rs` plumbing
//! doesn't distort its decision while shuffling data between hops.

// `hclient::mock` lives behind the `test-util` feature (see `mock.rs`);
// without this line `cargo test -p hclient` with no flags fails with
// E0432 instead of compiling down to nothing.
#![cfg(feature = "test-util")]

use hclient::mock::MockTransport;
use hclient::redirect::RedirectPolicy;
use hclient::{Client, RequestBody};

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
    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
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

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
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
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1
    );
}

#[test]
fn enforces_the_hop_limit() {
    let m = MockTransport::new();
    for _ in 0..5 {
        m.push_response(redirect_to("https://a/loop"));
    }

    let c = Client::builder(m)
        .redirect(RedirectPolicy::Limited(2))
        .build()
        .unwrap();
    let req = http::Request::builder()
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    let err = futures_executor::block_on(c.execute(req)).unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
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
        .redirect(RedirectPolicy::Limited(0))
        .build()
        .unwrap();
    let req = http::Request::builder()
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    let err = futures_executor::block_on(c.execute(req)).unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
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

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
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
    use hclient::Timeouts;
    let m = MockTransport::new(); // Capabilities::default() — timeouts unsupported
    let err = Client::builder(m)
        .timeouts(Timeouts {
            resolve: None,
            connect: Some(std::time::Duration::from_secs(1)),
            ..Default::default()
        })
        .build()
        .unwrap_err();
    assert_eq!(err.what, "connect_timeout");
}

/// Only `Location` is read from the response when building the next hop.
/// Nothing in `next_hop`/`decide()` today touches any other response
/// header, and nothing else checks this behaviourally — a
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

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert!(
        !seen[1].headers.contains_key("set-cookie"),
        "response header set-cookie must not leak into the next request"
    );
    assert!(
        !seen[1].headers.contains_key("x-injected"),
        "response header x-injected must not leak into the next request"
    );
}

/// `Timeouts` travels to the transport via the request's
/// `http::Extensions` — the entire mechanism it lives there for relies on
/// `extensions` surviving to every hop, not just the first one.
#[test]
fn per_request_extensions_survive_a_hop_unchanged() {
    use hclient_core::Timeouts;
    use std::time::Duration;

    // Capabilities aren't decorative anymore now that `Client::execute`
    // checks the merged timeouts: a mock
    // with `Capabilities::default()` now honestly rejects the `connect`
    // timeout, and this test is about carrying `extensions` across hops,
    // not about the gate.
    let mut caps = hclient::caps::Capabilities::default();
    caps.timeouts = hclient::caps::TimeoutSupport {
        resolve: false,
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
        resolve: None,
        connect: Some(Duration::from_secs(3)),
        ..Default::default()
    });
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
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
        type Error = hclient_core::Error;
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
    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(
        seen.len(),
        1,
        "no second request with an empty body is sent"
    );
}

// =====================================================================
// The per-request policy, the `Internal` rejection through a real
// `Client`, and the default `Option<RedirectPolicy>` carries.
// =====================================================================

/// The default limit is `RedirectPolicy::default()`'s ten, and it is ten
/// exactly — not "some positive number".
///
/// Both halves are needed. Ten hops followed alone would still pass if
/// `unwrap_or_default()` silently became a much larger limit; the eleventh
/// being rejected alone would still pass if it were much smaller. Together
/// they pin the boundary, which is what `Config.redirect` becoming an
/// `Option` had to leave untouched.
///
/// Against a `Transparent` backend specifically: that is what a real
/// non-browser backend reports (`wasi:http`), and it is the variant under
/// which the new check must NOT fire.
fn transparent_mock() -> MockTransport {
    let mut caps = hclient::caps::Capabilities::default();
    caps.redirects = hclient::caps::RedirectSupport::Transparent;
    MockTransport::new().with_capabilities(caps)
}

/// The body type is not `MockBody`: `Client::execute` wraps whatever the
/// transport returned, twice — v0.2 W4 so the whole-operation bound
/// survives past the response head, v0.2 W5 so a `Content-Encoding` can be
/// reversed. Both wrappers are inert here: no client in this file sets a
/// `total_timeout`, and the mock sends no `Content-Encoding`.
///
/// Written through the `hclient::body::ClientBody` alias rather than spelled
/// out, so that the ORDER of the two wrappers lives in one place (see that
/// alias's doc comment) instead of being restated by every test that names
/// the type.
type ClientBody = hclient::body::ClientBody;

fn get_from(c: &Client) -> Result<http::Response<ClientBody>, hclient::Error> {
    let req = http::Request::builder()
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    futures_executor::block_on(c.execute(req))
}

#[test]
fn an_unconfigured_client_follows_exactly_ten_hops() {
    let m = transparent_mock();
    for _ in 0..10 {
        m.push_response(redirect_to("https://a/loop"));
    }
    m.push_response(http::Response::builder().status(200).body("done").unwrap());

    let c = Client::builder(m).build().unwrap();
    let resp = get_from(&c).expect("ten hops are within the default limit");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        11,
        "the original request plus ten hops"
    );
}

#[test]
fn an_unconfigured_client_rejects_the_eleventh_hop() {
    let m = transparent_mock();
    for _ in 0..11 {
        m.push_response(redirect_to("https://a/loop"));
    }

    let c = Client::builder(m).build().unwrap();
    let err = get_from(&c).unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert!(
        err.to_string().contains("10"),
        "the message must name the limit that was hit: {err}"
    );
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        11,
        "the original request plus ten hops, and not a twelfth"
    );
}

/// A per-request limit overrides the client's, through the real
/// `RequestBuilder` — the shape `act`'s `http-client` component needs
/// (`follow_redirects ? 10 : 0`, computed per call) and which used to
/// require a whole new `Client`.
#[test]
fn a_per_request_redirect_policy_overrides_the_clients() {
    let m = transparent_mock();
    for _ in 0..5 {
        m.push_response(redirect_to("https://a/loop"));
    }

    let c = Client::builder(m)
        .redirect(RedirectPolicy::Limited(4))
        .build()
        .unwrap();
    let err = futures_executor::block_on(
        c.get("https://a/x")
            .redirect(RedirectPolicy::Limited(1))
            .send(),
    )
    .unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert!(
        err.to_string().contains('1') && !err.to_string().contains('4'),
        "the request's limit is the one enforced and the one reported: {err}"
    );
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        2,
        "the original request plus the one hop the REQUEST's limit allows, not the client's four"
    );
}

/// `limit: 0` per request — "do not follow redirects" — is honoured by a
/// `Transparent` backend even though the client asked to follow ten.
#[test]
fn a_per_request_limit_of_zero_stops_a_client_configured_to_follow() {
    let m = transparent_mock();
    m.push_response(redirect_to("https://a/loop"));

    let c = Client::builder(m)
        .redirect(RedirectPolicy::Limited(10))
        .build()
        .unwrap();
    let err = futures_executor::block_on(
        c.get("https://a/x")
            .redirect(RedirectPolicy::Limited(0))
            .send(),
    )
    .unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1,
        "only the original request, not a single hop"
    );
}

/// Nothing on the request leaves the client's policy in force — the
/// `.or(client)` half of the merge, which a function ignoring its client
/// argument would fail.
#[test]
fn without_a_per_request_policy_the_clients_limit_still_applies() {
    let m = transparent_mock();
    for _ in 0..5 {
        m.push_response(redirect_to("https://a/loop"));
    }

    let c = Client::builder(m)
        .redirect(RedirectPolicy::Limited(2))
        .build()
        .unwrap();
    let err = futures_executor::block_on(c.get("https://a/x").send()).unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        3,
        "the original request plus the client's two hops"
    );
}

// ── the `Internal` rejection, through a real `Client` ────────────────
//
// `config.rs`'s unit tests check `check_supported`/`check_redirect_supported`
// directly. These two check that the rejection actually reaches a caller
// of the public API, at both of the two points it has to: `build()` for a
// client-level policy, and `send()` for a per-request one. A mock standing
// in for the browser, so the property is checked on every `cargo test` run
// and not only under `wasm-pack`.

fn internal_mock() -> MockTransport {
    let mut caps = hclient::caps::Capabilities::default();
    caps.redirects = hclient::caps::RedirectSupport::Internal;
    MockTransport::new().with_capabilities(caps)
}

#[test]
fn a_client_level_policy_against_an_internal_backend_fails_at_build() {
    let err = Client::builder(internal_mock())
        .redirect(RedirectPolicy::Limited(0))
        .build()
        .unwrap_err();
    assert_eq!(err.what, "redirect_policy");
    assert!(err.backend.contains("MockTransport"), "{}", err.backend);
}

#[test]
fn a_per_request_policy_against_an_internal_backend_fails_at_send() {
    let m = internal_mock();
    m.push_response(http::Response::builder().status(200).body("done").unwrap());

    // The client itself configures nothing, so `build()` has nothing to
    // object to — this is exactly the path that would be silently
    // unchecked if the check read `Config` rather than the merged value.
    let c = Client::builder(m).build().unwrap();
    let err = futures_executor::block_on(
        c.get("https://a/x")
            .redirect(RedirectPolicy::Limited(0))
            .send(),
    )
    .unwrap_err();

    assert_eq!(*err.kind(), hclient::ErrorKind::Unsupported, "{err}");
    assert!(err.to_string().contains("redirect_policy"), "{err}");
    assert!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .is_empty(),
        "the request must be rejected before it is sent, not after"
    );
}

/// The other side of the same coin, and the one most likely to be lost in a
/// refactor: a browser-shaped backend must stay usable for every caller who
/// never mentioned redirects. `Client::new()`'s `.expect(...)` on
/// `wasm32-unknown-unknown` rests on exactly this.
#[test]
fn an_unconfigured_client_against_an_internal_backend_works_normally() {
    let m = internal_mock();
    m.push_response(http::Response::builder().status(200).body("done").unwrap());

    let c = Client::builder(m).build().expect("nothing was configured");
    let resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();
    assert_eq!(resp.status(), 200);
}

/// `Client::query` end-to-end: the method reaches the transport, and it
/// survives a 302 together with its body.
///
/// The body assertion uses `body_size_hint`, which is what `MockTransport`
/// records — a hint of `Some(0)` on the second hop would mean the body was
/// dropped even though the method was preserved, and that is the halfway
/// failure a method-only assertion would miss.
#[test]
fn query_reaches_the_transport_and_survives_a_redirect_with_its_body() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://a/search2"));
    m.push_response(http::Response::builder().status(200).body("hits").unwrap());

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(
        c.query("https://a/search")
            .body(RequestBody::Full(bytes::Bytes::from_static(
                b"filter=colour:blue",
            )))
            .send(),
    )
    .unwrap();
    assert_eq!(resp.status(), 200);

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(seen.len(), 2, "the redirect must have been followed");
    for (i, r) in seen.iter().enumerate() {
        assert_eq!(
            r.method,
            http::Method::QUERY,
            "hop {i} must still be a QUERY — a rewrite to GET would silently \
             turn a filtered search into a fetch of the whole collection"
        );
        assert_eq!(
            r.body_size_hint,
            Some(18),
            "hop {i} must carry the query body; the body IS the request"
        );
    }
}

/// **`Response::url()` is the last hop, not the first.** Pinned here as
/// well as in `error_for_status.rs`, because this is the file about
/// redirects and the property is a redirect property: it was the requested
/// URL, undocumented and untested, until an error that carries it made the
/// difference visible.
#[test]
fn the_response_url_is_the_hop_that_answered() {
    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a.test/second")
            .body("")
            .unwrap(),
    );
    t.push_response(http::Response::builder().status(200).body("here").unwrap());
    let c = Client::builder(t).build().expect("build");

    let resp = futures_executor::block_on(c.get("https://a.test/first").send()).expect("chain");
    assert_eq!(resp.url(), "https://a.test/second");
    let body = futures_executor::block_on(resp.collect()).expect("body");
    assert_eq!(
        body.url(),
        "https://a.test/second",
        "and `Collected` carries the same one through"
    );
}

/// The control, and it is what says the value is not simply *the second
/// request*: with no redirect, the URL is the one that was asked for.
#[test]
fn without_a_redirect_the_url_is_the_one_that_was_asked_for() {
    let t = MockTransport::new();
    t.push_response(http::Response::builder().status(200).body("here").unwrap());
    let c = Client::builder(t).build().expect("build");
    let resp = futures_executor::block_on(c.get("https://a.test/only").send()).expect("one hop");
    assert_eq!(resp.url(), "https://a.test/only");
}
