//! Windows: `icuuc.dll` through `windows-sys` (spec amendment C9).
//!
//! # Nothing is declared here, and that is the whole point
//!
//! `windows-sys` 0.61.2's `Win32_Globalization` already carries
//! `uidna_openUTS46`, `uidna_nameToASCII_UTF8`, `uidna_close`, the
//! `UIDNA` and `UIDNAInfo` types and every `UIDNA_*` constant —
//! **generated from Microsoft's own Win32 metadata**, not transcribed by
//! anyone here. So this file has no `extern` block and no function
//! signatures of its own; the `unsafe` left in it is only the three
//! calls, which is the smallest this boundary can be made.
//!
//! It also settles a question the ELF backend has to answer the hard way.
//! Windows' ICU is built with `U_DISABLE_RENAMING` — the SDK's `icu.h`
//! opens with `#define U_DISABLE_RENAMING 1` — so its exports are
//! **unsuffixed**, and `windows-sys` links the plain name, which could
//! not work otherwise. There is no version probing on this platform —
//! version-suffixed symbols are a fact about Linux, not about Windows.
//!
//! # What it costs, stated plainly rather than hedged
//!
//! `windows-link`'s `link!` expands to `#[link(kind = "raw-dylib")]`, an
//! ordinary **load-time** import. On a Windows with no `icuuc.dll` — 10
//! before 1703, and Server 2016, supported into 2027 — the process does
//! not fall back to anything, it fails to start, and it takes the
//! caller's whole HTTP client with it. This backend therefore states a
//! hard floor of **Windows 10 1703 / Server 2019** rather than degrading.
//! The ELF backend, which has no stable symbol to link and so must
//! resolve at run time anyway, gets the graceful behaviour for free; this
//! one trades it for a binding nobody has to write or maintain.
//!
//! # Never executed
//!
//! Every line here type-checks under `cargo clippy --target
//! x86_64-pc-windows-msvc`, confirmed with a planted `compile_error!`
//! rather than trusted to a warm cache. No part of it has ever run: there
//! is no Windows machine in the environment that produced it. What stands
//! behind it instead is `mod.rs`'s acceptance probe, which refuses to use
//! an ICU that does not answer `straße.de` correctly — so the first thing
//! that happens on a real Windows is a check of this file's output, and a
//! `Backend::None` rather than a wrong host if it is wrong.
#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C9
    reason = "calling the platform's UTS 46 is the only way to reach it; the declarations come from windows-sys, only the calls are here. See spec amendment C9"
)]

use super::{FIRST_TRY, U_BUFFER_OVERFLOW_ERROR, U_ZERO_ERROR, UIDNA_INFO_SIZE, accept, options};
use windows_sys::Win32::Globalization::{
    UErrorCode, UIDNA, UIDNAInfo, uidna_close, uidna_nameToASCII_UTF8, uidna_nameToUnicodeUTF8,
    uidna_openUTS46,
};

/// The generated struct, asserted against the size the removed ELF
/// backend's hand-written one also had — the two agreed field for field,
/// which is the only cross-check this layout ever got. If Microsoft's
/// metadata and ICU's header ever part company, the call is refused at
/// run time with `U_ILLEGAL_ARGUMENT_ERROR` on a machine nobody here can
/// test, and the acceptance gate turns that into a fallback.
const _: () = assert!(size_of::<UIDNAInfo>() == UIDNA_INFO_SIZE as usize);

/// `UIDNA_INFO_INITIALIZER`, which `windows-sys` does not generate: only
/// `size` matters going in, and the callee fills the rest.
fn info() -> UIDNAInfo {
    UIDNAInfo {
        size: UIDNA_INFO_SIZE,
        isTransitionalDifferent: 0,
        reservedB3: 0,
        errors: 0,
        reservedI2: 0,
        reservedI3: 0,
    }
}

/// Nothing to carry: the entry points are linked, not resolved, so there
/// is no handle and no library to keep alive. The type exists so that
/// `mod.rs` can treat both platforms the same way.
#[derive(Debug)]
pub(crate) struct Icu;

impl Icu {
    pub(crate) fn name(&self) -> &str {
        "icuuc.dll (windows-sys, load-time import)"
    }
}

/// Always `Some`: if `icuuc.dll` were missing, this process would not
/// have started (see the module docs). The acceptance probe in `mod.rs`
/// still runs, and is what decides whether the ICU behind that import is
/// usable.
pub(crate) fn find() -> Option<Icu> {
    Some(Icu)
}

