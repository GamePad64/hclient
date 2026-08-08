//! Two shape properties compression must not break, neither of which any
//! behavioural test in the suite can see — v0.2 W5.
//!
//! # 1. Decompressing does not change `Client`'s type
//!
//! This is the design document's whole argument for putting compression
//! inside `Client` rather than in a `tower` layer (§W5): a layer wraps the
//! transport, so `Client<T>` becomes `Client<Decompressing<T>>` and
//! `struct App { http: Client }` — the shape a consumer actually writes —
//! stops compiling. `tests/deadline_client_type.rs` pins exactly this for
//! W4's `total_timeout`, and its module doc names compression as the case
//! the same argument was originally made about. This file is that case,
//! now that it exists.
//!
//! Every annotation below is load-bearing. Deleting `: Client` from any of
//! them leaves a file that would go on compiling if compression had been
//! implemented by wrapping the transport, or by giving `Client` a third
//! type parameter for its decoder set.
//!
//! # 2. The two body wrappers are in the order the deadline needs
//!
//! `Client::execute` returns `Decompressed<Deadline<T::Body, Tm>>` — the
//! deadline INSIDE, so it is polled once per compressed frame off the
//! wire. The other way round, one poll of the decoder could pull an
//! unbounded number of frames without the clock being consulted at all,
//! and a slow server sending well-compressing padding would walk straight
//! around a `total_timeout`. See `src/decompress.rs`'s module doc comment
//! for the long form.
//!
//! The order is pinned here as a TYPE, because that is what it is: the
//! second test names `Decompressed<Deadline<..>>` explicitly, and
//! `into_inner()` then reaches a value with `total_timeout()` on it, which
//! only exists on the deadline. Swap the two wrappers in `execute` and
//! neither line compiles. A behavioural test cannot reach this: both
//! orders bound the operation for any body that ever returns `Pending`,
//! and the case that separates them — a body that yields frame after
//! frame without yielding to the executor — is not reproducible over a
//! real socket.
//!
//! The gate on `default-transport` is `deadline_client_type.rs`'s, and for
//! the same reason: `Client` with no parameter is what the first property
//! is about, and it only exists with that feature. The `gzip` gate is this
//! task's — without a decoder compiled in, the first property would hold
//! for a client that had no compression in it at all.
#![cfg(all(
    feature = "default-transport",
    feature = "gzip",
    feature = "test-util",
    not(target_family = "wasm")
))]

use http_ng::mock::{MockTransport, TestTimer};
use http_ng::{Client, Deadline, Decompressed};
use std::time::Duration;

/// The declaration the first half of this file exists for: a bare
/// `Client`, no parameters, in a struct field.
struct App {
    http: Client,
}

#[test]
fn a_client_that_decompresses_still_fits_a_bare_client_field() {
    let plain: Client = Client::new().expect("default transport supports the default config");
    // And it still composes with the other v0.2 feature that could have
    // widened it: neither of them may.
    let bounded: Client = plain.total_timeout(Duration::from_secs(5));

    let app = App { http: bounded };
    assert_eq!(
        app.http.config().total,
        Some(Duration::from_secs(5)),
        "a file that only declared types would also pass with every setter \
         a no-op; this is the liveness anchor"
    );
    let clone: Client = app.http.clone();
    assert_eq!(clone.config().total, Some(Duration::from_secs(5)));
}

/// The order, as a type — and then read back through it at runtime, so
/// this is not purely a declaration.
///
/// `MockTransport` rather than the default transport: no wire is involved
/// in a question about which wrapper is on the outside, and `TestTimer`
/// gives the deadline a clock that needs no runtime.
#[test]
fn the_deadline_sits_inside_the_decoder_not_outside_it() {
    let c = Client::builder(MockTransport::new())
        .total_timeout(TestTimer::new(), Duration::from_secs(30))
        .build()
        .expect("mock supports the default config");
    c.transport()
        .push_response(http::Response::builder().status(200).body("plain").unwrap());

    let resp = futures_executor::block_on(c.get("https://a/x").send()).expect("responds");

    // The annotation is the assertion: `Decompressed` outside, `Deadline`
    // in, and `MockBody` — the transport's own — innermost.
    let body: Decompressed<Deadline<http_ng::mock::MockBody, TestTimer>> = resp.into_parts().1;
    assert_eq!(
        body.coding(),
        None,
        "this response carries no `Content-Encoding`, so nothing is being decoded"
    );
    let deadline: Deadline<http_ng::mock::MockBody, TestTimer> = body.into_inner();
    assert_eq!(
        deadline.total_timeout(),
        Some(Duration::from_secs(30)),
        "the bound must be on the wrapper directly around the transport's body — \
         it is the compressed stream that has to be measured"
    );
}
