//! `error_for_status`, on both the streaming response and the collected
//! one.
#![cfg(all(feature = "test-util", not(target_family = "wasm")))]

use http_ng::mock::MockTransport;
use http_ng::{Client, UnexpectedStatus};

fn answering(status: u16) -> Client<MockTransport> {
    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(status)
            .header("location", "https://a.test/elsewhere")
            .body("body")
            .unwrap(),
    );
    Client::builder(t)
        // So a `3xx` reaches the caller rather than being followed, which
        // is the state the `3xx` assertion below is about.
        .redirect(http_ng::RedirectPolicy::None)
        .build()
        .expect("build")
}

/// **The boundary, both sides of it, on one axis.** `399` is `Ok` and
/// `400` is not — asserted as a sweep rather than two examples, because a
/// method that got the comparison backwards or off by one would pass any
/// pair of examples chosen to be far apart.
///
/// `3xx` being `Ok` is a decision rather than an omission: reaching one
/// means the redirect policy already decided to hand it back, and
/// `RedirectPolicy::None`'s own doc says a `3xx` is the caller's answer
/// rather than a failure to reach one. Erroring here would overrule that
/// from two layers up.
#[test]
fn every_status_below_400_is_ok_and_every_status_at_or_above_it_is_not() {
    for status in [
        100u16, 200, 204, 301, 302, 399, 400, 404, 418, 499, 500, 503,
    ] {
        let c = answering(status);
        let got = futures_executor::block_on(async {
            c.get("https://a.test/x").send().await?.error_for_status()
        });
        assert_eq!(
            got.is_ok(),
            status < 400,
            "{status} came back {}",
            if got.is_ok() { "Ok" } else { "Err" }
        );
    }
}

/// **The error names the status and the URL that produced it**, which is
/// the pair a caller acts on — and the URL is the hop that failed rather
/// than the one they typed, which is what makes carrying it worth
/// anything.
#[test]
fn the_error_carries_the_status_and_the_url_of_the_hop_that_failed() {
    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a.test/second")
            .body("")
            .unwrap(),
    );
    t.push_response(http::Response::builder().status(503).body("").unwrap());
    let c = Client::builder(t).build().expect("build");

    let err = futures_executor::block_on(async {
        c.get("https://a.test/first")
            .send()
            .await?
            .error_for_status()
    })
    .expect_err("the second hop failed");
    assert_eq!(*err.kind(), http_ng_core::ErrorKind::Status, "{err:?}");
    let unexpected = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<UnexpectedStatus>())
        .unwrap_or_else(|| panic!("the typed status: {err:?}"));
    assert_eq!(unexpected.status, 503);
    assert_eq!(
        unexpected.url, "https://a.test/second",
        "the hop that answered, not the one the caller asked for"
    );
}

/// **`Collected` has it too**, because the two are used at different
/// moments: before the body for a caller who will not read it, and after
/// for one who wants the server's error text and only then decides.
#[test]
fn a_collected_response_can_be_checked_after_its_body_was_read() {
    let c = answering(404);
    let body = futures_executor::block_on(async {
        c.get("https://a.test/x").send().await?.collect().await
    })
    .expect("the body arrives whatever the status");
    assert_eq!(
        body.text().expect("utf-8"),
        "body",
        "the server's own message is readable first, which is the point of \
         having it here as well"
    );
    assert!(body.error_for_status().is_err());
}

/// A `2xx` passes the response through untouched — the control that says
/// this is a test and not a wrapper.
#[test]
fn a_success_comes_back_whole() {
    let c = answering(200);
    let body = futures_executor::block_on(async {
        c.get("https://a.test/x")
            .send()
            .await?
            .error_for_status()?
            .collect()
            .await
    })
    .expect("200");
    assert_eq!(body.status(), 200);
    assert_eq!(body.bytes(), &b"body"[..]);
    assert_eq!(body.url(), "https://a.test/x");
}
