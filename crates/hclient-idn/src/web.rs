//! The browser's own URL parser, as a backend.
//!
//! **The difference from Foundation is that the standard says so.** Both
//! are reached by building a URL and reading its host back. `NSURL`
//! converts as an undocumented side effect of parsing; the WHATWG URL
//! Standard *defines* host parsing as UTS 46 with named parameters, so
//! what an engine gets right here it gets right on purpose — the mapping,
//! the punycode encoding, non-transitional processing, CheckBidi and
//! ContextJ, which is the half that needs the tables.
//!
//! # What it does not do is engine-dependent, and that was measured once
//!
//! The backend was written against 38 corpus rows in headless **Firefox**,
//! where 37 answered what `idna` answers and the one that did not was the
//! empty name. That number was then written down as *the browser*, and
//! there are two: **Chrome answers six of those rows differently**, four
//! of them because `new URL()` does not validate an ACE label there —
//! `xn--zzzz.test`, `xn--a.de`, `xn--.de` and `xn--a-.de` come back
//! unchanged where Firefox and `idna` refuse them — and two because it
//! percent-encodes a host it will not take rather than refusing it.
//! It reached `main` and the `browser (chrome)` job caught it.
//!
//! So this file supplies [`crate::ace`] **in full**, exactly as
//! `apple.rs` does, and the sentence that had this backend costing one
//! line against Apple's sixty-five is gone with the measurement behind
//! it. What survives is the rule rather than the number: **the OS carries
//! the tables, and a backend supplies whatever of UTS 46 the platform's
//! entry point leaves out** — nothing for `icuuc.dll` and ICU4J, and for
//! a URL parser the ASCII half, whose size is the engine's business and
//! not knowable from one of them.
//!
//! The four rows are all-ASCII inputs, which is what makes them cheap to
//! repair and what made them invisible in one engine: `ace::to_ascii_over`
//! answers an all-ASCII name without asking the parser anything, so the
//! engine is only ever handed a name containing non-ASCII — and on those
//! the two engines and `idna` agree, row for row.
//!
//! # What it saves
//!
//! The largest share this crate has anywhere, because a browser module
//! has almost nothing else in it. Measured through the full `wasm-pack`
//! pipeline — `opt-level = "z"`, fat LTO, `panic = "abort"`, strip,
//! `wasm-opt -O` — on one program that converts one name:
//!
//! | build | `_bg.wasm` |
//! |---|---|
//! | this backend | **20.7 KiB** |
//! | the bundled tables | **143.0 KiB** |
//!
//! **The share is the durable figure and the bytes are not.** These read
//! 17.4 against 159.1 for as long as the backend called the engine
//! directly, and both numbers moved for two reasons at once: `ace` is
//! 5.7 KiB of this build (measured by building the same program with the
//! parser called directly, 15.0 KiB), and the rest is a pipeline nothing
//! pins — `wasm-opt`'s flags, the toolchain, `idna`'s own release. The
//! saving was 89% and is **86%**, which is the number to quote.
//!
//! Measure after `wasm-bindgen`, not before: the raw `.wasm` has a custom
//! section of descriptors that the shim generator consumes and nothing
//! ships, and it made the browser build look 158 KiB *larger* than the
//! one carrying ICU.
//!
//! # It supplies one direction, and one direction is all there is
//!
//! `URL.hostname` hands back the A-label whatever went in, and no JS API
//! anywhere does ToUnicode — not `Intl`, not `URL`, not `URLPattern`.
//! That was this backend's one narrowness while the crate had a reverse
//! direction, and it is nobody's now: the crate converts U to A and
//! stops, because that is the only direction an HTTP client needs.

