//! `res_nquery(3)` — glibc, and glibc alone.
//!
//! The whole of the foreign-function boundary is [`query`]: it hands back
//! an owned buffer, and everything above it is safe code that walks bytes.
//!
//! # Why glibc does not share `res_query.rs`
//!
//! **glibc documents `res_query` as unsafe to call from more than one
//! thread.** `resolver(3)`: *"The traditional resolver interfaces such as
//! `res_init()` and `res_query()` use some static (global) state stored in
//! the `_res` structure, rendering these functions non-thread-safe"* — and
//! its `ATTRIBUTES` table lists **only** the `res_n*` family as `MT-Safe`.
//! `res_query` is not in that table at all.
//!
//! That is the property this whole backend rests on. `sys/mod.rs` names it
//! as one of two requirements, and this crate is called from a blocking
//! pool: `hclient-dns-system` wraps each of `Resolve`'s three methods in
//! its own `Blocking::run`, and one request asks for A, AAAA and the HTTPS
//! record at once — three calls in flight on three pool threads before a
//! connection is opened.
//!
//! musl and FreeBSD are the other side of the same question and keep
//! `res_query`, each for a measured reason of its own; `res_query.rs` says
//! which.
//!
//! # What it costs, and what it does not
//!
//! **No struct layout, which is what made this affordable.** `<resolv.h>`
//! defines `_res` as `(*__res_state())`, and `__res_state@@GLIBC_2.2.5` is
//! a linkable default symbol. The pointer is handed straight to
//! `res_nquery` and **never dereferenced here**, so nothing in this crate
//! needs to know what `struct __res_state` looks like — which matters:
//! `libc` 0.2.189 declares no `res_state` on any platform, and a
//! hand-transcribed layout at an FFI boundary is silent corruption rather
//! than a compile error.
//!
//! **One initialisation per thread, and the number is why.** Measured: on
//! a fresh thread `res_nquery` answers `-1` with nothing written until the
//! state is initialised, and either `__res_ninit` or `__res_init` fixes
//! it. Also measured: **re-initialising an already-initialised state
//! leaks** — 1660 KiB over 200 000 calls, about eight bytes each. So the
//! initialisation is guarded by a thread-local flag and happens once,
//! never per query and never on failure.
//!
//! **What is still an implementation fact rather than a contract** is that
//! `__res_state()` hands each thread its own object. It is asserted rather
//! than assumed — see this module's own test — and it is the same fact
//! `res_query` was already relying on silently, since glibc's `res_query`
//! reaches the resolver through that very object. What the move buys is
//! the documented entry point and a checkable claim in place of an opaque
//! one; if the test ever fails, both entry points are affected together
//! and the repair is a state of this crate's own, which needs the layout
//! above.

#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "res_nquery is glibc's documented MT-Safe way to ask the system resolver for a record type getaddrinfo cannot return; see spec amendment C8"
)]

use super::{MAX_MESSAGE, Written, classify_written, query_name};
use crate::error::Error;
use crate::message;
use crate::{CLASS_IN, Record};
use core::ffi::{c_char, c_int, c_uchar, c_void};

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

// `res_nquery` moved into `libc` at 2.34 and was in `libresolv` before
// that, so the library is named for the older glibc and is harmless on the
// newer — the same reasoning `res_query.rs` records for its own symbol.
//
// `__res_ninit` rather than `res_ninit`, and `__res_state` with no prefix
// dance at all: glibc exports `__res_ninit@@GLIBC_2.2.5` and no plain
// `res_ninit`, exactly as it exports `__res_init` and no `res_init`, while
// `res_nquery@@GLIBC_2.34` and `__res_state@@GLIBC_2.2.5` are both plain
// default symbols. Read out of `nm -D` on the installed library rather
// than from convention, because this crate has already been bitten by a
// name in this family that existed and could not be linked.
#[link(name = "resolv")]
unsafe extern "C" {
    // unsafe-code-exception: amendment-C8
    fn __res_state() -> *mut c_void;
    // unsafe-code-exception: amendment-C8
    #[link_name = "__res_ninit"]
    fn res_ninit(statp: *mut c_void) -> c_int;
    // unsafe-code-exception: amendment-C8
    fn res_nquery(
        statp: *mut c_void,
        dname: *const c_char,
        class: c_int,
        rtype: c_int,
        answer: *mut c_uchar,
        anslen: c_int,
    ) -> c_int;
}

/// This thread's resolver state, initialised the first time it is asked
/// for and not again.
///
/// **The flag is set even when the initialisation fails**, and that is
/// deliberate: a failed `res_ninit` means the machine's resolver
/// configuration could not be read, the query that follows reports it as
/// [`Error::NoResponse`], and retrying per query would leak at the rate
/// measured in this module's header for a condition that will not change
/// between two calls a microsecond apart.
fn thread_state() -> *mut c_void {
    thread_local! {
        static INITIALISED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    // SAFETY: `__res_state` takes no arguments and returns glibc's own
    // per-thread resolver state. The value is passed on and never
    // dereferenced by this crate, so nothing here depends on the layout of
    // what it points at.
    let statp = unsafe {
        // unsafe-code-exception: amendment-C8
        __res_state()
    };

    INITIALISED.with(|done| {
        if !done.get() {
            done.set(true);
            // SAFETY: `statp` is the pointer glibc just handed back for
            // this thread, and it is the only thread that can reach it.
            // `res_ninit` initialises the state it is given and retains no
            // pointer of ours.
            unsafe {
                // unsafe-code-exception: amendment-C8
                res_ninit(statp);
            }
        }
    });

    statp
}

/// Asks the system resolver for `name`'s records of type `rtype`.
///
/// Blocking — `res_nquery` does its own UDP/TCP I/O.
pub(crate) fn query(name: &str, rtype: u16) -> Result<Vec<Record>, Error> {
    let cname = query_name(name)?;
    let statp = thread_state();

    let mut buf = vec![0u8; FIRST_TRY];
    loop {
        // SAFETY: `statp` is this thread's own state, initialised above.
        // `cname` is a NUL-terminated C string that outlives the call.
        // `buf` is a live, uniquely borrowed allocation of exactly
        // `buf.len()` initialised bytes, and `buf.len()` is what is passed
        // as `anslen` — so the resolver cannot be told about more space
        // than exists. `buf.len()` is at most `MAX_MESSAGE` (65535), which
        // fits `c_int` on every supported target, so the cast cannot
        // truncate into a larger claim. `class` and `rtype` are values
        // this crate bounds to `u16` before the cast. `res_nquery` writes
        // only within `anslen` and retains no pointer past its return.
        let written = unsafe {
            // unsafe-code-exception: amendment-C8
            res_nquery(
                statp,
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

#[cfg(test)]
mod tests {
    use super::__res_state;

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
    /// takes on trust. Everything else here is documented — `res_nquery`
    /// is `MT-Safe` **given a state per thread**, and this is where that
    /// precondition is checked rather than assumed.
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
             so the precondition of the MT-Safe entry point is not met",
            distinct.len()
        );
        assert!(
            !distinct.contains(&here),
            "a spawned thread got the same resolver state as this one"
        );
    }
}
