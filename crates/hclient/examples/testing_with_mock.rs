//! Testing your own code against a **real** `Client` and no network.
//!
//! ```text
//! cargo run -p hclient --example testing_with_mock --features test-util,json
//! ```
//!
//! # Why this rather than a hand-built `Response`
//!
//! Because [`Response`](hclient::Response) has no public constructor, which
//! reads as a wall and is a signpost pointing the other way. The first
//! consumer to port onto this crate hit it and worked around it before
//! finding [`MockTransport`](hclient::mock::MockTransport) — a pointer
//! problem, not a missing feature, and the reason this example exists.
//!
//! A hand-built response would test a path production never takes. This
//! scripts what the *transport* returns and lets the real client do
//! everything above it: the redirect chain, the cookie jar, decompression,
//! retries, `error_for_status`, `.json()`. The code under test is exercised
//! through the same layers a live request goes through.
//!
//! # Three things the double refuses to do, each for a reason
//!
//! **It will not match requests.** The flows this exists for — a redirect
//! chain, a `425` replay, a retry — are **ordered**, and a matcher would
//! let a test pass while the code under test made its requests in the
//! wrong order. Responses come back in the order they were pushed, and
//! [`requests()`](hclient::mock::MockTransport::requests) is how a test
//! says what it expected.
//!
//! **It will not read a rewindable body for you.** Calling the factory
//! would make the mock a second caller, and a test in this crate counts
//! factory calls to pin *one snapshot per hop, not one per attempt*.
//! `RecordedBody` therefore has four cases — *no body*, *bytes*, *a body
//! this mock will not read for you*, and *a body nothing can read twice* —
//! because a silent empty would pass a test that an honest refusal fails.
//!
//! **It will not pretend to capabilities it lacks.** Configure something
//! it cannot honour and `build()` refuses, naming the field. That is the
//! same gate a real backend gets, so a test cannot be green over a setting
//! production would reject.
#![cfg(all(feature = "test-util", feature = "json"))]

use hclient::mock::MockTransport;
use hclient::{Client, Error};

/// The code under test: it knows nothing about mocks, takes a `&Client`,
/// and is the thing a library actually ships.
async fn latest_release(client: &Client, repo: &str) -> Result<String, Error> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }
    let release: Release = client
        .get(format!("/repos/{repo}/releases/latest"))
        .header("accept", "application/vnd.example+json")
        .send()
        .await?
        // A `404` becomes an `Err` here rather than a successful parse of
        // an error document — which is what this call is for.
        .error_for_status()?
        .collect()
        .await?
        .json()?;
    Ok(release.tag_name)
}

fn main() {
    // ── the happy path, through a redirect ────────────────────────────
    let transport = MockTransport::new();
    transport.push_response(
        http::Response::builder()
            .status(301)
            .header("location", "/repos/acme/tool/releases/latest?canonical")
            .body("")
            .unwrap(),
    );
    transport.push_response(
        http::Response::builder()
            .status(200)
            .body(r#"{"tag_name":"v2.1.0"}"#)
            .unwrap(),
    );

    let client = Client::builder(transport.clone())
        .base_url("https://api.example.test".parse().unwrap())
        .build()
        .expect("nothing configured that this double refuses");

    let tag = futures_executor::block_on(latest_release(&client, "acme/tool"))
        .expect("two scripted responses");
    assert_eq!(tag, "v2.1.0");

    // What the code under test actually sent — including the hop it never
    // knew about, because the client followed it.
    let sent = transport.requests();
    assert_eq!(sent.len(), 2, "the redirect was followed by the client");
    assert_eq!(sent[0].uri.path(), "/repos/acme/tool/releases/latest");
    assert_eq!(
        sent[1].headers["accept"], "application/vnd.example+json",
        "a header set by the caller survives the hop"
    );
    println!("tag: {tag}, requests: {}", sent.len());

    // ── the failure path, which is the half usually left untested ─────
    let transport = MockTransport::new();
    transport.push_response(
        http::Response::builder()
            .status(404)
            .body(r#"{"message":"Not Found"}"#)
            .unwrap(),
    );
    let client = Client::builder(transport)
        .base_url("https://api.example.test".parse().unwrap())
        .build()
        .unwrap();

    let err = futures_executor::block_on(latest_release(&client, "acme/gone"))
        .expect_err("a 404 must not parse as a release");
    // `ErrorKind` is an enum, so this is a match rather than a string
    // comparison against a message — which is what a `reqwest::Error`
    // forces, and why three of one consumer's tests had to make real
    // network requests before they ported.
    assert_eq!(*err.kind(), hclient::ErrorKind::Status);
    println!("404 -> {:?}: {err}", err.kind());
}
