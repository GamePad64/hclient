//! Walking a DNS response into [`Record`]s.
//!
//! **This is the only code in the crate that reads bytes it did not
//! write**, so it is also the only code here with a fuzz-shaped risk. Two
//! things bound it, and both are proofs rather than conventions:
//!
//! - every read is an index into a slice through [`slice::get`], so a
//!   truncated answer is a `None` and never a panic;
//! - a compression chain is bounded by a **jump budget** and by the 255-
//!   octet name limit together. A budget alone would allow a chain that
//!   grows a name without bound; the length limit alone would allow a
//!   cycle between two pointers with no label between them, which appends
//!   nothing and never ends.
//!
//! Nothing here is `unsafe`, and the module is compiled and tested on
//! every host regardless of which backend the target selects — which is
//! this workspace's rule that the untestable half holds no decisions,
//! applied to the half that would otherwise be hardest to reach.

use crate::Record;
use crate::error::{Error, MalformedAnswer};
use crate::rdata::Parsed;
use std::time::Duration;

/// RFC 1035 §4.1.1.
const HEADER_LEN: usize = 12;
/// RFC 1035 §4.1.1 / RFC 6895 §2.3.
const RCODE_NOERROR: u8 = 0;
const RCODE_NXDOMAIN: u8 = 3;
/// RFC 1035 §2.3.4.
const MAX_LABEL_LEN: usize = 63;
const MAX_WIRE_NAME_LEN: usize = 255;
/// RFC 1035 §4.1.4: the two high bits of a label length select the form.
const LABEL_MASK: u8 = 0b1100_0000;
const LABEL_POINTER: u8 = 0b1100_0000;
/// A name has at most 127 labels, so a chain that jumps more times than
/// that is not resolving a name, it is looping.
const MAX_JUMPS: usize = 128;

/// RFC 1035 §4.1.1, as much of it as this crate acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    /// `QR` — set on a response. The one bit that separates *an answer
    /// arrived* from *the buffer we zeroed is still zeroed*, which is how
    /// a failed platform call is classified at all.
    pub(crate) is_response: bool,
    /// `TC` — the answer did not fit and the resolver's own TCP retry did
    /// not replace it.
    pub(crate) truncated: bool,
    pub(crate) rcode: u8,
    pub(crate) qdcount: u16,
    pub(crate) ancount: u16,
}

impl TryFrom<&[u8]> for Header {
    type Error = Error;

    /// The twelve-byte header, or the reason there is not one.
    ///
    /// The whole message is taken rather than exactly twelve octets,
    /// because the length that comes back short is the thing worth
    /// reporting: `HeaderTruncated` names what did arrive.
    fn try_from(msg: &[u8]) -> Result<Self, Self::Error> {
        let head: &[u8; HEADER_LEN] = msg
            .get(..HEADER_LEN)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::Malformed(MalformedAnswer::HeaderTruncated {
                got: msg.len(),
            }))?;
        Ok(Self {
            is_response: head[2] & 0x80 != 0,
            truncated: head[2] & 0x02 != 0,
            rcode: head[3] & 0x0F,
            qdcount: u16::from_be_bytes([head[4], head[5]]),
            ancount: u16::from_be_bytes([head[6], head[7]]),
        })
    }
}

/// What a twelve-byte header on its own means, when that is all there is.
///
/// `res_query` and `android_res_nresult` report the ordinary *this name
/// has no records of this type* as a **failure**, and on that path neither
/// returns a length — so all that can be trusted is the fixed-size header.
/// `Ok(())` is that case; every other reading is an error.
///
/// **A header claiming answers is [`Error::LengthUnavailable`] and not a
/// silent zero.** If a libc ever breaks the contract above, the records
/// are provably there and provably unreadable, because the failure path
/// returns no length to bound them with. Under-reporting that as "no
/// records" would turn a broken platform into a client that quietly stops
/// discovering anything.
pub(crate) fn header_only(msg: &[u8]) -> Result<(), Error> {
    let header = Header::try_from(msg)?;
    if !header.is_response {
        return Err(Error::NoResponse);
    }
    if header.truncated {
        return Err(Error::Truncated);
    }
    match header.rcode {
        RCODE_NOERROR => {}
        RCODE_NXDOMAIN => return Err(Error::NameDoesNotExist),
        rcode => return Err(Error::ResponseCode { rcode }),
    }
    if header.ancount == 0 {
        Ok(())
    } else {
        Err(Error::LengthUnavailable {
            ancount: header.ancount,
        })
    }
}

