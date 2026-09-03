//! The platform's UTS 46, behind one interface and two very different
//! backends — and the policy that decides whether to trust either.
//!
//! **Nothing in this file is `unsafe`, and nothing in it may become so.**
//! `forbid` is not usable here (it propagates into the child modules,
//! which are the crate's foreign-function boundaries), so the crate
//! root's `deny` stands — and CI's `unsafe-code-policy.sh` path-scopes
//! the `amendment-C9` marker to `icu/windows.rs` alone, so an `unsafe`
//! block added HERE fails the build exactly as it would in any other
//! crate. That split is the point of the file: the backend does nothing
//! but call C, and every decision about whether to believe the answer
//! lives here, in safe code.
//!
//! # One backend, and the rule that makes it the only one
//!
//! **Static linking against an ABI whose version comes with the OS.**
//! `windows.rs` links `icuuc.dll` through `windows-sys`, whose
//! declarations are generated from Microsoft's own Win32 metadata.
//! Windows' ICU is built with `U_DISABLE_RENAMING`, so the exports are
//! unsuffixed and there is a stable name to link against; `windows-link`
//! emits a `raw-dylib` import that the *linker* resolves. Nothing in this
//! crate calls `LoadLibrary` or `dlopen`.
//!
//! The cost is that the import is load-time: a Windows without
//! `icuuc.dll` — 10 before 1703, and Server 2016 — does not fall back,
//! the process fails to start with `STATUS_DLL_NOT_FOUND` and no mention
//! of IDN. The floor is stated rather than degraded to.
//!
//! **An ELF backend lived here and was removed on purpose.** It reached
//! `libicuuc.so.NN` with `dlopen`, because both the soname and every
//! symbol carry the ICU major version (`uidna_openUTS46_78`) and there is
//! nothing stable to link. It worked — the corpus was validated against a
//! real ICU 78.2 through it, which is how the option word and the error
//! mask in `lib.rs` were established — but what it returned on any given
//! machine was whatever ICU that machine happens to carry, on a Unicode
//! version nobody chose and nothing reports. For IDN a Unicode difference
//! is a different host, so that is a correctness risk accepted for a size
//! saving, on the one platform where the version is not ours to know.
//! Linux and the other ELF unixes take the bundled tables now.
//!
//! # The acceptance probe, which is shared and lives here
//!
//! Neither backend is trusted because its symbols exist. Both are trusted
//! only after answering the transitional pair correctly — see
//! [`answers_the_trap_correctly`]. That check is the same on both
//! platforms, is pure policy, and would be the first thing to rot if each
//! backend carried its own copy.

// **Compiled everywhere, read in full by one backend.** This module is
// ICU's vocabulary — the option bits, the error mask, the acceptance rule
// — and it is unconditional so that those rules are tested on every host
// rather than only on a machine that has an ICU. Only the ICU4C backend
// reads all of it: Android takes `OPTIONS` and nothing else, because
// ICU4J reports its errors as an `EnumSet` that cannot be masked, and
// Foundation and the bundled `idna` read none of it. So the dead code is
// by construction rather than by oversight, and the gate names the one
// backend that leaves nothing unread — which is what keeps a genuinely
// unused item here catchable on Windows.
#![cfg_attr(
    not(icu_backend),
    allow(
        dead_code,
        reason = "ICU's vocabulary is compiled on every target so its rules are tested there; \
                  only the ICU4C backend reads all of it"
    )
)]

#[path = "windows.rs"]
#[cfg(icu_backend)]
mod imp;

#[cfg(icu_backend)]
pub(crate) use imp::Icu;

// ── ICU's vocabulary, and this crate's settings over it ────────────────
//
// **These moved here from `lib.rs`, and the second ICU backend is why.**
// They were the crate root's while Windows was the only caller;
// `android.rs` reads the same option word and forgives the same errors,
// because `android.icu.text.IDNA` *is* ICU — ICU4J rather than ICU4C, the
// same bits under the same names. A vocabulary two backends share belongs
// to the thing they share rather than to whichever named it first.
//
// **So this module is unconditional and only its binding is gated.** The
// constants and the error policy compile on every target, which is what
// lets `android.rs`'s test pin its enum names against `IGNORED_ERRORS` on
// a Linux host — and what gating the module on `icu_backend` would take
// away, since Android is not that.

