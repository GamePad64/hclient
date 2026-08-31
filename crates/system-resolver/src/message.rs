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

/// The twelve-byte header, or the reason there is not one.
pub(crate) fn read_header(msg: &[u8]) -> Result<Header, Error> {
    let head: &[u8; HEADER_LEN] = msg
        .get(..HEADER_LEN)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::Malformed(MalformedAnswer::HeaderTruncated {
            got: msg.len(),
        }))?;
    Ok(Header {
        is_response: head[2] & 0x80 != 0,
        truncated: head[2] & 0x02 != 0,
        rcode: head[3] & 0x0F,
        qdcount: u16::from_be_bytes([head[4], head[5]]),
        ancount: u16::from_be_bytes([head[6], head[7]]),
    })
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
    let header = read_header(msg)?;
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
    let header = read_header(msg)?;
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
        let rdata = at
            .checked_add(rdlength)
            .and_then(|end| msg.get(at..end))
            .ok_or_else(|| malformed(MalformedAnswer::RecordTruncated))?;
        at += rdlength;
        found.push(Record {
            name,
            rtype,
            class,
            ttl: Duration::from_secs(u64::from(ttl)),
            rdata: rdata.to_vec(),
        });
    }
    Ok(found)
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