/// The answer section of `msg`, as records.
///
/// `Ok(vec![])` is *the name exists and has no records of this type*;
/// every other outcome is an [`Error`]. The authority and additional
/// sections are not walked: nothing this crate promises is in them, and an
/// `OPT` record in the additional section is EDNS0 machinery rather than
/// an answer.
///
/// Records of a type other than the one asked for are **kept**, not
/// filtered: a CNAME chain answers with the CNAME beside the records it
/// leads to, and dropping it here would hide from the caller that the
/// answer is for a different name than the question.
pub(crate) fn records(msg: &[u8]) -> Result<Vec<Record>, Error> {
    let header = Header::try_from(msg)?;
    if !header.is_response {
        return Err(Error::NoResponse);
    }
    if header.truncated {
        return Err(Error::Truncated);
    }
    match header.rcode {
        RCODE_NOERROR => {}
        RCODE_NXDOMAIN => return Err(Error::NameDoesNotExist),
        rcode => return Err(Error::ResponseCode { rcode }),
    }

    let malformed = |reason| Error::Malformed(reason);
    let mut at = HEADER_LEN;
    for _ in 0..header.qdcount {
        // A question is a name then QTYPE and QCLASS; the name itself is
        // stepped over rather than read, since nothing here reports it.
        at = skip_name(msg, at).ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))?;
        at = at
            .checked_add(4)
            .ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))?;
    }

    let mut found = Vec::with_capacity(usize::from(header.ancount));
    for _ in 0..header.ancount {
        let (name, next) = read_name(msg, at)?;
        at = next;
        // Every offset is built with `checked_add` and read with `get`, so
        // a length a sender chose can end this walk and cannot index past
        // the buffer or wrap into a small one.
        let fixed: &[u8; 10] = at
            .checked_add(10)
            .and_then(|end| msg.get(at..end))
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))?;
        let rtype = u16::from_be_bytes([fixed[0], fixed[1]]);
        let class = u16::from_be_bytes([fixed[2], fixed[3]]);
        let ttl = u32::from_be_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]);
        let rdlength = usize::from(u16::from_be_bytes([fixed[8], fixed[9]]));
        at += 10;
        let end = at
            .checked_add(rdlength)
            .filter(|end| *end <= msg.len())
            .ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))?;
        let rdata = expand_names(msg, rtype, at, end)?;
        at = end;
        found.push(Record {
            name,
            rtype,
            class,
            ttl: Duration::from_secs(u64::from(ttl)),
            rdata,
        });
    }
    Ok(found)
}

/// Where a name sits inside the RDATA of a type that may compress one.
///
/// RFC 1035 §4.1.4 lets a name inside RDATA be a pointer into the rest of
/// the message, and RFC 3597 §4 closes the list: **only the types defined
/// alongside compression may use it**, so this table cannot grow with the
/// registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameLayout {
    /// The whole RDATA: `NS`, `MD`, `MF`, `CNAME`, `MB`, `MG`, `MR`, `PTR`.
    One,
    /// Two names and nothing else: `MINFO`, `RP`.
    Two,
    /// A 16-bit value then a name: `MX`, `AFSDB`, `RT`.
    U16ThenOne,
    /// RFC 1035 §3.3.13.
    Soa,
    /// RFC 2782. Its target is **not** supposed to be compressed — the type
    /// postdates RFC 1035 — and it is here because senders do it anyway,
    /// and because expanding a name that was never compressed is the
    /// identity.
    Srv,
}

