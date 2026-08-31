//! `DnsQuery_UTF8` — the path every Windows has, and the only one a
//! Windows without `DnsQueryRaw` has.
//!
//! # What comes back depends on the type, and nothing in the record says so
//!
//! `DnsQuery_UTF8` fills in a `DNS_RECORDA` whose `Data` is a union with
//! **no discriminator**. Which member is live is decided by `wType`, and
//! the rule is readable from the Win32 metadata:
//!
//! - a type the union **names** — forty-three of them — arrives parsed
//!   into that member, with `wDataLength` the size of the structure;
//! - a type the union **does not name** arrives as the record's own
//!   **RDATA**, with `wDataLength` its length.
//!
//! # The forty-three are not the obscure ones, which is why they are
//! # re-encoded rather than refused
//!
//! They are `A`, `AAAA`, `MX`, `TXT`, `SRV`, `NS`, `SOA`, `CNAME`, `PTR`,
//! `DS`, `DNSKEY`, `TLSA` — essentially every record in everyday use. A
//! path that refused all of them would refuse the reason anyone asks a
//! system resolver at all, so twenty-six of them are read out of their
//! structure and written back into the RDATA the wire would have carried.
//! [`crate::rdata`] does the writing, and says what a synthesised RDATA is
//! and is not.
//!
//! Seventeen are still refused, by name and before a query, and
//! [`unsupported`] says which and why. A refusal is a thing a caller can
//! act on; handing back a structure's bytes as though they were RDATA is
//! the defect the measurement below found already shipped.
//!
//! # Measured, on Windows 11, 2026-09-01
//!
//! `cloudflare.com`, RR type 65: `wDataLength = 61` and the union's bytes
//! are `0001 00 0001 0006 02 68 33 02 68 32 …` — SVCB wire format,
//! priority then a root TargetName then `alpn=h3,h2`. `DNS_SVCB_DATA` is
//! 32 bytes on x64 with `pszTargetName` at offset 8; offset 8 here holds
//! `h3`. Those 61 octets are **byte-for-byte** the RDATA inside the
//! `res_query` answer captured on Linux that `message.rs`'s `REAL_ANSWER`
//! carries.
//!
//! The controls separate in both directions. `MX` came back as a structure
//! — `wDataLength = 16`, a pointer and a preference — and `CAA`, a type
//! with no union member and no `DNS_TYPE_CAA` constant at all, came back
//! as its RDATA verbatim: `00 09 "issuewild" "comodoca.com"`, 23 bytes.
//!
//! And the rule is confirmed from a second direction by the metadata
//! itself: `DNS_TYPE_HTTPS`, `DNS_TYPE_CERT` and `DNS_TYPE_LOC` all exist
//! as constants with **no** union member — so it is the union that
//! decides, not the constant list.
//!
//! # Why this and not `DnsQueryEx`
//!
//! `DnsQuery_UTF8` returns `DNS_RECORDA` — narrow throughout, so `PSTR` is
//! unambiguously the right type for the names this module reads.
//! `DnsQueryEx` takes a `PCWSTR` and returns Unicode records; reading a
//! UTF-16 name through a `PSTR` yields a one-character name, which for a
//! resolver is a silently wrong answer rather than a crash.
//! `DNS_QUERY_RETURN_MESSAGE` is a third dead end: it hands back a
//! `DNS_MESSAGE_BUFFER` with no length field, and measured on the same
//! machine it is inert through both `DnsQuery_UTF8` and `DnsQueryEx`.
#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "reading the record list `DnsQuery_UTF8` allocated is the only way to reach a DNS record on a Windows without DnsQueryRaw; see spec amendment C8"
)]

use crate::error::Error;
use crate::rdata::{self, Parsed};
use crate::sys::query_name;
use crate::{CLASS_IN, Record};
use core::ffi::c_void;
use core::ptr::null_mut;
use std::sync::OnceLock;
use std::time::Duration;
use windows_sys::Win32::Foundation::{DNS_ERROR_RCODE_NAME_ERROR, ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::core::PSTR;
// The long paths are imported rather than written inline, and that is not
// only taste: `cargo fmt` breaks an over-long `unsafe { … }` across lines,
// which strands the `unsafe-code-exception` marker three lines away from
// the `unsafe` keyword and fails the `no-unsafe-code` job. Keeping each
// such expression on one line keeps the marker where the job can see it.
use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_QUERY_STANDARD, DNS_RECORDA, DnsFree, DnsFreeRecordList, DnsQuery_UTF8,
};

