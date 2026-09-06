//! The `gzip` coding — RFC 1952, through `flate2`.
//!
//! **A file of its own for symmetry rather than for length**, and the
//! symmetry is what it buys: `deflate` and `zstd` had files because each
//! needed a hand-written stream, so a reader asking what this client does
//! about a coding had two places to look and no rule saying which. Every
//! coding is one file now, and `decoder.rs` is the seam alone.
//!
//! There is genuinely little here, and that is worth being able to see:
//! `flate2`'s decoder is already push-shaped, so this is the wrapper that
//! names it plus the [`Registration`](super::Registration) that gives it
//! a token. Set beside `deflate`'s file, which is mostly the argument for
//! sniffing two wire formats apart, the difference in size is the
//! difference in how much of a coding this crate has to own.

use super::decoder::{Decode, take};
use bytes::Bytes;

pub(super) struct Gzip(flate2::write::GzDecoder<Vec<u8>>);

impl Gzip {
    pub(super) fn new() -> Self {
        Self(flate2::write::GzDecoder::new(Vec::new()))
    }
}

/// Hand-written for the reason every decoder here has one: an internal
/// window is not something to print.
impl std::fmt::Debug for Gzip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gzip")
    }
}

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
        "gzip"
    }
}
