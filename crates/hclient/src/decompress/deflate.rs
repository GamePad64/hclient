//! The `deflate` decoder.
//!
//! Split out of `decompress.rs` so that the argument below sits beside the
//! code it justifies. Every item here is behind the `deflate` feature, and
//! the gate is on the `mod` declaration rather than on 3 items.
//!
//! # The `deflate` wrapper question, and why it is answered by looking
//!
//! RFC 9110 §8.4.1.2 is unambiguous about what the token means and
//! equally unambiguous that the wire is not:
//!
//! > The "deflate" coding is a "zlib" data format [RFC1950] containing a
//! > "deflate" compressed data stream [RFC1951] […]
//! >
//! > | *Note:* Some non-conformant implementations send the "deflate"
//! > | compressed data without the zlib wrapper.
//!
//! So a client that advertises `deflate` is taking on a guess, and the
//! only question is where the guess is made. **It is made once, from the
//! first two bytes, before a single byte of output exists** —
//! [`DeflateStream::looks_like_zlib`]. A caller gets the same thing for
//! both encodings on the wire: the plaintext, `Content-Encoding` and
//! `Content-Length` stripped, and [`Decompressed::coding`] reading
//! `"deflate"` — which is what that accessor promises, the token *as it
//! appeared on the wire*, and the wire does not distinguish them either.
//! A stream that then fails to decode is [`DecodeFailed`] with
//! `coding == "deflate"`, never a short body and never the encoded bytes
//! handed on as if they were plain.
//!
//! The reason two bytes are enough is not probability, it is RFC 1951
//! §3.2.3. zlib's first byte carries `CM` in its low nibble and `deflate`
//! is `CM == 8`; a RAW stream's first byte carries `BFINAL` in bit 0 and
//! `BTYPE` in bits 1-2, so a low nibble of `8` means `BFINAL = 0`,
//! `BTYPE = 00` — a stored block — **with bit 3 set**, and §3.2.4 has a
//! decoder skip the remaining bits of that byte, which every encoder
//! therefore writes as zero. A conformant raw stream cannot present
//! `CM == 8`. The `(CMF << 8 | FLG) % 31 == 0` check is a second,
//! independent one.
//!
//! curl answers the same question the other way, and it is worth knowing
//! which is stronger. `lib/content_encoding.c` always tries zlib and, on
//! `Z_DATA_ERROR`, calls `inflateReset2(z, -MAX_WBITS)` and replays the
//! buffer — but only while `zlib_init == ZLIB_INIT`, and the comment
//! three lines below says why: *"If we are in a state that would wrongly
//! allow restart in raw mode at the next call, assume output has already
//! started."* A retry is available only before the first output byte, so
//! it is a rule with a window; looking at the header is a rule without
//! one, and it needs no replay buffer beyond those two bytes.
//!

use bytes::Bytes;
/// The `deflate` coding, both of it.
///
/// RFC 9110 §8.4.1.2 says zlib and its own Note says a long tail of
/// servers disagrees, so this picks between them from the first two bytes
/// off the wire. The module doc has the argument for why two bytes are
/// enough and why the decision is made here rather than after a failure,
/// as curl's is.
///
/// **`flate2::Decompress` rather than the two `flate2::write` decoders**,
/// and that is not a preference: `write::ZlibDecoder::try_finish` calls
/// `zio::finish`, which runs the decompressor until it stops producing
/// and then returns `Ok(())` **without ever asking whether the stream
/// ended** — read in flate2 1.1's `src/zio.rs:173`. So a truncated body
/// reached the caller as a complete, shorter document, which is the exact
/// defect the trailer checks on the other three codings exist against. It
/// was found by a wire test, in the shape `tests/compression.rs` warns
/// against.
/// `Decompress::decompress_vec` answers `Status::StreamEnd`, which is the
/// question, and `Decompress::new(zlib_header)` is the same switch in one
/// type instead of two.
///
/// The states are one-way: `Sniffing` -> `Running`, never back.
pub(crate) enum DeflateStream {
    /// Fewer than two bytes have arrived, so the question cannot be
    /// answered yet. The buffer holds at most one byte, which is why it
    /// is not a bound anybody has to think about.
    Sniffing(Vec<u8>),
    Running {
        dec: flate2::Decompress,
        /// `StreamEnd` has been seen — RFC 1951 §3.2.3's `BFINAL` block
        /// for the raw form, and that plus RFC 1950's Adler-32 for the
        /// wrapped one. What [`Self::finish`] asks about.
        done: bool,
    },
}

/// How many decoded bytes are asked for per `decompress_vec` call. Not a
/// bound on anything a caller sees — the loop runs until the decoder stops
/// making progress — only how often it grows a `Vec`.
const DEFLATE_CHUNK: usize = 16 * 1024;

impl DeflateStream {
    pub(super) fn new() -> Self {
        DeflateStream::Sniffing(Vec::new())
    }

