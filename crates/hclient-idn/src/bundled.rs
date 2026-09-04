//! The bundled `idna` crate, as a backend like any other.
//!
//! This is what a target with no system UTS 46 gets — the ELF unixes and
//! wasm — and what `--features idna` forces everywhere else. The call is
//! the exact one `hclient-proto` used to make itself, so a build that
//! resolves here answers what that crate answered before it took this
//! one: byte for byte, measured that way rather than argued.
//!
//! **It goes through [`crate::policy`] like the other three, and it was
//! the one that did not.** It called `idna::domain_to_ascii_cow` directly,
//! so on Linux and wasm the shared layer never ran — which is the ICU
//! path's own argument left unapplied: *the alternative is two statements
//! of one contract and the newer one is always the one that rots.*
//!
//! What that cost was the divergence this crate exists to prevent.
//! `ä..de` converted here and was refused by Foundation, so the same host
//! was reachable on Linux and Windows and not on macOS — and the
//! empty-label rule added to the policy fixed nothing until this path
//! joined it, because the rule was in a layer this path skipped. Measured,
//! not assumed: the `uri_resolution` corpus stayed green on Linux across
//! that change, which is what said the layer was being bypassed.
//!
//! # Its handle is a marker, and the probe still runs
//!
//! There is nothing to find and nothing to keep alive, so [`Handle`] is a
//! unit like Foundation's. The acceptance probe in [`crate::backend`] runs
//! over this backend anyway, which is a change from the scheme where the
//! bundled path was trusted by construction: `idna` is this crate's own
//! oracle, so it will pass, and running it removes a special case rather
//! than adding a check. A build whose `idna` answered the transitional
//! pair would be one this crate should refuse on every platform alike.

use std::borrow::Cow;

/// Nothing to carry: the tables are compiled in.
#[derive(Debug)]
pub(crate) struct Bundled;

/// The name every backend module exports, so that `lib.rs` can select one
/// with `cfg_select!` and then name no platform at all.
pub(crate) type Handle = Bundled;

/// Always `Some`: the tables are in the binary.
pub(crate) fn find() -> Option<Bundled> {
    Some(Bundled)
}

/// The A-label form of `domain`, or `None` if UTS 46 rejects it.
///
/// **`AsciiDenyList::EMPTY`, and the deny list is applied one level up.**
/// `idna` is the only one of the four backends that takes such a list at
/// all — neither Foundation nor ICU has one — so applying it here would
/// leave two backends without it and the third with it, which is how
/// `a<b.com` came to be refused on Linux and answered on Windows and
/// macOS. [`crate::domain_to_ascii`] applies
/// [`crate::is_forbidden_domain_byte`] to the converted name for every
/// backend alike.
///
/// Passing `URL` here as well would be harmless and is deliberately not
/// done: a second copy of the rule is a second place for it to stop
/// agreeing, and it would make the shared check unkillable by any test on
/// the one platform this workspace runs.
pub(crate) fn to_ascii(_b: &Bundled, domain: &str) -> Option<String> {
    idna::domain_to_ascii_cow(domain.as_bytes(), idna::AsciiDenyList::EMPTY)
        .ok()
        .map(Cow::into_owned)
}

/// The U-label form, through `idna::domain_to_unicode`.
///
/// That function hands back an answer *and* a `Result`, and the answer is
/// a best effort even when the errors are non-empty — so the errors are
/// what decides here, not the string. This crate has no bit mask to
/// forgive them with on this path, unlike the two ICU backends: `idna`
/// reports an opaque `Errors`, so any error at all is a refusal, which is
/// the safe direction and the one the round-trip in
/// `policy::to_unicode_over` would enforce anyway.
pub(crate) fn to_unicode(_b: &Bundled, domain: &str) -> Option<String> {
    let (unicode, result) = idna::domain_to_unicode(domain);
    result.ok().map(|()| unicode)
}
