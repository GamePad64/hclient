//! The crate's foreign-function boundary: `res_query(3)`, and nothing else.
//!
//! **One of the project's three `unsafe` files** (spec amendment C8, which
//! also covers `sys/windows.rs`; the first was
//! `hclient-fetch/src/promise.rs`, amendment C7). It is
//! deliberately the smallest module that can exist for the job: it declares
//! one foreign function, calls it into a buffer it sized itself, and hands
//! back an owned `Vec<u8>` or a copied `[u8; 12]`. It parses nothing,
//! borrows nothing, and frees nothing. Every byte it returns is decoded by
//! `dns-message-parser` (no `unsafe` in its `src`, errors rather than
//! panics on every path) and interpreted one module over in `crate::svcb`,
//! which is `#![forbid(unsafe_code)]` — so the code that reads
//! attacker-controlled input is safe Rust, and the code that is not safe
//! Rust reads nothing.
//!
//! # Why `res_query` and not something safer
//!
//! `getaddrinfo` — everything the rest of this crate uses — cannot return
//! an HTTPS/SVCB record in principle: its result type is a list of
//! `sockaddr`s. There is no libc call that returns RR type 65 in a
//! structured form, and no crate in this workspace's dependency graph
//! wraps one: `res_query` is absent from `rustix` 1.1.4 and from `libc`
//! 0.2.189 (checked against the vendored sources of both, and of every
//! `libc` release from 0.2.182 to 0.2.189). The alternative to this file
//! is not "the same thing in safe Rust" but "no ECH and no first-request
//! h3 discovery on any native target", which is what
//! `Resolve::supports_svcb` returning `false` used to mean here.
//!
//! # Why `res_query` and not `res_nquery`
//!
//! `res_nquery` takes a caller-owned `res_state`, and is usually the right
//! answer where a process-global resolver state would be a hazard. It is
//! rejected here because `struct __res_state` is a libc-private layout
//! that neither `libc` nor `rustix` declares, and it differs between
//! glibc, musl and Darwin. Constructing one from a hand-written
//! `#[repr(C)]` guess is not a safer version of this file; it is a memory
//! corruption waiting for a libc point release.
//!
//! **What is relied on instead, stated so a reviewer can check it.**
//! `res_query`'s state is per-thread on all three supported libcs: glibc
//! reaches it through `__res_state()` (`_res` has been a per-thread macro
//! since glibc 2.9), Darwin likewise through `__res_state()`, and musl's
//! `res_query` holds no resolver state at all — it re-reads
//! `/etc/resolv.conf` per call. This crate calls it from a
//! `Blocking`-pool thread, which may be any thread and may be several at
//! once; per-thread state is what makes that sound. Note this is a claim
//! about the *resolver state* only. The global `h_errno` is NOT per-thread
//! on Darwin (`netdb.h` declares a plain `extern int h_errno;`), which is
//! one of two reasons this file never reads it — see below.
//!
//! # Why the failure path reads the buffer instead of `h_errno`
//!
//! `res_query` reports the ordinary case "this name exists but has no
//! HTTPS record" as **failure**: it returns `-1`, and the reason lives in
//! `h_errno`. Two problems with reading that: it is a process-global on
//! Darwin (above), and it is sticky — measured on glibc 2.43, a successful
//! call left `h_errno` at `4` (`NO_DATA`) from the previous failing one,
//! so its value is only meaningful in combination with the return code
//! anyway.
//!
//! What is used instead was established by measurement, not by reading
//! source. On the `-1` path, glibc has already written the received
//! response into the answer buffer; the buffer is zeroed by this file
//! before the call, and a DNS response always has the `QR` bit set, so
//! `QR` distinguishes "a response arrived and the call still failed"
//! from "nothing arrived". Measured directly on glibc 2.43:
//!
//! ```text
//! ftp.gnu.org       type=65 -> ret=-1  qr=1 rcode=0 ancount=0   (NODATA)
//! zzz9.invalid      type=65 -> ret=-1  qr=1 rcode=3 ancount=0   (NXDOMAIN)
//! cloudflare.com    type=65 -> ret=116 qr=1 rcode=0 ancount=1   (an answer)
//! cloudflare.com    inside an empty network namespace:
//!                              ret=-1  qr=0                     (nothing arrived)
//! ```
//!
//! So the RCODE — read from the wire by `crate::svcb`, in safe code — is
//! what classifies the failure, and `h_errno` is not needed on any
//! platform. Only the 12-byte header is returned from that path, because
//! only the header has a knowable end: the `-1` return carries no length.
//!
//! # The buffer, and the one length that must never be trusted
//!
//! Also measured rather than assumed: given a 20-byte buffer for a
//! 116-byte answer, glibc's `res_query` returns **20** — the buffer size,
//! not the size the answer needed. It does not report the overflow. So a
//! return equal to the buffer's length is indistinguishable from a silent
//! truncation, and the only sound response is to retry at the largest a
//! DNS message can be (65535 — the width of the length field that frames
//! one over TCP) and fail if even that is filled exactly. `n` is clamped
//! against the buffer's own length before it reaches a slice index in
//! every case, including the ones this reasoning says cannot happen.
#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "res_query is the only way to ask the system resolver for a record type getaddrinfo cannot return; see spec amendment C8"
)]

use super::{MAX_MESSAGE, RawAnswer, SvcbLookupError, Written, classify_written};
use core::ffi::{c_char, c_int, c_uchar};
use std::ffi::CString;

