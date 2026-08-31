//! Putting a record Windows took apart back together.
//!
//! # Why this exists at all
//!
//! `DnsQuery_UTF8` parses forty-three record types into structures of
//! their own, and those are **not** the types nobody asks for — they are
//! `A`, `AAAA`, `MX`, `TXT`, `SRV`, `NS`, `SOA`, `CNAME`, `PTR`, `DS`,
//! `DNSKEY`, `TLSA`, essentially every record in everyday use. A Windows
//! without `DnsQueryRaw` that refused all of them would refuse the reason
//! anyone asks a system resolver in the first place.
//!
//! So the structure is encoded back into the RDATA the wire would have
//! carried. Each type's rule is RFC 1035 §3.3 or its own RFC, and each is
//! a handful of lines.
//!
//! # What a synthesised RDATA is, and what it is not
//!
//! **It is not the octets that arrived.** It is what Windows understood,
//! written out again, and the differences are worth knowing before
//! trusting one byte for byte:
//!
//! - **Names come back uncompressed.** On the wire a name in RDATA may be
//!   a pointer into the rest of the message; there is no message here to
//!   point into. For every type below that is the *correct* form anyway —
//!   RFC 3597 §4 forbids compression in the RDATA of a new type, and a
//!   consumer that compares RDATA byte for byte across resolvers would
//!   already be wrong.
//! - **Case is Windows'.** DNS names are case-insensitive and a resolver
//!   may return either case; nothing here restores what the origin sent.
//! - **A character-string cannot carry a NUL.** Windows hands `TXT`,
//!   `HINFO`, `X25`, `ISDN` and `NAPTR` strings back as NUL-terminated C
//!   strings, so an octet the RFCs permit is not representable. A record
//!   containing one is truncated **by Windows** before this code sees it,
//!   which is why this is stated rather than checked.
//! - **A type the OS parsed and this module cannot re-encode is refused**,
//!   by name and before a query. That is a shorter list than the
//!   forty-three, and it is in `parsed.rs` beside the one this module
//!   answers for.
//!
//! # Why this file has no `unsafe` in it
//!
//! Reading the union is `parsed.rs`'s, and it hands over [`Parsed`] —
//! owned values with no pointer in them. Encoding is a pure function over
//! that, so every rule here is tested on any host rather than on the one
//! platform that produces the input. That is this workspace's rule that
//! the untestable half holds no decisions, applied to the half that is
//! easiest to get quietly wrong.
//!
//! On Windows 11 the answer is checked against the real thing: both calls
//! exist there, so `sys/windows/mod.rs`'s test compares a synthesised
//! RDATA with the octets `DnsQueryRaw` reports for the same name, over a
//! record of every shape below.
#![forbid(unsafe_code)]

/// One record as Windows understood it, with every pointer already read
/// into an owned value.
///
/// The variants are **shapes** rather than types: `NS`, `CNAME`, `PTR` and
/// six more all arrive as one name, and `MX`, `AFSDB` and `RT` all arrive
/// as a number and a name. Grouping them is what keeps this file short,
/// and the mapping from a type number to a shape is `parsed.rs`'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// A type Windows did not interpret: the octets as they arrived, and
    /// the only variant that is not a synthesis.
    Raw(Vec<u8>),
    /// RFC 1035 §3.4.1.
    A([u8; 4]),
    /// RFC 3596 §2.2.
    Aaaa([u8; 16]),
    /// One domain name: `NS`, `MD`, `MF`, `CNAME`, `MB`, `MG`, `MR`,
    /// `PTR`, `DNAME`.
    Name(String),
    /// Two domain names: `MINFO`, `RP`.
    TwoNames(String, String),
    /// A 16-bit value then a name: `MX`, `AFSDB`, `RT`.
    NumberAndName(u16, String),
    /// A sequence of character-strings: `TXT`, `HINFO`, `X25`, `ISDN`.
    Strings(Vec<Vec<u8>>),
    /// RFC 1035 §3.3.13.
    Soa {
        mname: String,
        rname: String,
        serial: u32,
        refresh: u32,
        retry: u32,
        expire: u32,
        minimum: u32,
    },
    /// RFC 2782.
    Srv {
        priority: u16,
        weight: u16,
        port: u16,
        target: String,
    },
    /// RFC 2915.
    Naptr {
        order: u16,
        preference: u16,
        flags: Vec<u8>,
        service: Vec<u8>,
        regexp: Vec<u8>,
        replacement: String,
    },
    /// RFC 6698 §2.1.
    Tlsa {
        usage: u8,
        selector: u8,
        matching: u8,
        data: Vec<u8>,
    },
    /// RFC 4034 §5.1.
    Ds {
        key_tag: u16,
        algorithm: u8,
        digest_type: u8,
        digest: Vec<u8>,
    },
    /// RFC 4034 §2.1 — `DNSKEY`, and `KEY` shares the layout.
    Key {
        flags: u16,
        protocol: u8,
        algorithm: u8,
        key: Vec<u8>,
    },
}

