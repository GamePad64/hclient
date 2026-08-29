//! The dispatch: one enum over every coding this build can reverse.
//!
//! **gzip and brotli have no file of their own and that is not an
//! oversight.** Each is three lines here — a `flate2::write::GzDecoder`
//! and a `brotli_decompressor::writer::DecompressorWriter`, both already
//! push-shaped — so there is nothing to move. `deflate` and `zstd` have
//! files because each needed a hand-written stream: one to choose between
//! two wire formats, the other because its decoder is pull-shaped
//! upstream and the buffering is this crate's.
//!
//! `push`, `finish` and `token` are matches over all four, so they cannot
//! be split per coding without turning the enum into a trait object —
//! which would buy nothing and cost an allocation per response.

// `token()`'s arms are one per coding, so with none of the four on there
// is no arm left that names a `Coding` — the same shape as the
// `BROTLI_BUFFER` import one file over, and the sixteen-set powerset in
// `just features` is what finds both.
#[cfg(any(
    feature = "gzip",
    feature = "brotli",
    feature = "deflate",
    feature = "zstd"
))]
use super::Coding;
#[cfg(feature = "deflate")]
use super::deflate::DeflateStream;
#[cfg(feature = "zstd")]
use super::zstd::ZstdStream;
use bytes::Bytes;
use std::fmt::Debug;
/// One coding's incremental decoder.
///
/// Both are push-shaped (`std::io::Write`, with a `Vec<u8>` collecting the
/// plaintext), which is what lets this be driven a frame at a time from
/// `poll_frame` with no IO traits and no executor anywhere near it.
///
/// **Both payloads are boxed**, and not only to satisfy
/// `clippy::large_enum_variant` (which measures 224 bytes for the gzip
/// decoder against 2,656 for brotli's, whose ring buffer and Huffman
/// tables live inline). [`Decompressed`] wraps EVERY response body this
/// client hands back, decoded or not, and is moved around by value with
/// it; unboxed, a plain uncompressed response would carry 2.6 KB of
/// nothing on every `Response`. One allocation per body that actually
/// decodes is the cheaper side of that trade by a wide margin.
pub(crate) enum Decoder {
    #[cfg(feature = "gzip")]
    Gzip(Box<flate2::write::GzDecoder<Vec<u8>>>),
    /// `Option` so that the final `into_inner` — which is what checks the
    /// stream was not truncated — can consume the writer without consuming
    /// the enum.
    #[cfg(feature = "brotli")]
    Brotli(Option<Box<brotli_decompressor::writer::DecompressorWriter<Vec<u8>>>>),
    /// Two `flate2` types behind one token, chosen from the first two
    /// bytes — see [`DeflateStream`] and its module doc.
    #[cfg(feature = "deflate")]
    Deflate(Box<DeflateStream>),
    /// The one decoder here that is not push-shaped upstream, so this
    /// crate owns the buffering — see [`ZstdStream`].
    #[cfg(feature = "zstd")]
    Zstd(Box<ZstdStream>),
}

/// Hand-written: `brotli_decompressor`'s writer has no `Debug`, and a
/// decoder's internal window is not something to print anyway.
impl Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Decoder({})", self.token())
    }
}

/// The size of the brotli decoder's internal output buffer, in bytes. Not
/// a limit on anything a caller can see — the writer loops over it until
/// the input is consumed — only how often it hands decoded bytes to the
/// `Vec` behind it.
#[cfg(feature = "brotli")]
pub(super) const BROTLI_BUFFER: usize = 8 * 1024;

