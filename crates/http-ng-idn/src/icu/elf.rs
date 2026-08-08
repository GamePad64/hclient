//! ELF unixes: `libicuuc.so.NN`, resolved at run time (spec amendment C9).
//!
//! # Why this cannot be a `#[link]`, when the Windows backend is one
//!
//! **ICU's exported symbols here are version-suffixed, and so is the
//! soname.** Measured on the machine that produced this file — `nm -D
//! --defined-only /usr/lib/x86_64-linux-gnu/libicuuc.so.78` lists
//! `uidna_openUTS46_78`, `uidna_nameToASCII_UTF8_78` and `uidna_close_78`,
//! and nothing unsuffixed. The library that exports `_78` on one machine
//! exports `_74` on the next, and a build configured with
//! `-DU_DISABLE_RENAMING=1` exports the bare name. There is no stable
//! symbol to link against, which is exactly why `rust_icu_sys` runs
//! `pkg-config` and `bindgen` *at build time* — it resolves the version
//! of the machine doing the building. For a client library that has to
//! run on machines it was not built on, that is the wrong answer.
//!
//! So: open the library by name, then look for the entry points under the
//! suffix the file name implies, then unsuffixed, then every suffix in
//! range. On a miss that costs one failed `dlopen` per candidate — up to
//! 55, since the version is part of the *file* name here — which is a few
//! hundred `stat` calls, once per process.
//!
//! The upside of having to do it: a machine with no ICU gets an ordinary
//! `None` and the caller falls back, where the Windows backend's
//! load-time import would have failed to start the process at all.
//!
//! # The loading itself is not ours
//!
//! `libloading` does the `dlopen`/`dlsym` half, with its error handling
//! and its `Send`/`Sync` reasoning. 467M downloads, one transitive
//! dependency. What it does not remove is the `unsafe`: `Library::new`
//! and `Library::get` are both unsafe, because loading runs initialisers
//! and because the caller asserts each symbol's type. Those assertions
//! are the three signatures below, transcribed from `unicode/uidna.h` —
//! and cross-checked against `windows-sys`' generated declarations of the
//! same three functions, which agree field for field.
#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C9
    reason = "ICU's exported symbols are version-suffixed here, so the platform's UTS 46 cannot be reached by linking; see spec amendment C9"
)]

use super::{
    FIRST_TRY, U_BUFFER_OVERFLOW_ERROR, U_ZERO_ERROR, UErrorCode, UIDNA_INFO_SIZE, accept, options,
};
use core::ffi::{c_char, c_void};

/// `UIDNAInfo` from `unicode/uidna.h`, declared by hand because there is
/// no metadata to generate it from on this platform.
///
/// Cross-checked against a source that IS generated: `windows-sys`
/// 0.61.2's `UIDNAInfo` is `{ i16, i8, i8, u32, i32, i32 }`, field for
/// field the same. `windows.rs` uses that one directly.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct UIdnaInfo {
    size: i16,
    is_transitional_different: i8,
    reserved_b3: i8,
    errors: u32,
    reserved_i2: i32,
    reserved_i3: i32,
}

impl UIdnaInfo {
    fn new() -> Self {
        Self {
            size: UIDNA_INFO_SIZE,
            is_transitional_different: 0,
            reserved_b3: 0,
            errors: 0,
            reserved_i2: 0,
            reserved_i3: 0,
        }
    }
}

/// If this fires, the struct above stopped matching C's `UIDNAInfo` and
/// every call would be refused at run time. Better to hear about it here.
const _: () = assert!(size_of::<UIdnaInfo>() == UIDNA_INFO_SIZE as usize);

/// `UIDNA *uidna_openUTS46(uint32_t options, UErrorCode *pErrorCode);`
type UidnaOpenUts46 = unsafe extern "C" fn(u32, *mut UErrorCode) -> *mut c_void; // unsafe-code-exception: amendment-C9

/// `void uidna_close(UIDNA *idna);`
type UidnaClose = unsafe extern "C" fn(*mut c_void); // unsafe-code-exception: amendment-C9

