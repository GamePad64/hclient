//! `res_query(3)` — Linux (glibc, musl), macOS and iOS.
//!
//! The whole of the foreign-function boundary is [`query`]: it hands back
//! an owned buffer, and everything above it is safe code that walks bytes.

#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "res_query is the only way to ask the system resolver for a record type getaddrinfo cannot return; see spec amendment C8"
)]

use super::{MAX_MESSAGE, Written, classify_written, query_name};
use crate::error::Error;
use crate::message;
use crate::{CLASS_IN, Record};
use core::ffi::{c_char, c_int, c_uchar};

/// The wire message is what comes back, so any type at all can be asked
/// for — including one this crate, this libc and the reader have never
/// heard of.
pub(crate) fn support() -> crate::Support {
    crate::Support::Any
}

/// Comfortably larger than an EDNS0 UDP answer, so the common case takes
/// one call.
const FIRST_TRY: usize = 4096;
/// RFC 1035 §4.1.1.
const HEADER_LEN: usize = 12;

// Where the symbol lives, established by reading the installed libraries
// rather than by convention:
//
// - glibc 2.34 and later export `res_query` from `libc.so.6`
//   (`res_query@@GLIBC_2.34`); before 2.34 it was in `libresolv`. Linking
//   `resolv` is therefore correct on both — harmless on new glibc,
//   required on old. On glibc 2.43 that library still exports 66
//   functions, among them `ns_name_uncompress` and `inet_net_pton`; what
//   moved out of it is the `res_*` family, which is all this crate wanted.
//
//   **`__res_query` is the same code and cannot be linked, which is worth
//   recording because the idea is a good one.** Measured on glibc 2.43:
//   `__res_query@GLIBC_2.2.5` and `res_query@@GLIBC_2.34` are both at
//   address `0x1586d0` — one function, two names — so asking for the
//   first would record a dependency on `GLIBC_2.2.5` where the second
//   records `GLIBC_2.34`. It does not link. The single `@` is the whole
//   story: `__res_query` is a **compat** symbol, not the default version,
//   and a plain `link_name` binds only to a default one. rust-lld says so
//   in as many words — *did you mean: `__res_query@GLIBC_2.2.5`*.
//
//   **The `libc` crate's `link_name = "__res_init"` is not the precedent
//   it looks like.** That one is `__res_init@@GLIBC_2.2.5`, double `@@`,
//   and glibc exports no plain `res_init` at all — so the prefix there is
//   the only name, not an older one. Two symbols in one family, opposite
//   answers.
//
//   Reaching the compat version needs a `.symver` directive through
//   `global_asm!`, per architecture, and buys nothing here: a Rust binary
//   built on this host already needs `GLIBC_2.39` through the standard
//   library — measured on this crate's own test binary — so the floor is
//   somebody else's either way.
// - musl defines `res_query` as a strong symbol inside `libc.a` and has
//   **no `__res_query` at all** — checked with `nm` on Rust's
//   self-contained sysroot. That sysroot also ships **no `libresolv.a`**
//   (the directory holds `libc.a` and `libunwind.a` and nothing else), so
//   a `#[link(name = "resolv")]` here would fail to link and musl gets
//   none.
// - Apple exports only the BIND9-prefixed name: `libresolv.9.tbd` lists
//   `_res_9_query` and no plain `_res_query`. C code gets there through
//   `#define res_query res_9_query` in `<resolv.h>`; Rust has no
//   preprocessor, so the mapping is spelled out with `link_name`.
#[cfg_attr(all(target_os = "linux", target_env = "gnu"), link(name = "resolv"))]
#[cfg_attr(target_vendor = "apple", link(name = "resolv"))]
unsafe extern "C" {
    // unsafe-code-exception: amendment-C8
    #[cfg_attr(target_vendor = "apple", link_name = "res_9_query")]
    fn res_query(
        dname: *const c_char,
        class: c_int,
        rtype: c_int,
        answer: *mut c_uchar,
        anslen: c_int,
    ) -> c_int;
}

/// Asks the system resolver for `name`'s records of type `rtype`.
///
/// Blocking — `res_query` does its own UDP/TCP I/O.
pub(crate) fn query(name: &str, rtype: u16) -> Result<Vec<Record>, Error> {
    let cname = query_name(name)?;

    let mut buf = vec![0u8; FIRST_TRY];
    loop {
        // SAFETY: `cname` is a NUL-terminated C string that outlives the
        // call. `buf` is a live, uniquely borrowed allocation of exactly
        // `buf.len()` initialised bytes, and `buf.len()` is what is passed
        // as `anslen` — so the resolver cannot be told about more space
        // than exists. `buf.len()` is at most `MAX_MESSAGE` (65535), which
        // fits `c_int` on every supported target, so the cast cannot
        // truncate into a larger claim. `class` and `rtype` are values
        // this crate bounds to `u16` before the cast. `res_query` writes
        // only within `anslen` and retains no pointer past its return.
        let written = unsafe {
            // unsafe-code-exception: amendment-C8
            res_query(
                cname.as_ptr(),
                c_int::from(CLASS_IN),
                c_int::from(rtype),
                buf.as_mut_ptr(),
                buf.len() as c_int,
            )
        };

        if written < 0 {
            // The call reports no length on failure, so only the header
            // has a knowable end. The buffer was zeroed above and every
            // DNS response sets `QR`, so those twelve bytes also separate
            // *a response arrived and the call still failed* from
            // *nothing arrived*. That decision is not made here: it is
            // `message::header_only`'s, safe code with tests for every
            // shape. `buf` is at least `FIRST_TRY` bytes, so the slice
            // cannot be short.
            return message::header_only(&buf[..HEADER_LEN]).map(|()| Vec::new());
        }

        // `written` is non-negative, so this cast is exact. What it MEANS
        // for the buffer is `classify_written`'s, one module up, where the
        // bound is unit-tested — see its doc comment for why a return
        // equal to the buffer's length is not a length.
        match classify_written(written as usize, buf.len()) {
            Written::Complete(n) => {
                buf.truncate(n);
                return message::records(&buf);
            }
            Written::Retry => {
                buf.clear();
                buf.resize(MAX_MESSAGE, 0);
            }
            Written::TooLarge => return Err(Error::AnswerTooLarge),
        }
    }
}