/// Which member of the data union is live. One value per distinct
/// `DNS_*_DATA` layout rather than one per type: `NS`, `CNAME`, `PTR` and
/// six more all arrive as `DNS_PTR_DATAA`, and `MX`, `AFSDB` and `RT` all
/// arrive as `DNS_MX_DATAA`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    A,
    Aaaa,
    Name,
    TwoNames,
    NumberAndName,
    Strings,
    Soa,
    Srv,
    Naptr,
    Tlsa,
    Ds,
    Key,
}

/// Every type named by `DNS_RECORDA`'s data union, and therefore every
/// type that does **not** arrive as RDATA.
///
/// **Written as the metadata's own constants rather than as numbers**, so
/// a reader can check the list against the union by name and a value
/// corrected upstream is corrected here. The order is numeric, which is
/// the order the union is written in.
///
/// `DNS_TYPE_TEXT` is Windows' name for `TXT` (16); there is no
/// `DNS_TYPE_TXT`, while the union member is spelled `TXT`. That mismatch
/// is exactly why this list was derived by reading both and not by
/// upper-casing one.
const PARSED_BY_WINDOWS: &[u16] = {
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

/// The types this path answers for by re-encoding, and the shape each
/// arrives in.
const SYNTHESISED: &[(u16, Shape)] = {
    use windows_sys::Win32::NetworkManagement::Dns as d;
    &[
        (d::DNS_TYPE_A, Shape::A),
        (d::DNS_TYPE_NS, Shape::Name),
        (d::DNS_TYPE_MD, Shape::Name),
        (d::DNS_TYPE_MF, Shape::Name),
        (d::DNS_TYPE_CNAME, Shape::Name),
        (d::DNS_TYPE_SOA, Shape::Soa),
        (d::DNS_TYPE_MB, Shape::Name),
        (d::DNS_TYPE_MG, Shape::Name),
        (d::DNS_TYPE_MR, Shape::Name),
        (d::DNS_TYPE_PTR, Shape::Name),
        (d::DNS_TYPE_HINFO, Shape::Strings),
        (d::DNS_TYPE_MINFO, Shape::TwoNames),
        (d::DNS_TYPE_MX, Shape::NumberAndName),
        (d::DNS_TYPE_TEXT, Shape::Strings),
        (d::DNS_TYPE_RP, Shape::TwoNames),
        (d::DNS_TYPE_AFSDB, Shape::NumberAndName),
        (d::DNS_TYPE_X25, Shape::Strings),
        (d::DNS_TYPE_ISDN, Shape::Strings),
        (d::DNS_TYPE_RT, Shape::NumberAndName),
        (d::DNS_TYPE_KEY, Shape::Key),
        (d::DNS_TYPE_AAAA, Shape::Aaaa),
        (d::DNS_TYPE_SRV, Shape::Srv),
        (d::DNS_TYPE_NAPTR, Shape::Naptr),
        (d::DNS_TYPE_DNAME, Shape::Name),
        (d::DNS_TYPE_DS, Shape::Ds),
        (d::DNS_TYPE_DNSKEY, Shape::Key),
        (d::DNS_TYPE_TLSA, Shape::Tlsa),
    ]
};

/// The shape a type arrives in, or `None` where this path cannot answer.
fn shape_of(rtype: u16) -> Option<Shape> {
    SYNTHESISED
        .iter()
        .find(|(candidate, _)| *candidate == rtype)
        .map(|(_, shape)| *shape)
}

/// The types Windows parses that this path still cannot answer for.
///
/// **Derived rather than written down**, because a third list is a third
/// thing to forget: it is exactly [`PARSED_BY_WINDOWS`] minus the types
/// [`SYNTHESISED`] names.
///
/// What is on it, and why each is left: **`SIG` and `RRSIG`** carry a
/// signature over a canonical form this module would have to reproduce
/// exactly or hand back a record that fails validation; **`NSEC`, `NXT`,
/// `NSEC3` and `NSEC3PARAM`** carry type bitmaps their structures do not
/// expose as one; **`SVCB`** is a parameter list whose union is tagged per
/// parameter, so it is a re-encoder of its own — and `HTTPS`, the type
/// this workspace actually wants, is not parsed by Windows at all and
/// arrives raw. **`OPT`, `TKEY` and `TSIG`** are protocol machinery rather
/// than answers; **`WKS`, `ATMA`, `NULL`, `DHCID`, `WINS` and `WINSR`**
/// have either no consumer here or no wire form worth guessing at.
pub(super) fn unsupported() -> &'static [u16] {
    static LIST: OnceLock<Vec<u16>> = OnceLock::new();
    LIST.get_or_init(|| {
        PARSED_BY_WINDOWS
            .iter()
            .copied()
            .filter(|rtype| shape_of(*rtype).is_none())
            .collect()
    })
}

