//! Uploading a file — `multipart/form-data`, and the contract it hands you.
//!
//! ```text
//! cargo run -p hclient --example file_upload --features test-util
//! ```
//!
//! # The shape is decided by one requirement: a multipart body must stream
//!
//! Concatenating every part into one `Bytes` is four lines and is wrong
//! for the case multipart exists for — a file large enough that a second
//! copy of it is the thing that fails. So a form is a sequence of parts
//! and the body is written as it goes.
//!
//! # What that costs you, and how to know before you send
//!
//! **The replay contract is read off the parts rather than set by a
//! flag**, and it is knowable before the first attempt, which is
//! `RetryKind`'s whole promise:
//!
//! - every part resolved to bytes → a rewindable body, and a
//!   `Content-Length`, so a retry or a `425 Too Early` can send it again;
//! - any part a stream → the body is single-pass, `RetryKind::Impossible`,
//!   and no policy overrides that.
//!
//! There is no flag to opt into retries, because a flag would be a promise
//! this module could not keep for a stream it has already handed to a
//! transport. The way to opt in is to give the parts bytes — which is what
//! this example does, by reading the file first.
//!
//! # The boundary
//!
//! 128 bits from the OS, drawn **after** the caller supplied the content
//! and once per form. There is deliberately **no collision check**: a
//! streaming part's content is unreadable before it is sent, so a scan
//! could only cover the buffered parts — a guarantee for some inputs,
//! which reads as a guarantee for all of them and is worse than an honest
//! probability. The probability is the argument, and it rests on the draw
//! coming last: an adversary choosing a file cannot choose it to contain a
//! value that does not exist yet.
//!
//! An entropy failure is an **error**, never a fixed fallback. A degraded
//! value is only acceptable when the degradation has a direction, and a
//! constant boundary is the single string most likely to appear in
//! someone's content.
// **The gate is on the items, not on the file.** `#![cfg(..)]` at the
// top compiles the whole file away when the feature is off — `fn main`
// with it — and cargo then reports `main` function not found, which is
// what `just test-no-default` caught. `no_tls_no_resolver.rs` had the
// shape right already: gate the body and leave a `main` that says what
// is missing.
#[cfg(feature = "test-util")]
mod demo {

    use hclient::Client;
    use hclient::mock::MockTransport;
    use hclient::multipart::{Form, Part};

    pub fn run() {
        let transport = MockTransport::new();
        transport.push_response(
            http::Response::builder()
                .status(201)
                .body("stored")
                .unwrap(),
        );

        let client = Client::builder(transport.clone())
            .base_url("https://example.test".parse().unwrap())
            .build()
            .expect("nothing here needs a capability the mock lacks");

        // Stand in for `std::fs::read("report.csv")`. Reading it here rather
        // than streaming it is the choice that makes the body replayable —
        // see above.
        let file: Vec<u8> = b"date,total\n2026-09-05,41\n".to_vec();

        let form = Form::new()
            // An ordinary field.
            .part(Part::text("kind", "daily"))
            // The file. `file_name` is what puts `filename="..."` in the part's
            // `Content-Disposition`, and it is what makes a server treat this
            // as an upload rather than a field.
            //
            // **Names go out as UTF-8 with three bytes escaped** — LF, CR and
            // `"` — which is the WHATWG rule all three browser engines moved
            // to, and every other C0 control is rejected outright. That is a
            // framing property before an interoperability one: a raw CR LF in
            // caller data would end the header field and let the rest be read
            // as further part headers.
            //
            // There is no `filename*`: RFC 7578 §4.2 forbids it in as many
            // words.
            .part(
                Part::bytes("file", file)
                    .file_name("report.csv")
                    .mime("text/csv"),
            );

        let body = futures_executor::block_on(async {
            client
                .post("/uploads")
                .multipart(form)
                .send()
                .await?
                .collect()
                .await?
                .text()
        })
        .expect("scripted");

        assert_eq!(body, "stored");

        let sent = transport.requests();
        let req = &sent[0];

        // `multipart` sets the header, and the boundary in it is the one the
        // body was written with — the two cannot disagree, because the form
        // draws it once and both read that draw.
        let ct = req.headers["content-type"].to_str().unwrap();
        assert!(ct.starts_with("multipart/form-data; boundary="));

        // **The replay contract, read off the parts and knowable before the
        // first byte went out.** Both parts were bytes, so the body can be
        // sent again — which is what lets a retry policy, or a `425 Too
        // Early`, send it. Give any part a stream and this becomes
        // `RetryKind::Impossible`, and no policy overrides that.
        assert_eq!(req.retry_kind, hclient_core::RetryKind::ViaFactory);

        // **The length is not visible from here, and the reason is the mock
        // keeping its own rule.** A form of byte parts has an exact size, and
        // a real transport turns that into `Content-Length` — but reading it
        // means running the factory, and this double records only what it can
        // record *without calling anything*. So `body_size_hint` is `None`:
        // not "no length", but "not askable without becoming a second
        // caller". A faithful model of a backend, rather than something that
        // masks the defect under test.
        assert_eq!(req.body_size_hint, None);

        // Rewindable and still not reducible to bytes, which is the design
        // rather than a gap: a multipart body is written as it goes, so
        // `snapshot()` has nothing to hand back whole. That is the whole
        // reason a form is a sequence of parts and not one `Bytes`.
        assert!(req.body.snapshot().is_none());

        println!("{ct}");
        println!("retry_kind: {:?}", req.retry_kind);
        println!(
            "body size as the mock may report it: {:?}",
            req.body_size_hint
        );
    }
}

#[cfg(feature = "test-util")]
fn main() {
    demo::run();
}

#[cfg(not(feature = "test-util"))]
fn main() {
    eprintln!("this example needs `--features test-util`");
}
