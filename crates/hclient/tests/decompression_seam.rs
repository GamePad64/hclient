//! The content-coding seam, from outside: a caller choosing what this
//! client asks for, and a caller supplying a coding this crate does not
//! ship.
//!
//! `decompress`'s own unit tests pin what `negotiate` and `lookup`
//! *decide*, and they can see `SharedContentCoding` and the validator.
//! What they cannot see is the **wiring** — that
//! `ClientBuilder::decompression` reaches the list `execute_with` reads,
//! that the list's order reaches the wire, and that a decoder a caller
//! wrote is the one a response body is actually driven through. Every
//! assertion here is made on a request the mock transport really received
//! or on bytes a real `Client` really handed back.
//!
//! **Written against a coding of this file's own wherever it can be**, so
//! that the assertions say something in every feature build rather than
//! only where `gzip` happens to be compiled in. `decompression_is_off`
//! and the order test name no cargo feature at all; only the two that
//! need real compressed bytes do.
//!
//! No cfg gate on the target, for `compression_capability.rs`'s reason:
//! everything here is `hclient-mock` and byte literals, with no
//! `TcpListener` and no native-only dev-dependency, so this file builds
//! for every target the crate does — including
//! `wasm32-unknown-unknown`, where `wasm-pack test` compiles every test
//! target of the crate.
#![cfg(feature = "test-util")]

use bytes::Bytes;
use hclient::mock::MockTransport;
use hclient::{Client, ContentCoding, Decode};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A content coding written the way a caller outside this crate would
/// write one: a token, a decoder, and nothing this crate had to be told
/// about.
///
/// Its decoder **upper-cases** what it is given rather than passing it
/// through, which is what makes "this decoder ran" observable in the
/// response body rather than only in a counter. The counter is there too,
/// because a body that happens to be empty would make the transformation
/// invisible.
#[derive(Debug)]
struct Shouty {
    token: &'static str,
    built: Arc<AtomicUsize>,
}

impl ContentCoding for Shouty {
    /// **`&str` and not `&'static str`, and clippy is the witness.** The
    /// decoders below return literals and clippy's
    /// `unnecessary_literal_bound` insists they say `&'static str`; this
    /// one answers out of a **field** and is not flagged, which is the
    /// whole argument for the seam's signature — a coding whose token is
    /// configured rather than written can keep it and lend it, and pays
    /// nothing for the freedom because an `Arc<dyn ContentCoding>`
    /// outlives every call made on it.
    fn token(&self) -> &str {
        self.token
    }
    fn decoder(&self) -> hclient::Decoder {
        self.built.fetch_add(1, Ordering::SeqCst);
        Box::new(Upper)
    }
}

#[derive(Debug)]
struct Upper;

impl Decode for Upper {
    fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
        Ok(Bytes::from(input.to_ascii_uppercase()))
    }
    fn finish(&mut self) -> Result<Bytes, std::io::Error> {
        Ok(Bytes::new())
    }
    fn token(&self) -> &'static str {
        "shouty"
    }
}

fn shouty(token: &'static str) -> (hclient::SharedContentCoding, Arc<AtomicUsize>) {
    let built = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(Shouty {
            token,
            built: Arc::clone(&built),
        }),
        built,
    )
}

/// What the request that went out asked for, read off the transport
/// rather than off the config — the difference between *the builder
/// stored it* and *it reached the wire*.
fn asked(c: &Client) -> Option<String> {
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .requests()[0]
        .headers
        .get(http::header::ACCEPT_ENCODING)
        .map(|v| v.to_str().unwrap().to_owned())
}

fn get(c: &Client) -> Result<hclient::Collected, hclient::Error> {
    futures_executor::block_on(async { c.get("https://a/x").send().await?.collect().await })
}

/// **The CPU lever, and it is one call.** An empty list means no
/// `Accept-Encoding` goes out at all.
///
/// The measured argument this exists for: at 1900 MiB/s gzip decode, a
/// service doing 5000 RPS of 1.7 MiB responses spends 4.5 cores reversing
/// codings, and before this there was no way to turn that off for one
/// client — `default_headers` cannot, because `negotiate` runs before
/// they are applied.
#[test]
fn an_empty_list_sends_no_accept_encoding() {
    let c = Client::builder(MockTransport::new())
        .decompression([])
        .build()
        .expect("an empty list is a configuration, not an error");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(http::Response::builder().body(Vec::new()).unwrap());

    get(&c).expect("responds");
    assert_eq!(
        asked(&c),
        None,
        "a client asked to decode nothing must not ask the server to compress"
    );
}

