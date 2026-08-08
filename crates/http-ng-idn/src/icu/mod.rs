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

use crate::{OPTIONS, is_fatal};
use std::sync::OnceLock;

#[path = "windows.rs"]
mod imp;

pub(crate) use imp::Icu;

/// ICU's `UErrorCode`. Zero is success, negative values are warnings,
/// positive values are errors — the sign is the contract, not a
/// convention.
pub(crate) const U_ZERO_ERROR: i32 = 0;

/// `U_BUFFER_OVERFLOW_ERROR`. The one error worth retrying: the return
/// value is then the length the output needs.
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
pub(crate) const UIDNA_INFO_SIZE: i16 = 16;

/// The initial output buffer. A domain needing more than this is not an
/// error — the call is repeated at the length ICU asks for.
pub(crate) const FIRST_TRY: usize = 256;

/// Whether ICU's answer, and the `errors` word beside it, may be used.
///
/// Shared by both backends so that "what counts as a usable answer"
/// cannot come to mean two things. `written` is the length C reported and
/// is checked rather than trusted, which is the same line
/// `http-ng-dns-system::sys::classify_written` draws.
pub(crate) fn accept(buf: &[u8], written: i32, errors: u32, status: i32) -> Option<String> {
    if status > U_ZERO_ERROR || is_fatal(errors) {
        return None;
    }
    let written = usize::try_from(written).ok()?;
    let out = buf.get(..written)?;
    String::from_utf8(out.to_vec()).ok()
}

/// The ICU this process will use, or `None` where there is none to use.
///
/// Searched once, and the search includes the acceptance probe below, so
/// a `Some` here means "an ICU that has demonstrably answered the
/// transitional pair correctly", not "some symbols resolved".
///
/// # The `filter` below is not covered by any test, and cannot be
///
/// Measured, not assumed: deleting `.filter(answers_the_trap_correctly)`
/// leaves all of this crate's tests green. Do not read that as dead code.
///
/// The gate's *content* is covered — corrupt `PROBES` so it expects the
/// transitional answer and it rejects a working library, `backend()` stops
/// reporting `SystemIcu`, and the assertion on `backend()` goes red. What
/// nothing covers is its *presence*, because killing that mutation needs a
/// machine where ICU is **present but wrong**: `open` failing without COM,
/// a layout drift, an ICU that ignores `OPTIONS`. No runner offers it —
/// Linux has a working ICU 78.2, macOS has none at all, and
/// `windows-latest` has a working one. Unlike the version-suffixed symbol
/// name, a Windows runner does **not** close this; no environment does.
///
/// It stays because it changes behaviour in the failure mode that costs
/// most: without it, a broken ICU reports every good domain invalid, one
/// at a time, and the user is told their ordinary domain is malformed.
/// With it, that ICU is simply not used and the bundled path answers.
///
/// Making it killable would need an injection point for a deliberately
/// lying `Icu` — a test seam in the one file that must stay free of
/// `unsafe` and keep the whole policy in safe code. That price is higher
/// than the mutation is likely.
pub(crate) fn library() -> Option<&'static Icu> {
    static LIBRARY: OnceLock<Option<Icu>> = OnceLock::new();
    LIBRARY
        .get_or_init(|| imp::find().filter(answers_the_trap_correctly))
        .as_ref()
}

/// Which ICU answered, for a report that names it rather than saying "an
/// ICU" — `libicuuc.so.78 [uidna_openUTS46_78]`, `icuuc.dll (windows-sys,
/// load-time import)`.
pub(crate) fn library_name() -> Option<&'static str> {
    library().map(Icu::name)
}

/// Converts through the accepted ICU, or `None` if there is none.
pub(crate) fn name_to_ascii(domain: &str) -> Option<String> {
    imp::convert(library()?, domain)
}

