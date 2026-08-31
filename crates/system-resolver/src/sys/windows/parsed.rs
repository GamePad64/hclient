//! `DnsQuery_UTF8` — the path every Windows has, and the only one Windows
//! 10 has.
//!
//! # What comes back depends on the type, and nothing in the record says so
//!
//! `DnsQuery_UTF8` fills in a `DNS_RECORDA` whose `Data` is a union with
//! **no discriminator**. Which member is live is decided by `wType`, and
//! the rule is readable from the Win32 metadata:
//!
//! - a type the union **names** — `A`, `MX`, `SOA`, `TLSA`, `DS`,
//!   `DNSKEY`, `SVCB` and thirty-six more — arrives parsed into that
//!   member, with `wDataLength` the size of the structure;
//! - a type the union **does not name** arrives as the record's own
//!   **RDATA**, with `wDataLength` its length.
//!
//! So this path answers [`Support::AnyExcept`], listing the first group.
//! `DNS_TYPE_SVCB` is 64; `HTTPS` is **65** and the union names no member
//! for it — so `HTTPS` works here, and so do `CAA`, `CERT`, `LOC`, `SSHFP`,
//! `OPENPGPKEY` and most of the registry.
//!
//! # Measured, on Windows 11, 2026-09-01
//!
//! `cloudflare.com`, RR type 65: `wDataLength = 61` and the union's bytes
//! are `0001 00 0001 0006 02 68 33 02 68 32 …` — SVCB wire format,
//! priority then a root TargetName then `alpn=h3,h2`. `DNS_SVCB_DATA` is 32
//! bytes on x64 with `pszTargetName` at offset 8; offset 8 here holds `h3`.
//! Those 61 octets are **byte-for-byte** the RDATA inside the `res_query`
//! answer captured on Linux that `message.rs`'s `REAL_ANSWER` carries.
//!
//! The controls separate in both directions. `MX` came back as a structure
//! — `wDataLength = 16`, a pointer and a preference — and `CAA`, a type
//! with no union member and no `DNS_TYPE_CAA` constant at all, came back as
//! its RDATA verbatim: `00 09 "issuewild" "comodoca.com"` in 23 bytes. The
//! metadata confirms it from a second direction: `DNS_TYPE_HTTPS`,
//! `DNS_TYPE_CERT` and `DNS_TYPE_LOC` all exist as constants with **no**
//! union member, so it is the union that decides and not the constant list.
//!
//! # Why this and not `DnsQueryEx`
//!
//! `DnsQuery_UTF8` returns `DNS_RECORDA` — narrow throughout, so `PSTR` is
//! unambiguously the right type for the owner name, which is the one string
//! this module reads. `DnsQueryEx` takes a `PCWSTR` and returns Unicode
//! records; reading a UTF-16 name through a `PSTR` yields a one-character
//! name, which for a resolver is a silently wrong answer rather than a
//! crash. `DNS_QUERY_RETURN_MESSAGE` is a third dead end: it hands back a
//! `DNS_MESSAGE_BUFFER` with no length field, and measured on the same
//! machine it is inert through both `DnsQuery_UTF8` and `DnsQueryEx`.
//!
//! [`Support::AnyExcept`]: crate::Support::AnyExcept
#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "walking the record list `DnsQuery_UTF8` allocated is the only way to reach a DNS record on a Windows without DnsQueryRaw; see spec amendment C8"
)]

use crate::error::Error;
use crate::sys::query_name;
use crate::{CLASS_IN, Record};
use core::ffi::c_void;
use core::ptr::null_mut;
use std::time::Duration;
use windows_sys::Win32::Foundation::{DNS_ERROR_RCODE_NAME_ERROR, ERROR_SUCCESS, WIN32_ERROR};
// The long paths are imported rather than written inline, and that is not
// only taste: `cargo fmt` breaks an over-long `unsafe { … }` across lines,
// which strands the `unsafe-code-exception` marker three lines away from
// the `unsafe` keyword and fails the `no-unsafe-code` job. Keeping each
// such expression on one line keeps the marker where the job can see it.
use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_QUERY_STANDARD, DNS_RECORDA, DnsFree, DnsFreeRecordList, DnsQuery_UTF8,
};

