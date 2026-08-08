//! The foreign-function boundary: three `uidna_*` entry points from the
//! platform's ICU, resolved at run time (spec amendment C9).
//!
//! # Why this is not a `#[link]` and not a `-sys` crate
//!
//! **ICU's exported symbols are version-suffixed.** The library that
//! exports `uidna_openUTS46_78` on one machine exports
//! `uidna_openUTS46_74` on the next, and on a build configured with
//! `-DU_DISABLE_RENAMING=1` it exports the bare name. There is no stable
//! symbol to link against, which is exactly why `rust_icu_sys` runs
//! `icu-config`/`pkg-config` and `bindgen` *at build time* — it resolves
//! the version of the machine doing the building. For a client library
//! that has to run on machines it was not built on, that is the wrong
//! answer: the same binary must work against ICU 74 and ICU 78, and must
//! keep working where there is no ICU at all.
//!
//! So: open the library by name, then look for the entry point under a
//! descending range of suffixes and under the bare name. Measured on this
//! development machine — `nm -D --defined-only /usr/lib/x86_64-linux-gnu/
//! libicuuc.so.78` lists `uidna_openUTS46_78`, `uidna_nameToASCII_UTF8_78`
//! and `uidna_close_78`, and nothing unsuffixed.
//!
//! **On Windows the suffix problem does not exist** — a correction to
//! this project's own design note, which said the exported symbols are
//! version-suffixed there too. They are not: the Windows SDK's `icu.h`
//! opens with `#define U_DISABLE_RENAMING 1`, and `windows-sys` 0.61.2
//! binds the plain `uidna_openUTS46` against `icuuc.dll`, which could not
//! load if the DLL exported only a suffixed name. The unsuffixed
//! candidate is therefore tried first, and on Windows it is the one that
//! answers; the suffix loop below is for ELF, where the version really is
//! part of both the file name and every symbol.
//!
//! # Why not `windows-sys`, which already declares all of this
//!
//! It genuinely does — `Win32_Globalization` carries `uidna_openUTS46`,
//! `uidna_nameToASCII_UTF8`, `uidna_close`, the `UIDNAInfo` struct and
//! every `UIDNA_*` constant, generated from Microsoft's own Win32
//! metadata. It was rejected for one reason: `windows-link`'s `link!`
//! expands to `#[link(kind = "raw-dylib")]`, an ordinary **load-time**
//! import. On a Windows with no `icuuc.dll` — 10 before 1703, and Server
//! 2016, which is supported into 2027 — that does not degrade to the
//! bundled path, it stops the whole process from starting, and it takes
//! the caller's HTTP client with it. Runtime resolution is the only shape
//! that can fall back, and falling back is the requirement.
//!
//! It was not wasted, though: `windows-sys`' `UIDNAInfo` is
//! `{ i16, i8, i8, u32, i32, i32 }`, **field for field the struct below**,
//! which is an independent check on this transcription from a source that
//! is generated rather than typed.
//!
//! The same load-time-import argument is why nothing here is `#[link]`ed
//! on ELF either.
//!
//! # What is unsafe here, and what deliberately is not
//!
//! Three signatures, three calls, and the handle's `Drop`. Everything that
//! *decides* anything lives in `lib.rs` as safe, tested code: the option
//! word ([`crate::OPTIONS`]), which of ICU's error bits are fatal
//! ([`crate::is_fatal`]), and the WHATWG deny list
//! ([`crate::is_forbidden_domain_byte`]). This module reads no untrusted
//! input, parses nothing, and hands nothing borrowed upward — it returns
//! an owned `String` or `None`.
//!
//! # The loading itself is not ours either
//!
//! `libloading` does the `dlopen`/`LoadLibraryW` half, with its
//! platform-difference handling, its error types and its `Send`/`Sync`
//! reasoning. 467M downloads, one transitive dependency per platform.
//! Declaring `dlopen` and `LoadLibraryW` here instead would have doubled
//! the size of this file and added the one class of bug — a wrong loader
//! flag on one platform — that a widely used crate has already had found
//! for it.
#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C9
    reason = "ICU's exported symbols are version-suffixed, so the platform's UTS 46 cannot be reached by linking; see spec amendment C9"
)]

use crate::{OPTIONS, is_fatal};
use core::ffi::{c_char, c_void};
use std::sync::OnceLock;

/// ICU's `UErrorCode`. Zero is success, negative values are warnings,
/// positive values are errors — the sign is the contract, not a
/// convention.
type UErrorCode = i32;

const U_ZERO_ERROR: UErrorCode = 0;

/// `U_BUFFER_OVERFLOW_ERROR`. The one error worth retrying: the return
/// value is then the length the output needs.
const U_BUFFER_OVERFLOW_ERROR: UErrorCode = 15;