/// A constant, like every other backend's. This crate briefly carried a
/// `supports_svcb()` function instead, because an earlier Windows backend
/// resolved its entry point with `GetProcAddress` and so had a genuinely
/// run-time answer; that backend is gone (see `windows.rs`), every
/// backend's answer is decided by the build again, and a function that
/// always returns a constant would be machinery with no honesty to show
/// for it.
pub(crate) const SUPPORTS_SVCB: bool = true;

/// The backend seam: a name in, endpoints out.
///
/// One line, deliberately — everything it delegates to is safe. The
/// `unsafe` in this file stops at `query_https`, which returns an owned
/// buffer; the decoding is `dns-message-parser`'s, and the RFC 9460 client
/// rules are `crate::svcb`'s, shared with the Windows backend so the two
/// platforms cannot disagree about what a record means.
pub(crate) fn lookup(name: &str) -> Result<Vec<hclient_dns::SvcbEndpoint>, SvcbLookupError> {
    let answer = query_https(name)?;
    crate::svcb::endpoints_from_answer(&answer)
}

/// `IN`, RFC 1035 §3.2.4.
const CLASS_IN: c_int = 1;
/// `HTTPS`, RFC 9460 §14.1.
const TYPE_HTTPS: c_int = 65;
/// RFC 1035 §4.1.1.
const HEADER_LEN: usize = 12;
/// Comfortably larger than an EDNS0 UDP answer, so the common case takes
/// one call.
const FIRST_TRY: usize = 4096;
/// RFC 1035 §2.3.4 — the wire form of a name is at most 255 octets.
const MAX_NAME_LEN: usize = 255;

// Where the symbol lives, established by reading the installed libraries
// rather than by convention:
//
// - glibc 2.34 and later export `res_query` from `libc.so.6`
//   (`res_query@@GLIBC_2.34`) and leave `libresolv.so.2` an empty stub;
//   before 2.34 it was in `libresolv`. Linking `resolv` is therefore
//   correct on both — harmless on new glibc, required on old.
// - musl defines `res_query` as a strong symbol inside `libc.a`, and Rust's
//   self-contained musl sysroot ships **no `libresolv.a` at all** (checked:
//   the directory holds `libc.a` and `libunwind.a` and nothing else). A
//   `#[link(name = "resolv")]` here would fail to link, so musl gets none.
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

/// Asks the system resolver for `name`'s HTTPS (RR type 65) records.
///
/// Blocking — `res_query` does its own UDP/TCP I/O. The caller is
/// responsible for running this on a `Blocking` thread; see the crate
/// root.
fn query_https(name: &str) -> Result<RawAnswer, SvcbLookupError> {
    let unusable = || SvcbLookupError::NameNotUsable {
        name: name.to_owned(),
    };
    // 255 is the wire limit (RFC 1035 §2.3.4) and the textual form is never
    // shorter than the wire form, so this rejects nothing a resolver could
    // have answered. Checked here so that the `as c_int` casts below are
    // over values already known to be small, rather than trusted to be.
    if name.len() > MAX_NAME_LEN {
        return Err(unusable());
    }
    let cname = CString::new(name).map_err(|_| unusable())?;

    let mut buf = vec![0u8; FIRST_TRY];
    loop {
        // SAFETY: `cname` is a NUL-terminated C string that outlives the
        // call. `buf` is a live, uniquely borrowed allocation of exactly
        // `buf.len()` initialised bytes, and `buf.len()` is what is passed
        // as `anslen` — so the resolver cannot be told about more space
        // than exists. `buf.len()` is at most `MAX_MESSAGE` (65535), which
        // fits `c_int` on every supported target, so the cast cannot
        // truncate into a larger claim. `res_query` writes only within
        // `anslen` and retains no pointer past its return.
        let written = unsafe {
            // unsafe-code-exception: amendment-C8
            res_query(
                cname.as_ptr(),
                CLASS_IN,
                TYPE_HTTPS,
                buf.as_mut_ptr(),
                buf.len() as c_int,
            )
        };

        if written < 0 {
            // Only the header is returned, because only the header has a
            // knowable end on this path — the call reports no length. The
            // buffer was zeroed above and every DNS response sets `QR`, so
            // these twelve bytes are also what distinguishes "a response
            // arrived and the call still failed" from "nothing arrived".
            // That decision is NOT made here: it belongs to
            // `svcb::endpoints_from_answer`, which is safe code with tests
            // for both shapes. `buf` is at least `FIRST_TRY` bytes, so the
            // slice cannot be short.
            let header: [u8; HEADER_LEN] = buf[..HEADER_LEN]
                .try_into()
                .expect("buf is at least FIRST_TRY bytes, far more than a DNS header");
            return Ok(RawAnswer::HeaderOnly(header));
        }

        // `written` is non-negative, so this cast is exact. What it MEANS
        // for the buffer is decided by `classify_written`, one module up,
        // where the bound is unit-tested — see its doc comment for why a
        // return equal to the buffer's length is not a length.
        match classify_written(written as usize, buf.len()) {
            Written::Complete(n) => {
                buf.truncate(n);
                return Ok(RawAnswer::Message(buf));
            }
            Written::Retry => {
                buf.clear();
                buf.resize(MAX_MESSAGE, 0);
            }
            Written::TooLarge => return Err(SvcbLookupError::AnswerTooLarge),
        }
    }
}
