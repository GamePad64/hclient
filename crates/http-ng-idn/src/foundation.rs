//! Apple: UTS 46 through Foundation's URL parser.
//!
//! # The only platform backend with no spec amendment
//!
//! **Nothing here is `unsafe`, and that is not an accident of style — it
//! is measured.** Every `objc2-foundation` call this module makes is a
//! safe function; a draft that wrapped them in `unsafe` blocks produced
//! ten `unnecessary unsafe block` warnings from `cargo check --target
//! aarch64-apple-darwin`. So this file needs no `unsafe-code-exception`
//! marker, no entry in `unsafe-code-policy.sh`, and no amendment in the
//! spec, where `icu/windows.rs` needs all three (C9).
//!
//! That is the cheapest reason to keep it. `objc2` has done the work that
//! would otherwise be ours: message sends, retain/release, and the
//! nullability of every return are encoded in its types, so what is left
//! here is ordinary Rust over `Option`.
//!
//! # Why this is not just "call `URL(string:)`"
//!
//! Foundation converts to A-labels as a **side effect of parsing a whole
//! URL**, and that is a different shape from `uidna_nameToASCII_UTF8`.
//! Four consequences, each handled rather than hoped away:
//!
//! 1. **A host can change where the URL ends.** Hand Foundation
//!    `ex@ample.com` and the authority parses as userinfo `ex` plus host
//!    `ample.com`; `ex/ample.com` gives host `ex` and a path. Either is a
//!    wrong origin returned as a clean success. Every input in that family
//!    carries a byte from the WHATWG forbidden-domain set — `@ / ? # : [
//!    ] \ %`, space and the controls — so [`crate::is_forbidden_domain_byte`]
//!    over the INPUT rejects all of them before Foundation sees anything.
//!
//!    Note this is the opposite call from the ICU backend, where the same
//!    input scan was measured to be redundant and deleted: ICU is handed
//!    a host and a length and parses no URL, so a denied byte could only
//!    survive into the output, where the output scan already caught it.
//!    Here the denied byte is precisely what *disappears* — consumed as a
//!    delimiter — so the input scan is the only thing that can see it.
//!
//! 2. **The scheme is ours, not the caller's.** The string handed to
//!    Foundation is always `https://{domain}/`, built here. A caller
//!    cannot smuggle `file:` or a relative reference through, because
//!    nothing of theirs reaches the parser except the authority, and that
//!    has already passed the deny list.
//!
//! 3. **A failure carries no reason.** ICU fills a `UIDNAInfo.errors`
//!    word; Foundation returns `nil` for the whole URL. So this backend
//!    can only ever produce [`crate::IdnError::NotAnIdn`], never a more
//!    specific one. Recorded rather than papered over: the error type is
//!    shared with a backend that *could* say more.
//!
//! 4. **Which getter returns the A-label is settled by a test, not by
//!    this comment.** `NSURL::host` and `NSURLComponents::host` /
//!    `percentEncodedHost` do not agree — Apple's documentation for
//!    Swift's `URL.host(percentEncoded:)` describes the `true` case as
//!    returning the *less* decoded form, which for an IDN is the opposite
//!    of what the name suggests. Nobody here has a macOS machine, so this
//!    module picks [`NSURL::host`] and
//!    `tests/differential.rs::macos_getter_that_returns_the_a_label`
//!    asserts on `macos-latest` which getter actually does. If the choice
//!    is wrong, that test names the right one — and meanwhile the
//!    acceptance gate in `lib.rs` refuses a backend that cannot answer
//!    `straße.de`, so a wrong getter degrades to `Backend::None` rather
//!    than to a wrong host.
//!
//! # Never executed here
//!
//! Every line type-checks under `cargo check --target
//! aarch64-apple-darwin`, confirmed with a planted `compile_error!`
//! rather than trusted to a warm cache. No part of it has run: there is
//! no Apple machine in the environment that produced it. The workspace
//! `test` job's `macos-latest` leg is what runs it, on every push.

use objc2_foundation::{NSString, NSURL};

/// Nothing to carry: Foundation is part of the OS and is linked, so there
/// is no handle and no library to keep alive. The type exists so the
/// backend has the same shape as the ICU one.
#[derive(Debug)]
pub(crate) struct Foundation;

impl Foundation {
    pub(crate) fn name(&self) -> &str {
        "Foundation NSURL (objc2-foundation, safe bindings)"
    }
}

/// Always `Some`: Foundation cannot be absent on an Apple target. The
/// acceptance gate in `lib.rs` is what decides whether it is *usable*.
pub(crate) fn find() -> Option<Foundation> {
    Some(Foundation)
}

/// The A-label form of `domain`, or `None` if Foundation would not parse
/// it — which, for the reasons in the module docs, is the only failure
/// signal there is.
pub(crate) fn convert(_f: &Foundation, domain: &str) -> Option<String> {
    // Consequence 1: before the parser, never after. A denied byte here
    // is one Foundation would consume as a delimiter, silently changing
    // which host comes back.
    if domain.bytes().any(crate::is_forbidden_domain_byte) {
        return None;
    }
    // Consequence 2: the scheme is ours. The trailing slash makes the
    // authority unambiguous even for an empty path.
    let text = NSString::from_str(&format!("https://{domain}/"));
    let url = NSURL::URLWithString(&text)?;
    let host = url.host()?;
    let host = host.to_string();
    // Foundation is a URL parser, so a host it did not like can come back
    // as something that is not a host at all. The output must still be
    // ASCII and must still be free of denied bytes; `to_string` on an
    // `NSString` is lossless, so anything non-ASCII here means the
    // conversion did not happen.
    if !host.is_ascii() || host.bytes().any(crate::is_forbidden_domain_byte) {
        return None;
    }
    Some(host)
}
