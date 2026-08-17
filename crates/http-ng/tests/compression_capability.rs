//! The gate: **which** transports get decompressed for, and which get
//! nothing at all — v0.2 W5.
//!
//! `tests/compression.rs` proves that a real gzip body comes back as
//! plaintext. That is the easy half. This file is the other one: against a
//! transport that decodes for itself, the client must NOT decode again and
//! must NOT ask for a coding, and both must be observable from outside.
//!
//! # The trap this file exists for
//!
//! `http-ng-fetch` lists `Accept-Encoding` among its
//! `forbidden_request_headers` AND decompresses internally. Reading the
//! first as the gate for the second would pass every test written against
//! that one backend, and would be wrong: "this header cannot be sent" and
//! "the body reaching you is already decoded" are different claims. The
//! third test below is the one that tells them apart — a transport that
//! forbids the header while decoding nothing must get no header from us
//! and must still have its response decoded, because a `Content-Encoding`
//! the server applied unbidden is still ours to reverse. Deriving one from
//! the other is the "capability that lies" defect this workspace has now
//! caught four times, and it is half-set already, which is why it is
//! pinned here rather than argued about.
//!
//! No cfg gate on the target, unlike `compression.rs`: everything here is
//! `http-ng-mock` and a byte literal, with no `TcpListener` and no
//! native-only dev-dependency, so this file BUILDS for every target the
//! crate does — including `wasm32-unknown-unknown`, where `wasm-pack test`
//! compiles every test target of the crate and a native-only helper would
//! break the browser job. The tests themselves are plain `#[test]`s and so
//! run on the host, as they should: the gate is a decision made from
//! `Capabilities`, and the same decision on every target.
//!
//! It IS gated on `gzip`, because without a decoder compiled in every
//! assertion below would hold for the wrong reason.
#![cfg(all(feature = "test-util", feature = "gzip"))]

use bytes::Bytes;
use http_ng::mock::MockTransport;
use http_ng::{Capabilities, Client, DecompressionSupport, ErrorKind};

/// A real gzip stream, produced by **GNU gzip 1.14**, not by this crate's
/// own encoder:
///
/// ```text
/// printf 'hello, gzip — the plaintext this test asserts on\n' \
///   | gzip -9 -n -c | xxd -i
/// ```
///
/// An independent implementation on the producing side is the point. A
/// blob made by `flate2` and read back by `flate2` would still pass if
/// both agreed on something that is not gzip; this one cannot.
const GZIP_BLOB: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7,
    0x51, 0x48, 0xaf, 0xca, 0x2c, 0x50, 0x78, 0xd4, 0x30, 0x45, 0xa1, 0x24, 0x23, 0x55, 0xa1, 0x20,
    0x27, 0x31, 0x33, 0xaf, 0x24, 0xb5, 0xa2, 0x04, 0xc8, 0xcb, 0x2c, 0x56, 0x28, 0x49, 0x2d, 0x2e,
    0x51, 0x48, 0x2c, 0x2e, 0x4e, 0x2d, 0x2a, 0x29, 0x56, 0xc8, 0xcf, 0xe3, 0x02, 0x00, 0x6d, 0x30,
    0xb3, 0x4c, 0x33, 0x00, 0x00, 0x00,
];

const PLAINTEXT: &str = "hello, gzip — the plaintext this test asserts on\n";

fn caps(decompression: DecompressionSupport) -> Capabilities {
    let mut c = Capabilities::none();
    c.response_decompression = decompression;
    c
}

/// The blob, split across two frames, so nothing here can pass by
/// accidentally treating a whole stream as one buffer.
fn gzip_frames() -> Vec<Bytes> {
    let (a, b) = GZIP_BLOB.split_at(20);
    vec![Bytes::from_static(a), Bytes::from_static(b)]
}

fn get(c: &Client<MockTransport>) -> Result<http_ng::Collected, http_ng::Error> {
    futures_executor::block_on(async { c.get("https://a/x").send().await?.collect().await })
}

/// The premise every other test in this file rests on: this client really
/// does decode, so a later "it did not decode" is a fact about the gate
/// and not about a build with no decoder in it.
#[test]
fn the_baseline_transport_does_get_its_body_decoded() {
    let m = MockTransport::new().with_capabilities(caps(DecompressionSupport::None));
    let c = Client::builder(m).build().expect("supported");
    c.transport().push_response_bytes(
        http::Response::builder()
            .header(http::header::CONTENT_ENCODING, "gzip")
            .body(gzip_frames())
            .unwrap(),
    );

    let got = get(&c).expect("decodes");
    assert_eq!(got.text().unwrap(), PLAINTEXT);
    // The property, not the literal. This file is gated on `gzip` alone,
    // so the header's exact value depends on which of the four codings
    // this build compiled in — `"gzip"`, `"br, gzip"`, `"zstd, br, gzip,
    // deflate"` are all correct answers here. What is asserted is what
    // this test is named after: the coding that was decoded was asked
    // for. The full list and its order are `decompress.rs`'s decision and
    // are pinned by its own tests, which can see `Decoders`.
    let asked = c.transport().requests()[0]
        .headers
        .get(http::header::ACCEPT_ENCODING)
        .map(|v| v.to_str().unwrap().to_owned())
        .expect("a transport that does not decode must be asked for a coding");
    assert!(
        asked.split(", ").any(|t| t == "gzip"),
        "the body decoded was gzip and the header must name it: {asked}"
    );
}