/// The control for the test above, and the half a header check alone
/// would miss: with the default list the header **is** sent, so the
/// absence above is the setting rather than a client that never asks.
///
/// Gated, because a build with no coding features has nothing to ask for
/// and the header is legitimately absent there.
#[cfg(any(
    feature = "gzip",
    feature = "brotli",
    feature = "zstd",
    feature = "deflate"
))]
#[test]
fn the_default_list_still_asks_for_what_it_always_did() {
    let c = Client::builder(MockTransport::new())
        .build()
        .expect("the default configuration is supported");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(http::Response::builder().body(Vec::new()).unwrap());

    get(&c).expect("responds");
    let asked = asked(&c).expect("a caller who said nothing keeps the compiled-in behaviour");
    // The property rather than a literal: which tokens appear depends on
    // the feature set this file is compiled under, and the *order* is
    // pinned in `decompress`'s own test where the four-feature build can
    // be named.
    for token in asked.split(", ") {
        assert!(
            ["zstd", "br", "gzip", "deflate"].contains(&token),
            "the default list is this crate's own codings, saw {token:?}"
        );
    }
}

/// **A one-element list sends exactly that token** — the narrowing a
/// build could never express, because Cargo unifies features and a
/// library deep in the tree decides what every client in the process asks
/// for.
#[test]
fn a_one_element_list_sends_exactly_that_token() {
    let (coding, _) = shouty("frob");
    let c = Client::builder(MockTransport::new())
        .decompression([coding])
        .build()
        .expect("supported");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(http::Response::builder().body(Vec::new()).unwrap());

    get(&c).expect("responds");
    assert_eq!(
        asked(&c).as_deref(),
        Some("frob"),
        "exactly the one coding configured, and nothing the build happens to carry"
    );
}

/// **The slice order is the wire order**, which is what replaced
/// `Registration::preference` — a field that existed only because a
/// `BTreeMap` orders by key and would have advertised alphabetically.
///
/// Asserted in both directions on the same two codings, because one
/// ordering alone passes for an implementation that sorts.
#[test]
fn the_slice_order_reaches_the_wire() {
    for (first, second) in [("aaa", "zzz"), ("zzz", "aaa")] {
        let (a, _) = shouty(first);
        let (b, _) = shouty(second);
        let c = Client::builder(MockTransport::new())
            .decompression([a, b])
            .build()
            .expect("supported");
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .push_response_bytes(http::Response::builder().body(Vec::new()).unwrap());

        get(&c).expect("responds");
        assert_eq!(
            asked(&c).as_deref(),
            Some(format!("{first}, {second}").as_str()),
            "the header is the list as written; a sort would answer `aaa, zzz` both times"
        );
    }
}

/// **The whole seam, end to end**: a coding implemented in this test file
/// is advertised, is matched against the `Content-Encoding` that comes
/// back, and its decoder is what the response body is driven through.
///
/// The body is the assertion rather than the counter, because a counter
/// alone would pass for a decoder that was constructed and never fed. The
/// counter is beside it because an empty body would make the
/// transformation invisible, and `Decompressed` deliberately does not run
/// the integrity check over a body with no bytes at all.
#[test]
fn a_coding_from_the_test_file_is_asked_for_and_its_decoder_used() {
    let (coding, built) = shouty("frob");
    let c = Client::builder(MockTransport::new())
        .decompression([coding])
        .build()
        .expect("supported");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(
            http::Response::builder()
                .header(http::header::CONTENT_ENCODING, "frob")
                .body(vec![
                    Bytes::from_static(b"quiet "),
                    Bytes::from_static(b"words"),
                ])
                .unwrap(),
        );

    let got = get(&c).expect("the caller's own coding is one this client can reverse");
    assert_eq!(
        got.text().unwrap(),
        "QUIET WORDS",
        "the bytes went through the decoder this test wrote, not through one of ours"
    );
    assert_eq!(
        built.load(Ordering::SeqCst),
        1,
        "one decoder per response body, built by the coding rather than shared"
    );
    assert_eq!(asked(&c).as_deref(), Some("frob"));
}

/// **A coding that was not configured is not reversed**, which is the
/// other half of the list being the only list: the body reaches the
/// caller as it arrived rather than through a decoder nobody asked for.
///
/// The control for the test above — same server, same header, the one
/// difference being what the client carries.
#[test]
fn a_coding_the_client_does_not_carry_is_left_alone() {
    let (coding, built) = shouty("frob");
    let c = Client::builder(MockTransport::new())
        .decompression([coding])
        .build()
        .expect("supported");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(
            http::Response::builder()
                .header(http::header::CONTENT_ENCODING, "other")
                .body(vec![Bytes::from_static(b"quiet words")])
                .unwrap(),
        );

    let got = get(&c).expect("a coding we cannot reverse is not an error");
    assert_eq!(
        got.text().unwrap(),
        "quiet words",
        "untouched, because nothing in the list claims `other`"
    );
    assert_eq!(built.load(Ordering::SeqCst), 0);
}

