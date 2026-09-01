//! `res_query(3)` — glibc, musl and FreeBSD.
//!
//! The whole of the foreign-function boundary is [`query`]: it hands back
//! an owned buffer, and everything above it is safe code that walks bytes.
//!
//! # glibc calls this documented non-thread-safe, and the implementation
//! # disagrees
//!
//! `resolver(3)`: *"The traditional resolver interfaces such as
//! `res_init()` and `res_query()` use some static (global) state stored in
//! the `_res` structure, rendering these functions non-thread-safe"*, and
//! its `ATTRIBUTES` table lists **only** the `res_n*` family as `MT-Safe`.
//! This crate is called from a blocking pool, so that property is the one
//! everything here rests on — `sys/mod.rs` names it as one of two
//! requirements.
//!
//! Measured three ways on glibc 2.43, and they agree against the sentence:
//!
//! - `__res_state()` returns a **distinct pointer per thread** — nine
//!   threads, nine addresses;
//! - disassembled out of the shipped `libc.so.6`, `res_query` is
//!   `__resolv_context_get` -> `__res_context_query` ->
//!   `__resolv_context_put`, and `__resolv_context_get` reads through
//!   `%fs:`, the thread-local segment;
//! - the live suite's concurrency burst answers 64 of 64 from eight
//!   threads.
//!
//! So the "global state" of that sentence is thread-local here, and the
//! wording is inherited from the BIND-era API rather than describing this
//! libc.
//!
//! **`res_nquery` was built as the alternative and then withdrawn, because
//! it buys no weaker assumption.** It is glibc's documented `MT-Safe`
//! entry, it needs no struct layout — `_res` is `(*__res_state())`, that
//! symbol links, and the pointer is never dereferenced — and it costs one
//! initialisation per thread, since a fresh thread's state answers `-1`
//! until `__res_ninit` while re-initialising an initialised one **leaks**,
//! 1660 KiB over 200 000 calls. What settled it is that
//! `res_nquery(__res_state(), ..)` is handed **the very object**
//! `res_query` fetches itself, so both stand on the same per-thread fact:
//! the documented entry documents the other half. The route that would
//! rest on the contract alone is a state of this crate's own, and that
//! needs the layout `libc` declines to declare on any platform.
//!
//! What is kept from the exercise is the part with teeth:
//! [`tests::glibc_hands_each_thread_its_own_resolver_state`] asserts that
//! fact directly instead of through the outcome of a burst — which is the
//! difference between catching Apple's defect and being lucky about it.
//!
//! # The other two
//!
//! **musl is here because there is nowhere else to go**, measured with
//! `nm` on Rust's self-contained sysroot: it exports `res_query`,
//! `res_search`, `res_querydomain` and **no `res_nquery` at all**, and its
//! resolver keeps no `_res` between calls for one to be needed. What it
//! does have is a ceiling — see [`support`].
//!
//! **FreeBSD's own manual says the opposite of glibc's** — *"This
//! implementation of the resolver is thread-safe"*, with `_res` described
//! as *"the per-thread version"*.
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
    crate::Support::up_to(255)
}

#[cfg(not(target_env = "musl"))]
pub(crate) fn support() -> crate::Support {
    crate::Support::any()
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
#[cfg_attr(all(target_os = "linux", target_env = "gnu"), link(name = "resolv"))]
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

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests {
    use core::ffi::c_void;

    unsafe extern "C" {
        // unsafe-code-exception: amendment-C8
        fn __res_state() -> *mut c_void;
    }

    /// **The requirement `sys/mod.rs` names, asserted at its cause rather
    /// than through its symptom.**
    ///
    /// That module says this backend needs "a libc whose resolver state is
    /// per-thread", and the live suite checks it the way a caller would
    /// feel it — a burst of concurrent lookups that all have to answer.
    /// That is an outcome, and an outcome can be right for the wrong
    /// reason: a shared state under a light load loses nothing most of the
    /// time, which is exactly how Apple's arm passed a reading and failed
    /// a run.
    ///
    /// This asserts the fact itself, and it is the one thing this module
    /// takes on trust on glibc — where the manual says the opposite, so
    /// there is no contract to fall back on and a check is the whole of
    /// the evidence. See this module's header for the other two
    /// measurements that agree with it, and for why moving to `res_nquery`
    /// would not have removed the assumption.
    #[test]
    fn glibc_hands_each_thread_its_own_resolver_state() {
        const THREADS: usize = 8;

        // SAFETY: `__res_state` takes nothing, returns this thread's state
        // pointer, and is never dereferenced here — the value is compared
        // as an address and nothing more.
        let here = unsafe {
            // unsafe-code-exception: amendment-C8
            __res_state()
        } as usize;

        let elsewhere: Vec<usize> = std::thread::scope(|scope| {
            let running: Vec<_> = (0..THREADS)
                .map(|_| {
                    scope.spawn(|| {
                        // SAFETY: as above.
                        let statp = unsafe {
                            // unsafe-code-exception: amendment-C8
                            __res_state()
                        };
                        statp as usize
                    })
                })
                .collect();
            running
                .into_iter()
                .map(|t| t.join().expect("a thread panicked"))
                .collect()
        });

        let distinct: std::collections::BTreeSet<usize> = elsewhere.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            THREADS,
            "{THREADS} threads got {} distinct resolver states: glibc is sharing one, \
             so the way this crate calls the resolver is a data race",
            distinct.len()
        );
        assert!(
            !distinct.contains(&here),
            "a spawned thread got the same resolver state as this one"
        );
    }
}