impl Decoder {
    /// Feeds `input` in and takes whatever plaintext came out — possibly
    /// nothing, when the coding needs more input before it can produce a
    /// byte.
    pub(super) fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
        #[allow(unused_imports)]
        use std::io::Write as _;
        match self {
            #[cfg(feature = "gzip")]
            Decoder::Gzip(d) => {
                d.write_all(input)?;
                Ok(take(d.get_mut()))
            }
            #[cfg(feature = "brotli")]
            Decoder::Brotli(d) => {
                let w = d.as_mut().ok_or_else(brotli_after_end)?;
                w.write_all(input)?;
                Ok(take(w.get_mut()))
            }
            #[cfg(feature = "deflate")]
            Decoder::Deflate(d) => d.push(input),
            #[cfg(feature = "zstd")]
            Decoder::Zstd(d) => d.push(input),
            #[cfg(not(any(
                feature = "gzip",
                feature = "brotli",
                feature = "deflate",
                feature = "zstd"
            )))]
            _ => {
                let _ = input;
                match *self {}
            }
        }
    }

    /// The end of the compressed stream: whatever is still buffered, plus
    /// the integrity check each coding carries.
    ///
    /// This is where a TRUNCATED body becomes an error rather than a short
    /// read — gzip's trailing CRC and length, brotli's own end-of-stream
    /// marker, zlib's Adler-32, zstd's last-block flag and its optional
    /// XXH64. A body cut off mid-transfer that was merely flushed would
    /// reach the caller as a complete, shorter document.
    ///
    /// Raw DEFLATE is the one coding here with no trailer of its own, and
    /// it is not an exception: RFC 1951 §3.2.3's `BFINAL` bit is the end
    /// marker, and `flate2` reports a stream that ended without one.
    pub(super) fn finish(&mut self) -> Result<Bytes, std::io::Error> {
        match self {
            #[cfg(feature = "gzip")]
            Decoder::Gzip(d) => {
                d.try_finish()?;
                Ok(take(d.get_mut()))
            }
            #[cfg(feature = "brotli")]
            Decoder::Brotli(d) => {
                // `into_inner` closes the stream, and its `Err` is exactly
                // "the input ended before the brotli stream did". It
                // consumes the writer, hence the `Option`: a second
                // `finish` cannot happen (`Decompressed` moves to `Ended`
                // first), and if it ever did it would be an error, not a
                // silent second end.
                let w = d.take().ok_or_else(brotli_after_end)?;
                match w.into_inner() {
                    Ok(mut out) => Ok(take(&mut out)),
                    Err(_) => Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the brotli stream ended before its end-of-stream marker",
                    )),
                }
            }
            #[cfg(feature = "deflate")]
            Decoder::Deflate(d) => d.finish(),
            #[cfg(feature = "zstd")]
            Decoder::Zstd(d) => d.finish(),
            #[cfg(not(any(
                feature = "gzip",
                feature = "brotli",
                feature = "deflate",
                feature = "zstd"
            )))]
            _ => match *self {},
        }
    }

    pub(super) fn token(&self) -> &'static str {
        match self {
            #[cfg(feature = "gzip")]
            Decoder::Gzip(_) => Coding::Gzip.token(),
            #[cfg(feature = "brotli")]
            Decoder::Brotli(_) => Coding::Brotli.token(),
            // `"deflate"` whichever wrapper the sniff chose: the token is
            // what appeared on the wire, and the wire has exactly one
            // spelling for both — see `deflate`'s module doc.
            #[cfg(feature = "deflate")]
            Decoder::Deflate(_) => Coding::Deflate.token(),
            #[cfg(feature = "zstd")]
            Decoder::Zstd(_) => Coding::Zstd.token(),
            #[cfg(not(any(
                feature = "gzip",
                feature = "brotli",
                feature = "deflate",
                feature = "zstd"
            )))]
            _ => match *self {},
        }
    }
}

#[cfg(feature = "brotli")]
fn brotli_after_end() -> std::io::Error {
    std::io::Error::other("the brotli decoder was used after its stream ended")
}

/// Takes the accumulated plaintext out of a decoder's output buffer,
/// leaving it empty for the next frame.
#[cfg(any(feature = "gzip", feature = "brotli", feature = "deflate"))]
fn take(out: &mut Vec<u8>) -> Bytes {
    Bytes::from(std::mem::take(out))
}