/// **A token that is not an RFC 9110 §5.6.2 token is refused at
/// `build()`**, naming it.
///
/// This is the check that replaced an `expect` whose justification died
/// with the closed set: `accept_encoding` assembled its header value
/// under *"every token is a compile-time ASCII constant… nothing here
/// comes from the network or the caller"*, which stopped being true the
/// moment a caller could supply one. A space is the character that
/// matters — the header separator is `", "`, so `"my coding"` would be
/// read by a server as two codings.
#[test]
fn a_coding_whose_token_is_not_a_token_is_refused_at_build() {
    let (coding, _) = shouty("my coding");
    let err = Client::builder(MockTransport::new())
        .decompression([coding])
        .build()
        .expect_err("a token with a space in it cannot go in `Accept-Encoding`");
    let named = err
        .invalid_coding_token()
        .expect("a coding refusal, not a capability one");
    assert_eq!(
        named.token, "my coding",
        "the refusal must name the offending spelling: {err}"
    );
}

/// The control for the test above: an unusual but legal token builds, so
/// the refusal is about the grammar rather than about anything this crate
/// has not heard of.
#[test]
fn an_unusual_but_legal_token_is_accepted_at_build() {
    let (coding, _) = shouty("x-my-coding1.5");
    let c = Client::builder(MockTransport::new())
        .decompression([coding])
        .build()
        .expect("`x-my-coding1.5` is a token; only the grammar decides");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(http::Response::builder().body(Vec::new()).unwrap());
    get(&c).expect("responds");
    assert_eq!(asked(&c).as_deref(), Some("x-my-coding1.5"));
}

/// **The four refusals still stand over a caller's list**, which is worth
/// pinning because they were written when the list was the build's: a
/// `HEAD` asks for nothing however the client is configured.
///
/// One of the four here rather than all: the other three are
/// `decompress`'s own unit tests, which can vary the capability and the
/// headers directly. What this adds is that a *configured* list does not
/// route around them.
#[test]
fn a_configured_list_does_not_route_around_the_head_refusal() {
    let (coding, built) = shouty("frob");
    let c = Client::builder(MockTransport::new())
        .decompression([coding])
        .build()
        .expect("supported");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(
            http::Response::builder()
                .header(http::header::CONTENT_ENCODING, "frob")
                .body(Vec::new())
                .unwrap(),
        );

    futures_executor::block_on(async {
        c.head("https://a/x")
            .send()
            .await
            .expect("responds")
            .collect()
            .await
            .expect("collects")
    });
    assert_eq!(
        asked(&c),
        None,
        "a HEAD response has no body for a coding to apply to"
    );
    assert_eq!(
        built.load(Ordering::SeqCst),
        0,
        "and a `Content-Encoding` on a bodiless response is not decoded either"
    );
}

/// **A coding's token reaches a decode error**, which is the other reader
/// of `Decode::token` and the one that had to become a `Cow` when the set
/// opened.
#[test]
fn a_failing_third_party_decoder_names_its_coding_in_the_error() {
    #[derive(Debug)]
    struct Boom;
    impl ContentCoding for Boom {
        fn token(&self) -> &'static str {
            "boom"
        }
        fn decoder(&self) -> hclient::Decoder {
            Box::new(Boom)
        }
    }
    impl Decode for Boom {
        fn push(&mut self, _: &[u8]) -> Result<Bytes, std::io::Error> {
            Err(std::io::Error::other("nope"))
        }
        fn finish(&mut self) -> Result<Bytes, std::io::Error> {
            Ok(Bytes::new())
        }
        fn token(&self) -> &'static str {
            "boom"
        }
    }

    let c = Client::builder(MockTransport::new())
        .decompression([Arc::new(Boom) as hclient::SharedContentCoding])
        .build()
        .expect("supported");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response_bytes(
            http::Response::builder()
                .header(http::header::CONTENT_ENCODING, "boom")
                .body(vec![Bytes::from_static(b"anything")])
                .unwrap(),
        );

    let err = get(&c).expect_err("the decoder refused the body");
    assert_eq!(*err.kind(), hclient::ErrorKind::Decode);
    let named = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<hclient::error::DecodeFailed>())
        .expect("a decode failure carries which coding failed");
    assert_eq!(
        named.coding, "boom",
        "a coding this crate never heard of still names itself in the error"
    );
}