/// `DNS_INFO_NO_RECORDS`. The name resolved and has no records of this
/// type — an answer, not a failure. `windows-sys` declares this one as
/// `i32` while the query returns `WIN32_ERROR` (`u32`), so it is restated
/// here with the type the comparison actually needs.
const DNS_INFO_NO_RECORDS: WIN32_ERROR = 9501;

/// Frees the record list `DnsQuery_UTF8` allocated, on every exit path.
///
/// A guard rather than a call at the end: the walk below returns early on
/// a record it cannot read, and a `Drop` impl is the only way to be sure
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
/// on the platforms that hand over a message: this path would have to know
/// that type's shape too, and the one place it matters — a CNAME beside
/// the answer — is a fact the caller can obtain by asking for `CNAME`.
/// Stated rather than papered over.
pub(super) fn query(name: &str, rtype: u16) -> Result<Vec<Record>, Error> {
    let cname = query_name(name)?;
    let shape = shape_of(rtype);

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

    let unreadable = || Error::Malformed(crate::MalformedAnswer::RecordTruncated);
    let mut found = Vec::new();
    let mut cursor = records.0;
    while !cursor.is_null() {
        // SAFETY: `cursor` is either the head `DnsQuery_UTF8` returned or a
        // `pNext` from a record in that same list, and is checked non-null
        // immediately above. The list is owned by `records` and is not
        // freed until this function returns.
        let record = unsafe { &*cursor }; // unsafe-code-exception: amendment-C8
        if record.wType == rtype {
            // SAFETY: `shape` was derived from the same `rtype` the record
            // reports, which is what selects the union's member; `None`
            // means the type is not one the union names, so the union is
            // the RDATA itself.
            let parsed = unsafe { read_union(record, shape) }; // unsafe-code-exception: amendment-C8
            found.push(Record {
                name: owner_name(record).ok_or_else(unreadable)?,
                rtype,
                // Not read back: `DNS_RECORDA` has no class field. This is
                // the class that was asked for, which is the only class
                // this crate ever asks for.
                class: CLASS_IN,
                ttl: Duration::from_secs(u64::from(record.dwTtl)),
                rdata: parsed
                    .as_ref()
                    .and_then(rdata::encode)
                    .ok_or_else(unreadable)?,
            });
        }
        cursor = record.pNext;
    }
    Ok(found)
}