/// What [`NameLayout::try_from`] answers for a type with no name in its
/// RDATA — which is **most** types and the ordinary case, not a failure.
///
/// It is a named type rather than `()` so that the call site reads as the
/// question it is asking. Nothing constructs it but the conversion, and
/// nothing reads it: the one caller matches on `Ok` and treats everything
/// else as *hand the octets over untouched*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NoNameInRdata;

impl TryFrom<u16> for NameLayout {
    type Error = NoNameInRdata;

    fn try_from(rtype: u16) -> Result<Self, Self::Error> {
        Ok(match rtype {
            2 | 3 | 4 | 5 | 7 | 8 | 9 | 12 => Self::One,
            14 | 17 => Self::Two,
            15 | 18 | 21 => Self::U16ThenOne,
            6 => Self::Soa,
            33 => Self::Srv,
            _ => return Err(NoNameInRdata),
        })
    }
}

/// The record's RDATA with every name in it written out in full.
///
/// **A pointer is meaningless the moment the message is gone**, and this
/// crate hands back records rather than messages — so RDATA that still
/// carried one would be bytes a caller cannot read and cannot resolve. For
/// a type with no name in it, or one whose names cannot be compressed,
/// this is the octets as they arrived.
///
/// It is also what makes the two Windows paths agree. `DnsQuery_UTF8`
/// hands over a parsed structure holding a name in full, so
/// `sys/windows/parsed.rs` re-encodes it uncompressed; without this, the
/// same record read through `DnsQueryRaw` would differ by exactly the
/// compression the wire happened to use. The test comparing them found
/// that on `NS` the first time it ran.
fn expand_names(msg: &[u8], rtype: u16, at: usize, end: usize) -> Result<Vec<u8>, Error> {
    let malformed = |reason| Error::Malformed(reason);
    let verbatim = || {
        msg.get(at..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))
    };
    let Ok(layout) = NameLayout::try_from(rtype) else {
        return verbatim();
    };

    // A fixed-width prefix, read before the names that follow it.
    let numbers = match layout {
        NameLayout::One | NameLayout::Two | NameLayout::Soa => 0,
        NameLayout::U16ThenOne => 1,
        NameLayout::Srv => 3,
    };
    let mut cursor = at;
    let mut prefix = Vec::with_capacity(numbers * 2);
    for _ in 0..numbers {
        let pair: &[u8; 2] = msg
            .get(cursor..cursor + 2)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))?;
        prefix.extend_from_slice(pair);
        cursor += 2;
    }

    let (first, next) = read_name(msg, cursor)?;
    cursor = next;
    let parsed = match layout {
        NameLayout::One => Parsed::Name(first),
        NameLayout::Two => {
            let (second, _) = read_name(msg, cursor)?;
            Parsed::TwoNames(first, second)
        }
        NameLayout::U16ThenOne => {
            Parsed::NumberAndName(u16::from_be_bytes([prefix[0], prefix[1]]), first)
        }
        NameLayout::Srv => Parsed::Srv {
            priority: u16::from_be_bytes([prefix[0], prefix[1]]),
            weight: u16::from_be_bytes([prefix[2], prefix[3]]),
            port: u16::from_be_bytes([prefix[4], prefix[5]]),
            target: first,
        },
        NameLayout::Soa => {
            let (rname, after) = read_name(msg, cursor)?;
            let counters: &[u8; 20] = msg
                .get(after..after + 20)
                .and_then(|s| s.try_into().ok())
                .ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))?;
            let word = |i: usize| {
                u32::from_be_bytes([
                    counters[i * 4],
                    counters[i * 4 + 1],
                    counters[i * 4 + 2],
                    counters[i * 4 + 3],
                ])
            };
            Parsed::Soa {
                mname: first,
                rname,
                serial: word(0),
                refresh: word(1),
                retry: word(2),
                expire: word(3),
                minimum: word(4),
            }
        }
    };

    // A name this crate read and cannot write back is a name no resolver
    // reported — the encoder's bounds are RFC 1035's and the decoder
    // enforces the same ones — so the octets as they arrived are the
    // honest answer rather than a refusal of a record that is fine.
    parsed.to_vec().map_or_else(verbatim, Ok)
}