// ── The six ICU option bits, from `unicode/uidna.h` ─────────────────────
//
// Spelled out rather than imported, because the crate that would supply
// them does not exist: `rust_icu_sys` 5.7.0's `BINDGEN_SOURCE_MODULES`
// does not include `uidna`, and its function allow-list has no
// `uidna_.*` — checked in the published source, not inferred.

/// `UIDNA_USE_STD3_RULES`. **Deliberately not set**: it restricts ASCII to
/// letters/digits/hyphen, which rejects the underscore that appears in
/// `_dmarc`-style pseudo-hosts, while *allowing* `%` and `<` — so it is
/// neither a superset nor a subset of the WHATWG deny list this crate has
/// to reproduce. `is_forbidden_domain_byte` does that job instead.
#[allow(
    dead_code,
    reason = "documented as not set; named so the reader can see it was considered"
)]
pub(crate) const UIDNA_USE_STD3_RULES: u32 = 0x0002;

/// `UIDNA_CHECK_BIDI`. `idna` applies CheckBidi unconditionally — it is
/// not one of its configurable flags — so this must be on to match.
#[cfg_attr(
    idna_backend,
    allow(
        dead_code,
        reason = "the UTS 46 settings this crate asks for are its own statement and are \
                  documented as such; only the ICU and Android backends read them, so a \
                  build whose backend is the bundled crate has no reader — gating them away \
                  would make the crate's settings a thing you have to pick a target to see"
    )
)]
pub(crate) const UIDNA_CHECK_BIDI: u32 = 0x0004;

/// `UIDNA_CHECK_CONTEXTJ`. Same story: `idna`'s CheckJoiners is always
/// true, so ZWJ/ZWNJ context rules have to be on here too.
#[cfg_attr(
    idna_backend,
    allow(
        dead_code,
        reason = "the UTS 46 settings this crate asks for are its own statement and are \
                  documented as such; only the ICU and Android backends read them, so a \
                  build whose backend is the bundled crate has no reader — gating them away \
                  would make the crate's settings a thing you have to pick a target to see"
    )
)]
pub(crate) const UIDNA_CHECK_CONTEXTJ: u32 = 0x0008;

/// `UIDNA_NONTRANSITIONAL_TO_ASCII`. **The bit this whole crate turns on.**
#[cfg_attr(
    idna_backend,
    allow(
        dead_code,
        reason = "the UTS 46 settings this crate asks for are its own statement and are \
                  documented as such; only the ICU and Android backends read them, so a \
                  build whose backend is the bundled crate has no reader — gating them away \
                  would make the crate's settings a thing you have to pick a target to see"
    )
)]
pub(crate) const UIDNA_NONTRANSITIONAL_TO_ASCII: u32 = 0x0010;

/// `UIDNA_NONTRANSITIONAL_TO_UNICODE`. Set together with its sibling: this
/// crate only converts to ASCII today, but a handle opened with a
/// half-transitional option set would answer differently the moment
/// anything calls `nameToUnicode` on it, and that is not a difference
/// worth leaving lying around.
#[cfg_attr(
    idna_backend,
    allow(
        dead_code,
        reason = "the UTS 46 settings this crate asks for are its own statement and are \
                  documented as such; only the ICU and Android backends read them, so a \
                  build whose backend is the bundled crate has no reader — gating them away \
                  would make the crate's settings a thing you have to pick a target to see"
    )
)]
pub(crate) const UIDNA_NONTRANSITIONAL_TO_UNICODE: u32 = 0x0020;

/// `UIDNA_CHECK_CONTEXTO`. **Deliberately not set**: UTS 46 makes ContextO
/// optional and `idna` does not implement it, so setting it would reject
/// names the bundled path accepts.
#[allow(
    dead_code,
    reason = "documented as not set; named so the reader can see it was considered"
)]
pub(crate) const UIDNA_CHECK_CONTEXTO: u32 = 0x0040;