/// The record's payload, as owned values with no pointer left in them.
///
/// `None` where a string the structure points at is not valid UTF-8, which
/// for a name would mean a different host and for a character-string means
/// Windows handed back something it did not read as text.
///
/// # Safety
///
/// `shape` must be `shape_of(record.wType)`: it is what selects the live
/// member of the untagged data union, and reading the wrong one is reading
/// a different type's bytes as pointers.
unsafe fn read_union(record: &DNS_RECORDA, shape: Option<Shape>) -> Option<Parsed> {
    // unsafe-code-exception: amendment-C8
    let Some(shape) = shape else {
        // Not a type the union names, so the union IS the RDATA and
        // `wDataLength` is its length. **The one length this path trusts,
        // and it does not come from the DNS response** — the OS wrote it
        // beside the buffer it allocated.
        //
        // SAFETY: `Data` is where the payload begins and `wDataLength` is
        // the record's own count for it. The slice borrows for this
        // expression; the `Vec` owns its bytes.
        let bytes = unsafe {
            // unsafe-code-exception: amendment-C8
            core::slice::from_raw_parts(
                std::ptr::from_ref(&record.Data).cast::<u8>(),
                usize::from(record.wDataLength),
            )
        };
        return Some(Parsed::Raw(bytes.to_vec()));
    };

    // SAFETY: each arm reads the member `shape` names, and `shape` was
    // derived from `wType` — see this function's contract. Every pointer
    // the members hold is one `dnsapi` allocated with the record and
    // NUL-terminates; each is copied into an owned value here, so nothing
    // borrows past the list's lifetime.
    unsafe {
        // unsafe-code-exception: amendment-C8
        Some(match shape {
            // `IP4_ADDRESS` is a `DWORD` holding the address in NETWORK
            // byte order, so the bytes as they sit in memory already read
            // a.b.c.d. `to_ne_bytes` is that, and says so; `from(u32)`
            // would reverse them on a little-endian target and silently
            // name a different host.
            Shape::A => Parsed::A(record.Data.A.IpAddress.to_ne_bytes()),
            Shape::Aaaa => Parsed::Aaaa(record.Data.AAAA.Ip6Address.IP6Byte),
            Shape::Name => Parsed::Name(pstr(record.Data.PTR.pNameHost)?),
            Shape::TwoNames => Parsed::TwoNames(
                pstr(record.Data.MINFO.pNameMailbox)?,
                pstr(record.Data.MINFO.pNameErrorsMailbox)?,
            ),
            Shape::NumberAndName => Parsed::NumberAndName(
                record.Data.MX.wPreference,
                pstr(record.Data.MX.pNameExchange)?,
            ),
            Shape::Strings => {
                let txt = &record.Data.TXT;
                let first = std::ptr::from_ref(&txt.pStringArray).cast::<PSTR>();
                let mut strings = Vec::with_capacity(txt.dwStringCount as usize);
                for index in 0..txt.dwStringCount as usize {
                    strings.push(pstr_bytes(*first.add(index))?);
                }
                Parsed::Strings(strings)
            }
            Shape::Soa => {
                let soa = &record.Data.SOA;
                Parsed::Soa {
                    mname: pstr(soa.pNamePrimaryServer)?,
                    rname: pstr(soa.pNameAdministrator)?,
                    serial: soa.dwSerialNo,
                    refresh: soa.dwRefresh,
                    retry: soa.dwRetry,
                    expire: soa.dwExpire,
                    minimum: soa.dwDefaultTtl,
                }
            }
            Shape::Srv => {
                let srv = &record.Data.SRV;
                Parsed::Srv {
                    priority: srv.wPriority,
                    weight: srv.wWeight,
                    port: srv.wPort,
                    target: pstr(srv.pNameTarget)?,
                }
            }
            Shape::Naptr => {
                let naptr = &record.Data.NAPTR;
                Parsed::Naptr {
                    order: naptr.wOrder,
                    preference: naptr.wPreference,
                    flags: pstr_bytes(naptr.pFlags)?,
                    service: pstr_bytes(naptr.pService)?,
                    regexp: pstr_bytes(naptr.pRegularExpression)?,
                    replacement: pstr(naptr.pReplacement)?,
                }
            }
            Shape::Tlsa => {
                let tlsa = &record.Data.TLSA;
                Parsed::Tlsa {
                    usage: tlsa.bCertUsage,
                    selector: tlsa.bSelector,
                    matching: tlsa.bMatchingType,
                    data: flexible(
                        std::ptr::from_ref(&tlsa.bCertificateAssociationData).cast::<u8>(),
                        tlsa.bCertificateAssociationDataLength,
                    ),
                }
            }
            Shape::Ds => {
                let ds = &record.Data.DS;
                Parsed::Ds {
                    key_tag: ds.wKeyTag,
                    algorithm: ds.chAlgorithm,
                    digest_type: ds.chDigestType,
                    digest: flexible(
                        std::ptr::from_ref(&ds.Digest).cast::<u8>(),
                        ds.wDigestLength,
                    ),
                }
            }
            Shape::Key => {
                let key = &record.Data.KEY;
                Parsed::Key {
                    flags: key.wFlags,
                    protocol: key.chProtocol,
                    algorithm: key.chAlgorithm,
                    key: flexible(std::ptr::from_ref(&key.Key).cast::<u8>(), key.wKeyLength),
                }
            }
        })
    }
}

/// The flexible array at `first`, of the length the structure beside it
/// reports.
///
/// # Safety
///
/// `first` must point at a flexible array member of a `DNS_RECORD` the OS
/// allocated, and `len` must be that structure's own count for it.
unsafe fn flexible(first: *const u8, len: u16) -> Vec<u8> {
    // unsafe-code-exception: amendment-C8
    // SAFETY: the caller's contract. The slice borrows for this
    // expression; the `Vec` owns its bytes.
    unsafe { core::slice::from_raw_parts(first, usize::from(len)) }.to_vec() // unsafe-code-exception: amendment-C8
}