/// RFC 1035 §2.3.4 and §3.3.
const MAX_LABEL_LEN: usize = 63;
const MAX_WIRE_NAME_LEN: usize = 255;
/// A character-string is a length octet and up to 255 octets.
const MAX_STRING_LEN: usize = 255;

impl Parsed {
    /// The RDATA for a record of this shape, or `None` where the value cannot
    /// be one.
    ///
    /// `None` is a refusal rather than a best effort: a name too long to
    /// encode or a string over 255 octets is not something a resolver can have
    /// reported, so producing shorter bytes would be inventing a record.
    pub(crate) fn to_vec(&self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        match self {
            Parsed::Raw(bytes) => out.extend_from_slice(bytes),
            Parsed::A(octets) => out.extend_from_slice(octets),
            Parsed::Aaaa(octets) => out.extend_from_slice(octets),
            Parsed::Name(name) => out.extend(wire_name(name)?),
            Parsed::TwoNames(first, second) => {
                out.extend(wire_name(first)?);
                out.extend(wire_name(second)?);
            }
            Parsed::NumberAndName(number, name) => {
                out.extend_from_slice(&number.to_be_bytes());
                out.extend(wire_name(name)?);
            }
            Parsed::Strings(strings) => {
                for string in strings {
                    out.extend(character_string(string)?);
                }
            }
            Parsed::Soa {
                mname,
                rname,
                serial,
                refresh,
                retry,
                expire,
                minimum,
            } => {
                out.extend(wire_name(mname)?);
                out.extend(wire_name(rname)?);
                for field in [serial, refresh, retry, expire, minimum] {
                    out.extend_from_slice(&field.to_be_bytes());
                }
            }
            Parsed::Srv {
                priority,
                weight,
                port,
                target,
            } => {
                for field in [priority, weight, port] {
                    out.extend_from_slice(&field.to_be_bytes());
                }
                out.extend(wire_name(target)?);
            }
            Parsed::Naptr {
                order,
                preference,
                flags,
                service,
                regexp,
                replacement,
            } => {
                out.extend_from_slice(&order.to_be_bytes());
                out.extend_from_slice(&preference.to_be_bytes());
                out.extend(character_string(flags)?);
                out.extend(character_string(service)?);
                out.extend(character_string(regexp)?);
                out.extend(wire_name(replacement)?);
            }
            Parsed::Tlsa {
                usage,
                selector,
                matching,
                data,
            } => {
                out.extend_from_slice(&[*usage, *selector, *matching]);
                out.extend_from_slice(data);
            }
            Parsed::Ds {
                key_tag,
                algorithm,
                digest_type,
                digest,
            } => {
                out.extend_from_slice(&key_tag.to_be_bytes());
                out.extend_from_slice(&[*algorithm, *digest_type]);
                out.extend_from_slice(digest);
            }
            Parsed::Key {
                flags,
                protocol,
                algorithm,
                key,
            } => {
                out.extend_from_slice(&flags.to_be_bytes());
                out.extend_from_slice(&[*protocol, *algorithm]);
                out.extend_from_slice(key);
            }
        }
        Some(out)
    }
}

/// `example.com` as length-prefixed labels, or `None` for a name with no
/// wire form.
///
/// The root is the empty string or a bare `.`, and a trailing dot is
/// dropped — Windows writes names without one, but a caller of this file's
/// tests may not.
fn wire_name(name: &str) -> Option<Vec<u8>> {
    let name = name.strip_suffix('.').unwrap_or(name);
    let mut out = Vec::with_capacity(name.len() + 2);
    if !name.is_empty() {
        for label in name.split('.') {
            if label.is_empty() || label.len() > MAX_LABEL_LEN {
                return None;
            }
            out.push(u8::try_from(label.len()).ok()?);
            out.extend_from_slice(label.as_bytes());
        }
    }
    out.push(0);
    (out.len() <= MAX_WIRE_NAME_LEN).then_some(out)
}