/// The negative half. The transport says it already decoded, so the body
/// it hands over is PLAINTEXT that still carries `Content-Encoding: gzip`
/// — which is exactly what a browser `fetch` response looks like, and the
/// reason `http-ng-fetch`'s `Body::size_hint` distrusts `Content-Length`.
///
/// Two things must hold, and the first would be missed by a test that only
/// looked at headers: the plaintext must arrive intact. A client that
/// decoded anyway would not merely be wasteful — it would meet bytes that
/// are not a gzip stream and turn a perfectly good response into
/// `ErrorKind::Decode`.
#[test]
fn against_a_transport_that_decodes_for_itself_the_client_does_neither() {
    let m = MockTransport::new().with_capabilities(caps(DecompressionSupport::Internal));
    let c = Client::builder(m).build().expect("supported");
    c.transport().push_response_bytes(
        http::Response::builder()
            // Still there, still describing the wire — the transport
            // decoded, it did not tidy up after itself.
            .header(http::header::CONTENT_ENCODING, "gzip")
            .body(vec![Bytes::from_static(PLAINTEXT.as_bytes())])
            .unwrap(),
    );

    let got = get(&c).expect("a body the transport already decoded must arrive intact");
    assert_eq!(
        got.text().unwrap(),
        PLAINTEXT,
        "decoding a second time corrupts every compressed response"
    );
    assert!(
        !c.transport().requests()[0]
            .headers
            .contains_key(http::header::ACCEPT_ENCODING),
        "the transport chose what to ask for; a header of ours could only contradict it"
    );
    assert_eq!(
        got.headers().get(http::header::CONTENT_ENCODING).unwrap(),
        "gzip",
        "we decoded nothing, so we rewrite nothing — the header belongs to the \
         transport that owns the exchange"
    );
}

/// The two claims come apart here, and the answers must differ.
///
/// This transport forbids `Accept-Encoding` — like `fetch` — but declares
/// `DecompressionSupport::None`: it hands the bytes over as they arrived.
/// A client that read the forbidden-header list as its decompression gate
/// would skip decoding and hand a caller a gzip stream. A client that
/// ignored the forbidden-header list would send a header the transport
/// promises to reject.
#[test]
fn a_transport_that_forbids_the_header_but_decodes_nothing_still_gets_its_body_decoded() {
    let mut caps = caps(DecompressionSupport::None);
    caps.forbidden_request_headers = &[http::header::ACCEPT_ENCODING];
    let c = Client::builder(MockTransport::new().with_capabilities(caps))
        .build()
        .expect("supported");
    c.transport().push_response_bytes(
        http::Response::builder()
            .header(http::header::CONTENT_ENCODING, "gzip")
            .body(gzip_frames())
            .unwrap(),
    );

    let got = get(&c).expect("decodes");
    assert_eq!(
        got.text().unwrap(),
        PLAINTEXT,
        "a `Content-Encoding` the server applied unbidden is still ours to reverse"
    );
    assert!(
        !c.transport().requests()[0]
            .headers
            .contains_key(http::header::ACCEPT_ENCODING),
        "the transport forbids this header; we must not add it"
    );
}

/// A caller who did their own negotiating keeps it, and keeps the raw
/// body with it. Decoding an answer to a question we did not ask is the
/// same surprise as overriding the header.
#[test]
fn a_caller_who_sets_accept_encoding_gets_their_header_and_the_undecoded_body() {
    let c =
        Client::builder(MockTransport::new().with_capabilities(caps(DecompressionSupport::None)))
            .build()
            .expect("supported");
    c.transport().push_response_bytes(
        http::Response::builder()
            .header(http::header::CONTENT_ENCODING, "gzip")
            .body(gzip_frames())
            .unwrap(),
    );

    let got = futures_executor::block_on(async {
        c.get("https://a/x")
            .header("accept-encoding", "identity")
            .send()
            .await?
            .collect()
            .await
    })
    .expect("no decoding attempted, so no decode error");
    assert_eq!(
        c.transport().requests()[0].headers[http::header::ACCEPT_ENCODING],
        "identity",
        "the caller's own negotiation stands, unedited"
    );
    assert_eq!(
        got.bytes().as_ref(),
        GZIP_BLOB,
        "the body arrives exactly as the server sent it"
    );
    assert_eq!(
        got.headers().get(http::header::CONTENT_ENCODING).unwrap(),
        "gzip",
        "and its header still describes it"
    );
}

