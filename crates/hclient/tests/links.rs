//! `Response::links` / `Collected::links` at the `Client` level.
//!
//! The grammar is `hclient-proto`'s and is tested there — commas inside
//! quoted parameters, a repeated relation, the case rules. What is checked
//! here is the one thing this layer adds: the base a relative target is
//! resolved against, and that both types answer it.

// `hclient::mock` lives behind the `test-util` feature (see `mock.rs`);
// without this line `cargo test -p hclient` with no flags fails with
// E0432 instead of compiling down to nothing.
#![cfg(feature = "test-util")]

use hclient::Client;
use hclient::mock::MockTransport;

fn page_with(link: &'static str) -> http::Response<&'static str> {
    http::Response::builder()
        .status(200)
        .header("link", link)
        .body("{}")
        .unwrap()
}

#[test]
fn the_paginated_case_is_one_call_and_the_target_is_ready_to_request() {
    let m = MockTransport::new();
    m.push_response(page_with(r#"</items?page=2>; rel="next""#));
    let c = Client::builder(m).build().unwrap();

    let resp =
        futures_executor::block_on(c.get("https://api.example.com/items?page=1").send()).unwrap();
    assert_eq!(
        resp.links()["next"].target(),
        "https://api.example.com/items?page=2",
        "a relative target is resolved against the URL that answered, or the \
         caller's next request would fail a layer away from anything that \
         could explain it"
    );
}

/// The base is the **last hop**, RFC 3986 §5.1.3 — and the two URLs differ
/// exactly when a redirect was followed, which is the case this pins.
/// `Response::url` already answers *where did this come from* rather than
/// *where did you send this*; a relative `Link` on the answer is relative
/// to the answer.
#[test]
fn a_relative_target_is_resolved_against_the_hop_that_answered_not_the_one_asked_for() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://second.example/v2/items")
            .body("")
            .unwrap(),
    );
    m.push_response(page_with(r#"<next-page>; rel="next""#));
    let c = Client::builder(m).build().unwrap();

    let resp = futures_executor::block_on(c.get("https://first.example/items").send()).unwrap();
    assert_eq!(
        resp.links()["next"].target(),
        "https://second.example/v2/next-page",
        "the first host never enters into it"
    );
}

/// Both types carry it for `error_for_status`'s reason: they are held at
/// different moments. A caller who wrote `.collect().await?` has finished
/// with the response and must not have to go back for a header.
#[test]
fn collected_answers_the_same_links_as_the_response_it_came_from() {
    let m = MockTransport::new();
    m.push_response(page_with(
        r#"</items?page=2>; rel="next", </items?page=9>; rel="last""#,
    ));
    let c = Client::builder(m).build().unwrap();

    let (from_response, from_collected) = futures_executor::block_on(async {
        let resp = c.get("https://api.example.com/items").send().await.unwrap();
        let before = resp.links();
        let collected = resp.collect().await.unwrap();
        (before, collected.links())
    });

    assert_eq!(from_response, from_collected);
    assert_eq!(
        from_collected["last"].target(),
        "https://api.example.com/items?page=9"
    );
}

#[test]
fn a_response_with_no_link_header_answers_an_empty_set() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("{}").unwrap());
    let c = Client::builder(m).build().unwrap();

    let resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();
    assert!(resp.links().is_empty());
    assert!(resp.links().get("next").is_none());
}