/// The library is not accepted because its symbols resolved — it is
/// accepted because it **answers the trap input correctly**.
///
/// Three failures collapse into this one check, and all three would
/// otherwise be silent or wrong:
///
/// - **`uidna_openUTS46` failing at run time.** Microsoft documents
///   `CoInitializeEx` as a prerequisite for Win32 apps using the split
///   `icuuc.dll`/`icuin.dll`, waived on 1903+ with the combined
///   `icu.dll`. Nothing here calls it, and whether it is needed is
///   **unverified** — no Windows machine produced this crate. Without
///   this probe, an `open` that failed would surface one name at a time
///   as `IdnError::NotAnIdn`: the client telling a user their perfectly
///   good domain is invalid. With it, the ICU is simply not used.
/// - **An ABI or layout disagreement** with some future ICU: caught here
///   rather than at the first request.
/// - **A transitional answer.** If `OPTIONS` were ever wrong, or an ICU
///   ignored it, this rejects the library outright rather than letting it
///   resolve a different origin. The two inputs are the exact pair the
///   whole crate turns on.
///
/// The cost is two conversions, once per process.
///
/// # No test kills the deletion of this gate, and that is not an oversight
///
/// Measured rather than assumed: replacing `imp::find().filter(
/// answers_the_trap_correctly)` with `imp::find` leaves all 13 tests
/// passing. It has to. Every machine this suite runs on has a *working*
/// ICU, so the gate never rejects anything, and a guard that never fires
/// cannot be observed by removing it.
///
/// What is killed, and what that pins down:
///
/// - changing a [`PROBES`] expectation to the transitional answer reddens
///   `the_platform_column_is_not_silently_empty`, so the gate is
///   demonstrably *consulted* and its contents are load-bearing;
/// - [`accepts`] has unit tests over a fake conversion, so its policy
///   (all probes not any; a refusal is a failure) is pinned independently
///   of any real ICU.
///
/// So the call site is proven live and the policy is proven correct; only
/// "someone deletes the whole thing" is invisible. Making that visible
/// needs an injection point handing the loader a deliberately wrong ICU,
/// which would exist for no other purpose and would be the only caller of
/// its own seam. Written down here instead — in a project where tests
/// that cannot fail are the dominant defect, an untested guard that
/// *looks* tested is worse than one that says so.
fn answers_the_trap_correctly(icu: &Icu) -> bool {
    accepts(|input| imp::convert(icu, input))
}

/// The two inputs an ICU has to get right, and what "right" is.
///
/// They are the pair the entire crate turns on: a transitional ICU
/// answers `strasse.de` and `fass.de` here, which are different origins,
/// registrable by different people.
const PROBES: [(&str, &str); 2] = [
    ("straße.de", "xn--strae-oqa.de"),
    ("faß.de", "xn--fa-hia.de"),
];

/// The gate's *policy*, over any conversion function.
///
/// Split out from [`answers_the_trap_correctly`] for one reason: a
/// function that only takes a real `Icu` can only be tested by owning a
/// broken ICU, and nobody here does. Over a closure it is four
/// exhaustively testable cases — the same move `http-ng-dns-system` makes
/// with `sys::classify_written`.
///
/// **Every probe must pass, not any**: an ICU that gets `straße.de` right
/// and `faß.de` wrong is not one to trust with the rest.
fn accepts(convert: impl Fn(&str) -> Option<String>) -> bool {
    PROBES
        .iter()
        .all(|(input, want)| convert(input).as_deref() == Some(*want))
}

/// The option word, re-exported for the backends so neither reaches past
/// this module for it.
pub(crate) const fn options() -> u32 {
    OPTIONS
}

#[cfg(test)]
mod tests {
    use super::{PROBES, accepts};

    /// A conversion that answers correctly is accepted.
    #[test]
    fn a_correct_icu_is_accepted() {
        assert!(accepts(|input| PROBES
            .iter()
            .find(|(i, _)| *i == input)
            .map(|(_, want)| (*want).to_owned())));
    }

    /// **The case the gate exists for.** A transitional ICU converts both
    /// probes successfully, to the wrong origin — no error, no null, just
    /// a different host. It must be refused.
    #[test]
    fn a_transitional_icu_is_refused() {
        assert!(!accepts(|input| match input {
            "straße.de" => Some("strasse.de".to_owned()),
            "faß.de" => Some("fass.de".to_owned()),
            _ => None,
        }));
    }

    /// Right on one probe, wrong on the other. `all`, not `any` — if this
    /// passed, one correct answer would vouch for an ICU wrong everywhere
    /// else.
    #[test]
    fn getting_only_one_probe_right_is_not_enough() {
        assert!(!accepts(|input| match input {
            "straße.de" => Some("xn--strae-oqa.de".to_owned()),
            _ => Some("fass.de".to_owned()),
        }));
    }

    /// A conversion that fails outright — `uidna_openUTS46` refusing for
    /// want of `CoInitializeEx` looks exactly like this — is refused too,
    /// rather than read as "nothing to object to".
    #[test]
    fn an_icu_that_cannot_convert_at_all_is_refused() {
        assert!(!accepts(|_| None));
    }

    /// The probes are the transitional pair and nothing else has crept
    /// in. If one were softened to a name both flavours agree on, the
    /// gate would still be consulted, would still pass, and would no
    /// longer check the one thing it is for.
    #[test]
    fn the_probes_are_the_inputs_the_two_flavours_disagree_on() {
        assert_eq!(PROBES.len(), 2);
        assert!(PROBES.iter().all(|(_, want)| want.starts_with("xn--")));
        assert!(PROBES.iter().any(|(i, _)| *i == "straße.de"));
        assert!(PROBES.iter().any(|(i, _)| *i == "faß.de"));
    }
}
