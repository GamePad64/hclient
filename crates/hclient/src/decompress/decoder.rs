//! The dispatch: one trait, one implementor per coding.
//!
//! # Why a trait where this was an enum, and what the measurement said
//!
//! It was `enum Decoder` with an arm per coding, and this file's own doc
//! argued against the change: *"`push`, `finish` and `token` are matches
//! over all four, so they cannot be split per coding without turning the
//! enum into a trait object — which would buy nothing and cost an
//! allocation per response."*
//!
//! **The cost half of that was measured and is wrong.** Every arm of that
//! enum was already `Box`ed — deliberately, because [`Decompressed`] wraps
//! every response body this client hands back and brotli's decoder is
//! 2,656 bytes inline — so the allocation the sentence warns about was
//! already being paid, on exactly the same bodies. Measured on this tree:
//! the enum was **16 bytes** and `Box<dyn Decode>` is **16 bytes**, and
//! the allocation count per decoded body is one either way. There is no
//! `Box::new` here that was not there before; the vtable pointer replaced
//! the discriminant.
//!
//! What it buys is what that sentence was right to be sceptical about, so
//! it is worth stating precisely rather than as tidiness:
//!
//! **The empty-build arms are gone, structurally.** With none of the four
//! features on, the enum had no variants — so `push`, `finish` and
//! `token` each needed a `#[cfg(not(any(..)))]` arm whose body was
//! `match *self {}`: three copies of a four-feature condition existing
//! only to tell the compiler that an empty enum cannot be matched. A
//! trait object has no empty case — such a build has no implementors, and
//! [`Coding::decoder`] answers `None` as it always did. **The three
//! `match *self {}` arms are gone and 25 `#[cfg]`s became 12** — the
//! remainder is one per item per coding, which is the honest floor for
//! four optional codings in one file and was 25 only because each method
//! carried a four-feature condition of its own.
//!
//! **And a coding's rules now sit with the coding.** `deflate` and `zstd`
//! already had files of their own; gzip and brotli had their bodies
//! spread across three `match` arms here, so brotli's
//! `Option`-to-allow-`into_inner` dance sat forty lines from the buffer
//! constant it uses and three lines from an error it shares with nothing.
//! Each is a type with one `impl` now.
//!
//! What it does **not** buy, and here the old sentence was right: nothing
//! a caller can see, and no coding becomes possible that was not possible
//! before. This is a shape, and what recommends it is the `#[cfg]` count
//! and where a coding's rules live.
//!
//! [`Decompressed`]: super::Decompressed
//! [`Coding::decoder`]: super::Coding::decoder

use bytes::Bytes;
use std::fmt::Debug;

/// One coding's incremental decoder.
///
/// Implementors are push-shaped — bytes in with [`push`](Self::push),
/// plaintext out — which is what lets a body be driven a frame at a time
/// from `poll_frame` with no IO traits and no executor anywhere near it.
/// Where a library's decoder is pull-shaped instead, the buffering is
/// this crate's: see `zstd`'s module doc.
///
/// **This trait declares no auto trait, and [`Decoder`] does** — see
/// there for the defect that made the bound necessary and for why it is
/// stated at the alias rather than here.
///
/// **`&mut self` on `finish`, although two of the four codings would
/// rather consume their writer.** `Decompressed` holds its decoder in a
/// struct field and moves to `Ended` after finishing, so a by-value
/// `finish` would need that field to be an `Option` — pushing brotli's
/// local problem onto the three codings that do not have it. Brotli keeps
/// the `Option` inside its own type instead, where its own doc explains
/// it.
pub(crate) trait Decode: Debug {
    // send-bound-exception: amendment-C14
    /// Feeds `input` in and takes whatever plaintext came out — possibly
    /// nothing, when the coding needs more input before it can produce a
    /// byte.
    fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error>;

    /// The end of the compressed stream: whatever is still buffered, plus
    /// the integrity check each coding carries.
    ///
    /// **This is where a TRUNCATED body becomes an error rather than a
    /// short read** — gzip's trailing CRC and length, brotli's own
    /// end-of-stream marker, zlib's Adler-32, zstd's last-block flag and
    /// its optional XXH64. A body cut off mid-transfer that was merely
    /// flushed would otherwise reach the caller as a complete, shorter
    /// document.
    ///
    /// Raw DEFLATE is the one coding here with no trailer of its own, and
    /// it is not an exception: RFC 1951 §3.2.3's `BFINAL` bit is the end
    /// marker, and `flate2` reports a stream that ended without one.
    fn finish(&mut self) -> Result<Bytes, std::io::Error>;

    /// The token as it appeared on the wire.
    ///
    /// `"deflate"` whichever wrapper that coding's sniff chose: the wire
    /// has one spelling for both — see `deflate`'s module doc.
    fn token(&self) -> &'static str;
}

