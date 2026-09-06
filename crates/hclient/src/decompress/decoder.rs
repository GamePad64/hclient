//! The seam every coding implements — and nothing else.
//!
//! One file per coding sits beside this one: [`gzip`](super::gzip),
//! [`brotli`](super::brotli), [`deflate`](super::deflate) and
//! [`zstd`](super::zstd). Which of them exist is the `mod` declarations in
//! `mod.rs`, and which are reachable is the registry there; **none of
//! those four files contains a `#[cfg]` at all**, which is what putting
//! the gate on the declaration buys.
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
//! the registry simply has no entry. **The three `match *self {}` arms
//! are gone, and this file went from 25 `#[cfg]`s to one**, which is the
//! `dead_code` allowance on [`take`] and is explained there.
//!
//! **And a coding's rules sit with the coding.** `deflate` and `zstd`
//! already had files, because each needed a hand-written stream; gzip and
//! brotli had their bodies spread across three `match` arms here, so
//! brotli's `Option`-to-allow-`into_inner` dance sat forty lines from the
//! buffer constant it uses and three from an error it shares with
//! nothing. All four are files now, which also means a reader asking what
//! this client does about a coding has one place to look rather than two
//! and no rule for which.
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
/// **No `#[cfg]`, although three of the four codings use it.** The gate
/// it used to carry named those three, which is the shape that goes stale
/// the moment a fourth wants it — `zstd` does not only because its own
/// buffering hands back a `Bytes` directly, which is a fact about that
/// implementation rather than a rule. An unused private function in a
/// build with no codings is one `dead_code` warning away from being
/// noticed, and `just features` compiles all sixteen sets; a condition
/// listing coding names is a second statement of the registry.
#[allow(
    dead_code,
    reason = "a build with no coding features has no caller; checked by removing it and watching `--no-default-features` warn"
)]
pub(super) fn take(out: &mut Vec<u8>) -> Bytes {
    Bytes::from(std::mem::take(out))
}
