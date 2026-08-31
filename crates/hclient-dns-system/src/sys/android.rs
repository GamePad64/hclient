//! Android's resolver, through `android_res_nquery`.
//!
//! # Why not `res_query`
//!
//! bionic has one, and it is not in the NDK: `<resolv.h>` is not a stable
//! NDK header and the `res_*` family is not in the stable ABI, so a crate
//! that declared `res_query` here would be linking a symbol Android does
//! not promise. What Android does promise is
//! `<android/multinetwork.h>` — `android_res_nquery` and its neighbours,
//! in `libandroid.so`, and **that pair is what the platform's own
//! resolver work goes through**, which is why an HTTPS record asked for
//! this way is the one the device would use.
//!
//! # The shape is two calls rather than one, and the second closes the fd
//!
//! `android_res_nquery` hands back a **file descriptor**, not an answer:
//! the query is in flight and the caller waits on the descriptor.
//! `android_res_nresult` is the wait, and it **closes the descriptor
//! before returning** — documented, and the reason nothing here calls
//! `close`. The only path that must clean up is the one between them,
//! The only path that could leak is a query abandoned *between* them, and
//! `android_res_cancel` is the NDK's answer for one — declared here
//! neither as a function nor as a use, because this file asks for the
//! result immediately and so never abandons a query. A declaration with
//! no caller is what this workspace deletes rather than keeps against a
//! future need; that is the rule `UpgradeSupport`'s spare variants went
//! under.
//!
//! Blocking, like `res_query` one file over, and for the same reason: the
//! caller runs this on a `Blocking` thread. `nresult` is where the wait
//! happens.
//!
//! # API level 29, and it is a link-time requirement rather than a
//! run-time one
//!
//! `__INTRODUCED_IN(29)` — Android 10, September 2019. A build whose
//! `minSdkVersion` is lower links a symbol its devices may not have, and
//! that is a failure at load rather than at the first lookup: visible at
//! once, on every device, instead of on some. **That is the direction to
//! fail in**, and it is why this does not go through `dlsym` and a
//! run-time `SUPPORTS_SVCB`. The alternative was measured against this
//! crate's own history: the Windows backend once resolved its entry point
//! at run time and the capability became a function; that backend is gone
//! and `windows.rs` says why.

#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "android_res_nquery is the NDK's only stable way to ask for a record type getaddrinfo cannot return; see spec amendment C8"
)]

use super::{MAX_MESSAGE, RawAnswer, SvcbLookupError, Written, classify_written};
use std::ffi::{CString, c_char, c_int, c_uint};

/// `true`, and it is the whole of the claim: a build that links this file
/// is a build whose devices have the entry point — see the module doc on
/// why that is a link-time question.
pub(crate) const SUPPORTS_SVCB: bool = true;

/// The backend seam: a name in, endpoints out.
///
/// One line, for `res_query.rs`'s reason — the `unsafe` stops at
/// [`query_https`], which hands back an owned buffer, and every rule about
/// what a record *means* is `crate::svcb`'s, shared with the other two
/// backends so no two platforms can disagree.
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
/// RFC 1035 §2.3.4 — the wire form of a name is at most 255 octets.
const MAX_NAME_LEN: usize = 255;

/// `<android/multinetwork.h>`: `typedef uint64_t net_handle_t;`
type NetHandle = u64;

/// `#define NETWORK_UNSPECIFIED ((net_handle_t)0)` — the device's default
/// network, which is what a client with no opinion about interfaces
/// wants. Asking for a specific one is `android.net.Network`'s
/// `getNetworkHandle()`, a value only the application has, so it is not
/// something this crate could choose.
const NETWORK_UNSPECIFIED: NetHandle = 0;

// `libandroid.so`, where the NDK puts the multinetwork API. It is part of
// the platform on every device that has the symbols at all, so there is
// no version to pick and nothing to bundle.
#[link(name = "android")]
unsafe extern "C" {
    // unsafe-code-exception: amendment-C8
    fn android_res_nquery(
        network: NetHandle,
        dname: *const c_char,
        ns_class: c_int,
        ns_type: c_int,
        flags: c_uint,
    ) -> c_int;
    fn android_res_nresult(fd: c_int, rcode: *mut c_int, answer: *mut u8, anslen: usize) -> c_int;
}

/// Asks the device's resolver for `name`'s HTTPS (RR type 65) records.
fn query_https(name: &str) -> Result<RawAnswer, SvcbLookupError> {
    let unusable = || SvcbLookupError::NameNotUsable {
        name: name.to_owned(),
    };
    // The wire limit, checked here so the buffer arithmetic below is over
    // a value already known to be small — `res_query.rs` states the same
    // bound for the same reason.
    if name.len() > MAX_NAME_LEN {
        return Err(unusable());
    }
    let cname = CString::new(name).map_err(|_| unusable())?;

    // SAFETY: `cname` is a NUL-terminated C string that outlives the call.
    // The other four arguments are plain integers. The call starts a query
    // and returns a descriptor this function owns from here.
    let fd = unsafe {
        // unsafe-code-exception: amendment-C8
        android_res_nquery(NETWORK_UNSPECIFIED, cname.as_ptr(), CLASS_IN, TYPE_HTTPS, 0)
    };
    if fd < 0 {
        // A negative return is a POSIX error code and no descriptor was
        // created, so there is nothing to cancel. The header-only answer
        // is `res_query.rs`'s shape for the same case: what a failure
        // *means* is `svcb::endpoints_from_answer`'s to decide, in safe
        // code with tests for both shapes.
        return Ok(RawAnswer::HeaderOnly([0u8; HEADER_LEN]));
    }

    // **One buffer, sized at the maximum, because there is no second
    // try.** `res_query` reports the length it wanted and can be asked
    // again with a larger buffer; `android_res_nresult` consumes the
    // descriptor, so a short buffer is not a retry — it is the answer
    // gone. `MAX_MESSAGE` is 65535, the largest a DNS message can be, so
    // the case cannot arise.
    let mut buf = vec![0u8; MAX_MESSAGE];
    let mut rcode: c_int = 0;
    // SAFETY: `fd` is the descriptor the call above returned and has not
    // been used since. `rcode` is a live local. `buf` is a uniquely
    // borrowed allocation of exactly `buf.len()` initialised bytes, and
    // `buf.len()` is what is passed as `anslen`, so the resolver cannot be
    // told about more space than exists. `android_res_nresult` closes
    // `fd` before returning, which is why nothing below does.
    let written = unsafe {
        // unsafe-code-exception: amendment-C8
        android_res_nresult(fd, &raw mut rcode, buf.as_mut_ptr(), buf.len())
    };

    if written < 0 {
        // The descriptor is closed by `nresult` whatever it answers, so
        // there is still nothing to close and nothing to cancel: the
        // NDK's `android_res_cancel` is for abandoning a query before
        // asking for its result, which this function never does.
        return Ok(RawAnswer::HeaderOnly([0u8; HEADER_LEN]));
    }

    // `written` is non-negative, so the cast is exact; what a length equal
    // to the buffer means is `classify_written`'s, one module up, where it
    // is unit-tested and shared with the other backends.
    match classify_written(written as usize, buf.len()) {
        Written::Complete(n) => {
            buf.truncate(n);
            Ok(RawAnswer::Message(buf))
        }
        // Unreachable with a `MAX_MESSAGE` buffer — a DNS message cannot
        // be longer — and it is answered rather than `unreachable!()`
        // because the arm costs one line and a panic in a resolver costs
        // a process.
        Written::Retry | Written::TooLarge => Err(SvcbLookupError::AnswerTooLarge),
    }
}
