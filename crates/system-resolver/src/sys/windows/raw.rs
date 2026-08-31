//! `DnsQueryRaw` — Windows 11 / Server 2025 and later.
//!
//! It hands back the **wire message**, so a machine that has it behaves
//! exactly like the four platforms that call `res_query`: any record type,
//! walked by the same code in `message.rs`, with the same rules about
//! truncation and rcodes.
//!
//! # Measured, on Windows 11, 2026-09-01
//!
//! Every number below came from running it; three of them decide the code.
//!
//! - **The call returns `DNS_REQUEST_PENDING` (9506) and the completion
//!   routine then always runs.** Given a `protocol` of 0 it returns `87`
//!   (`ERROR_INVALID_PARAMETER`) and the routine **never runs**. So the
//!   rule that cannot hang is: wait for the routine exactly when the return
//!   is `DNS_REQUEST_PENDING`, and treat anything else as an answer in
//!   itself.
//! - **The two-octet length prefix is TCP's, not the API's.** Over
//!   `DNS_PROTOCOL_TCP` the first two octets equalled the rest of the
//!   buffer's length every time (195/195, 594/594, 154/154, 256/256); over
//!   UDP they are the message's own ID and match nothing. This module asks
//!   for TCP and checks the prefix rather than assuming it.
//! - **UDP truncates and TCP does not.** `CAA` for `cloudflare.com` came
//!   back as 32 octets over UDP — a header with `TC` set — and 596 over
//!   TCP. `protocol` is required, so there is no *let the OS decide*, and
//!   TCP is the only choice that answers the question that was asked.
//! - **`NXDOMAIN` arrives as a whole message**, `queryStatus` 9003 beside
//!   258 octets of answer, so the rcode is read by the shared walker rather
//!   than translated from a status code here.
#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "DnsQueryRaw must be resolved at run time, because a static import of it stops the process from starting on Windows 10; see spec amendment C8"
)]

use crate::Record;
use crate::error::Error;
use crate::message;
use crate::sys::query_name;
use core::ffi::c_void;
use std::sync::OnceLock;
use std::sync::mpsc;
// The long paths are imported rather than written inline, and that is not
// only taste: `cargo fmt` breaks an over-long `unsafe { … }` across lines,
// which strands the `unsafe-code-exception` marker three lines away from
// the `unsafe` keyword and fails the `no-unsafe-code` job. Keeping each
// such expression on one line keeps the marker where the job can see it.
use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_PROTOCOL_TCP, DNS_QUERY_RAW_CANCEL, DNS_QUERY_RAW_REQUEST, DNS_QUERY_RAW_REQUEST_VERSION1,
    DNS_QUERY_RAW_RESULT, DNS_QUERY_RAW_RESULTS_VERSION1,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

/// `DNS_REQUEST_PENDING`, `winerror.h`. The **only** return after which the
/// completion routine runs; see the header.
const DNS_REQUEST_PENDING: i32 = 9506;

// The three signatures, each with its marker on the line below rather than
// trailing: `cargo fmt` wraps the first of them, and a trailing comment on
// a line the formatter reflows is a marker this project has lost before.
type DnsQueryRawFn =
    unsafe extern "system" fn(*const DNS_QUERY_RAW_REQUEST, *mut DNS_QUERY_RAW_CANCEL) -> i32;
// unsafe-code-exception: amendment-C8
type DnsQueryRawResultFreeFn = unsafe extern "system" fn(*const DNS_QUERY_RAW_RESULT);
// unsafe-code-exception: amendment-C8
/// What `GetProcAddress` hands back: a function pointer with no signature.
/// Named so the two transmutes below can state both of their types, which
/// is what makes them read as a pair of declarations rather than as a pair
/// of assertions.
type ProcAddress = unsafe extern "system" fn() -> isize;
// unsafe-code-exception: amendment-C8

/// The two entry points this path needs, or `None` on a Windows that has
/// neither.
pub(super) struct Api {
    query: DnsQueryRawFn,
    free: DnsQueryRawResultFreeFn,
}

// SAFETY: both fields are plain function pointers into a module that is
// never unloaded — `LoadLibraryA` below takes a reference this process
// keeps for its lifetime — so they are valid from any thread.
unsafe impl Send for Api {} // unsafe-code-exception: amendment-C8
// SAFETY: as above; the struct is immutable once built.
unsafe impl Sync for Api {} // unsafe-code-exception: amendment-C8