/// Closes the `UIDNA` handle on every exit path, including the early
/// returns below.
struct Handle(*mut UIDNA);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `uidna_openUTS46` and was checked
        // non-null before this type was constructed. This type is the
        // only owner, is not `Clone`, and the pointer is never written
        // after construction, so `uidna_close` runs exactly once on it.
        unsafe { uidna_close(self.0) }; // unsafe-code-exception: amendment-C9
    }
}

/// The two ICU entry points have one signature, so the conversion below
/// is written once and handed whichever is wanted.
///
/// `uidna_nameToASCII_UTF8` and `uidna_nameToUnicodeUTF8` are declared by
/// `windows-sys` from Microsoft's own metadata, so this alias transcribes
/// nothing: it names a shape the generator produced.
type Entry = unsafe extern "C" fn(
    // unsafe-code-exception: amendment-C9
    *const UIDNA,
    windows_sys::core::PCSTR,
    i32,
    windows_sys::core::PCSTR,
    i32,
    *mut UIDNAInfo,
    *mut UErrorCode,
) -> i32;

fn through(entry: Entry, domain: &str) -> Option<String> {
    let len = i32::try_from(domain.len()).ok()?;

    let mut status = U_ZERO_ERROR;
    // SAFETY: `status` is a live local and `options()` is a plain
    // integer. The call returns either a null pointer with `status` set,
    // or an owned handle this function is responsible for closing.
    let handle = unsafe { uidna_openUTS46(options(), &raw mut status) }; // unsafe-code-exception: amendment-C9
    if status > U_ZERO_ERROR || handle.is_null() {
        return None;
    }
    // Bind the guard immediately, before anything that can return.
    let handle = Handle(handle);

    let mut buf = vec![0u8; FIRST_TRY];
    // Two attempts at most: the first at `FIRST_TRY`, the second at
    // exactly the length ICU reported. A third would mean ICU asked for a
    // length and then did not fit in it.
    for _ in 0..2 {
        let capacity = i32::try_from(buf.len()).ok()?;
        let mut info = info();
        let mut status = U_ZERO_ERROR;
        // SAFETY: `handle.0` is a live handle from `uidna_openUTS46`.
        // `domain` is a `&str`, so its pointer is valid for `len` bytes
        // and `len` is its exact length — no NUL terminator is involved,
        // because the length is passed explicitly. `buf` is a live `Vec`
        // of `capacity` bytes and `capacity` is its exact length. `info`
        // and `status` are live locals. The callee retains none of them.
        //
        // `dest` is `PCSTR` — a CONST pointer — in the Win32 metadata,
        // for an out-parameter that ICU writes through. That is a quirk
        // of the generated binding, not of the C API, whose `dest` is
        // `char *`; the cast restores what the callee actually gets. The
        // buffer is a fresh `Vec` owned here, so writing through it is
        // sound.
        let written = unsafe {
            // unsafe-code-exception: amendment-C9
            entry(
                handle.0,
                domain.as_ptr(),
                len,
                buf.as_mut_ptr(),
                capacity,
                &raw mut info,
                &raw mut status,
            )
        };

        if status == U_BUFFER_OVERFLOW_ERROR {
            let needed = usize::try_from(written).ok()?;
            if needed <= buf.len() {
                // ICU reported an overflow at a length that already fits.
                // Nothing sensible left to try.
                return None;
            }
            buf = vec![0u8; needed];
            continue;
        }
        return accept(&buf, written, info.errors, status);
    }
    None
}

/// The A-label form, through `uidna_nameToASCII_UTF8`.
pub(crate) fn to_ascii(_icu: &Icu, domain: &str) -> Option<String> {
    through(uidna_nameToASCII_UTF8, domain)
}

/// The U-label form, through `uidna_nameToUnicodeUTF8`.
///
/// **The same ICU, the same options and the same error mask**, which is
/// the whole reason the reverse direction is the platform's here rather
/// than a punycode decoder of ours: a name ICU will not convert back is
/// one it did not consider legal going out either, and asking it twice
/// keeps the two answers on one implementation.
pub(crate) fn to_unicode(_icu: &Icu, domain: &str) -> Option<String> {
    through(uidna_nameToUnicodeUTF8, domain)
}
