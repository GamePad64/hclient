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
//! # One direction, and the browser is why
//!
//! [`REVERSES`] is `false`. `URL.hostname` hands back the A-label
//! whatever went in, and no JS API anywhere does ToUnicode — not `Intl`,
//! not `URL`, not `URLPattern`. So [`crate::domain_to_unicode`] answers
//! `IdnError::NoImplementation` on this target, which is the honest
//! refusal rather than a punycode decoder of ours; a build that needs the
//! reverse turns on the `idna` feature and gets the tables, which is what
//! a forcing switch is for.

/// Nothing to carry: `URL` is a global.
#[derive(Debug)]
pub(crate) struct Web;

/// The name every backend module exports, so that `lib.rs` can select one
/// with `cfg_select!` and then name no operating system at all.
pub(crate) type Handle = Web;

/// **Forward only.** See the module docs: the browser has no ToUnicode.
pub(crate) const REVERSES: bool = false;

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
    // The scheme is ours and the trailing slash makes the authority
    // unambiguous even for an empty path — the same two consequences the
    // Apple backend was written under, because the shape is the same.
    let url = web_sys::Url::new(&format!("https://{domain}/")).ok()?;
    let host = url.hostname();
    // A host the parser did not like comes back empty rather than as an
    // error on some engines; `domain` is non-empty here, so an empty
    // answer is a refusal.
    if host.is_empty() {
        return None;
    }
    Some(host)
}

/// Unreachable: [`REVERSES`] is `false`, so `lib.rs` never calls this.
///
/// It exists because every backend module exports the same four items —
/// the alias in `lib.rs` names one module and nothing past that line
/// knows which — and a backend that answered here would have to prove it
/// against the acceptance probe like any other.
pub(crate) fn to_unicode(_w: &Web, _domain: &str) -> Option<String> {
    None
}