/// A NUL-terminated string from `dnsapi`, as text.
///
/// `None` on invalid UTF-8 rather than a lossy conversion: these become
/// DNS names, and a replacement character in one names a different host.
///
/// # Safety
///
/// `ptr` must be null or a NUL-terminated string the OS allocated.
unsafe fn pstr(ptr: PSTR) -> Option<String> {
    // unsafe-code-exception: amendment-C8
    // SAFETY: the caller's contract; `from_ptr` is reached only for a
    // non-null pointer.
    let bytes = unsafe { pstr_bytes(ptr) }?; // unsafe-code-exception: amendment-C8
    String::from_utf8(bytes).ok()
}

/// The same, as octets — for a character-string, which RFC 1035 §3.3 does
/// not require to be text.
///
/// # Safety
///
/// As [`pstr`].
unsafe fn pstr_bytes(ptr: PSTR) -> Option<Vec<u8>> {
    // unsafe-code-exception: amendment-C8
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null by the check above, and `dnsapi` NUL-terminates the
    // strings in a record it allocated. The `CStr` borrows for this
    // expression only.
    let text = unsafe { core::ffi::CStr::from_ptr(ptr.cast()) }; // unsafe-code-exception: amendment-C8
    Some(text.to_bytes().to_vec())
}

/// The record's owner name, without a trailing dot; the root becomes the
/// empty string.
fn owner_name(record: &DNS_RECORDA) -> Option<String> {
    // SAFETY: `pName` is the name `dnsapi` allocated with the record.
    let text = unsafe { pstr(record.pName) }?; // unsafe-code-exception: amendment-C8
    Some(if text == "." {
        String::new()
    } else {
        text.strip_suffix('.').unwrap_or(&text).to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two lists are what make this path's support matrix a promise
    /// rather than a guess, so their shape is asserted: sorted, without
    /// duplicates, and every synthesised type is one the union actually
    /// names — a shape written for a type Windows hands over raw would be
    /// dead code that looks like coverage.
    #[test]
    fn the_two_lists_agree_with_each_other_and_with_what_was_measured() {
        assert!(
            PARSED_BY_WINDOWS.windows(2).all(|pair| pair[0] < pair[1]),
            "sorted and duplicate-free, so `contains` reads as a set"
        );
        for (rtype, _) in SYNTHESISED {
            assert!(
                PARSED_BY_WINDOWS.contains(rtype),
                "type {rtype} has a shape but is not one the union names"
            );
        }
        // Measured as structures, and all four are re-encoded rather than
        // refused.
        for parsed in [1u16, 15, 16, 52] {
            assert!(PARSED_BY_WINDOWS.contains(&parsed), "type {parsed}");
            assert!(shape_of(parsed).is_some(), "type {parsed}");
        }
        // Measured as RDATA. `65` is the one this workspace depends on and
        // `257` is the control that has no `DNS_TYPE_` constant at all.
        for raw in [65u16, 257] {
            assert!(!PARSED_BY_WINDOWS.contains(&raw), "type {raw}");
            assert!(shape_of(raw).is_none(), "type {raw}");
        }
    }

    /// The refusal list is exactly what is left over, and it is checked
    /// against its own doc: the types named there and no others.
    #[test]
    fn the_refusal_list_is_the_leftover_and_holds_only_what_it_names() {
        use windows_sys::Win32::NetworkManagement::Dns as d;
        let mut expected = vec![
            d::DNS_TYPE_NULL,
            d::DNS_TYPE_WKS,
            d::DNS_TYPE_SIG,
            d::DNS_TYPE_NXT,
            d::DNS_TYPE_ATMA,
            d::DNS_TYPE_OPT,
            d::DNS_TYPE_RRSIG,
            d::DNS_TYPE_NSEC,
            d::DNS_TYPE_DHCID,
            d::DNS_TYPE_NSEC3,
            d::DNS_TYPE_NSEC3PARAM,
            d::DNS_TYPE_SVCB,
            d::DNS_TYPE_TKEY,
            d::DNS_TYPE_TSIG,
            d::DNS_TYPE_WINS,
            d::DNS_TYPE_WINSR,
        ];
        expected.sort_unstable();
        assert_eq!(unsupported(), expected);
        assert_eq!(
            PARSED_BY_WINDOWS.len(),
            SYNTHESISED.len() + unsupported().len(),
            "every type the union names is either re-encoded or refused"
        );
    }
}
