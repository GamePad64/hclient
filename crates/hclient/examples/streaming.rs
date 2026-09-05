//! Reading a body without holding it all — `chunk()` and `lines()`.
//!
//! The front page's two-line example ends in `collect()`, and `collect` is
//! the point at which a caller says *read all of it into memory*. This is
//! the other answer, and it is the one an NDJSON feed, a log tail or a
//! multi-gigabyte download needs.
//!
//! ```text
//! cargo run -p hclient --example streaming --features test-util
//! ```
//!
//! # Why `collect()` is a step at all
//!
//! Because a response arrives as a stream and this client does not decide
//! for you. `Collected` is what you get after asking; `Response` is what
//! the transport handed over, and it yields frames as they arrive. The
//! bound on the first is [`ClientBuilder::response_limit`]; the bound on
//! the second is whatever you do with each chunk.
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

    pub fn run() {
        let transport = MockTransport::new();

        // Three frames on one response — a body that arrives in pieces, which
        // is what makes the difference between the two loops below visible.
        transport.push_response_frames(
            http::Response::builder()
                .status(200)
                .body(vec!["one two ", "three ", "four"])
                .unwrap(),
        );
        // NDJSON: one object per line, and the line breaks do not line up with
        // the frame boundaries — which is exactly why `lines()` exists rather
        // than a `split` over each chunk.
        transport.push_response_frames(
            http::Response::builder()
                .status(200)
                .body(vec!["{\"n\":1}\n{\"n\":", "2}\n{\"n\":3}", "\n"])
                .unwrap(),
        );

        let client = Client::builder(transport)
            .base_url("https://example.test".parse().unwrap())
            .build()
            .expect("nothing configured that the mock refuses");

        futures_executor::block_on(async {
            // ── frame by frame ────────────────────────────────────────────
            //
            // `chunk()` hands back the next frame, or `None` at the end. What
            // arrives is what the transport produced: no reassembly, no
            // buffering, and nothing held after you have used it.
            let mut resp = client.get("/blob").send().await.expect("scripted");
            let mut frames = 0usize;
            let mut bytes = 0usize;
            while let Some(chunk) = resp.chunk().await {
                let chunk = chunk.expect("frames");
                frames += 1;
                bytes += chunk.len();
            }
            assert_eq!(frames, 3, "three frames in, three frames out");
            assert_eq!(bytes, "one two three four".len());
            println!("chunk(): {frames} frames, {bytes} bytes");

            // ── line by line ──────────────────────────────────────────────
            //
            // `lines()` re-frames the stream: it joins across chunk boundaries
            // and splits on newlines, so a line split over two frames arrives
            // whole. That is the case a `split('\n')` per chunk gets wrong,
            // and the reason this is on `Response` rather than in a caller.
            //
            // It is bounded — `DEFAULT_MAX_LINE` — because a stream with no
            // newline in it is otherwise a way to spend all the memory there
            // is.
            let resp = client.get("/feed").send().await.expect("scripted");
            let mut lines = resp.lines();
            let mut seen = Vec::new();
            while let Some(line) = lines.next().await {
                seen.push(line.expect("well formed"));
            }
            assert_eq!(seen, ["{\"n\":1}", "{\"n\":2}", "{\"n\":3}"]);
            let text: Vec<String> = seen
                .iter()
                .map(|l| String::from_utf8_lossy(l).into_owned())
                .collect();
            println!("lines(): {text:?}");
        });
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
