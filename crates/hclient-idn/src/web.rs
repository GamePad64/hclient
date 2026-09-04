//! The browser's own URL parser, as a backend.
//!
//! **The difference from Foundation is that the standard says so.** Both
//! are reached by building a URL and reading its host back, and that shape
//! is what removed the Apple backend — `NSURL` converts as an undocumented
//! side effect of parsing, and got eight rows of the differential corpus
//! wrong. The WHATWG URL Standard instead *defines* host parsing as UTS 46
//! with named parameters, and the run agrees with the specification: 37 of
//! the same 38 rows answer what `idna` answers, measured in Firefox,
//! headless, before this file existed.
//!
//! The one row is the empty name. `idna` answers `Some("")`; `https:///`
//! is not a URL a browser will parse, so it is special-cased here in one
//! line rather than papered over — the difference in scale from Apple's
//! sixty-five is the whole reason this backend exists and that one does
//! not.
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
//! | this backend | **17.4 KiB** |
//! | the bundled tables | **159.1 KiB** |
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
pub(crate) fn to_ascii(_w: &Web, domain: &str) -> Option<String> {
    // The one corpus row a URL parser cannot express: `idna` answers the
    // empty name with itself, and `https:///` does not parse. One line
    // rather than a layer, which is the measurement that separates this
    // backend from the one Foundation could not sustain.
    if domain.is_empty() {
        return Some(String::new());
    }
    engine(domain)
}

/// The engine's answer alone, with nothing of this crate's around it.
///
/// Reached from [`crate::testing`] so that `tests/web_corpus.rs` can say
/// *what the browser answered* and *what a caller gets* as two different
/// claims — the corpus diverges from `idna` on one row and the public
/// entry point on none, and the difference between those two numbers is
/// the line above. A second binding in the test would have measured a
/// copy of this rather than this.
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
