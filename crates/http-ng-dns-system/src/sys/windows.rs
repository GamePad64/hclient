//! HTTPS/SVCB on Windows, using the record the operating system has
//! already parsed.
//!
//! # Why this does not parse DNS bytes at all
//!
//! For `wType == DNS_TYPE_HTTPS` the DNS Client service hands back
//! `DNS_SVCB_DATA` — priority, target name, and a counted array of
//! `DNS_SVCB_PARAM`. Each parameter carries `wSvcParamKey`, and **that key
//! is the discriminator for the parameter's own union**
//! (`DNS_SVCB_PARAM_0`: `pAlpn`, `pIpv4Hints`, `pIpv6Hints`, `pMandatory`,
//! `wPort`, `pszDohPath`, `pUnknown`). So the SVCB wire format is decoded
//! by the OS, and this module only walks a structure with a tag in it.
//! Nothing here reads a length out of a DNS response; the only count it
//! trusts is `cSvcParams`, which the OS wrote next to the array it
//! allocated.
//!
//! **An earlier version of this file got that wrong and is worth recording
//! so the mistake is not repeated.** It looked at the OUTER union,
//! `DNS_RECORDW.Data`, saw no discriminator for it, concluded that a
//! structured Windows path was unsafe in principle, and reached instead for
//! `DnsQueryRaw` — the only Windows call that returns the raw wire response
//! *with its length* — so the bytes could go through the shared RFC 9460
//! decoder. That worked, but it cost: `DnsQueryRaw` exists only on Windows
//! 11 / Server 2025, `windows-link` emits it as a static `raw-dylib`
//! import, and an absent import stops the process at load time, so the
//! symbol had to be fetched with `GetProcAddress` and the capability had to
//! become a run-time answer. All of that machinery was solving a problem
//! that does not exist: `wType` already says the payload is
//! `DNS_SVCB_DATA`, and the inner union is tagged.
//!
//! Two dead ends from that round remain real, and are recorded in spec
//! amendment C8 rather than here: `DNS_QUERY_RETURN_MESSAGE` hands back a
//! `DNS_MESSAGE_BUFFER` with no length field, and a parser without a buffer
//! end cannot detect an overrun because the read that would reveal it is
//! already out of bounds.
//!
//! # Why `DnsQuery_UTF8` and not `DnsQueryEx`
//!
//! Both are applicable and both are statically linkable. `DnsQuery_UTF8` is
//! chosen for one specific reason: **charset**. `DnsQueryEx` takes its name
//! as `PCWSTR` and therefore returns Unicode records, but `windows-sys`
//! 0.61.2 declares exactly one `DNS_SVCB_DATA` — there is no
//! `DNS_SVCB_DATAW` — and its `pszTargetName` is `PSTR`, a *narrow*
//! pointer, in both the `DNS_RECORDA` and `DNS_RECORDW` unions. Either the
//! Win32 metadata models no A/W split for this struct because none exists,
//! or it models it imprecisely; the two cannot be told apart from the
//! bindings, and no machine was available to settle it by running the code.
//! Reading a UTF-16 target name through a `PSTR` yields a one-character
//! host name — a silently *wrong host to connect to*, which is worse than a
//! crash because nothing announces it.
//!
//! `DnsQuery_UTF8` removes the question rather than answering it: its
//! results are `DNS_RECORDA`, narrow throughout, so `PSTR` is
//! unambiguously the right type. Same OS-parsed structure, same static
//! link, same availability. If the project ever learns that `DNS_SVCB_DATA`
//! is genuinely narrow-only, switching to `DnsQueryEx` is a small change
//! and buys nothing this module needs.
//!
//! # What is established, and what is taken on someone's word
//!
//! Stated separately on purpose, because they carry different weight.
//!
//! - **Read from the bindings** (`windows-sys` 0.61.2,
//!   `Win32/NetworkManagement/Dns/mod.rs`): the declarations of
//!   `DnsQuery_UTF8`, `DNS_RECORDA`, `DNS_SVCB_DATA`, `DNS_SVCB_PARAM` and
//!   its union, and that `wSvcParamKey` is that union's discriminator.
//! - **Taken on the project owner's word, not verified here:** that Windows
//!   parses HTTPS records into `DNS_SVCB_DATA` from **Windows 10 onward**,
//!   which is what makes a static link and a compile-time capability
//!   correct. The presence of `DNS_SVCB_DATA` in the Win32 metadata does
//!   *not* establish this — the metadata is not versioned by OS release. If
//!   the claim is wrong for some older Windows, that OS would return the
//!   record's payload in `DNS_UNKNOWN_DATA` instead, and reading it as
//!   `DNS_SVCB_DATA` would dereference `pszTargetName` built from raw
//!   response bytes. There is no in-process check that distinguishes the
//!   two: the outer union is untagged, and comparing `wDataLength` against
//!   `size_of::<DNS_SVCB_DATA>()` is a coincidence of sizes, not a tag.
//! - **Never executed.** Every line here type-checks under `cargo check`
//!   and `cargo clippy --target x86_64-pc-windows-msvc`, and the
//!   compilation was confirmed with a planted `compile_error!` rather than
//!   trusted to a warm cache. No part of this file has ever run: there is
//!   no Windows machine in the environment that produced it. The RFC 9460
//!   semantics it feeds (`svcb::endpoint_from_binding`) are shared with the
//!   Unix path and covered by that path's byte-vector tests; that sharing
//!   is the only reason any part of the Windows result can be called
//!   tested.
#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "reading the DNS_SVCB_DATA the OS parsed is the only way to reach an HTTPS record on Windows; see spec amendment C8"
)]

