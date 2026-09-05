//! Every knob a `Client` has, set in one place — and it runs.
//!
//! This example exists because the question *"how do I plug in the cache,
//! the cookie jar and the rest?"* was asked out loud and the answer had to
//! be assembled out of five doc comments. That is the finding this crate's
//! first outside consumer already reported: **the gap is a pointer, not a
//! feature.** The front page's table says where each piece is; this says
//! what they look like together.
//!
//! It sends its requests through [`hclient::mock::MockTransport`], so it
//! opens no socket and CI can **run** it rather than only build it — which
//! is the difference between an example that is checked and one that
//! merely compiles. Everything below is asserted at the end.
//!
//! ```text
//! cargo run -p hclient --example configured \
//!     --features test-util,cookies,cache
//! ```
//!
//! # What is not here, and why
//!
//! **Compression** takes no call at all: switch on `gzip`, `brotli`,
//! `deflate` or `zstd` and the client asks for what it can reverse and
//! reverses what the server chose. **Authentication** is per request
//! rather than per client — `RequestBuilder::digest_auth` and its
//! neighbours — because a credential belongs to the exchange that carries
//! it. And **a proxy or a hook** goes on the *transport*, which is the
//! thing that opens connections: `default_transport()?.proxy(..)`.
// **The gate is on the items, not on the file.** `#![cfg(..)]` at the
// top compiles the whole file away when the feature is off — `fn main`
// with it — and cargo then reports `main` function not found, which is
// what `just test-no-default` caught. `no_tls_no_resolver.rs` had the
// shape right already: gate the body and leave a `main` that says what
// is missing.
#[cfg(all(feature = "test-util", feature = "cookies", feature = "cache"))]
mod demo {

    use std::time::Duration;

    use hclient::cache::HttpCache;
    use hclient::cookie::CookieJar;
    use hclient::mock::{MockTransport, TestTimer};
    use hclient::redirect::Limit;
    use hclient::retry::Standard;
    use hclient::{Client, Timeouts};

    pub fn run() {
        let transport = MockTransport::new();

        // A redirect, then the answer — so the hop limit and the jar both have
        // something to do. The `Set-Cookie` is on the second response, which is
        // where a jar earns its place: it is stored here and sent on the *next*
        // request, not this one.
        transport.push_response(
            http::Response::builder()
                .status(302)
                .header("location", "/moved")
                .body("")
                .unwrap(),
        );
        transport.push_response(
            http::Response::builder()
                .status(200)
                .header("set-cookie", "session=abc; Path=/")
                .header("cache-control", "max-age=60")
                .body("hello")
                .unwrap(),
        );

        let timer = TestTimer::new();
        let client = Client::builder(transport.clone())
            // Where a relative URL is resolved against. With it, the requests
            // below are `.get("/one")` rather than a repeated origin.
            .base_url("https://example.test".parse().unwrap())
            .user_agent("configured-example/1".parse().unwrap())
            // RFC 6265 rules, plus the compiled-in public suffix list.
            .cookie_jar(CookieJar::new())
            // RFC 9111 freshness and validation, in memory.
            .cache(HttpCache::new())
            // **The clock is an argument, and that is not decoration**: the
            // first version of `retry` used the client's own timer, which
            // without `default-transport` is `NoClock`, whose `Sleep` is
            // `std::future::Pending` — so it compiled everywhere and hung for
            // ever at the first backoff. The precondition moved into the
            // signature.
            //
            // `ClientBuilder::total_timeout` takes one for the same reason and
            // is **not** set here, which is a fact about this example rather
            // than about the client: `TestTimer::Sleep` is
            // `std::future::Ready`, so every sleep finishes at once — right
            // for a test, where an assertion should be about the decision and
            // never about a delay, and useless for showing a deadline, which
            // would fire before the first request.
            .retry(timer, Standard::default())
            // Counted on what the caller receives, not on what crossed the
            // wire — a decompression bomb is small on the wire.
            .response_limit(8 * 1024 * 1024)
            // **Composing narrows.** `Limit::new(5)` permits at most five hops;
            // `.and(..)` may only ever refuse more, never allow more, so the
            // meet of two policies is the more conservative answer.
            .redirect(Limit::new(5))
            .build()
            .expect("the mock supports every setting above");

        let body = futures_executor::block_on(async {
            client.get("/one").send().await?.collect().await?.text()
        })
        .expect("two scripted responses, one redirect between them");

        assert_eq!(body, "hello");

        // The redirect was followed by the client, so the transport saw two
        // requests for one `send()`.
        let sent = transport.requests();
        assert_eq!(sent.len(), 2, "one hop, then the answer");
        assert_eq!(sent[0].uri.path(), "/one");
        assert_eq!(sent[1].uri.path(), "/moved");

        // The `User-Agent` the builder set is on the wire, on both hops.
        for req in &sent {
            assert_eq!(req.headers["user-agent"], "configured-example/1");
        }

        // The jar stored what the second response set. It is not on either
        // request above — a cookie is sent on the request *after* the one that
        // set it.
        let jar = client.cookies().expect("a jar was configured");
        assert_eq!(jar.len(), 1, "one cookie, from the 200");
        drop(jar);

        // **A setting the transport cannot honour is an error, not a silence.**
        // Per-phase bounds are the example: `Timeouts::connect` is a promise
        // about a connect, and a transport that opens no connection cannot
        // keep it. `build()` refuses and names the field — where a client that
        // accepted the value would leave a caller believing a ceiling was in
        // force. Every capability on the seam is a gate of this shape.
        let refused = Client::builder(transport.clone())
            .timeouts(Timeouts {
                connect: Some(Duration::from_secs(5)),
                ..Default::default()
            })
            .build()
            .expect_err("the mock declares no connect phase");
        println!("refused, and it says which field: {refused}");

        println!("body: {body}");
        println!("requests the transport saw: {}", sent.len());
        println!("cookies held: 1");
        println!("cache: {:?}", client.cache().is_some());
    }
}

#[cfg(all(feature = "test-util", feature = "cookies", feature = "cache"))]
fn main() {
    demo::run();
}

#[cfg(not(all(feature = "test-util", feature = "cookies", feature = "cache")))]
fn main() {
    eprintln!("this example needs `--features test-util,cookies,cache`");
}