/// A coding this build cannot reverse is left alone in full: body, header
/// and all. The alternative — stripping `Content-Encoding` and handing
/// over bytes nothing can read — is the corruption this test names.
#[test]
fn an_unknown_coding_is_left_untouched_rather_than_half_handled() {
    let c =
        Client::builder(MockTransport::new().with_capabilities(caps(DecompressionSupport::None)))
            .build()
            .expect("supported");
    c.transport().push_response_bytes(
        http::Response::builder()
            .header(http::header::CONTENT_ENCODING, "compress")
            .header(http::header::CONTENT_LENGTH, "3")
            .body(vec![Bytes::from_static(b"abc")])
            .unwrap(),
    );

    let got = get(&c).expect("nothing to decode, nothing to fail");
    assert_eq!(got.bytes().as_ref(), b"abc");
    assert_eq!(got.headers()[http::header::CONTENT_ENCODING], "compress");
    assert_eq!(
        got.headers()[http::header::CONTENT_LENGTH],
        "3",
        "the body really is three encoded bytes — nothing we did changed that"
    );
}

/// A body that is not what its `Content-Encoding` claims is a typed
/// `ErrorKind::Decode`, not a silent pass-through of garbage.
///
/// The distinction matters because the safe-looking alternative — "if it
/// does not decode, hand over the raw bytes" — would deliver a gzip stream
/// to a caller who asked for text, with no indication anything went wrong.
#[test]
fn a_body_that_is_not_the_coding_it_claims_is_a_decode_error() {
    let c =
        Client::builder(MockTransport::new().with_capabilities(caps(DecompressionSupport::None)))
            .build()
            .expect("supported");
    c.transport().push_response_bytes(
        http::Response::builder()
            .header(http::header::CONTENT_ENCODING, "gzip")
            .body(vec![Bytes::from_static(b"this is not gzip at all")])
            .unwrap(),
    );

    let err = get(&c).expect_err("garbage under a gzip header must not pass for a body");
    assert_eq!(*err.kind(), ErrorKind::Decode);
    assert!(
        std::error::Error::source(&err)
            .and_then(|s| s.downcast_ref::<http_ng::DecodeFailed>())
            .is_some_and(|d| d.coding == "gzip"),
        "the caller must be able to tell a bad gzip stream from bad UTF-8: {err:?}"
    );
}

/// A truncated stream is an error too — the half that a decoder without an
/// end-of-stream check would get wrong, since the bytes it did receive
/// decode perfectly well.
#[test]
fn a_truncated_stream_is_an_error_not_a_shorter_document() {
    let c =
        Client::builder(MockTransport::new().with_capabilities(caps(DecompressionSupport::None)))
            .build()
            .expect("supported");
    c.transport().push_response_bytes(
        http::Response::builder()
            .header(http::header::CONTENT_ENCODING, "gzip")
            // Everything but the last eight bytes: gzip's CRC32 and length.
            .body(vec![Bytes::from_static(
                GZIP_BLOB.split_at(GZIP_BLOB.len() - 8).0,
            )])
            .unwrap(),
    );

    let err = get(&c).expect_err("a cut-off body must not read as a complete, shorter one");
    assert_eq!(*err.kind(), ErrorKind::Decode);
}

/// A body error that arrives while a coding is being reversed keeps the
/// category the backend gave it, and does not become `Decode`.
///
/// The seam is new — v0.2 W4's `Deadline` had to make the same promise one
/// layer down, and a fired total timeout reaching the caller as "the gzip
/// stream was corrupt" would be exactly the mislabelling `classify_body_
/// error` exists against. The frame list is deliberately EMPTY, so the
/// error arrives before the decoder has been fed anything at all: that is
/// the path where re-labelling would look most natural.
#[test]
fn an_error_from_underneath_keeps_its_category_through_the_decoder() {
    let c =
        Client::builder(MockTransport::new().with_capabilities(caps(DecompressionSupport::None)))
            .build()
            .expect("supported");
    c.transport().push_response_frames_then_error(
        http::Response::builder()
            .header(http::header::CONTENT_ENCODING, "gzip")
            .body(Vec::new())
            .unwrap(),
        http_ng::Error::new(ErrorKind::Cancelled, NotADecodeProblem),
    );

    let err = get(&c).expect_err("the body broke");
    assert_eq!(
        *err.kind(),
        ErrorKind::Cancelled,
        "the decoder must not relabel a failure that has nothing to do with it: {err:?}"
    );
}

#[derive(Debug, thiserror::Error)]
#[error("the connection went away")]
struct NotADecodeProblem;

/// `Content-Encoding` with no body at all — a HEAD response, a 204, a 304.
/// Running gzip's trailer check over zero bytes would turn each of them
/// into a spurious decode error, which is why an empty stream is not a
/// truncated one.
#[test]
fn an_empty_body_under_a_content_encoding_is_not_a_truncated_stream() {
    let c =
        Client::builder(MockTransport::new().with_capabilities(caps(DecompressionSupport::None)))
            .build()
            .expect("supported");
    c.transport().push_response_bytes(
        http::Response::builder()
            .status(204)
            .header(http::header::CONTENT_ENCODING, "gzip")
            .body(Vec::new())
            .unwrap(),
    );

    let got =
        futures_executor::block_on(async { c.head("https://a/x").send().await?.collect().await })
            .expect("a response with no body at all is not a broken stream");
    assert!(got.bytes().is_empty());
    assert_eq!(got.status(), 204);
}