/// The option word passed to `uidna_openUTS46`. `0x3c`.
///
/// **`UIDNA_DEFAULT` is 0 and 0 is transitional** — see the crate docs for
/// the measurement. Four bits, each justified on its own constant above.
///
/// **Independently arrived at by Apple, which is the best corroboration
/// available for a number like this.** `swift-foundation`'s
/// `Sources/FoundationInternationalization/URLParser+ICU.swift` (`struct
/// UIDNAHookICU`) opens its own handle with
/// `UIDNA_CHECK_BIDI | UIDNA_CHECK_CONTEXTJ |
/// UIDNA_NONTRANSITIONAL_TO_UNICODE | UIDNA_NONTRANSITIONAL_TO_ASCII` —
/// the same four bits, in a URL parser solving the same problem, written
/// by people with no connection to this one.
///
/// They then diverge from us on the *errors*, which is exactly why
/// `IGNORED_ERRORS` is a separate decision and not a footnote:
/// `shouldAllow(_:encodeToASCII: true)` sets `allowedErrors = 0`, so
/// Apple's `URL` refuses a name on any bit at all, the six this crate
/// masks included.
///
/// **`a..b` and `ab--cd.com` are NOT examples of this**, and
/// `macos-latest` measures both as accepted. The reason is one level up
/// and is the whole of `foundation.rs`'s first
/// section: those two are all-ASCII, so Foundation's IDNA hook never runs
/// on them and there is no `errors` word to be strict about. The
/// divergence is real, but its inputs are names with a non-ASCII label
/// *and* one of those defects — `-münchen.de`, `münchen..de`,
/// `ab--cd.münchen`.
#[cfg_attr(
    idna_backend,
    allow(
        dead_code,
        reason = "the UTS 46 settings this crate asks for are its own statement and are \
                  documented as such; only the ICU and Android backends read them, so a \
                  build whose backend is the bundled crate has no reader — gating them away \
                  would make the crate's settings a thing you have to pick a target to see"
    )
)]
pub(crate) const OPTIONS: u32 = UIDNA_NONTRANSITIONAL_TO_ASCII
    | UIDNA_NONTRANSITIONAL_TO_UNICODE
    | UIDNA_CHECK_BIDI
    | UIDNA_CHECK_CONTEXTJ;

// ── The `UIDNAInfo.errors` bits this crate ignores ──────────────────────
//
// ICU has no options for CheckHyphens or VerifyDnsLength: it always runs
// both checks and reports them as bits. `idna`, called the way
// `hclient-proto` calls it, has both OFF. So agreement is not a matter of
// options alone — the six bits below have to be masked out of ICU's
// answer, or every one of `a..b`, `-lead.de`, `ab--cd.com`, a 64-byte
// label and a 254-byte name would be rejected by the platform path and
// accepted by the bundled one.

pub(crate) const UIDNA_ERROR_EMPTY_LABEL: u32 = 0x0001;
pub(crate) const UIDNA_ERROR_LABEL_TOO_LONG: u32 = 0x0002;
pub(crate) const UIDNA_ERROR_DOMAIN_NAME_TOO_LONG: u32 = 0x0004;
pub(crate) const UIDNA_ERROR_LEADING_HYPHEN: u32 = 0x0008;
pub(crate) const UIDNA_ERROR_TRAILING_HYPHEN: u32 = 0x0010;
pub(crate) const UIDNA_ERROR_HYPHEN_3_4: u32 = 0x0020;

