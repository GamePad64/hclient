//! RFC 9297 §3.2 capsules, over a QUIC (RFC 9000 §16) varint length.
//!
//! A capsule is `varint type, varint length, value` — three fields, no
//! framing beyond that — and the only capsule this crate produces or
//! reads for real is DATAGRAM (`0x00`), whose value is a context-id
//! varint (RFC 9298 §5 fixes it at `0` for a CONNECT-UDP payload) followed
//! by the payload itself. Every other capsule type is a fact this crate
//! reports and does not act on: RFC 9297 §3.2 requires an unrecognized
//! type to be skipped rather than to end the connection, and
//! [`Capsule::Other`] is that skip made visible to a caller rather than
//! silent.

use bytes::{Bytes, BytesMut};

use crate::error::CapsuleError;

/// QUIC's variable-length integer encoding, RFC 9000 §16 — two bits of
/// length tag in the first byte, six/fourteen/thirty/sixty-two bits of
/// value.
pub mod varint {
    /// Appends `value`'s varint encoding to `out`, choosing the shortest
    /// of the four RFC 9000 §16 lengths that fits.
    ///
    /// Reads the value's own big-endian bytes and tags the leading one
    /// with the length rather than casting to a narrower integer, so
    /// there is no truncation for a lint to catch — the same shape RFC
    /// 9000 §16's worked examples are read back with elsewhere in this
    /// workspace.
    ///
    /// # Panics
    ///
    /// Panics if `value` exceeds the 62-bit range a QUIC varint can carry
    /// — every value this crate encodes (a capsule type, a capsule
    /// length, or the fixed context id `0`) is far below that ceiling.
    pub fn encode(value: u64, out: &mut Vec<u8>) {
        assert!(
            value < (1 << 62),
            "{value} does not fit in a 62-bit QUIC varint"
        );
        let b = value.to_be_bytes();
        if value < (1 << 6) {
            out.push(b[7]);
        } else if value < (1 << 14) {
            out.push(0x40 | b[6]);
            out.push(b[7]);
        } else if value < (1 << 30) {
            out.push(0x80 | b[4]);
            out.extend_from_slice(&b[5..]);
        } else {
            out.push(0xC0 | b[0]);
            out.extend_from_slice(&b[1..]);
        }
    }

    /// Reads one varint off the front of `bytes`, returning its value and
    /// how many bytes it occupied — or `None` when `bytes` does not yet
    /// hold that many, which a streaming decoder reads as "wait for more".
    pub fn decode(bytes: &[u8]) -> Option<(u64, usize)> {
        let first = *bytes.first()?;
        let len = 1usize << (first >> 6);
        let rest = bytes.get(1..len)?;
        Some((
            rest.iter()
                .fold(u64::from(first & 0x3f), |v, b| (v << 8) | u64::from(*b)),
            len,
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn varints_take_the_rfc_9000_lengths() {
            for (v, len) in [
                (0u64, 1),
                (63, 1),
                (64, 2),
                (16383, 2),
                (16384, 4),
                (1_073_741_823, 4),
                (1_073_741_824, 8),
            ] {
                let mut out = Vec::new();
                encode(v, &mut out);
                assert_eq!(out.len(), len, "{v}");
                assert_eq!(decode(&out), Some((v, len)));
            }
        }

        #[test]
        fn decode_of_an_empty_slice_waits_for_more() {
            assert_eq!(decode(&[]), None);
        }

        #[test]
        fn decode_of_a_truncated_multi_byte_varint_waits_for_more() {
            // The length tag (top two bits of the first byte) says four
            // bytes; only two have arrived.
            assert_eq!(decode(&[0x80, 0x00]), None);
        }
    }
}

/// The RFC 9297 §3.2 capsule type of a DATAGRAM capsule.
const DATAGRAM: u64 = 0x00;

/// A decoded capsule.
///
/// `#[non_exhaustive]` would cost every match here a wildcard arm for a
/// third variant RFC 9297 never adds — a capsule is either the one type
/// this crate reads for real, or it is not, and `Other` already is that
/// catch-all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capsule {
    /// A DATAGRAM capsule (RFC 9297 §3.2, type `0x00`) carrying a
    /// CONNECT-UDP payload — one whose context id (RFC 9298 §5) is `0`,
    /// with the context-id varint itself already stripped.
    Datagram(Bytes),
    /// A capsule this crate does not act on: either its type is not
    /// DATAGRAM, or it is a DATAGRAM capsule whose context id is not `0`
    /// (RFC 9298 §5 reserves every other context id, and this crate
    /// speaks CONNECT-UDP alone). RFC 9297 §3.2 requires an unrecognized
    /// capsule to be skipped, not to end the connection; `kind` is the
    /// capsule's own type, so a caller can tell *which* fact was skipped.
    Other {
        /// The capsule's own RFC 9297 §3.2 type.
        kind: u64,
    },
}

/// Appends a DATAGRAM capsule (RFC 9297 §3.2, type `0x00`) wrapping
/// `payload` as a CONNECT-UDP datagram (RFC 9298 §5: context id `0`,
/// then the payload) to `out`.
pub fn encode_datagram(payload: &[u8], out: &mut Vec<u8>) {
    varint::encode(DATAGRAM, out);

    let mut value = Vec::with_capacity(1 + payload.len());
    varint::encode(0, &mut value);
    value.extend_from_slice(payload);

    varint::encode(value.len() as u64, out);
    out.extend_from_slice(&value);
}

/// A streaming RFC 9297 §3.2 capsule decoder.
///
/// Bytes arrive through [`push`](Decoder::push) in whatever chunks the
/// transport hands over; [`next`](Decoder::next) answers `Ok(None)` for as
/// long as a complete capsule has not yet arrived, so a capsule split
/// across two reads is simply not there until the second `push` completes
/// it.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: BytesMut,
}

