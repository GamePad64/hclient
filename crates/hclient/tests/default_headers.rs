//! `ClientBuilder::{user_agent, default_header, default_headers}`, watched
//! on the wire.
//!
//! Three claims, and each fails differently: that a default reaches the
//! server at all, that it reaches **every redirect hop**, and that a
//! header the caller wrote on the request wins over the client's default.
//! The fourth is the refusal — a default a backend forbids is an error at
//! `build()`, which is a fact about a type that never sends anything.
#![cfg(all(feature = "test-util", not(target_family = "wasm")))]

use hclient::Client;
use hclient::mock::MockTransport;

fn sent(t: &MockTransport, nth: usize, name: &str) -> Option<String> {
    t.requests()
        .get(nth)?
        .headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// **A default reaches the wire, and the caller's own header wins over
/// it.** Asserted together, because a client that applied defaults
/// blindly would pass the first and a client that applied none would pass
/// the second.
#[test]
fn a_default_reaches_the_wire_and_the_requests_own_header_wins() {
    let t = MockTransport::new();
    t.push_response(http::Response::builder().status(200).body("").unwrap());
    t.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(t)
        .user_agent(http::HeaderValue::from_static("probe/1.0"))
        .default_header(
            http::HeaderName::from_static("x-tenant"),
            http::HeaderValue::from_static("acme"),
        )
        .build()
        .expect("build");

    futures_executor::block_on(c.get("https://a/one").send()).expect("one");
    futures_executor::block_on(
        c.get("https://a/two")
            .header("user-agent", "mine/2.0")
            .send(),
    )
    .expect("two");

    assert_eq!(
        sent(
            c.transport_as::<MockTransport>().expect("the mock"),
            0,
            "user-agent"
        )
        .as_deref(),
        Some("probe/1.0")
    );
    assert_eq!(
        sent(
            c.transport_as::<MockTransport>().expect("the mock"),
            0,
            "x-tenant"
        )
        .as_deref(),
        Some("acme")
    );
    assert_eq!(
        sent(
            c.transport_as::<MockTransport>().expect("the mock"),
            1,
            "user-agent"
        )
        .as_deref(),
        Some("mine/2.0"),
        "a header the caller wrote on the request is a decision about that request"
    );
    assert_eq!(
        sent(
            c.transport_as::<MockTransport>().expect("the mock"),
            1,
            "x-tenant"
        )
        .as_deref(),
        Some("acme"),
        "and the other default is untouched by that"
    );
}

/// **Every hop, not just the first.** A `User-Agent` that vanished after a
/// redirect would be a stranger thing than one that was never there — and
/// it is the shape a default applied once, outside the loop, would have.
#[test]
fn a_default_travels_to_every_redirect_hop() {
    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a/second")
            .body("")
            .unwrap(),
    );
    t.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let c = Client::builder(t)
        .user_agent(http::HeaderValue::from_static("probe/1.0"))
        .build()
        .expect("build");

    futures_executor::block_on(c.get("https://a/first").send()).expect("the chain completes");
    let reqs = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(reqs.len(), 2, "one redirect, two requests");
    assert_eq!(
        sent(
            c.transport_as::<MockTransport>().expect("the mock"),
            0,
            "user-agent"
        )
        .as_deref(),
        Some("probe/1.0")
    );
    assert_eq!(
        sent(
            c.transport_as::<MockTransport>().expect("the mock"),
            1,
            "user-agent"
        )
        .as_deref(),
        Some("probe/1.0"),
        "the second hop is a request this client made too"
    );
}

/// **A default the transport forbids is refused at `build()`**, not
/// dropped on the way out.
///
/// `hclient-fetch` forbids `User-Agent` among others because the browser
/// writes them, and it cannot be built for this target — so the capability
/// is fabricated on the mock, exactly as the cookie-jar refusal one file
/// over does. The check reads a list; where the list came from is not
/// something it can tell.
///
/// The asymmetry with `RequestBuilder::header` is deliberate and is about
/// **when the caller finds out**: a per-request header sits next to the
/// request that carries it, where a default is written once and applies to
/// traffic its author may never look at again.
#[test]
fn a_forbidden_default_is_refused_at_build() {
    let mut caps = hclient::caps::Capabilities::none();
    caps.forbidden_request_headers = &[http::header::USER_AGENT];
    let err = Client::builder(MockTransport::new().with_capabilities(caps))
        .user_agent(http::HeaderValue::from_static("probe/1.0"))
        .build()
        .expect_err("a header the transport owns cannot be a client default");
    assert_eq!(
        err.what, "default_headers",
        "the refusal names the setting: {err}"
    );

    // The control, and it is not ceremony: the same builder against a
    // transport that forbids nothing builds, so the check is reading the
    // list rather than refusing every default.
    assert!(
        Client::builder(MockTransport::new())
            .user_agent(http::HeaderValue::from_static("probe/1.0"))
            .build()
            .is_ok()
    );
}
