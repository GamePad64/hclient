//! `android_res_nquery` — Android API 29 and later.
//!
//! **Why not `res_query`.** The `res_*` family is not in the NDK's stable
//! ABI: Bionic exports the symbols, and the NDK's headers do not declare
//! them, so linking against one is relying on a private symbol that has
//! changed before. `android_res_nquery` is the declared replacement, it
//! takes a network handle Android's multi-network model needs, and it goes
//! through the same resolver the platform uses — which is the point of
//! this crate, and which is what makes Private DNS and per-network servers
//! apply.
//!
//! **No JNI.** These are C entry points in `libandroid.so`, part of the
//! platform on every device that has them. That is worth knowing before
//! assuming Android integrations are alike: reading the system *proxy*
//! settings on Android costs `jni` and `ndk-context`, because those live
//! behind the JVM, and this costs nothing at all.

#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "android_res_nquery is the NDK's only declared way to ask the device's resolver for an arbitrary record type; see spec amendment C8"
)]

use super::{MAX_MESSAGE, Written, classify_written, query_name};
use crate::error::Error;
use crate::message;
use crate::{CLASS_IN, Record};
use core::ffi::{c_char, c_int, c_uint};

/// The wire message is what comes back, so any type at all can be asked
/// for.
pub(crate) fn support() -> crate::Support {
    crate::Support::Any
}

/// RFC 1035 §4.1.1.
const HEADER_LEN: usize = 12;

/// `<android/multinetwork.h>`: `typedef uint64_t net_handle_t;`
type NetHandle = u64;

/// `#define NETWORK_UNSPECIFIED ((net_handle_t)0)` — the device's default
/// network, which is what a caller with no opinion about interfaces wants.
/// Asking for a specific one needs `android.net.Network`'s
/// `getNetworkHandle()`, a value only the application has, so it is not
/// something this crate could choose.
const NETWORK_UNSPECIFIED: NetHandle = 0;

// `libandroid.so`, where the NDK puts the multinetwork API. It is part of
// the platform on every device that has the symbols at all, so there is no
// version to pick and nothing to bundle.
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

/// Asks the device's resolver for `name`'s records of type `rtype`.
pub(crate) fn query(name: &str, rtype: u16) -> Result<Vec<Record>, Error> {
    let cname = query_name(name)?;

    // SAFETY: `cname` is a NUL-terminated C string that outlives the call.
    // The other four arguments are plain integers, `class` and `rtype`
    // bounded to `u16` before their conversions. The call starts a query
    // and returns a descriptor this function owns from here.
    let fd = unsafe {
        // unsafe-code-exception: amendment-C8
        android_res_nquery(
            NETWORK_UNSPECIFIED,
            cname.as_ptr(),
            c_int::from(CLASS_IN),
            c_int::from(rtype),
            0,
        )
    };
    if fd < 0 {
        // A negative return is a POSIX error code and no descriptor was
        // created, so there is nothing to cancel. A zeroed header is the
        // "nothing arrived" shape, and what that *means* is
        // `message::header_only`'s, in safe code with tests.
        return message::header_only(&[0u8; HEADER_LEN]).map(|()| Vec::new());
    }

    // **One buffer, sized at the maximum, because there is no second
    // try.** `res_query` reports the length it wanted and can be asked
    // again with a larger buffer; `android_res_nresult` consumes the
    // descriptor, so a short buffer is not a retry — it is the answer
    // gone. `MAX_MESSAGE` is the largest a DNS message can be, so the case
    // cannot arise.
    let mut buf = vec![0u8; MAX_MESSAGE];
    let mut rcode: c_int = 0;
    // SAFETY: `fd` is the descriptor the call above returned and has not
    // been used since. `rcode` is a live local. `buf` is a uniquely
    // borrowed allocation of exactly `buf.len()` initialised bytes, and
    // `buf.len()` is what is passed as `anslen`, so the resolver cannot be
    // told about more space than exists. `android_res_nresult` closes `fd`
    // before returning, which is why nothing below does.
    let written = unsafe {
        // unsafe-code-exception: amendment-C8
        android_res_nresult(fd, &raw mut rcode, buf.as_mut_ptr(), buf.len())
    };

    if written < 0 {
        // The descriptor is closed by `nresult` whatever it answers, so
        // there is still nothing to close and nothing to cancel: the NDK's
        // `android_res_cancel` is for abandoning a query before asking for
        // its result, which this function never does.
        return message::header_only(&[0u8; HEADER_LEN]).map(|()| Vec::new());
    }

    // `written` is non-negative, so the cast is exact; what a length equal
    // to the buffer means is `classify_written`'s, one module up.
    match classify_written(written as usize, buf.len()) {
        Written::Complete(n) => {
            buf.truncate(n);
            message::records(&buf)
        }
        // Unreachable with a `MAX_MESSAGE` buffer — a DNS message cannot
        // be longer — and answered rather than `unreachable!()` because
        // the arm costs one line and a panic in a resolver costs a
        // process.
        Written::Retry | Written::TooLarge => Err(Error::AnswerTooLarge),
    }
}