/// The decoder for one response body.
///
/// **`Box<dyn Decode>` rather than an enum, and it costs nothing**: every
/// arm of the enum this replaced was already boxed, for the reason the
/// module doc records and measures — 16 bytes and one allocation per
/// decoded body, either shape.
///
/// # `Send` is declared here, and leaving it off was this change's one
/// real defect
///
/// The enum this replaced *inferred* `Send` from its concrete payloads. A
/// `dyn` declares its own auto traits, so the first version of this alias
/// was `!Send` — and [`Decompressed`](super::Decompressed) wraps every
/// response body, so `tokio::spawn` of one stopped compiling. That is
/// this workspace's own rule met from the direction that costs
/// something: *a `dyn` that declares no auto traits does not hide `Send`,
/// it removes it.* `tests/spawnable_body.rs` said so on the first
/// `--all-targets` build, which is amendment C14's own test doing exactly
/// what C14 exists for.
///
/// **The bound is on this alias and not on the trait**, and that is not a
/// preference: a marker on a `pub trait X: Send {` line is **deleted** by
/// `cargo fmt`, so `just fmt-check` and `just invariants` cannot both
/// pass with it there — reproduced here, on this trait, before it was
/// believed. This workspace already met that once and drew the rule then:
/// demand the auto trait where the value is *stored*, never on the seam.
/// A one-line `type` is a line `cargo fmt` does not reflow.
///
/// It excludes nobody. All four codings are `Send` at their concrete
/// types, and this is `pub(crate)` — not a seam a caller implements — so
/// the bound is a fact about four types in this crate rather than a
/// demand on anybody outside it.
pub(crate) type Decoder = Box<dyn Decode + Send>; // send-bound-exception: amendment-C14

/// Takes the accumulated plaintext out of a decoder's output buffer,
/// leaving it empty for the next frame.
#[cfg(any(feature = "gzip", feature = "brotli", feature = "deflate"))]
pub(super) fn take(out: &mut Vec<u8>) -> Bytes {
    Bytes::from(std::mem::take(out))
}

/// The `gzip` coding — RFC 1952, through `flate2`.
///
/// A type of its own where it used to be three `match` arms, and it is
/// three lines: `flate2`'s decoder is already push-shaped, so there is
/// nothing here but the name.
#[cfg(feature = "gzip")]
pub(super) struct Gzip(flate2::write::GzDecoder<Vec<u8>>);

#[cfg(feature = "gzip")]
impl Gzip {
    pub(super) fn new() -> Self {
        Self(flate2::write::GzDecoder::new(Vec::new()))
    }
}

/// Hand-written for the reason every decoder here has one: an internal
/// window is not something to print.
#[cfg(feature = "gzip")]
impl Debug for Gzip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gzip")
    }
}

#[cfg(feature = "gzip")]
impl Decode for Gzip {
    fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
        use std::io::Write as _;
        self.0.write_all(input)?;
        Ok(take(self.0.get_mut()))
    }
    fn finish(&mut self) -> Result<Bytes, std::io::Error> {
        self.0.try_finish()?;
        Ok(take(self.0.get_mut()))
    }
    fn token(&self) -> &'static str {
        super::Coding::Gzip.token()
    }
}

/// The size of the brotli decoder's internal output buffer, in bytes.
///
/// Not a limit on anything a caller can see — the writer loops over it
/// until the input is consumed — only how often it hands decoded bytes to
/// the `Vec` behind it.
#[cfg(feature = "brotli")]
const BROTLI_BUFFER: usize = 8 * 1024;

/// The `br` coding — RFC 7932, through `brotli-decompressor`.
///
/// **The `Option` is this coding's own problem and now lives with it**,
/// which is one of the two things splitting the enum bought.
/// `DecompressorWriter::into_inner` is what checks the stream was not
/// truncated, and it consumes the writer — so [`Decode::finish`], which
/// takes `&mut self` for the other three codings' sake, needs somewhere
/// to leave a hole. A second `finish` cannot happen (`Decompressed` moves
/// to `Ended` first), and if it ever did it would be an error rather than
/// a silent second end.
#[cfg(feature = "brotli")]
pub(super) struct Brotli(Option<brotli_decompressor::writer::DecompressorWriter<Vec<u8>>>);

#[cfg(feature = "brotli")]
impl Brotli {
    pub(super) fn new() -> Self {
        Self(Some(brotli_decompressor::writer::DecompressorWriter::new(
            Vec::new(),
            BROTLI_BUFFER,
        )))
    }
}

/// Hand-written because `brotli_decompressor`'s writer has no `Debug`.
#[cfg(feature = "brotli")]
impl Debug for Brotli {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Brotli")
    }
}

#[cfg(feature = "brotli")]
fn brotli_after_end() -> std::io::Error {
    std::io::Error::other("the brotli decoder was used after its stream ended")
}

#[cfg(feature = "brotli")]
impl Decode for Brotli {
    fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
        use std::io::Write as _;
        let w = self.0.as_mut().ok_or_else(brotli_after_end)?;
        w.write_all(input)?;
        Ok(take(w.get_mut()))
    }
    fn finish(&mut self) -> Result<Bytes, std::io::Error> {
        // `into_inner` closes the stream, and its `Err` is exactly "the
        // input ended before the brotli stream did".
        let w = self.0.take().ok_or_else(brotli_after_end)?;
        match w.into_inner() {
            Ok(mut out) => Ok(take(&mut out)),
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the brotli stream ended before its end-of-stream marker",
            )),
        }
    }
    fn token(&self) -> &'static str {
        super::Coding::Brotli.token()
    }
}