    /// Does `head` open an RFC 1950 stream?
    ///
    /// Three conditions, and the first is the one that carries the
    /// argument (module doc): `CM == 8` cannot be the first nibble of a
    /// conformant raw stream, because RFC 1951 §3.2.3 packs `BFINAL` into
    /// bit 0 and `BTYPE` into bits 1-2, so a low nibble of `8` is a
    /// stored block with the padding bit §3.2.4 tells encoders to zero.
    ///
    /// `FDICT` is deliberately NOT tested. A zlib stream needing a preset
    /// dictionary is still a zlib stream; classifying it as raw would
    /// swap flate2's honest "a dictionary is required" for a confusing
    /// failure inside the wrong decoder.
    fn looks_like_zlib(head: [u8; 2]) -> bool {
        let cmf = head[0];
        // CM, the low nibble: 8 is "deflate", and RFC 1950 §2.2 defines
        // no other value a client would meet.
        let method = cmf & 0x0f == 8;
        // CINFO, the high nibble: log2(window) - 8, and §2.2 forbids
        // values above 7 outright.
        let window = cmf >> 4 <= 7;
        // FCHECK: the two header bytes, read big-endian, are a multiple
        // of 31 by construction.
        let check = u16::from_be_bytes(head).is_multiple_of(31);
        method && window && check
    }

    pub(super) fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
        // The bytes to feed: normally just `input`, but on the frame that
        // completes the two-byte header it is that header plus whatever
        // came with it, because nothing buffered has been fed yet.
        let mut carried = None;
        if let DeflateStream::Sniffing(buf) = self {
            buf.extend_from_slice(input);
            if buf.len() < 2 {
                return Ok(Bytes::new());
            }
            let all = std::mem::take(buf);
            *self = DeflateStream::Running {
                dec: flate2::Decompress::new(Self::looks_like_zlib([all[0], all[1]])),
                done: false,
            };
            carried = Some(all);
        }
        let bytes: &[u8] = carried.as_deref().unwrap_or(input);
        let DeflateStream::Running { dec, done } = self else {
            // Unreachable: the block above leaves `Sniffing` only by
            // returning. An empty answer rather than a panic, for the
            // reason `Coding::decoder`'s `Option` is an `Option`.
            return Ok(Bytes::new());
        };
        if *done && !bytes.is_empty() {
            // Mirrors brotli's arm one type over. Discarding them would be
            // this crate deciding that bytes a server sent are not part of
            // the document.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bytes arrived after the end of the `deflate` stream",
            ));
        }
        Self::drive(dec, done, bytes, flate2::FlushDecompress::None)
    }

    /// Runs the decoder until it stops making progress, or until the
    /// stream ends.
    ///
    /// The loop deliberately makes one **more** call than there is input
    /// for, and that is the defect this shape was written to fix: zlib
    /// reports `StreamEnd` from the call that reads RFC 1950's four-byte
    /// Adler-32 trailer, and with a `consumed < bytes.len()` guard that
    /// call never happens on the frame that carried it — so a complete
    /// body finished with `done == false` and was reported as truncated.
    /// With an empty input slice and no output, the next iteration makes
    /// no progress and breaks, so the extra call costs one iteration and
    /// cannot spin.
    fn drive(
        dec: &mut flate2::Decompress,
        done: &mut bool,
        bytes: &[u8],
        flush: flate2::FlushDecompress,
    ) -> Result<Bytes, std::io::Error> {
        let mut out = Vec::new();
        let mut consumed = 0usize;
        while !*done {
            out.reserve(DEFLATE_CHUNK);
            let (before_in, before_out) = (dec.total_in(), dec.total_out());
            let status = dec
                .decompress_vec(&bytes[consumed..], &mut out, flush)
                .map_err(std::io::Error::other)?;
            consumed += (dec.total_in() - before_in) as usize;
            if status == flate2::Status::StreamEnd {
                *done = true;
                break;
            }
            if dec.total_in() == before_in && dec.total_out() == before_out {
                // Neither read nor wrote: the decoder wants bytes that
                // have not arrived.
                break;
            }
        }
        if *done && consumed < bytes.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bytes arrived after the end of the `deflate` stream",
            ));
        }
        Ok(Bytes::from(out))
    }

    pub(super) fn finish(&mut self) -> Result<Bytes, std::io::Error> {
        match self {
            // The whole body was one byte or none. The shortest possible
            // raw stream is two bytes and the shortest zlib one is eight,
            // so this is a truncation whatever the wrapper would have
            // been — there is no guess to get wrong.
            DeflateStream::Sniffing(_) => Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the `deflate` body ended before even a two-byte header arrived",
            )),
            DeflateStream::Running { dec, done } => {
                // One last call, with nothing left to give it: the
                // decoder may still be holding output, and `StreamEnd`
                // may still be one call away — see `drive`.
                let last = Self::drive(dec, done, &[], flate2::FlushDecompress::Finish)?;
                if !*done {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the `deflate` body ended in the middle of the compressed stream",
                    ));
                }
                Ok(last)
            }
        }
    }
}