/// ```c
/// int32_t uidna_nameToASCII_UTF8(const UIDNA *idna,
///                                const char *name, int32_t length,
///                                char *dest, int32_t capacity,
///                                UIDNAInfo *pInfo, UErrorCode *pErrorCode);
/// ```
/// The UTF-8 entry point, so there is no UTF-16 round trip in either
/// direction.
type UidnaNameToAsciiUtf8 = unsafe extern "C" fn(
    // unsafe-code-exception: amendment-C9
    *const c_void,
    *const c_char,
    i32,
    *mut c_char,
    i32,
    *mut UIdnaInfo,
    *mut UErrorCode,
) -> i32;

/// `uidna_openUTS46` first appeared in ICU 4.6, whose symbol suffix is
/// `_46`. Below that there is nothing to find.
const MIN_SUFFIX: u32 = 46;

/// ICU 78 is current as this is written; the headroom is for the machine
/// this runs on in five years, not for this build.
const MAX_SUFFIX: u32 = 99;

/// The three entry points, and the library they came from.
///
/// The `libloading::Library` is kept in the same struct as the pointers
/// taken out of it, and is never dropped: this type only ever exists
/// inside the `OnceLock` in `mod.rs`, and a `OnceLock` in a `static` is
/// not dropped at exit. Unloading it while the function pointers were
/// still reachable is the one way to invalidate them, and there is no
/// code path that can.
#[derive(Debug)]
pub(crate) struct Icu {
    _lib: libloading::Library,
    name: String,
    open: UidnaOpenUts46,
    close: UidnaClose,
    name_to_ascii: UidnaNameToAsciiUtf8,
}

impl Icu {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// Candidate file names, most specific first.
///
/// **macOS is absent, for one reason and not the reason first written
/// down.** `libicucore.dylib` — Apple's own ICU — is private: no headers
/// ship for it, Apple documents its symbols as not for third-party
/// linking, and since Big Sur it is not on disk to `dlopen` at all. What
/// does *not* stand is the claim that macOS has no public IDN API:
/// Foundation's `URL(string:)` punycodes a host, and
/// `swift-foundation`'s `URLParser+ICU.swift` opens its handle with the
/// same four bits this crate uses. It is reachable only as a side effect
/// of a whole URL parse, with no error channel and no flag control, so
/// this crate does not use it — see the crate docs. Apple targets are
/// excluded from this backend at the manifest level too, so on macOS the
/// `platform` feature resolves to the bundled tables.
fn candidates() -> Vec<(String, Option<u32>)> {
    // The soname carries the version, so opening `libicuuc.so.78` already
    // tells us to look for `_78` first. The bare `libicuuc.so` is the
    // development symlink; it is tried first because when it exists it is
    // one call instead of fifty.
    let mut v = vec![("libicuuc.so".to_owned(), None)];
    v.extend(
        (MIN_SUFFIX..=MAX_SUFFIX)
            .rev()
            .map(|n| (format!("libicuuc.so.{n}"), Some(n))),
    );
    v
}

pub(crate) fn find() -> Option<Icu> {
    for (name, hint) in candidates() {
        // SAFETY: `Library::new` is unsafe because loading a library runs
        // its initialisers, which can do anything. These names are built
        // from compile-time constants naming the platform's own ICU, not
        // from anything derived from input; the process would load the
        // same file if it linked against it.
        let Ok(lib) = (unsafe { libloading::Library::new(&name) }) else {
            // unsafe-code-exception: amendment-C9
            continue;
        };
        if let Some(found) = resolve(lib, name, hint) {
            return Some(found);
        }
    }
    None
}

/// Finds the three entry points in an already-open library, trying the
/// suffix the file name implied, then no suffix at all, then every suffix
/// in range.
///
/// All three or none: a library with `uidna_openUTS46_78` but no
/// `uidna_nameToASCII_UTF8_78` is not one this crate can use, and
/// half-resolving it would leave an `Icu` that cannot convert.
fn resolve(lib: libloading::Library, name: String, hint: Option<u32>) -> Option<Icu> {
    let suffixes = hint
        .into_iter()
        .map(Some)
        .chain(core::iter::once(None))
        .chain((MIN_SUFFIX..=MAX_SUFFIX).rev().map(Some));
    for suffix in suffixes {
        let sfx = suffix.map_or_else(String::new, |n| format!("_{n}"));
        // SAFETY: `Library::get` is unsafe because the caller asserts the
        // symbol's type. The three signatures are transcribed from
        // `unicode/uidna.h`, restated above each type alias, and agree
        // with `windows-sys`' generated declarations of the same
        // functions; they have not changed since ICU 4.6, and ICU's ABI
        // stability promise covers the C API. A mismatch would be caught
        // by `UIdnaInfo::size`, which the callee validates.
        //
        // The returned `Symbol` borrows `lib`; dereferencing it copies
        // out a plain function pointer, and `lib` is then moved into the
        // struct that holds those pointers and never dropped.
        let found = unsafe {
            // unsafe-code-exception: amendment-C9
            let open = lib.get::<UidnaOpenUts46>(format!("uidna_openUTS46{sfx}\0").as_bytes());
            let close = lib.get::<UidnaClose>(format!("uidna_close{sfx}\0").as_bytes());
            let to_ascii = lib
                .get::<UidnaNameToAsciiUtf8>(format!("uidna_nameToASCII_UTF8{sfx}\0").as_bytes());
            match (open, close, to_ascii) {
                (Ok(open), Ok(close), Ok(to_ascii)) => Some((*open, *close, *to_ascii)),
                _ => None,
            }
        };
        if let Some((open, close, name_to_ascii)) = found {
            return Some(Icu {
                _lib: lib,
                // The suffix is the ICU major version, and it is the only
                // place the version is knowable: `libicuuc.so` is a
                // development symlink that names no version at all, and
                // reading `u_getVersion` would need a fourth symbol
                // resolved for no other purpose. A report that says
                // `libicuuc.so [uidna_openUTS46_78]` names the ICU that
                // answered; one that says `libicuuc.so` does not.
                name: format!("{name} [uidna_openUTS46{sfx}]"),
                open,
                close,
                name_to_ascii,
            });
        }
    }
    None
}

/// Closes the `UIDNA` handle on every exit path, including the early
/// returns below.
struct Handle {
    ptr: *mut c_void,
    close: UidnaClose,
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `uidna_openUTS46` and was checked
        // non-null before this type was constructed. This type is the
        // only owner, is not `Clone`, and the pointer is never written
        // after construction, so `uidna_close` runs exactly once on it.
        unsafe { (self.close)(self.ptr) }; // unsafe-code-exception: amendment-C9
    }
}