/// RFC 1035 §3.3: one length octet, then that many octets.
fn character_string(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() > MAX_STRING_LEN {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(u8::try_from(bytes.len()).ok()?);
    out.extend_from_slice(bytes);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// Every shape against the octets its RFC prints, written out by hand
    /// rather than produced by this file's own encoder.
    ///
    /// **The names are the part worth checking**, and they are why this
    /// table exists rather than a round-trip: a length-prefixed name is
    /// exactly the thing an encoder gets subtly wrong — a missing root
    /// octet, a dot written as itself, a length counted over the whole
    /// name rather than the label.
    #[rstest]
    #[case::a(Parsed::A([104, 16, 132, 229]), "681084e5")]
    #[case::aaaa(
        Parsed::Aaaa([0x26, 0x06, 0x47, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x68, 0x10, 0x84, 0xe5]),
        "260647000000000000000000681084e5"
    )]
    // RFC 1035 §3.3.11: one name, and the root octet is part of it.
    #[case::ns(Parsed::Name("ns.example".to_owned()), "026e73076578616d706c6500")]
    #[case::root_name(Parsed::Name(String::new()), "00")]
    #[case::root_dot(Parsed::Name(".".to_owned()), "00")]
    // RFC 1035 §3.3.9: a 16-bit preference, then the exchange.
    #[case::mx(
        Parsed::NumberAndName(10, "mx.example".to_owned()),
        "000a026d78076578616d706c6500"
    )]
    // RFC 1035 §3.3.14: each string carries its own length.
    #[case::txt_one(Parsed::Strings(vec![b"hello".to_vec()]), "0568656c6c6f")]
    #[case::txt_two(Parsed::Strings(vec![b"ab".to_vec(), b"c".to_vec()]), "0261620163")]
    // An empty string is a length octet and nothing else, which is a legal
    // TXT record and not the same as no strings at all.
    #[case::txt_empty(Parsed::Strings(vec![Vec::new()]), "00")]
    #[case::txt_none(Parsed::Strings(Vec::new()), "")]
    // RFC 2782: priority, weight, port, target.
    #[case::srv(
        Parsed::Srv { priority: 1, weight: 5, port: 443, target: "a.example".to_owned() },
        "0001000501bb0161076578616d706c6500"
    )]
    // RFC 6698 §2.1: three octets then the association data.
    #[case::tlsa(
        Parsed::Tlsa { usage: 3, selector: 1, matching: 1, data: hex("aabb") },
        "030101aabb"
    )]
    // RFC 4034 §5.1: key tag, algorithm, digest type, digest.
    #[case::ds(
        Parsed::Ds { key_tag: 2371, algorithm: 13, digest_type: 2, digest: hex("aabb") },
        "09430d02aabb"
    )]
    // RFC 4034 §2.1: flags, protocol, algorithm, key.
    #[case::dnskey(
        Parsed::Key { flags: 256, protocol: 3, algorithm: 13, key: hex("aabb") },
        "0100030daabb"
    )]
    #[case::raw_is_handed_through(Parsed::Raw(hex("00050001")), "00050001")]
    fn each_shape_encodes_to_the_octets_its_rfc_prints(
        #[case] parsed: Parsed,
        #[case] expected: &str,
    ) {
        assert_eq!(parsed.to_vec().expect("encodable"), hex(expected));
    }

    /// RFC 1035 §3.3.13, in full, because it is the one shape with two
    /// names and five numbers and therefore the one where a field can be
    /// swapped with its neighbour and still look plausible.
    #[test]
    fn a_soa_carries_its_two_names_then_five_counters_in_order() {
        let encoded = Parsed::Soa {
            mname: "a.example".to_owned(),
            rname: "root.example".to_owned(),
            serial: 1,
            refresh: 2,
            retry: 3,
            expire: 4,
            minimum: 5,
        }
        .to_vec()
        .expect("encodable");
        assert_eq!(
            encoded,
            hex(concat!(
                "0161076578616d706c6500",       // a.example
                "04726f6f74076578616d706c6500", // root.example
                "00000001",                     // serial
                "00000002",                     // refresh
                "00000003",                     // retry
                "00000004",                     // expire
                "00000005",                     // minimum
            ))
        );
    }

    /// A value with no wire form is refused rather than shortened. None of
    /// these is something a resolver can have reported — which is why they
    /// are a guard, and why the guard is tested rather than assumed.
    #[rstest]
    #[case::label_over_63(Parsed::Name("a".repeat(64)))]
    #[case::empty_label(Parsed::Name("a..b".to_owned()))]
    #[case::name_over_255(Parsed::Name(vec!["abcdefgh"; 40].join(".")))]
    #[case::string_over_255(Parsed::Strings(vec![vec![b'x'; 256]]))]
    fn a_value_with_no_wire_form_is_refused(#[case] parsed: Parsed) {
        assert_eq!(parsed.to_vec(), None);
    }
}