/// Whether this machine has the call at all — the whole of what
/// `support()` needs to know.
pub(super) fn available() -> bool {
    api().is_some()
}

/// Resolved once. `None` means a Windows without `DnsQueryRaw`, and it is
/// cached because the answer cannot change while the process runs.
pub(super) fn api() -> Option<&'static Api> {
    static RESOLVED: OnceLock<Option<Api>> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            // SAFETY: a NUL-terminated literal. The handle is deliberately
            // never freed: the function pointers below outlive this call,
            // and `dnsapi.dll` is already in the process anyway, since
            // `DnsQuery_UTF8` is statically imported beside it.
            let module = unsafe { LoadLibraryA(c"dnsapi.dll".as_ptr().cast()) }; // unsafe-code-exception: amendment-C8
            if module.is_null() {
                return None;
            }
            // SAFETY: a live module handle and NUL-terminated literals.
            // `GetProcAddress` answers `None` for a symbol the module does
            // not export, which is the older-Windows case and the whole
            // reason this is a run-time question.
            let (query, free) = unsafe {
                // unsafe-code-exception: amendment-C8
                (
                    GetProcAddress(module, c"DnsQueryRaw".as_ptr().cast()),
                    GetProcAddress(module, c"DnsQueryRawResultFree".as_ptr().cast()),
                )
            };
            // **Both or neither**, which is what these two `?`s buy: a
            // result that could be allocated and never freed is worse than
            // falling back to the older call.
            let (query, free) = (query?, free?);
            // SAFETY: the signatures are `windows-sys` 0.61.2's own
            // declarations for these two symbols, restated above as pointer
            // types because naming the linked functions would put them in
            // the import table and stop the process from starting on a
            // Windows that lacks them.
            Some(Api {
                query: unsafe { core::mem::transmute::<ProcAddress, DnsQueryRawFn>(query) }, // unsafe-code-exception: amendment-C8
                free: unsafe { core::mem::transmute::<ProcAddress, DnsQueryRawResultFreeFn>(free) }, // unsafe-code-exception: amendment-C8
            })
        })
        .as_ref()
}

/// What the completion routine sends back: the call's own verdict and the
/// bytes, copied out before the result is freed.
struct Answer {
    status: i32,
    bytes: Vec<u8>,
}

/// The completion routine. Runs on a thread pool thread the caller does not
/// own, which is why everything it touches is owned or copied.
unsafe extern "system" fn completed(ctx: *const c_void, result: *const DNS_QUERY_RAW_RESULT) {
    // unsafe-code-exception: amendment-C8
    // SAFETY: `ctx` is the `Box` leaked by `query` and reclaimed here
    // exactly once — this routine runs once per query, and `query`
    // reclaims it itself only on the path where this routine provably does
    // not run (see `DNS_REQUEST_PENDING` in the header).
    let tx = unsafe { Box::from_raw(ctx.cast::<mpsc::Sender<Answer>>().cast_mut()) }; // unsafe-code-exception: amendment-C8
    if result.is_null() {
        let _ = tx.send(Answer {
            status: -1,
            bytes: Vec::new(),
        });
        return;
    }
    // SAFETY: the result the API allocated for this call, valid until it is
    // freed below.
    let raw = unsafe { &*result }; // unsafe-code-exception: amendment-C8
    let mut bytes = Vec::new();
    if !raw.queryRawResponse.is_null() && raw.queryRawResponseSize > 0 {
        // SAFETY: the pointer and the length the result itself reports, for
        // a buffer the API owns until the free below. The bytes are copied,
        // so nothing borrows past that point.
        bytes = unsafe {
            // unsafe-code-exception: amendment-C8
            core::slice::from_raw_parts(raw.queryRawResponse, raw.queryRawResponseSize as usize)
        }
        .to_vec();
    }
    let status = raw.queryStatus;
    if let Some(api) = api() {
        // SAFETY: freed exactly once, after the copy above and with no
        // borrow outstanding. `api()` is `Some` because this routine is
        // only ever installed by `query`, which is only reached through it.
        unsafe { (api.free)(result) }; // unsafe-code-exception: amendment-C8
    }
    let _ = tx.send(Answer { status, bytes });
}