pub(crate) fn convert(icu: &Icu, domain: &str) -> Option<String> {
    let len = i32::try_from(domain.len()).ok()?;

    let mut status = U_ZERO_ERROR;
    // SAFETY: `status` is a live local and `options()` is a plain
    // integer. The call returns either a null pointer with `status` set,
    // or an owned handle this function is responsible for closing.
    let handle = unsafe { (icu.open)(options(), &raw mut status) }; // unsafe-code-exception: amendment-C9
    if status > U_ZERO_ERROR || handle.is_null() {
        return None;
    }
    // Bind the guard immediately, before anything that can return.
    let handle = Handle {
        ptr: handle,
        close: icu.close,
    };

    let mut buf = vec![0u8; FIRST_TRY];
    // Two attempts at most: the first at `FIRST_TRY`, the second at
    // exactly the length ICU reported. A third would mean ICU asked for a
    // length and then did not fit in it.
    for _ in 0..2 {
        let capacity = i32::try_from(buf.len()).ok()?;
        let mut info = UIdnaInfo::new();
        let mut status = U_ZERO_ERROR;
        // SAFETY: `handle.ptr` is a live handle from `uidna_openUTS46`.
        // `domain` is a `&str`, so its pointer is valid for `len` bytes
        // and `len` is its exact length — no NUL terminator is involved,
        // because the length is passed explicitly. `buf` is a live `Vec`
        // of `capacity` bytes and `capacity` is its exact length. `info`
        // and `status` are live locals. The callee retains none of them.
        let written = unsafe {
            // unsafe-code-exception: amendment-C9
            (icu.name_to_ascii)(
                handle.ptr,
                domain.as_ptr().cast::<c_char>(),
                len,
                buf.as_mut_ptr().cast::<c_char>(),
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
