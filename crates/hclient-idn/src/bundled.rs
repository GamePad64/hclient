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

impl Bundled {
    pub(crate) fn name(&self) -> &str {
        "idna (bundled Unicode tables, compiled_data)"
    }
}

/// Always `Some`: the tables are in the binary.
pub(crate) fn find() -> Option<Bundled> {
    Some(Bundled)
}

/// The A-label form of `domain`, or `None` if UTS 46 rejects it.
///
/// `AsciiDenyList::URL` rather than `EMPTY`, which is the same list
/// [`crate::is_forbidden_domain_byte`] states and the same one the other
/// backends apply by hand — they have to, because neither Foundation nor
/// ICU takes a deny list.
pub(crate) fn convert(_b: &Bundled, domain: &str) -> Option<String> {
    idna::domain_to_ascii_cow(domain.as_bytes(), idna::AsciiDenyList::URL)
        .ok()
        .map(Cow::into_owned)
}
