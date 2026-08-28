//! The verb methods take `impl AsRef<str>`, and the case that matters is
//! `url::Url`.
//!
//! ACT — the first consumer to port onto this crate — reported that call
//! sites holding a `Url` had to write `url.as_str()` or `&format!(..)`, and
//! asked for something `IntoUrl`-shaped. The answer is narrower than a
//! trait: `url::Url` implements `AsRef<str>`, so widening the parameter
//! reaches it **with no dependency on `url`** — the crate `hclient-proto`
//! removed at a measured cost of 1.9 MB of ICU tables.
//!
//! So this file's whole point is that `url` appears in `[dev-dependencies]`
//! and nowhere else. A test that used `&str` alone would pass for the
//! signature this change replaced.

use hclient_mock::MockTransport;

fn client() -> (hclient::Client, MockTransport) {
    let mock = MockTransport::new();
    mock.push_response(http::Response::new("ok"));
    mock.push_response(http::Response::new("ok"));
    mock.push_response(http::Response::new("ok"));
    mock.push_response(http::Response::new("ok"));
    let client = hclient::Client::builder(mock.clone()).build().unwrap();
    (client, mock)
}

#[test]
fn a_url_crate_url_is_accepted_where_a_str_is() {
    futures_executor::block_on(run());
}

async fn run() {
    let (client, mock) = client();
    let parsed = url::Url::parse("https://example.com/a?b=1").unwrap();

    // The four shapes a caller actually holds. `&str` is the one that
    // already worked; the other three are what the widening bought, and
    // `Url` is the one that needed a crate this library does not depend on.
    // The borrow is deliberate: `&Url` is one of the four shapes, and
    // clippy's `needless_borrow` would rewrite it to the shape one line
    // down, collapsing two cases into one.
    #[allow(
        clippy::needless_borrow,
        reason = "the borrowed shape is the case under test"
    )]
    client.get(&parsed).send().await.unwrap();
    client.get(parsed.clone()).send().await.unwrap();
    client.get(parsed.as_str()).send().await.unwrap();
    client
        .get("https://example.com/a?b=1")
        .send()
        .await
        .unwrap();

    let seen: Vec<String> = mock.requests().iter().map(|r| r.uri.to_string()).collect();
    assert_eq!(seen.len(), 4, "every shape reached the transport");
    assert!(
        seen.iter().all(|u| u == "https://example.com/a?b=1"),
        "and they all named the same URL: {seen:?}"
    );
}