/// `UIDNA_INFO_INITIALIZER` fills this in C. Here it is built by
/// [`UIdnaInfo::new`], which is the only place `size` is set — and `size`
/// is load-bearing: `uidna_nameToASCII_UTF8` compares it against its own
/// `sizeof(UIDNAInfo)` and returns `U_ILLEGAL_ARGUMENT_ERROR` if they
/// differ. That turns a layout disagreement with a future ICU into a
/// clean refusal and a fallback, rather than into a write past the end of
/// this struct.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct UIdnaInfo {
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
            // `as` rather than `try_into().unwrap()`: the value is
            // `size_of` a `#[repr(C)]` struct of six fixed-width fields,
            // pinned at 16 by the const assertion below.
            size: SIZE_OF_UIDNA_INFO as i16,
            is_transitional_different: 0,
            reserved_b3: 0,
            errors: 0,
            reserved_i2: 0,
            reserved_i3: 0,
        }
    }
}

const SIZE_OF_UIDNA_INFO: usize = size_of::<UIdnaInfo>();

/// If this ever fires, the struct above stopped matching C's
/// `UIDNAInfo` and every call would be refused at run time with
/// `U_ILLEGAL_ARGUMENT_ERROR`. Better to hear about it here.
const _: () = assert!(SIZE_OF_UIDNA_INFO == 16);

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
/// inside the `OnceLock` below, and a `OnceLock` in a `static` is not
/// dropped at exit. Unloading it while the function pointers were still
/// reachable is the one way to invalidate them, and there is no code path
/// that can.
#[derive(Debug)]
pub(crate) struct Library {
    _lib: libloading::Library,
    name: String,
    open: UidnaOpenUts46,
    close: UidnaClose,
    name_to_ascii: UidnaNameToAsciiUtf8,
}

impl Library {
    /// Which file answered — `libicuuc.so.78`, `icu.dll`, … Used by the
    /// differential corpus so its report names the ICU it measured.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// The library, or `None` on a machine with no usable ICU.
///
/// Searched once. On a miss that costs one failed `dlopen`/`LoadLibrary`
/// per candidate file name — up to 55 on ELF platforms, where the version
/// is part of the *file* name — which is a few hundred `stat` calls, once,
/// and only in a build that asked for `system-icu`.
pub(crate) fn library() -> Option<&'static Library> {
    static LIBRARY: OnceLock<Option<Library>> = OnceLock::new();
    LIBRARY.get_or_init(find_library).as_ref()
}

/// Candidate file names, most specific first.
///
/// **macOS is absent, for one reason and not the reason first written
/// down.** `libicucore.dylib` — Apple's own ICU — is private: no headers
/// ship for it and Apple documents its symbols as not for third-party
/// linking, so this module cannot reach `uidna_*` there. That much
/// stands. What does *not* stand is the claim that macOS has no public
/// IDN API at all: Foundation's `URL(string:)` punycodes a Unicode host,
/// and it is public. It is a *URL parser* rather than a domain-to-ASCII
/// function, though, so it tells you nothing about which IDNA flavour it
/// implements — and "it produces punycode" is true of IDNA2003 too. That
/// question is **unverified**: it needs one `straße.de` on a macOS
/// machine, and there has never been one here. Until it is answered,
/// macOS takes the bundled path and [`crate::backend`] reports `Bundled`
/// rather than claiming a platform implementation nobody has measured.
fn candidates() -> Vec<(String, Option<u32>)> {
    #[cfg(windows)]
    {
        // Unversioned file names, so the suffix has to be found by
        // probing symbols: `icu.dll` is the combined library (Windows 10
        // 1903+), `icuuc.dll` the split one (1703+).
        vec![("icu.dll".to_owned(), None), ("icuuc.dll".to_owned(), None)]
    }
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        // The soname carries the version, so opening `libicuuc.so.78`
        // already tells us to look for `_78` first. The bare
        // `libicuuc.so` is the development symlink; it is tried first
        // because when it exists it is one call instead of fifty.
        let mut v = vec![("libicuuc.so".to_owned(), None)];
        v.extend(
            (MIN_SUFFIX..=MAX_SUFFIX)
                .rev()
                .map(|n| (format!("libicuuc.so.{n}"), Some(n))),
        );
        v
    }
    #[cfg(not(any(windows, all(unix, not(target_vendor = "apple")))))]
    {
        // macOS, wasm, and anything else: nothing to look for.
        Vec::new()
    }
}

/// The library is not accepted because its symbols resolved — it is
/// accepted because it **answers the trap input correctly**.
///
/// Three failures collapse into this one check, and all three would
/// otherwise be silent or wrong:
///
/// - **`uidna_openUTS46` failing at run time.** Microsoft documents
///   `CoInitializeEx` as a prerequisite for Win32 apps using the split
///   `icuuc.dll`/`icuin.dll` (waived on 1903+ with the combined
///   `icu.dll`). Nothing here calls it. Without this probe, an `open`
///   that failed would surface one name at a time as
///   `IdnError::NotAnIdn` — the client telling a user their perfectly
///   good domain is invalid. With it, the library is simply not used and
///   the bundled path takes over.
/// - **An ABI or layout disagreement** with some future ICU: caught here
///   rather than at the first request.
/// - **A transitional answer.** If `OPTIONS` were ever wrong, or an ICU
///   ignored it, this probe rejects the library outright rather than
///   letting it resolve a different origin. The two inputs are the exact
///   pair the whole crate turns on.
///
/// The cost is two conversions, once per process.
fn answers_the_trap_correctly(lib: &Library) -> bool {
    const PROBES: [(&str, &str); 2] = [
        ("straße.de", "xn--strae-oqa.de"),
        ("faß.de", "xn--fa-hia.de"),
    ];
    PROBES
        .iter()
        .all(|(input, want)| convert(lib, input).as_deref() == Some(*want))
}