/// The `UIDNAInfo.errors` bits that do **not** make a name unusable here.
///
/// The first three are _VerifyDnsLength=false_, the last three are
/// _CheckHyphens=false_. Nothing else is masked: a disallowed code point,
/// a leading combining mark, bad punycode, a dot inside a decoded label,
/// an invalid ACE label, a bidi violation or a ContextJ violation all
/// stand, because `idna` rejects all of those too.
#[cfg_attr(
    idna_backend,
    allow(
        dead_code,
        reason = "the UTS 46 settings this crate asks for are its own statement and are \
                  documented as such; only the ICU and Android backends read them, so a \
                  build whose backend is the bundled crate has no reader — gating them away \
                  would make the crate's settings a thing you have to pick a target to see"
    )
)]
pub(crate) const IGNORED_ERRORS: u32 = UIDNA_ERROR_EMPTY_LABEL
    | UIDNA_ERROR_LABEL_TOO_LONG
    | UIDNA_ERROR_DOMAIN_NAME_TOO_LONG
    | UIDNA_ERROR_LEADING_HYPHEN
    | UIDNA_ERROR_TRAILING_HYPHEN
    | UIDNA_ERROR_HYPHEN_3_4;

/// Whether ICU's `UIDNAInfo.errors` word means "reject this name".
///
/// Lives here, in safe tested code, and not in `icu.rs`, for the same
/// reason `sys::classify_written` lives outside the FFI file in
/// `hclient-dns-system`: this is a *decision about what the answer means*,
/// and it is worth more as ordinary Rust with tests around it than as one
/// more line inside an `unsafe` block.
#[must_use]
#[cfg_attr(
    idna_backend,
    allow(
        dead_code,
        reason = "the UTS 46 settings this crate asks for are its own statement and are \
                  documented as such; only the ICU and Android backends read them, so a \
                  build whose backend is the bundled crate has no reader — gating them away \
                  would make the crate's settings a thing you have to pick a target to see"
    )
)]
pub(crate) const fn is_fatal(errors: u32) -> bool {
    errors & !IGNORED_ERRORS != 0
}

// **ICU4C's vocabulary, and only Windows speaks it.** A `UErrorCode`, a
// `UIDNAInfo` size and a first-try buffer are facts about the C API;
// ICU4J reports a `Set<IDNA.Error>` and needs none of them. So this group
// is gated where the shared bits above are not, which is the line between
// *what ICU decides* and *how one binding asks it*.
#[cfg(icu_backend)]
pub(crate) const U_ZERO_ERROR: i32 = 0;

/// `U_BUFFER_OVERFLOW_ERROR`. The one error worth retrying: the return
/// value is then the length the output needs.
#[cfg(icu_backend)]
pub(crate) const U_BUFFER_OVERFLOW_ERROR: i32 = 15;

/// The `UIDNAInfo.size` field is load-bearing on both platforms:
/// `uidna_nameToASCII_UTF8` compares it against its own
/// `sizeof(UIDNAInfo)` and returns `U_ILLEGAL_ARGUMENT_ERROR` if they
/// differ. That turns a layout disagreement with a future ICU into a
/// clean refusal — and then, through the acceptance probe, into a
/// fallback — rather than a write past the end of the struct.
///
/// The struct itself is NOT declared here: `windows.rs` uses
/// `windows-sys`' `UIDNAInfo`, generated from Microsoft's Win32 metadata,
/// which is a better source than anything transcribed by hand. The
/// removed ELF backend declared the same six fields itself, and that the
/// two agreed field for field was the one cross-check available for a
/// layout nobody here can run; the constant survives that check, and
/// `windows.rs` asserts its own struct against it.
#[cfg(icu_backend)]
pub(crate) const UIDNA_INFO_SIZE: i16 = 16;

/// The initial output buffer. A domain needing more than this is not an
/// error — the call is repeated at the length ICU asks for.
#[cfg(icu_backend)]
pub(crate) const FIRST_TRY: usize = 256;