use super::SvcbLookupError;
use crate::svcb::{RawBinding, RawParam, endpoint_from_binding};
use core::ffi::c_void;
use core::ptr::null_mut;
use http_ng_dns::SvcbEndpoint;
use std::ffi::CString;
use std::net::{Ipv4Addr, Ipv6Addr};
use windows_sys::Win32::Foundation::{DNS_ERROR_RCODE_NAME_ERROR, ERROR_SUCCESS, WIN32_ERROR};
// The long paths are imported rather than written inline, and that is not
// only taste: `cargo fmt` breaks an over-long `unsafe { … }` across lines,
// which strands the `unsafe-code-exception` marker three lines away from
// the `unsafe` keyword and fails the `no-unsafe-code` job. Keeping each
// such expression on one line keeps the marker where the job can see it.
use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_QUERY_STANDARD, DNS_RECORDA, DNS_SVCB_PARAM, DNS_SVCB_PARAM_ALPN_ID, DNS_TYPE_HTTPS,
    DnsFree, DnsFreeRecordList, DnsQuery_UTF8, IP6_ADDRESS,
};

/// Compile-time, and back to a constant deliberately. The run-time form
/// this crate briefly had existed only to report whether `GetProcAddress`
/// had found `DnsQueryRaw`; with a static link to `DnsQuery_UTF8` — present
/// since Windows 2000 — the answer is decided by the build again, and a
/// function that always returns a constant would be machinery without
/// honesty to show for it.
pub(crate) const SUPPORTS_SVCB: bool = true;

/// `DNS_INFO_NO_RECORDS`. The name resolved and has no records of this
/// type — the ordinary outcome for a host that publishes no HTTPS record,
/// and emphatically not an error. `windows-sys` declares this one as `i32`
/// while the query returns `WIN32_ERROR` (`u32`), so it is restated here
/// with the type the comparison actually needs.
const DNS_INFO_NO_RECORDS: WIN32_ERROR = 9501;

/// RFC 1035 §2.3.4 — the wire form of a name is at most 255 octets.
const MAX_NAME_LEN: usize = 255;

/// SvcParamKeys this module reads a value for (RFC 9460 §14.3.2).
const KEY_MANDATORY: u16 = 0;
const KEY_ALPN: u16 = 1;
const KEY_NO_DEFAULT_ALPN: u16 = 2;
const KEY_PORT: u16 = 3;
const KEY_IPV4HINT: u16 = 4;
const KEY_ECH: u16 = 5;
const KEY_IPV6HINT: u16 = 6;

/// Frees the record list `DnsQuery_UTF8` allocated, on every exit path.
///
/// A guard rather than a call at the end: the walk below returns early on a
/// malformed `mandatory` list, and a `Drop` impl is the only way to be sure
/// that path frees too. `dnsapi` owns the allocation; nothing in this
/// module keeps a pointer into it past `lookup`.
struct RecordList(*mut DNS_RECORDA);