impl Decoder {
    /// Appends `bytes` to the decoder's buffer.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Decodes and removes one capsule from the front of the buffer, if a
    /// complete one has arrived.
    ///
    /// # Errors
    ///
    /// Returns [`CapsuleError`] when a complete capsule has arrived and
    /// its contents are malformed — never for an incomplete one, which
    /// answers `Ok(None)` instead so a caller cannot tell "wait for more"
    /// from "this will never be valid".
    #[allow(
        clippy::should_implement_trait,
        reason = "this is a fallible, buffered pull (`push`/`next`) rather than `Iterator`, whose `next` cannot report a malformed capsule"
    )]
    pub fn next(&mut self) -> Result<Option<Capsule>, CapsuleError> {
        let Some((kind, kind_len)) = varint::decode(&self.buf) else {
            return Ok(None);
        };
        let Some((len, len_len)) = varint::decode(&self.buf[kind_len..]) else {
            return Ok(None);
        };
        let len = usize::try_from(len).map_err(|_| CapsuleError::TooLarge)?;

        let header_len = kind_len + len_len;
        let total = header_len + len;
        if self.buf.len() < total {
            return Ok(None);
        }

        let value = self.buf.split_to(total).split_off(header_len).freeze();

        if kind == DATAGRAM {
            let Some((context_id, context_len)) = varint::decode(&value) else {
                return Err(CapsuleError::Malformed);
            };
            if context_id == 0 {
                return Ok(Some(Capsule::Datagram(value.slice(context_len..))));
            }
        }
        Ok(Some(Capsule::Other { kind }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_datagram_capsule_round_trips() {
        let mut out = Vec::new();
        encode_datagram(b"quic", &mut out);
        assert_eq!(out, [0x00, 0x05, 0x00, b'q', b'u', b'i', b'c']);

        let mut d = Decoder::default();
        d.push(&out);
        assert_eq!(
            d.next().unwrap(),
            Some(Capsule::Datagram(Bytes::from_static(b"quic")))
        );
        assert_eq!(d.next().unwrap(), None);
    }

    #[test]
    fn a_capsule_split_across_reads_waits_for_the_rest() {
        let mut out = Vec::new();
        encode_datagram(&[7u8; 100], &mut out);

        let mut d = Decoder::default();
        d.push(&out[..3]);
        assert_eq!(d.next().unwrap(), None);
        d.push(&out[3..]);
        assert!(matches!(d.next().unwrap(), Some(Capsule::Datagram(p)) if p.len() == 100));
    }

    #[test]
    fn an_unknown_capsule_is_skipped() {
        let mut d = Decoder::default();
        d.push(&[0x40, 0x99, 0x02, 0xaa, 0xbb, 0x00, 0x02, 0x00, b'x']);
        assert_eq!(d.next().unwrap(), Some(Capsule::Other { kind: 0x99 }));
        assert_eq!(
            d.next().unwrap(),
            Some(Capsule::Datagram(Bytes::from_static(b"x")))
        );
    }

    #[test]
    fn a_datagram_with_a_nonzero_context_id_is_other() {
        let mut out = Vec::new();
        varint::encode(DATAGRAM, &mut out);
        let mut value = Vec::new();
        varint::encode(7, &mut value); // context id 7, not the CONNECT-UDP 0
        value.extend_from_slice(b"x");
        varint::encode(value.len() as u64, &mut out);
        out.extend_from_slice(&value);

        let mut d = Decoder::default();
        d.push(&out);
        assert_eq!(d.next().unwrap(), Some(Capsule::Other { kind: DATAGRAM }));
    }

    #[test]
    fn an_empty_datagram_value_is_malformed() {
        let mut out = Vec::new();
        varint::encode(DATAGRAM, &mut out);
        varint::encode(0, &mut out); // zero-length value: no context id fits
        let mut d = Decoder::default();
        d.push(&out);
        assert_eq!(d.next(), Err(CapsuleError::Malformed));
    }
}
