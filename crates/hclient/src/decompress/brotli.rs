//! The `br` coding — RFC 7932, through `brotli-decompressor`.
//!
//! A file of its own for the reason `gzip`'s is, and with one thing to
//! say that `gzip` has not: this coding needs an `Option` inside its own
//! type, and keeping that here rather than in the seam is what splitting
//! the old enum bought.

use super::decoder::{Decode, take};
use bytes::Bytes;

/// The size of the brotli decoder's internal output buffer, in bytes.
///
/// Not a limit on anything a caller can see — the writer loops over it
/// until the input is consumed — only how often it hands decoded bytes to
/// the `Vec` behind it.
const BUFFER: usize = 8 * 1024;

/// **The `Option` is this coding's own problem and lives with it.**
///
/// `DecompressorWriter::into_inner` is what checks the stream was not
/// truncated, and it consumes the writer — so [`Decode::finish`], which
/// takes `&mut self` for the other three codings' sake, needs somewhere
/// to leave a hole. A second `finish` cannot happen (`Decompressed` moves
/// to `Ended` first), and if it ever did it would be an error rather than
/// a silent second end.
pub(super) struct Brotli(Option<brotli_decompressor::writer::DecompressorWriter<Vec<u8>>>);

impl Brotli {
    pub(super) fn new() -> Self {
        Self(Some(brotli_decompressor::writer::DecompressorWriter::new(
            Vec::new(),
            BUFFER,
        )))
    }
}

/// Hand-written because `brotli_decompressor`'s writer has no `Debug`.
impl std::fmt::Debug for Brotli {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Brotli")
    }
}

fn after_end() -> std::io::Error {
    std::io::Error::other("the brotli decoder was used after its stream ended")
}

impl Decode for Brotli {
    fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
        use std::io::Write as _;
        let w = self.0.as_mut().ok_or_else(after_end)?;
        w.write_all(input)?;
        Ok(take(w.get_mut()))
    }
    fn finish(&mut self) -> Result<Bytes, std::io::Error> {
        // `into_inner` closes the stream, and its `Err` is exactly "the
        // input ended before the brotli stream did".
        let w = self.0.take().ok_or_else(after_end)?;
        match w.into_inner() {
            Ok(mut out) => Ok(take(&mut out)),
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the brotli stream ended before its end-of-stream marker",
            )),
        }
    }
    fn token(&self) -> &'static str {
        "br"
    }
}
