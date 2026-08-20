//! `ClientBuilder::response_limit`, and the axis it counts on.
//!
//! The interesting test in this file is the compressed one. A limit that
//! counted wire bytes would let a decompression bomb through **by
//! definition** — small on the wire is what makes it a bomb — so the
//! claim that separates a real bound from a plausible one is that a 60-byte
//! gzip stream expanding to a megabyte is stopped by a 1 KiB limit.
#![cfg(all(feature = "test-util", not(target_family = "wasm")))]

use hclient::mock::MockTransport;
use hclient::{Client, ResponseTooLarge};

fn drain(c: &Client<MockTransport>) -> Result<usize, hclient_core::Error> {
    futures_executor::block_on(async {
        let body = c.get("https://a/x").send().await?.collect().await?;
        Ok(body.bytes().len())
    })
}

/// Under the limit is untouched, over it is a typed error carrying both
/// numbers. The pair is the assertion: a wrapper that always errored would
/// pass the second alone, and one that never did would pass the first.
#[test]
fn a_body_over_the_limit_is_refused_and_one_under_it_is_not() {
    for (limit, len, over) in [(100u64, 50usize, false), (100, 500, true)] {
        let t = MockTransport::new();
        t.push_response(
            http::Response::builder()
                .status(200)
                .body(Box::leak("x".repeat(len).into_boxed_str()) as &'static str)
                .unwrap(),
        );
        let c = Client::builder(t)
            .response_limit(limit)
            .build()
            .expect("build");
        match drain(&c) {
            Ok(n) => {
                assert!(
                    !over,
                    "a {len}-byte body under a {limit}-byte limit must pass"
                );
                assert_eq!(n, len);
            }
            Err(e) => {
                assert!(
                    over,
                    "a {len}-byte body under a {limit}-byte limit must not fail: {e:?}"
                );
                let too_large = std::error::Error::source(&e)
                    .and_then(|s| s.downcast_ref::<ResponseTooLarge>())
                    .unwrap_or_else(|| panic!("the typed refusal, not any error: {e:?}"));
                assert_eq!(too_large.limit, limit);
                assert!(
                    too_large.seen > limit,
                    "and the count that tripped it: {too_large:?}"
                );
            }
        }
    }
}

/// **The control, and it is the whole reason the default is unset**: the
/// same oversized body with no limit configured is handed over in full.
#[test]
fn with_no_limit_the_same_body_is_handed_over_whole() {
    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(200)
            .body(Box::leak("x".repeat(500).into_boxed_str()) as &'static str)
            .unwrap(),
    );
    let c = Client::builder(t).build().expect("build");
    assert_eq!(drain(&c).expect("no bound was set"), 500);
}

/// **The limit counts what the caller receives, not what crossed the
/// wire**, which is the claim a decompression bomb tests.
///
/// The stream here is a few dozen bytes of gzip that expands to a
/// megabyte. A limit applied inside the decompressor would see the small
/// number and pass it; this one sees the large one and stops.
#[cfg(feature = "gzip")]
#[test]
fn a_small_gzip_that_expands_past_the_limit_is_stopped() {
    let plain = vec![b'z'; 1024 * 1024];
    let compressed = {
        use std::io::Write as _;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(&plain).expect("encode");
        e.finish().expect("finish")
    };
    assert!(
        compressed.len() < 4096,
        "the premise: {} wire bytes for a megabyte, or this tests nothing",
        compressed.len()
    );

    let t = MockTransport::new();
    t.push_response_bytes(
        http::Response::builder()
            .status(200)
            .header("content-encoding", "gzip")
            .body(vec![bytes::Bytes::from(compressed.clone())])
            .unwrap(),
    );
    let c = Client::builder(t)
        .response_limit(4096)
        .build()
        .expect("build");
    let err = drain(&c).expect_err("a megabyte under a 4 KiB limit must be refused");
    let too_large = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<ResponseTooLarge>())
        .unwrap_or_else(|| panic!("the typed refusal: {err:?}"));
    assert!(
        too_large.seen > 4096,
        "the count is of DECOMPRESSED bytes — {} wire bytes would have passed silently",
        compressed.len()
    );
}
