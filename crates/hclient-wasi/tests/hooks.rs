//! The two things about `WasiHttp`'s hook that need no host.
//!
//! The live half — what a real `wasi:http` exchange actually reports —
//! lives in `tests/live_roundtrip.rs`, and the split is not tidiness.
//! `just test-wasi` names `--test live_roundtrip`, and it is the only
//! recipe that runs with `wasmtime` installed; a live test in a file no
//! recipe names would print its `NOTICE` and report `ok` on every CI
//! runner, which is the defect `require_wasmtime` exists one level up to
//! stop.
//!
//! What is left here runs everywhere, including the CI legs with no
//! `wasmtime`: a source check that carries half the zero-cost claim, and
//! the capability whose value the missing `Reused` event depends on.
//!
//! `#![cfg(not(target_arch = "wasm32"))]`: the source check reads the
//! crate's own `src/` off disk, which a guest has no business doing.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

// ---------------------------------------------------------------------
// The half of the zero-cost claim a counting clock would otherwise carry.
// ---------------------------------------------------------------------

/// **`src/hooks.rs` is the only place this crate reads a clock**, checked
/// by walking `src/` rather than by reading it.
///
/// This is the second half of the zero-cost proof and it is not decoration.
/// `src/hooks.rs`'s own test asserts that `mark::<NoHooks>()` returns
/// `None` — that the clock read inside the closure does not happen. That
/// only says something about the *request path* if `mark` and `since` are
/// where the clock is read, and nowhere else is; `hclient-native` and
/// `hclient-h3` get that for free by counting reads through a `Timer` they
/// were handed, and `hclient-fetch` by replacing the browser's clock.
/// Neither is available here (see that module doc for the two that were
/// tried and measured to fail), so the claim is closed mechanically
/// instead.
///
/// A directory walk rather than `include_str!`: a new module added to
/// `src/` must be covered by this the day it is added, and an
/// `include_str!` list would silently not cover it.
///
/// The count is exact for the same reason `hclient-h3`'s clock counts are:
/// a tripwire that has to be looked at. Both reads are legitimate — the
/// mark that opens `Head::elapsed` and the one that closes it — and a
/// third means a decision was made.
///
/// The needle is `Instant::now` without parentheses, because one of the
/// two reads is `H::WATCHING.then(Instant::now)` — a function *reference*,
/// which is still a clock read and which a `now()` needle would miss. The
/// first version of this test used the parenthesised form, counted six,
/// and was measuring prose.
#[test]
fn the_clock_is_read_in_exactly_one_place() {
    const NEEDLE: &str = "Instant::now";
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut total = 0usize;
    let mut visited = 0usize;
    for entry in std::fs::read_dir(&src).expect("the crate has a src/ directory") {
        let path = entry.expect("readable directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        visited += 1;
        let is_hooks = path.file_name().and_then(|f| f.to_str()) == Some("hooks.rs");
        let text = std::fs::read_to_string(&path).expect("readable source file");
        let n = code_of(&text, is_hooks).matches(NEEDLE).count();
        if !is_hooks {
            assert_eq!(
                n,
                0,
                "{} reads a clock, and `src/hooks.rs` is supposed to be the \
                 only place that does — the zero-cost claim is that the read \
                 sits behind `H::WATCHING`, and a read outside that gate is \
                 not covered by it",
                path.display()
            );
        }
        total += n;
    }
    assert!(
        visited >= 4,
        "the walk must have found src/, not an empty directory"
    );
    assert_eq!(
        total, 2,
        "exactly two clock reads: the mark that starts `Head::elapsed` and \
         the one that closes it. A third is a decision that has to be made \
         on purpose, and changing this number is how it gets made"
    );
}

/// The shippable half of a source file: comments removed, and — for
/// `src/hooks.rs` — everything from its `#[cfg(test)]` module onwards.
///
/// Both exclusions are needed and both are the reason the first version of
/// the test above was wrong: `hooks.rs` *writes about* `Instant::now` at
/// length, and its own unit tests read a clock to check that `since`
/// returns something sane. Neither is on the request path.
///
/// Comments are cut at the first `//` on a line, which is naive about `//`
/// inside a string literal — `"http://…"` would be truncated. That can
/// only make this undercount on such a line, and no line in this crate
/// puts a clock read after a URL; the alternative is a Rust lexer for a
/// two-word needle.
fn code_of(text: &str, truncate_at_tests: bool) -> String {
    let body = match (truncate_at_tests, text.find("#[cfg(test)]")) {
        (true, Some(at)) => &text[..at],
        _ => text,
    };
    body.lines()
        .map(|l| l.split_once("//").map_or(l, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The declaration and the code are checked together, the same discipline
/// `wasi_declares_the_cancellation_it_performs` follows one file over.
///
/// `wasi:http` reports no connection reuse and `WasiHttp` declares
/// `ReuseSupport::None` for a measured reason. That is the same fact a
/// `Reused` event would carry, so the absence of the event and the value
/// of the field must not be able to drift apart: a future host that pooled
/// would move this field, and this line is where someone is made to
/// notice that `Reused` is still not emitted and to say why.
///
/// Not behind `require_wasmtime`: reading a capability involves no host.
#[test]
fn the_reuse_the_event_set_cannot_report_is_the_reuse_the_capability_denies() {
    use hclient_core::unversioned::Transport;
    assert_eq!(
        hclient_wasi::WasiHttp::new()
            .capabilities()
            .connection_reuse,
        hclient_core::ReuseSupport::None,
        "`Reused` has no emitter here partly because there is no reuse to \
         report; if this value ever moves, the event set has to be revisited \
         rather than left alone"
    );
}