impl Drop for RecordList {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` came from `DnsQuery_UTF8`'s out-parameter and
            // has not been freed before — this type is the only owner, it
            // is not `Clone`, and the pointer is never written after
            // construction. `DnsFreeRecordList` is the documented free
            // mode for that allocation.
            unsafe { DnsFree(self.0.cast::<c_void>(), DnsFreeRecordList) }; // unsafe-code-exception: amendment-C8
        }
    }
}

/// Asks the system resolver for `name`'s HTTPS (RR type 65) records.
///
/// Blocking — `DnsQuery_UTF8` does its own I/O. The caller runs this on a
/// `Blocking` thread; see the crate root.
pub(crate) fn lookup(name: &str) -> Result<Vec<SvcbEndpoint>, SvcbLookupError> {
    let unusable = || SvcbLookupError::NameNotUsable {
        name: name.to_owned(),
    };
    if name.len() > MAX_NAME_LEN {
        return Err(unusable());
    }
    let cname = CString::new(name).map_err(|_| unusable())?;

    let mut records: *mut DNS_RECORDA = null_mut();
    // SAFETY: `cname` is a NUL-terminated C string that outlives the call.
    // `records` is a live local the callee fills in; `pextra` and
    // `preserved` are documented as optional and are passed null. The call
    // retains no pointer to anything owned here.
    let status = unsafe {
        // unsafe-code-exception: amendment-C8
        DnsQuery_UTF8(
            cname.as_ptr().cast(),
            DNS_TYPE_HTTPS,
            DNS_QUERY_STANDARD,
            null_mut(),
            &raw mut records,
            null_mut(),
        )
    };
    // Bind the guard immediately, before anything that can return.
    let records = RecordList(records);

    match status {
        ERROR_SUCCESS => {}
        // Both are definitive "there are no HTTPS records for this name",
        // not failures to ask — the same line the Unix path draws between
        // NODATA/NXDOMAIN and a resolver that could not answer. A caller
        // looking for "this host does not exist" finds it in the A/AAAA
        // stream, where it belongs.
        DNS_INFO_NO_RECORDS | DNS_ERROR_RCODE_NAME_ERROR => return Ok(Vec::new()),
        code => return Err(SvcbLookupError::WindowsDnsError { code }),
    }

    let mut found = Vec::new();
    let mut cursor = records.0;
    while !cursor.is_null() {
        // SAFETY: `cursor` is either the head `DnsQuery_UTF8` returned or a
        // `pNext` from a record in that same list, and is checked non-null
        // immediately above. The list is owned by `records` and is not
        // freed until this function returns.
        let record = unsafe { &*cursor }; // unsafe-code-exception: amendment-C8
        if record.wType == DNS_TYPE_HTTPS
            && let Some(binding) = binding_from_record(record)
            && let Some(endpoint) = endpoint_from_binding(&binding)?
        {
            found.push(endpoint);
        }
        cursor = record.pNext;
    }
    Ok(found)
}

/// One `DNS_RECORDA` known to be an HTTPS record, in the backend-neutral
/// form `svcb` applies RFC 9460's client rules to.
///
/// `None` when the record cannot be read as one — today only a target name
/// that is not valid UTF-8, which cannot be a host name and must not be
/// lossily converted into one that resolves elsewhere.
fn binding_from_record(record: &DNS_RECORDA) -> Option<RawBinding> {
    // SAFETY: the caller checked `wType == DNS_TYPE_HTTPS`, which is what
    // selects `SVCB` out of `Data`. See this module's header for what that
    // rests on, including the part taken on the owner's word.
    let svcb = unsafe { record.Data.SVCB }; // unsafe-code-exception: amendment-C8

    let owner = pstr_to_string(record.pName)?;
    // A target of `.` is the root, which `RawBinding` spells as the empty
    // string; `pszTargetName` may also be null for it.
    let target = if svcb.pszTargetName.is_null() {
        String::new()
    } else {
        let text = pstr_to_string(svcb.pszTargetName)?;
        if text == "." { String::new() } else { text }
    };

    let mut params = Vec::new();
    if !svcb.pSvcParams.is_null() {
        for index in 0..usize::from(svcb.cSvcParams) {
            // SAFETY: `cSvcParams` is the OS's own count for the array it
            // allocated at `pSvcParams`, and `index` is strictly below it.
            // This is the only count this module trusts, and it does not
            // come from the DNS response — it comes from the struct beside
            // the array.
            let parameter = unsafe { &*svcb.pSvcParams.add(index) }; // unsafe-code-exception: amendment-C8
            if let Some(parsed) = read_parameter(parameter) {
                params.push(parsed);
            }
        }
    }

    Some(RawBinding {
        priority: svcb.wSvcPriority,
        owner: strip_root_dot(owner),
        target: strip_root_dot(target),
        params,
    })
}

/// One `DNS_SVCB_PARAM`, dispatched on `wSvcParamKey` — the union's own
/// discriminator, which is the whole reason this module is sound.
///
/// `None` drops a parameter whose pointer the OS left null; the record
/// around it stays usable, and a `mandatory` entry naming that key is then
/// correctly reported as absent by `endpoint_from_binding`.
fn read_parameter(parameter: &DNS_SVCB_PARAM) -> Option<RawParam> {
    let key = parameter.wSvcParamKey;
    match key {
        KEY_MANDATORY => {
            // SAFETY: `wSvcParamKey == 0` selects `pMandatory`.
            let list = unsafe { parameter.Anonymous.pMandatory }; // unsafe-code-exception: amendment-C8
            if list.is_null() {
                return None;
            }
            // SAFETY: non-null, and `cMandatoryKeys` is the OS's count for
            // the flexible array that follows it in the same allocation.
            let (count, first) = unsafe {
                // unsafe-code-exception: amendment-C8
                (
                    (*list).cMandatoryKeys,
                    (&raw const (*list).rgwMandatoryKeys).cast::<u16>(),
                )
            };
            let mut keys = Vec::with_capacity(usize::from(count));
            for index in 0..usize::from(count) {
                // SAFETY: `index` is strictly below the OS's own count.
                keys.push(unsafe { *first.add(index) }); // unsafe-code-exception: amendment-C8
            }
            Some(RawParam::Mandatory(keys))
        }
        KEY_ALPN => {
            // SAFETY: `wSvcParamKey == 1` selects `pAlpn`.
            let list = unsafe { parameter.Anonymous.pAlpn }; // unsafe-code-exception: amendment-C8
            if list.is_null() {
                return None;
            }
            // SAFETY: non-null, and `cIds` counts the ids in the flexible
            // array that follows.
            let count = unsafe { (*list).cIds }; // unsafe-code-exception: amendment-C8
            let first = unsafe { (&raw const (*list).rgIds).cast::<DNS_SVCB_PARAM_ALPN_ID>() }; // unsafe-code-exception: amendment-C8
            let mut ids = Vec::with_capacity(usize::from(count));
            for index in 0..usize::from(count) {
                // SAFETY: `index` is strictly below the OS's count, and
                // each id's `cBytes` is the OS's length for the buffer at
                // its own `pbId`.
                let id = unsafe { *first.add(index) }; // unsafe-code-exception: amendment-C8
                if id.pbId.is_null() {
                    continue;
                }
                let bytes = unsafe { core::slice::from_raw_parts(id.pbId, usize::from(id.cBytes)) }; // unsafe-code-exception: amendment-C8
                ids.push(bytes.to_vec());
            }
            Some(RawParam::Alpn(ids))
        }
        KEY_NO_DEFAULT_ALPN => Some(RawParam::NoDefaultAlpn),
        // The one union member that is a value, not a pointer.
        // SAFETY: `wSvcParamKey == 3` selects `wPort`.
        KEY_PORT => Some(RawParam::Port(unsafe { parameter.Anonymous.wPort })), // unsafe-code-exception: amendment-C8
        KEY_IPV4HINT => {
            // SAFETY: `wSvcParamKey == 4` selects `pIpv4Hints`.
            let list = unsafe { parameter.Anonymous.pIpv4Hints }; // unsafe-code-exception: amendment-C8
            if list.is_null() {
                return None;
            }
            // SAFETY: non-null, `cIps` counts the addresses that follow.
            let (count, first) =
                unsafe { ((*list).cIps, (&raw const (*list).rgIps).cast::<u32>()) }; // unsafe-code-exception: amendment-C8
            let mut hints = Vec::with_capacity(usize::from(count));
            for index in 0..usize::from(count) {
                // SAFETY: `index` is strictly below the OS's count.
                let raw = unsafe { *first.add(index) }; // unsafe-code-exception: amendment-C8
                // `IP4_ADDRESS` is a `DWORD` holding the address in NETWORK
                // byte order, so the bytes as they sit in memory already
                // read a.b.c.d. `to_ne_bytes` is that, and says so; a
                // `from(u32)` would reverse them on a little-endian target
                // and silently produce a different host.
                hints.push(Ipv4Addr::from(raw.to_ne_bytes()));
            }
            Some(RawParam::Ipv4Hint(hints))
        }
        KEY_IPV6HINT => {
            // SAFETY: `wSvcParamKey == 6` selects `pIpv6Hints`.
            let list = unsafe { parameter.Anonymous.pIpv6Hints }; // unsafe-code-exception: amendment-C8
            if list.is_null() {
                return None;
            }
            // SAFETY: non-null, `cIps` counts the addresses that follow.
            let count = unsafe { (*list).cIps }; // unsafe-code-exception: amendment-C8
            let first = unsafe { (&raw const (*list).rgIps).cast::<IP6_ADDRESS>() }; // unsafe-code-exception: amendment-C8
            let mut hints = Vec::with_capacity(usize::from(count));
            for index in 0..usize::from(count) {
                // SAFETY: `index` is strictly below the OS's count.
                // `IP6Byte` is the 16 address octets in network order,
                // which is exactly what `Ipv6Addr::from` wants.
                let raw = unsafe { (*first.add(index)).IP6Byte }; // unsafe-code-exception: amendment-C8
                hints.push(Ipv6Addr::from(raw));
            }
            Some(RawParam::Ipv6Hint(hints))
        }
        KEY_ECH => {
            // There is no `pEch` member: ECH arrives through `pUnknown` as
            // the raw SvcParamValue. That is the form RFC 9460 §7.3
            // defines — an ECHConfigList "including the redundant length
            // prefix" — and the form rustls parses, so unlike the Unix
            // decoder (which strips the prefix and has it added back), this
            // path passes the bytes through untouched.
            //
            // SAFETY: `wSvcParamKey == 5` is not one of the members the
            // union names individually, so it selects `pUnknown`.
            let value = unsafe { parameter.Anonymous.pUnknown }; // unsafe-code-exception: amendment-C8
            if value.is_null() {
                return None;
            }
            // SAFETY: non-null, and `cBytes` is the OS's length for the
            // flexible array that follows it.
            let count = unsafe { (*value).cBytes }; // unsafe-code-exception: amendment-C8
            let first = unsafe { (&raw const (*value).pbSvcParamValue).cast::<u8>() }; // unsafe-code-exception: amendment-C8
            let bytes = unsafe { core::slice::from_raw_parts(first, usize::from(count)) }; // unsafe-code-exception: amendment-C8
            Some(RawParam::Ech(bytes.to_vec()))
        }
        // Everything else — `dohpath` (7) included — is carried as its key
        // number only. That is all `endpoint_from_binding` needs to honour
        // RFC 9460 §8, and `SvcbEndpoint` has nowhere to put a value for
        // it. No pointer in the union is dereferenced on this path.
        other => Some(RawParam::Other(other)),
    }
}

/// A NUL-terminated narrow string from `dnsapi`, as an owned `String`.
///
/// `None` on invalid UTF-8 rather than a lossy conversion: these strings
/// become host names, and a replacement character in one produces a name
/// that resolves somewhere else or nowhere at all. `DnsQuery_UTF8`'s
/// results are UTF-8 by construction, so this is a guard, not an expected
/// path.
fn pstr_to_string(ptr: windows_sys::core::PSTR) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null, and `dnsapi` NUL-terminates the strings in a record
    // it allocated. The `CStr` borrows for the length of this expression
    // only; the `String` below owns its bytes.
    let text = unsafe { core::ffi::CStr::from_ptr(ptr.cast()) }; // unsafe-code-exception: amendment-C8
    text.to_str().ok().map(str::to_owned)
}

/// `example.com.` becomes `example.com`; a bare `.` becomes empty.
fn strip_root_dot(name: String) -> String {
    if name == "." {
        return String::new();
    }
    match name.strip_suffix('.') {
        Some(stripped) => stripped.to_owned(),
        None => name,
    }
}
