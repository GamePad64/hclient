//! `res_query(3)` — musl and FreeBSD.
//!
//! The whole of the foreign-function boundary is [`query`]: it hands back
//! an owned buffer, and everything above it is safe code that walks bytes.
//!
//! # Who is here, and who left
//!
//! **glibc is not**, and the reason is a documented one rather than a
//! measured one: `resolver(3)` calls the traditional interfaces
//! non-thread-safe and lists only the `res_n*` family as `MT-Safe`. This
//! crate is called from a blocking pool, so that property is the whole
//! game — `sys/res_nquery.rs` is glibc's backend and says the rest.
//!
//! **musl is here because there is nowhere else to go**, measured with
//! `nm` on Rust's self-contained sysroot: it exports `res_query`,
//! `res_search`, `res_querydomain` and **no `res_nquery` at all**. There
//! is no `res_n*` family on musl to move to, and its resolver keeps no
//! `_res` between calls.
//!
//! **FreeBSD is here because its own manual says the opposite of
//! glibc's** — *"This implementation of the resolver is thread-safe"*,
//! with `_res` described as *"the per-thread version"*. It exports only
//! the prefixed `__res_nquery`, so moving would add a `link_name` and a
//! second `__res_state` question for a guarantee the platform already
//! states.
//!
//! **Apple used to be here and is not**, which this header went on
//! claiming after it stopped being true: `res_9_query` was measured
//! unusable from more than one thread, and that platform moved to
//! `sys/apple.rs`.

#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "res_query is the only way to ask the system resolver for a record type getaddrinfo cannot return; see spec amendment C8"
)]

use super::{MAX_MESSAGE, Written, classify_written, query_name};
use crate::error::Error;
use crate::message;
use crate::{CLASS_IN, Record};
use core::ffi::{c_char, c_int, c_uchar};

/// The wire message is what comes back, so on FreeBSD any type at all can
/// be asked for — including one this crate, this libc and the reader have
/// never heard of.
///
/// **musl answers a bound instead, and it is measured rather than read.**
/// Against glibc on one host, one name, one moment: `CAA` (257) comes back
/// as 515 octets through glibc and `-1` through musl, while `A`, `AAAA`,
/// `HTTPS` and `ANY` (255) answer through both. So musl's `res_query` will
/// not carry a type number above 255, and saying `Any` here would be the
/// *capability that lies* this crate exists to prevent — with `CAA` and
/// `URI` on the wrong side of it, which are types callers actually ask
/// for.
#[cfg(target_env = "musl")]
pub(crate) fn support() -> crate::Support {
    crate::Support::UpTo(255)
}

#[cfg(not(target_env = "musl"))]
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
// - FreeBSD exports **both** names from `libc`: `lib/libc/resolv/Symbol.map`
//   lists `res_query` and `__res_query`, each under `FBSD_1.0`. So the
//   plain name links, no `link_name` is needed, and neither is
//   `-lresolv` — there is no separate resolver library, which
//   `resolver(3)` states from the other side as *"Standard C Library
//   (libc, -lc)"*. Each name appears in exactly one version node, so the
//   glibc trap above — a prefixed name existing only as a non-default
//   compat alias — has no counterpart here.
//
//   Read out of the source's own symbol map rather than from `libc`'s
//   `link_name` table, which covers `res_init` alone and would have
//   suggested the `__res_` prefix for a reason that turns out not to
//   apply.
//
// Apple is no longer in this list; `sys/apple.rs` is why.
unsafe extern "C" {
    // unsafe-code-exception: amendment-C8
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