fn find_library() -> Option<Library> {
    for (name, hint) in candidates() {
        // SAFETY: `Library::new` is unsafe because loading a library runs
        // its initialisers, which can do anything. These names are
        // compile-time constants naming the platform's own ICU, not
        // anything derived from input; the process would load the same
        // file if it linked against it.
        let Ok(lib) = (unsafe { libloading::Library::new(&name) }) else {
            // unsafe-code-exception: amendment-C9
            continue;
        };
        if let Some(found) = resolve(lib, name, hint)
            && answers_the_trap_correctly(&found)
        {
            return Some(found);
        }
    }
    None
}

/// Finds the three entry points in an already-open library, trying the
/// suffix the file name implied, then no suffix at all, then every
/// suffix in range.
///
/// All three or none: a library that has `uidna_openUTS46_78` but no
/// `uidna_nameToASCII_UTF8_78` is not one this crate can use, and
/// half-resolving it would leave a `Backend::SystemIcu` that cannot
/// convert.
fn resolve(lib: libloading::Library, name: String, hint: Option<u32>) -> Option<Library> {
    let suffixes = hint
        .into_iter()
        .map(Some)
        .chain(core::iter::once(None))
        .chain((MIN_SUFFIX..=MAX_SUFFIX).rev().map(Some));
    for suffix in suffixes {
        let sfx = suffix.map_or_else(String::new, |n| format!("_{n}"));
        // SAFETY: `Library::get` is unsafe because the caller asserts the
        // symbol's type. The three signatures are transcribed from
        // `unicode/uidna.h` and restated above each type alias; they have
        // not changed since ICU 4.6, and ICU's own ABI stability promise
        // covers the C API. A mismatch would be caught by the `size`
        // field of `UIdnaInfo`, which the callee validates.
        //
        // The returned `Symbol` borrows `lib`; dereferencing it copies out
        // a plain function pointer, and `lib` is then moved into the
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
            return Some(Library {
                _lib: lib,
                // The suffix is the ICU major version, and it is the
                // only place the version is knowable: `libicuuc.so` is a
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
        // non-null before this type was constructed. This type is the only
        // owner, is not `Clone`, and the pointer is never written after
        // construction, so `uidna_close` runs exactly once on it.
        unsafe { (self.close)(self.ptr) }; // unsafe-code-exception: amendment-C9
    }
}

/// The initial output buffer. A domain that needs more than this is not
/// an error — the call is simply repeated at the length ICU asks for.
const FIRST_TRY: usize = 256;

/// Converts `domain` to A-labels through the platform's ICU, or `None` if
/// ICU refused it, if the call could not be made, or if the answer was not
/// UTF-8.
///
/// `None` covers all three because the caller has one thing to say about
/// any of them (`IdnError::NotAnIdn`), and because an error type here
/// would invite putting *why* into it — which is the decision this module
/// is deliberately not allowed to make.
pub(crate) fn name_to_ascii(domain: &str) -> Option<String> {
    convert(library()?, domain)
}

/// The conversion itself, against a library that has been resolved but
/// not necessarily accepted yet.
///
/// Separate from [`name_to_ascii`] for exactly one reason:
/// [`answers_the_trap_correctly`] has to run *before* the `OnceLock` is
/// filled, and so cannot go through [`library`].
fn convert(lib: &Library, domain: &str) -> Option<String> {
    let len = i32::try_from(domain.len()).ok()?;

    let mut status = U_ZERO_ERROR;
    // SAFETY: `status` is a live local. `OPTIONS` is a plain integer.
    // The call returns either a null pointer with `status` set, or an
    // owned handle this function is responsible for closing.
    let handle = unsafe { (lib.open)(OPTIONS, &raw mut status) }; // unsafe-code-exception: amendment-C9
    if status > U_ZERO_ERROR || handle.is_null() {
        return None;
    }
    // Bind the guard immediately, before anything that can return.
    let handle = Handle {
        ptr: handle,
        close: lib.close,
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
            (lib.name_to_ascii)(
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
        if status > U_ZERO_ERROR || is_fatal(info.errors) {
            return None;
        }
        let written = usize::try_from(written).ok()?;
        // `written` came from C. It is used as a slice bound, so it is
        // checked rather than trusted — the same line
        // `http-ng-dns-system::sys::classify_written` draws.
        let out = buf.get(..written)?;
        return String::from_utf8(out.to_vec()).ok();
    }
    None
}