// **`wasm-bindgen` and not `web-sys`, and the reason is one crate up.**
// `web-sys` would give this backend `Url` for free and costs
// `hclient-proto` its sans-io property: `web-sys` pulls `js-sys`, whose
// optional `futures-core-03-stream` feature is switched on elsewhere in
// any graph that also has `hclient-fetch`, and Cargo unifies features.
// `graph-proto-sans-io` caught it — *this crate must stay sans-io on
// every target, not just the host* — which is the guard doing exactly
// what it is for.
//
// The whole of what is needed is a constructor and a getter, so they are
// declared here. `wasm-bindgen` alone pulls nothing asynchronous.
#[wasm_bindgen]
unsafe extern "C" {
    // unsafe-code-exception: amendment-C20
    /// The `URL` global. `catch` because the constructor throws on a
    /// string it will not parse, which is the refusal this backend reads.
    #[wasm_bindgen(js_name = URL)]
    type Url;

    #[wasm_bindgen(constructor, js_class = "URL", catch)]
    fn new(url: &str) -> Result<Url, JsValue>;

    /// The authority's host, which for an IDN is the A-label — this is
    /// the conversion, and it is the only thing this file wants.
    #[wasm_bindgen(method, getter)]
    fn hostname(this: &Url) -> String;
}

use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;

/// Nothing to carry: `URL` is a global.
#[derive(Debug)]
pub(crate) struct Web;

/// The name every backend module exports, so that `lib.rs` can select one
/// with `cfg_select!` and then name no operating system at all.
pub(crate) type Handle = Web;

/// Always `Some`: `URL` is in every browsing context this target runs in,
/// windows and workers alike, and a build for it that had no `URL` would
/// have nothing to run in.
pub(crate) fn find() -> Option<Web> {
    Some(Web)
}

/// The A-label form, out of `new URL(..).hostname`.
///
/// **Three steps, and only the middle one is the browser's**, which is
/// `apple.rs`'s sentence verbatim because the shape is the same: a URL
/// parser maps and converts a Unicode host and does not answer for the
/// ASCII half of UTS 46. So the case folding and the ACE check come out
/// of [`crate::ace`] and the conversion is asked of the engine.
pub(crate) fn to_ascii(_w: &Web, domain: &str) -> Option<String> {
    // Before the parser, never after, and for the reason `apple.rs`
    // records under its consequence 1: a denied byte here is one the
    // parser would consume as a delimiter, so `ex@ämple.com` comes back
    // as `xn--mple-hva.com` — a *different origin* returned as a clean
    // success. `ascii_labels_survived` cannot catch it, because the
    // label that lost the `ex@` is the non-ASCII one it does not compare.
    //
    // The cost is a narrow strictness: a forbidden byte that UTS 46's
    // mapping *removes* is refused here where `idna` answers, which is
    // the `">\u{338}"` case the fuzzer found. Refusing is the safe
    // direction and it is the same trade the Apple backend makes.
    if domain.bytes().any(crate::is_forbidden_domain_byte) {
        return None;
    }

    // Everything but the conversion is `crate::ace`'s. The sequence lives
    // there so that it runs on a host with no browser — with `idna` as
    // the stand-in — which is what puts a test on the arithmetic under
    // the four rows Chrome gets wrong.
    crate::ace::to_ascii_over(engine, domain)
}

/// The engine's answer alone, with nothing of this crate's around it.
///
/// Reached from [`crate::testing`] so that `tests/web_corpus.rs` can say
/// *what the browser answered* and *what a caller gets* as two different
/// claims. The gap between them is [`crate::ace`], and measuring it is
/// the whole point: it is one row in Firefox and six in Chrome, and a
/// backend written against either number alone is wrong in the other
/// engine. A second `URL` binding in the test would have measured a copy
/// of this rather than this.
pub(crate) fn engine(domain: &str) -> Option<String> {
    // The scheme is ours and the trailing slash makes the authority
    // unambiguous even for an empty path — the same two consequences the
    // Apple backend was written under, because the shape is the same.
    let url = Url::new(&format!("https://{domain}/")).ok()?;
    let host = url.hostname();
    // A host the parser did not like comes back empty rather than as an
    // error on some engines; `domain` is non-empty here, so an empty
    // answer is a refusal.
    if host.is_empty() {
        return None;
    }
    Some(host)
}
