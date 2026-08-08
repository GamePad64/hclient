//! The platform's UTS 46, behind one interface and two very different
//! backends — and the policy that decides whether to trust either.
//!
//! **Nothing in this file is `unsafe`, and nothing in it may become so.**
//! `forbid` is not usable here (it propagates into the child modules,
//! which are the crate's foreign-function boundaries), so the crate
//! root's `deny` stands — and CI's `unsafe-code-policy.sh` path-scopes
//! the `amendment-C9` marker to `icu/windows.rs` and `icu/elf.rs` alone,
//! so an `unsafe` block added HERE fails the build exactly as it would in
//! any other crate. That split is the point of the file: the two backends
//! do nothing but call C, and every decision about whether to believe the
//! answer lives here, in safe code.
//!
//! # Why the two backends are not the same shape
//!
//! They differ in the one thing that matters for an HTTP client — what
//! happens on a machine that has no ICU.
//!
//! - **Windows (`windows.rs`)** links `icuuc.dll` through `windows-sys`,
//!   whose declarations are generated from Microsoft's own Win32
//!   metadata. Windows' ICU is built with `U_DISABLE_RENAMING`, so the
//!   exports are unsuffixed and there is a stable name to link against.
//!   That makes the binding free, and it makes the dependency
//!   **load-time**: a Windows without `icuuc.dll` — 10 before 1703, and
//!   Server 2016 — does not fall back, it fails to start the process.
//!   That is the trade this backend accepts, and it is the reason the
//!   floor is stated as Windows 10 1703 rather than hedged.
//! - **ELF unixes (`elf.rs`)** cannot do that: ICU's symbols there ARE
//!   version-suffixed (`uidna_openUTS46_78`) and so is the soname, so
//!   there is nothing stable to link. It resolves at run time through
//!   `libloading` and reports "not found" as an ordinary `None`.
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

#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(not(windows), path = "elf.rs")]
mod imp;

pub(crate) use imp::Icu;

/// ICU's `UErrorCode`. Zero is success, negative values are warnings,
/// positive values are errors — the sign is the contract, not a
/// convention.
pub(crate) type UErrorCode = i32;

pub(crate) const U_ZERO_ERROR: UErrorCode = 0;

/// `U_BUFFER_OVERFLOW_ERROR`. The one error worth retrying: the return
/// value is then the length the output needs.
pub(crate) const U_BUFFER_OVERFLOW_ERROR: UErrorCode = 15;

/// The `UIDNAInfo.size` field is load-bearing on both platforms:
/// `uidna_nameToASCII_UTF8` compares it against its own
/// `sizeof(UIDNAInfo)` and returns `U_ILLEGAL_ARGUMENT_ERROR` if they
/// differ. That turns a layout disagreement with a future ICU into a
/// clean refusal — and then, through the acceptance probe, into a
/// fallback — rather than a write past the end of the struct.
///
/// The struct itself is NOT declared here, because the two backends have
/// different and better sources for it: Windows uses `windows-sys`'
/// `UIDNAInfo`, generated from Microsoft's Win32 metadata, and `elf.rs`
/// declares the same six fields by hand. That they agree field for field
/// is the one cross-check available for a layout nobody here can run —
/// and each file asserts this size against its own struct.
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
pub(crate) fn accept(buf: &[u8], written: i32, errors: u32, status: UErrorCode) -> Option<String> {
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
fn answers_the_trap_correctly(icu: &Icu) -> bool {
    const PROBES: [(&str, &str); 2] = [
        ("straße.de", "xn--strae-oqa.de"),
        ("faß.de", "xn--fa-hia.de"),
    ];
    PROBES
        .iter()
        .all(|(input, want)| imp::convert(icu, input).as_deref() == Some(*want))
}

/// The option word, re-exported for the backends so neither reaches past
/// this module for it.
pub(crate) const fn options() -> u32 {
    OPTIONS
}