/// Every type named by `DNS_RECORDA`'s data union, and therefore every type
/// this path must refuse.
///
/// **Written as the metadata's own constants rather than as numbers**, so a
/// reader can check the list against the union by name and a value
/// corrected upstream is corrected here. The order is numeric, which is the
/// order the union is written in.
///
/// `DNS_TYPE_TEXT` is Windows' name for `TXT` (16); there is no
/// `DNS_TYPE_TXT`, while the union member is spelled `TXT`. That mismatch
/// is exactly why this list was derived by reading both and not by
/// upper-casing one.
pub(super) const PARSED_BY_WINDOWS: &[u16] = {
    use windows_sys::Win32::NetworkManagement::Dns as d;
    &[
        d::DNS_TYPE_A,
        d::DNS_TYPE_NS,
        d::DNS_TYPE_MD,
        d::DNS_TYPE_MF,
        d::DNS_TYPE_CNAME,
        d::DNS_TYPE_SOA,
        d::DNS_TYPE_MB,
        d::DNS_TYPE_MG,
        d::DNS_TYPE_MR,
        d::DNS_TYPE_NULL,
        d::DNS_TYPE_WKS,
        d::DNS_TYPE_PTR,
        d::DNS_TYPE_HINFO,
        d::DNS_TYPE_MINFO,
        d::DNS_TYPE_MX,
        d::DNS_TYPE_TEXT,
        d::DNS_TYPE_RP,
        d::DNS_TYPE_AFSDB,
        d::DNS_TYPE_X25,
        d::DNS_TYPE_ISDN,
        d::DNS_TYPE_RT,
        d::DNS_TYPE_SIG,
        d::DNS_TYPE_KEY,
        d::DNS_TYPE_AAAA,
        d::DNS_TYPE_NXT,
        d::DNS_TYPE_SRV,
        d::DNS_TYPE_ATMA,
        d::DNS_TYPE_NAPTR,
        d::DNS_TYPE_DNAME,
        d::DNS_TYPE_OPT,
        d::DNS_TYPE_DS,
        d::DNS_TYPE_RRSIG,
        d::DNS_TYPE_NSEC,
        d::DNS_TYPE_DNSKEY,
        d::DNS_TYPE_DHCID,
        d::DNS_TYPE_NSEC3,
        d::DNS_TYPE_NSEC3PARAM,
        d::DNS_TYPE_TLSA,
        d::DNS_TYPE_SVCB,
        d::DNS_TYPE_TKEY,
        d::DNS_TYPE_TSIG,
        d::DNS_TYPE_WINS,
        d::DNS_TYPE_WINSR,
    ]
};

/// `DNS_INFO_NO_RECORDS`. The name resolved and has no records of this
/// type — an answer, not a failure. `windows-sys` declares this one as
/// `i32` while the query returns `WIN32_ERROR` (`u32`), so it is restated
/// here with the type the comparison actually needs.
const DNS_INFO_NO_RECORDS: WIN32_ERROR = 9501;

/// Frees the record list `DnsQuery_UTF8` allocated, on every exit path.
///
/// A guard rather than a call at the end: the walk below returns early on a
/// name that cannot be read, and a `Drop` impl is the only way to be sure
/// that path frees too. `dnsapi` owns the allocation; nothing in this
/// module keeps a pointer into it past [`query`].
struct RecordList(*mut DNS_RECORDA);