/// Whether ICU's answer, and the `errors` word beside it, may be used.
///
/// Shared by both backends so that "what counts as a usable answer"
/// cannot come to mean two things. `written` is the length C reported and
/// is checked rather than trusted, which is the same line
/// `hclient-dns-system::sys::classify_written` draws.
#[cfg(icu_backend)]
pub(crate) fn accept(buf: &[u8], written: i32, errors: u32, status: i32) -> Option<String> {
    if status > U_ZERO_ERROR || is_fatal(errors) {
        return None;
    }
    let written = usize::try_from(written).ok()?;
    let out = buf.get(..written)?;
    String::from_utf8(out.to_vec()).ok()
}
/// The three names every backend module exports, so that `lib.rs` can
/// select one with `cfg_select!` and then name no platform at all.
///
/// **The `OnceLock` and the acceptance probe moved out of this module**
/// when the four backends were given one shape. They were here because
/// this was the only backend that had to *find* anything; they are in
/// `lib.rs` now because all four have to be gated on answering the
/// transitional pair correctly, and one gate is what keeps them from
/// drifting into four.
#[cfg(icu_backend)]
pub(crate) use imp::{find, to_ascii, to_unicode};

/// The handle this backend hands back, under the name every backend uses.
#[cfg(icu_backend)]
pub(crate) type Handle = Icu;

/// The option word, re-exported for the backends so neither reaches past
/// this module for it.
#[cfg(icu_backend)]
pub(crate) const fn options() -> u32 {
    OPTIONS
}

#[cfg(test)]
mod vocabulary_tests {
    use super::*;

    #[test]
    fn the_option_word_is_non_transitional_and_nothing_is_transitional_about_it() {
        assert_eq!(
            OPTIONS, 0x3c,
            "OPTIONS changed; the bits are 0x10|0x20|0x04|0x08"
        );
        assert_ne!(
            OPTIONS, 0,
            "UIDNA_DEFAULT is 0 and 0 is transitional — see the crate docs"
        );
        assert_eq!(
            OPTIONS & UIDNA_NONTRANSITIONAL_TO_ASCII,
            UIDNA_NONTRANSITIONAL_TO_ASCII,
            "the one bit that decides whether `straße.de` becomes `strasse.de` or `xn--strae-oqa.de`"
        );
        assert_eq!(
            OPTIONS & UIDNA_NONTRANSITIONAL_TO_UNICODE,
            UIDNA_NONTRANSITIONAL_TO_UNICODE
        );
        assert_eq!(OPTIONS & UIDNA_CHECK_BIDI, UIDNA_CHECK_BIDI);
        assert_eq!(OPTIONS & UIDNA_CHECK_CONTEXTJ, UIDNA_CHECK_CONTEXTJ);
        assert_eq!(
            OPTIONS & UIDNA_USE_STD3_RULES,
            0,
            "STD3 is a different set from the WHATWG deny list, not a stricter one"
        );
        assert_eq!(
            OPTIONS & UIDNA_CHECK_CONTEXTO,
            0,
            "`idna` does not implement ContextO, so neither may this"
        );
    }

    #[test]
    fn the_ignored_error_bits_are_exactly_check_hyphens_and_verify_dns_length() {
        assert_eq!(IGNORED_ERRORS, 0x3f);
        // VerifyDnsLength=false.
        assert!(!is_fatal(UIDNA_ERROR_EMPTY_LABEL));
        assert!(!is_fatal(UIDNA_ERROR_LABEL_TOO_LONG));
        assert!(!is_fatal(UIDNA_ERROR_DOMAIN_NAME_TOO_LONG));
        // CheckHyphens=false.
        assert!(!is_fatal(UIDNA_ERROR_LEADING_HYPHEN));
        assert!(!is_fatal(UIDNA_ERROR_TRAILING_HYPHEN));
        assert!(!is_fatal(UIDNA_ERROR_HYPHEN_3_4));
        // Everything else stands. 0x0040 LEADING_COMBINING_MARK, 0x0080
        // DISALLOWED, 0x0100 PUNYCODE, 0x0200 LABEL_HAS_DOT, 0x0400
        // INVALID_ACE_LABEL, 0x0800 BIDI, 0x1000 CONTEXTJ, 0x2000/0x4000
        // CONTEXTO.
        for bit in [
            0x0040, 0x0080, 0x0100, 0x0200, 0x0400, 0x0800, 0x1000, 0x2000, 0x4000,
        ] {
            assert!(is_fatal(bit), "0x{bit:04x} must not be masked away");
        }
        assert!(!is_fatal(0), "a clean answer is a clean answer");
    }
}