/// Asks the system resolver for `name`'s records of type `rtype`.
pub(super) fn query(api: &Api, name: &str, rtype: u16) -> Result<Vec<Record>, Error> {
    // Refused here for the same reason every other backend refuses here:
    // one answer for a name with no wire form, on every platform.
    query_name(name)?;
    let mut wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();

    let (tx, rx) = mpsc::channel::<Answer>();
    let context = Box::into_raw(Box::new(tx));

    // SAFETY: every field is either zero or written below; the struct is
    // plain data with no invariant of its own.
    let mut request: DNS_QUERY_RAW_REQUEST = unsafe { core::mem::zeroed() }; // unsafe-code-exception: amendment-C8
    request.version = DNS_QUERY_RAW_REQUEST_VERSION1;
    request.resultsVersion = DNS_QUERY_RAW_RESULTS_VERSION1;
    request.dnsQueryName = wide.as_mut_ptr();
    request.dnsQueryType = rtype;
    request.queryCompletionCallback = Some(completed);
    request.queryContext = context.cast::<c_void>();
    // **TCP, and it is not a preference.** `protocol` is required — a zero
    // is `ERROR_INVALID_PARAMETER` — and UDP answers a large RRSet with a
    // truncated message this crate would have to refuse. Measured: `CAA`
    // for `cloudflare.com` is 32 octets over UDP and 596 over TCP.
    request.protocol = DNS_PROTOCOL_TCP;

    // SAFETY: zeroed plain data, and it outlives the wait below.
    let mut cancel: DNS_QUERY_RAW_CANCEL = unsafe { core::mem::zeroed() }; // unsafe-code-exception: amendment-C8

    // SAFETY: `request` and the name buffer it points into are live for the
    // whole of this call and of the wait below, which is what makes
    // blocking on the channel a correctness requirement rather than a
    // convenience. `cancel` is a live local the callee fills in.
    let rc = unsafe { (api.query)(&raw const request, &raw mut cancel) }; // unsafe-code-exception: amendment-C8

    if rc != DNS_REQUEST_PENDING {
        // Measured: the completion routine does not run on this path, so
        // nothing will reclaim the context and this function must.
        // SAFETY: the pointer is still this function's only owner.
        drop(unsafe { Box::from_raw(context) }); // unsafe-code-exception: amendment-C8
        return Err(Error::Platform {
            code: rc.unsigned_abs(),
        });
    }

    // No timeout, deliberately. `DnsQueryRaw` bounds its own query the way
    // `res_query` does, and a bound of ours would be a second policy with a
    // number nobody measured — and, worse, one that returns while the
    // request still points at this stack frame.
    let answer = rx.recv().map_err(|_| Error::NoResponse)?;

    if answer.bytes.is_empty() {
        return Err(if answer.status == 0 {
            Error::NoResponse
        } else {
            Error::Platform {
                code: answer.status.unsigned_abs(),
            }
        });
    }
    // The status is not translated: a failure that came with a message —
    // `NXDOMAIN` did, measured — is read out of the message's own rcode by
    // the same walker every other platform uses, so there is one statement
    // of that rule rather than a second one in Windows' vocabulary.
    message::records(strip_tcp_length(&answer.bytes))
}

/// The message inside a TCP-framed answer.
///
/// RFC 1035 §4.2.2 prefixes a message sent over TCP with its own length,
/// and `DnsQueryRaw` hands the frame over as it arrived. The prefix is
/// **checked rather than assumed**: it is stripped only where those two
/// octets are exactly the length of the rest, which is what separates a
/// frame from a message that merely starts with plausible bytes.
fn strip_tcp_length(bytes: &[u8]) -> &[u8] {
    match bytes.split_at_checked(2) {
        Some((prefix, rest))
            if usize::from(u16::from_be_bytes([prefix[0], prefix[1]])) == rest.len() =>
        {
            rest
        }
        _ => bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TCP frame is unwrapped and a bare message is left alone, and the
    /// second case is what the length check buys: without it, an answer
    /// whose first two octets happened to read as a plausible length would
    /// silently lose its header.
    #[test]
    fn a_tcp_length_prefix_is_stripped_only_when_it_is_one() {
        let framed = [0x00, 0x03, 0xaa, 0xbb, 0xcc];
        assert_eq!(strip_tcp_length(&framed), &[0xaa, 0xbb, 0xcc]);

        // The measured UDP shape: the first two octets are the message ID
        // and say nothing about the length.
        let bare = [0x82, 0x5d, 0x81, 0x80, 0x00];
        assert_eq!(strip_tcp_length(&bare), &bare);

        assert_eq!(strip_tcp_length(&[0x00]), &[0x00]);
        assert_eq!(strip_tcp_length(&[]), &[] as &[u8]);
    }
}