impl Drop for RecordList {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` came from `DnsQuery_UTF8`'s out-parameter
            // and has not been freed before — this type is the only owner,
            // it is not `Clone`, and the pointer is never written after
            // construction. `DnsFreeRecordList` is the documented free mode
            // for that allocation.
            unsafe { DnsFree(self.0.cast::<c_void>(), DnsFreeRecordList) }; // unsafe-code-exception: amendment-C8
        }
    }
}

/// Asks the system resolver for `name`'s records of type `rtype`.
///
/// Records of a type other than the one asked for are **skipped**, unlike
/// on the platforms that hand over a message: a `DNS_RECORD` of another
/// type is a parsed structure of that type, and this path has no RDATA for
/// it to hand back. That asymmetry is this call's, and it is stated rather
/// than papered over — a CNAME beside the answer is visible everywhere else
/// and not here.
pub(super) fn query(name: &str, rtype: u16) -> Result<Vec<Record>, Error> {
    let cname = query_name(name)?;

    let mut records: *mut DNS_RECORDA = null_mut();
    // SAFETY: `cname` is a NUL-terminated C string that outlives the call.
    // `records` is a live local the callee fills in; `pextra` and
    // `preserved` are documented as optional and are passed null. The call
    // retains no pointer to anything owned here.
    let status = unsafe {
        // unsafe-code-exception: amendment-C8
        DnsQuery_UTF8(
            cname.as_ptr().cast(),
            rtype,
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
        // The name exists and has nothing of this type. An answer.
        DNS_INFO_NO_RECORDS => return Ok(Vec::new()),
        // And this is a different answer, kept apart for the reason
        // `Error::NameDoesNotExist` states.
        DNS_ERROR_RCODE_NAME_ERROR => return Err(Error::NameDoesNotExist),
        code => return Err(Error::Platform { code }),
    }

    let mut found = Vec::new();
    let mut cursor = records.0;
    while !cursor.is_null() {
        // SAFETY: `cursor` is either the head `DnsQuery_UTF8` returned or a
        // `pNext` from a record in that same list, and is checked non-null
        // immediately above. The list is owned by `records` and is not
        // freed until this function returns.
        let record = unsafe { &*cursor }; // unsafe-code-exception: amendment-C8
        if record.wType == rtype {
            found.push(Record {
                name: owner_name(record).ok_or_else(|| Error::NameNotUsable {
                    name: name.to_owned(),
                })?,
                rtype,
                // Not read back: `DNS_RECORDA` has no class field. This is
                // the class that was asked for, which is the only class
                // this crate ever asks for.
                class: CLASS_IN,
                ttl: Duration::from_secs(u64::from(record.dwTtl)),
                rdata: rdata(record),
            });
        }
        cursor = record.pNext;
    }
    Ok(found)
}

/// The record's RDATA: `wDataLength` bytes from where the union begins.
///
/// **The one length this path trusts, and it does not come from the DNS
/// response** — it comes from the fixed part of the record, written by the
/// OS beside the buffer it allocated. For a type in [`PARSED_BY_WINDOWS`]
/// these would be a structure's bytes rather than RDATA, which is why the
/// support matrix refuses those before a query is ever made.
fn rdata(record: &DNS_RECORDA) -> Vec<u8> {
    // SAFETY: `wDataLength` is the record's own count for its data, and
    // `Data` is where that data begins. The slice borrows for the length of
    // this expression; the `Vec` below owns its bytes.
    let bytes = unsafe {
        // unsafe-code-exception: amendment-C8
        core::slice::from_raw_parts(
            std::ptr::from_ref(&record.Data).cast::<u8>(),
            usize::from(record.wDataLength),
        )
    };
    bytes.to_vec()
}

/// The record's owner name, without a trailing dot; the root becomes the
/// empty string.
///
/// `None` on invalid UTF-8 rather than a lossy conversion: this is a DNS
/// name, and a replacement character in one names a different host.
/// `DnsQuery_UTF8`'s results are UTF-8 by construction, so this is a guard,
/// not an expected path.
fn owner_name(record: &DNS_RECORDA) -> Option<String> {
    if record.pName.is_null() {
        return None;
    }
    // SAFETY: non-null, and `dnsapi` NUL-terminates the strings in a record
    // it allocated. The `CStr` borrows for the length of this expression
    // only; the `String` below owns its bytes.
    let text = unsafe { core::ffi::CStr::from_ptr(record.pName.cast()) }; // unsafe-code-exception: amendment-C8
    let text = text.to_str().ok()?;
    Some(if text == "." {
        String::new()
    } else {
        text.strip_suffix('.').unwrap_or(text).to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list is what makes this path's support matrix a promise rather
    /// than a guess, so its shape is asserted: sorted, without duplicates,
    /// and holding the types measured on both sides of the rule.
    #[test]
    fn the_parsed_list_is_sorted_and_holds_what_was_measured() {
        assert!(
            PARSED_BY_WINDOWS.windows(2).all(|pair| pair[0] < pair[1]),
            "sorted and duplicate-free, so `contains` reads as a set"
        );
        // Measured as structures.
        for parsed in [1u16, 15, 16, 52, 64] {
            assert!(PARSED_BY_WINDOWS.contains(&parsed), "type {parsed}");
        }
        // Measured as RDATA. `65` is the one this workspace depends on and
        // `257` is the control that has no `DNS_TYPE_` constant at all.
        for raw in [65u16, 257] {
            assert!(!PARSED_BY_WINDOWS.contains(&raw), "type {raw}");
        }
    }
}