/// The offset just past the name at `at`, without reading it.
///
/// A pointer ends a name, so this never follows one and needs no budget —
/// which is the whole reason the question section is stepped over with
/// this rather than with [`read_name`].
fn skip_name(msg: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *msg.get(at)?;
        match len & LABEL_MASK {
            0 if len == 0 => return at.checked_add(1),
            0 => at = at.checked_add(1 + usize::from(len))?,
            LABEL_POINTER => return at.checked_add(2),
            _ => return None,
        }
    }
}

/// The name at `at`, and the offset just past it **in the record stream**
/// — which for a compressed name is two octets on, wherever the labels
/// themselves were read from.
fn read_name(msg: &[u8], at: usize) -> Result<(String, usize), Error> {
    let malformed = |reason| Error::Malformed(reason);
    let mut name = String::new();
    let mut cursor = at;
    let mut after: Option<usize> = None;
    let mut jumps = 0usize;

    loop {
        let len = *msg
            .get(cursor)
            .ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))?;
        match len & LABEL_MASK {
            LABEL_POINTER => {
                let second = *msg
                    .get(cursor + 1)
                    .ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))?;
                // The first pointer is what ends the name in the stream;
                // later ones are inside the chain and move nothing.
                after.get_or_insert(cursor + 2);
                jumps += 1;
                if jumps > MAX_JUMPS {
                    return Err(malformed(MalformedAnswer::PointerLoop));
                }
                cursor = usize::from(u16::from_be_bytes([len & !LABEL_MASK, second]));
            }
            0 if len == 0 => {
                let end = after.unwrap_or(cursor + 1);
                return Ok((name, end));
            }
            0 => {
                let len = usize::from(len);
                if len > MAX_LABEL_LEN {
                    // Unreachable through this arm — the mask above admits
                    // only 0..=63 — and stated rather than assumed, since
                    // the constant and the mask are two facts that could
                    // drift apart.
                    return Err(malformed(MalformedAnswer::NameTooLong));
                }
                let label = msg
                    .get(cursor + 1..cursor + 1 + len)
                    .ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))?;
                // The wire form is one length octet per label plus the
                // terminating zero, so this bound is the wire's rather
                // than the text's.
                if name.len() + len + 2 > MAX_WIRE_NAME_LEN {
                    return Err(malformed(MalformedAnswer::NameTooLong));
                }
                if !name.is_empty() {
                    name.push('.');
                }
                // A label is arbitrary octets on the wire. Lossy would
                // produce a name that resolves somewhere else or nowhere;
                // this crate hands back what it can read and refuses the
                // rest, which is the same direction every other refusal
                // here takes.
                name.push_str(
                    std::str::from_utf8(label)
                        .map_err(|_| malformed(MalformedAnswer::ReservedLabel))?,
                );
                cursor += 1 + len;
            }
            _ => return Err(malformed(MalformedAnswer::ReservedLabel)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// A real answer, captured through `res_query` on Linux for
    /// `cloudflare.com` type 65: a compressed owner name, a root
    /// TargetName, and an EDNS0 `OPT` record in the additional section
    /// that this walker must not reach for.
    const REAL_ANSWER: &str = concat!(
        "825d818000010001000000010a636c6f7564666c61726503636f6d0000410001",
        "c00c004100010000012c003d000100000100060268330268320004000868108",
        "4e5681085e500060020260647000000000000000000681084e5260647000000",
        "000000000000681085e5000029ffd6000000000000",
    );

    #[test]
    fn a_real_answer_walks_into_one_record_with_its_name_decompressed() {
        let found = records(&hex(REAL_ANSWER)).expect("walks");
        assert_eq!(found.len(), 1, "one answer, and the OPT is not one");
        let record = &found[0];
        assert_eq!(record.name, "cloudflare.com");
        assert_eq!(record.rtype, 65);
        assert_eq!(record.class, crate::CLASS_IN);
        assert_eq!(record.ttl, Duration::from_secs(300));
        assert_eq!(record.rdata.len(), 61);
        assert_eq!(&record.rdata[..3], &[0x00, 0x01, 0x00]);
    }

    /// The header's own three refusals, each a different instruction to a
    /// caller. `NXDOMAIN` is the one worth its own case: it is an answer
    /// and it is not an empty one.
    #[rstest::rstest]
    #[case::no_response("825d010000000000000000000000")]
    #[case::truncated("825d838000010000000000000000")]
    #[case::servfail("825d818200010000000000000000")]
    #[case::nxdomain("825d818300010000000000000000")]
    fn a_header_that_forbids_an_answer_is_not_an_empty_answer(#[case] head: &str) {
        assert_matches!(records(&hex(head)), Err(_));
    }

    #[test]
    fn nxdomain_and_no_records_are_different_values() {
        let nothing = records(&hex("825d818000010000000000000000")).expect("NOERROR walks");
        assert!(nothing.is_empty());
        assert_matches!(
            records(&hex("825d818300010000000000000000")),
            Err(Error::NameDoesNotExist)
        );
    }

    // ---- the twelve bytes, field by field ------------------------------
    //
    // Everything else here reaches the conversion through `records` or
    // `header_only`, which can only observe it through an outcome; these
    // read the fields directly, because a header field that lands in the
    // wrong variable is invisible from a distance whenever some other
    // field happens to reject the message first.
    //
    // `assert_eq!` on a whole `Header` is safe here: it is five primitives
    // with a derived `PartialEq`, so every field is genuinely compared.

    /// `ID`, flags, and the four RFC 1035 §4.1.1 counts, written out so a
    /// case below can put a value in one field and zero everywhere else.
    fn header_bytes(flags_hi: u8, flags_lo: u8, qdcount: u16, ancount: u16) -> Vec<u8> {
        let mut out = vec![0x12, 0x34, flags_hi, flags_lo];
        out.extend_from_slice(&qdcount.to_be_bytes());
        out.extend_from_slice(&ancount.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
        out
    }

    #[rstest::rstest]
    // The header of the captured answer, so at least one case is a shape a
    // resolver really produced rather than one this file invented.
    #[case::a_real_answer(
        hex(REAL_ANSWER)[..12].to_vec(),
        Header { is_response: true, truncated: false, rcode: 0, qdcount: 1, ancount: 1 }
    )]
    // The buffer a platform zeroed and nothing was written into. Every
    // field false or zero — and `is_response` false is the only thing that
    // keeps it from reading as a perfectly good NOERROR answer with no
    // records.
    #[case::nothing_arrived(
        vec![0u8; 12],
        Header { is_response: false, truncated: false, rcode: 0, qdcount: 0, ancount: 0 }
    )]
    // `AA` without `QR`: only bit 0x80 means "response".
    #[case::authoritative_but_not_a_response(
        header_bytes(0x40, 0x00, 0, 0),
        Header { is_response: false, truncated: false, rcode: 0, qdcount: 0, ancount: 0 }
    )]
    // `RD` is the neighbouring bit to `TC` and is set on nearly every real
    // query; reading it as truncation would reject almost every answer.
    #[case::recursion_desired_is_not_truncation(
        header_bytes(0x01, 0x00, 0, 0),
        Header { is_response: false, truncated: false, rcode: 0, qdcount: 0, ancount: 0 }
    )]
    #[case::truncated(
        header_bytes(0x02, 0x00, 0, 0),
        Header { is_response: false, truncated: true, rcode: 0, qdcount: 0, ancount: 0 }
    )]
    // RCODE shares its byte with `RA` and the `Z`/`AD`/`CD` bits, all of
    // which are set here. Unmasked, this reads as RCODE 243 and NXDOMAIN
    // stops being NXDOMAIN.
    #[case::the_rcode_is_the_low_nibble_only(
        header_bytes(0x82, 0xf3, 1, 0),
        Header { is_response: true, truncated: true, rcode: 3, qdcount: 1, ancount: 0 }
    )]
    // ANCOUNT is bytes 6..8, big-endian. Swapping the pair gives 513 and
    // reading QDCOUNT instead gives 1, so this one case rules out both.
    #[case::ancount_is_the_third_count_and_network_order(
        header_bytes(0x81, 0x80, 1, 258),
        Header { is_response: true, truncated: false, rcode: 0, qdcount: 1, ancount: 258 }
    )]
    // The mirror image: a huge QDCOUNT with no answers must still be no
    // answers.
    #[case::a_large_qdcount_is_not_an_ancount(
        header_bytes(0x81, 0x80, 0xffff, 0),
        Header { is_response: true, truncated: false, rcode: 0, qdcount: 0xffff, ancount: 0 }
    )]
    fn the_header_reads_each_field_from_its_own_bits(
        #[case] bytes: Vec<u8>,
        #[case] expected: Header,
    ) {
        assert_eq!(Header::try_from(&bytes[..]), Ok(expected));
    }

    /// Anything shorter than the fixed twelve octets is refused with the
    /// length it did have, and the boundary is exact: eleven is short,
    /// twelve is a header. The bound guards five reads into the fixed
    /// array, so an off-by-one here is a panic on a short answer rather
    /// than a wrong value.
    #[rstest::rstest]
    fn fewer_than_twelve_octets_is_never_a_header(#[values(0, 1, 6, 8, 11)] len: usize) {
        assert_eq!(
            Header::try_from(&vec![0u8; len][..]),
            Err(Error::Malformed(MalformedAnswer::HeaderTruncated {
                got: len
            }))
        );
    }

    #[test]
    fn exactly_twelve_octets_is_a_header() {
        assert_matches!(Header::try_from(&[0u8; 12][..]), Ok(_));
    }

    /// **The header on its own is classified by the same rules**, which is
    /// what `res_query`'s and Android's failure paths get: no length, so
    /// nothing but the twelve octets can be trusted.
    ///
    /// `Ok(())` means *no records of this type* — the ordinary outcome
    /// those calls report as a failure — and every other reading is an
    /// error, kept apart because a caller acts differently on each.
    #[rstest::rstest]
    #[case::no_records("825d818000010000000000000000", None)]
    #[case::nothing_arrived("000000000000000000000000", Some("NoResponse"))]
    #[case::truncated("825d838000010000000000000000", Some("Truncated"))]
    #[case::servfail("825d818200010000000000000000", Some("ResponseCode"))]
    #[case::nxdomain("825d818300010000000000000000", Some("NameDoesNotExist"))]
    #[case::claims_records("825d818000010001000000000000", Some("LengthUnavailable"))]
    fn a_bare_header_is_classified_by_the_rules_the_whole_message_uses(
        #[case] head: &str,
        #[case] expected: Option<&str>,
    ) {
        let got = header_only(&hex(head));
        match expected {
            None => assert_matches!(got, Ok(())),
            // Compared by variant name rather than by shape, so the table
            // above reads as the six distinct answers it is.
            Some(name) => {
                let err = got.expect_err("this header forbids an empty answer");
                let rendered = format!("{err:?}");
                assert!(
                    rendered.starts_with(name),
                    "expected {name}, got {rendered}"
                );
            }
        }
    }

    /// **A header claiming answers is not a silent zero**, and this is the
    /// case that would be one. `res_query` reports failure on a `NOERROR`
    /// response only when the answer section is empty; a libc that broke
    /// that would leave the records provably present and provably
    /// unreadable, because the path that reports it returns no length.
    #[test]
    fn a_header_claiming_records_over_a_failed_call_names_how_many() {
        assert_matches!(
            header_only(&hex("825d818000010003000000000000")),
            Err(Error::LengthUnavailable { ancount: 3 })
        );
    }

    /// **A compression pointer inside RDATA is expanded, not handed
    /// over.** This is the shape `NS`, `CNAME`, `MX` and `SOA` really
    /// arrive in — a pointer back into the question section — and a
    /// pointer is meaningless the moment the message is gone, which for a
    /// crate that hands back records is immediately.
    ///
    /// Found by running the two Windows calls against each other: the
    /// parsed one has the name in full and the raw one had `c0 0c`.
    #[test]
    fn a_compressed_name_inside_rdata_is_written_out_in_full() {
        let msg = hex(concat!(
            "825d818000010001 0000 0000", // one question, one answer
            "076578616d706c6503636f6d00", // example.com
            "0002 0001",                  // NS IN
            "c00c",                       // the owner, compressed
            "0002 0001 0000012c",         // NS IN, ttl 300
            "0005",                       // five octets of RDATA…
            "026e73 c00c",                // …"ns" then a pointer
        )
        .replace(' ', "")
        .as_str());
        let found = records(&msg).expect("walks");
        let record = found.first().expect("one answer");
        assert_eq!(record.name, "example.com");
        assert_eq!(
            record.rdata,
            hex("026e73076578616d706c6503636f6d00"),
            "the pointer must have become the name it points at"
        );
    }

    /// The control: a type with no name in its RDATA is handed over
    /// untouched, pointer-shaped bytes and all. Without it, an
    /// "expansion" that rewrote every record would pass the test above.
    #[test]
    fn rdata_with_no_name_in_it_is_handed_over_verbatim() {
        let msg = hex(concat!(
            "825d818000010001 0000 0000",
            "076578616d706c6503636f6d00",
            "0001 0001", // A IN
            "c00c",
            "0001 0001 0000012c",
            "0004",
            "c00cc00c", // four octets that look like
        ) // two pointers and are an address
        .replace(' ', "")
        .as_str());
        let found = records(&msg).expect("walks");
        assert_eq!(found.first().expect("one answer").rdata, hex("c00cc00c"));
    }

    /// **The pair that says the two bounds are both load-bearing.** A
    /// pointer to itself appends nothing, so the length limit never fires
    /// and only the jump budget ends it; a chain that appends a label per
    /// jump ends on the length limit first. Neither test covers the other.
    #[test]
    fn a_pointer_that_points_at_itself_terminates() {
        // Header, then one answer whose owner name is a pointer to its own
        // offset (12).
        let msg = hex("825d81800000000100000000c00c");
        assert_matches!(
            records(&msg),
            Err(Error::Malformed(MalformedAnswer::PointerLoop))
        );
    }

    #[test]
    fn a_chain_that_grows_a_name_without_end_is_refused_for_its_length() {
        // Offset 12: a one-octet label "a" then a pointer back to 12.
        let msg = hex("825d818000000001000000000161c00c");
        assert_matches!(
            records(&msg),
            Err(Error::Malformed(MalformedAnswer::NameTooLong))
        );
    }

    /// Every prefix of a real answer is a refusal and never a panic, and
    /// never a record built out of bytes that are not there. This is the
    /// property a fuzzer would look for, asserted exhaustively over one
    /// input because the input is short.
    #[test]
    fn no_prefix_of_a_real_answer_panics_or_invents_a_record() {
        let full = hex(REAL_ANSWER);
        let whole = records(&full).expect("the whole answer walks");
        for cut in 0..full.len() {
            if let Ok(found) = records(&full[..cut]) {
                assert!(
                    found.is_empty() || found == whole,
                    "a prefix produced records the whole answer does not have"
                );
            }
        }
    }

    /// A reserved label form (`0b01` / `0b10`) is refused rather than
    /// guessed at. RFC 1035 §4.1.4 defines neither, and a walker that
    /// treated one as a length would read a length nobody wrote.
    #[test]
    fn a_reserved_label_form_is_refused() {
        let msg = hex("825d8180000000010000000040");
        assert_matches!(
            records(&msg),
            Err(Error::Malformed(MalformedAnswer::ReservedLabel))
        );
    }
}
