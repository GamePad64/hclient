//! The seam every coding implements — and nothing else.
//!
//! One file per coding sits beside this one: [`gzip`](super::gzip),
//! [`brotli`](super::brotli), [`deflate`](super::deflate) and
//! [`zstd`](super::zstd). Which of them exist is the `mod` declarations
//! in `mod.rs`, and which are *reachable* is the
//! [`ContentCoding`](super::ContentCoding) list a client carries —
//! **which was a registry in `mod.rs` until the set of codings became
//! open**, and is now `Config::decompression`, so the answer is
//! per-client rather than per-build. **None of those four files contains
//! a `#[cfg]` at all**, which is what putting the gate on the
//! declaration buys and which the change did not disturb.
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
//! What it did **not** buy at the time, and here the old sentence was
//! right: nothing a caller could see, and no coding became possible that
//! was not possible before. That has since stopped being true, and the
//! trait object is why — publishing [`Decode`] was a visibility change
//! and not a rewrite, because the three methods were already IO-free,
//! object-safe and stateful per body. A shape chosen for the `#[cfg]`
//! count turned out to be the shape an open seam needs, which is worth
//! recording as luck rather than foresight.
//!

use bytes::Bytes;
use std::fmt::Debug;

// Maintainer notes (not rendered):
//
// Where a library's decoder is pull-shaped instead, the buffering is
// this crate's: see `zstd`'s module doc.
//
// The defect that made the bound necessary, and why it is stated at the
// alias rather than here, is recorded on that alias in this file's
// source.
//
// **`&mut self` on `finish`, although two of the four codings would
// rather consume their writer.** `Decompressed` holds its decoder in a
// struct field and moves to `Ended` after finishing, so a by-value
// `finish` would need that field to be an `Option` — pushing brotli's
// local problem onto the three codings that do not have it. Brotli keeps
// the `Option` inside its own type instead, where its own doc explains
// it.
//
// # Public, because the set of codings is open
//
// This was `pub(crate)`, and its doc could say *"not a seam a caller
// implements"* while the codings were a compiled-in table. They are a
// [`ContentCoding`](super::ContentCoding) list a caller writes now, and
// [`ContentCoding::decoder`](super::ContentCoding::decoder) hands one of
// these back — so implementing this trait is how a coding of somebody
// else's does its work. Nothing about the three methods changed to make
// that possible; they were already IO-free, object-safe and stateful per
// body, which is what the decision to publish rested on rather than a
// rewrite.
/// One coding's incremental decoder.
///
/// Implementors are push-shaped — bytes in with [`push`](Self::push),
/// plaintext out — which is what lets a body be driven a frame at a time
/// from `poll_frame` with no IO traits and no executor anywhere near it.
///
/// Implement it for a coding of your own and hand it back, boxed, from
/// [`ContentCoding::decoder`](super::ContentCoding::decoder).
///
/// **This trait declares no auto trait, and the alias this crate boxes it
/// into does** — `Box<dyn Decode + Send>`, which is what
/// [`ContentCoding::decoder`](super::ContentCoding::decoder) hands back.
pub trait Decode: Debug {
    // send-bound-exception: amendment-C14
    /// Feeds `input` in and takes whatever plaintext came out — possibly
    /// nothing, when the coding needs more input before it can produce a
    /// byte.
    ///
    /// # Errors
    ///
    /// Whatever the coding makes of bytes that are not a valid stream in
    /// it. The error reaches the caller as an
    /// [`ErrorKind::Decode`](hclient_core::error::ErrorKind::Decode)
    /// carrying a [`DecodeFailed`](crate::error::DecodeFailed) that names
    /// this coding, and the body ends there: a decoder that has answered
    /// `Err` is never pushed to again.
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
    ///
    /// # Errors
    ///
    /// An incomplete stream, and that is what this method is **for**: a
    /// body cut off mid-transfer decodes perfectly well up to the cut, so
    /// the only thing that can report it is the check at the end. A coding
    /// with no integrity check of its own has nothing to raise here and
    /// should answer `Ok` — which is a decision about that coding rather
    /// than about this trait, and it is the difference between a truncated
    /// response and a shorter document.
    fn finish(&mut self) -> Result<Bytes, std::io::Error>;

    // Maintainer notes (not rendered):
    //
    // the wire has one spelling for both — see `deflate`'s module doc.
    /// The token as it appeared on the wire.
    ///
    /// `"deflate"` whichever wrapper that coding's sniff chose: the wire
    /// has one spelling for both.
    ///
    /// **`&str` rather than `&'static str`, which is what
    /// [`ClientBody::coding`](crate::body::ClientBody::coding) pays for in
    /// a [`Cow`](std::borrow::Cow).** A decoder may name itself out of its
    /// own state — a coding configured with a token at run time builds
    /// decoders that answer it — and a body holds the decoder rather than
    /// the coding, so the string a caller is handed has to be copied out
    /// of a borrow that ends with the call. The coding's own
    /// [`token`](super::ContentCoding::token) is the same signature for
    /// the opposite reason: it lends, because an `Arc<dyn ContentCoding>`
    /// outlives every call made on it.
    fn token(&self) -> &str;
}

// Maintainer notes (not rendered):
//
// **`Box<dyn Decode>` rather than an enum, and it costs nothing**: every
// arm of the enum this replaced was already boxed, for the reason the
// module doc records and measures — 16 bytes and one allocation per
// decoded body, either shape.
//
// # `Send` is declared here, and leaving it off was this change's one
// real defect
//
// The enum this replaced *inferred* `Send` from its concrete payloads. A
// `dyn` declares its own auto traits, so the first version of this alias
// was `!Send` — and the wrapper this crate puts round every
// response body carries one, so `tokio::spawn` of a response body
// stopped compiling. That is
// this workspace's own rule met from the direction that costs
// something: *a `dyn` that declares no auto traits does not hide `Send`,
// it removes it.* `tests/spawnable_body.rs` said so on the first
// `--all-targets` build, which is amendment C14's own test doing exactly
// what C14 exists for.
//
// **The bound is on this alias and not on the trait**, and that is not a
// preference: a marker on a `pub trait X: Send {` line is **deleted** by
// `cargo fmt`, so `just fmt-check` and `just invariants` cannot both
// pass with it there — reproduced here, on this trait, before it was
// believed. This workspace already met that once and drew the rule then:
// demand the auto trait where the value is *stored*, never on the seam.
// A one-line `type` is a line `cargo fmt` does not reflow.
//
// **Public, and it is the seam's return type rather than a spelling
// repeated at every implementor.** Seven `impl`s wrote
// `Box<dyn Decode + Send>` out by hand for a day, and
// `no-send-or-sync-in-the-core-surface.sh` refused all seven — correctly,
// because each was a bare `Send` in the core surface with no marker, and
// a marker on seven `fn` lines is seven chances for one to be deleted by
// a reflow. Naming the type once is what the rule above already says to
// do, and this alias was already the place.
//
// It excludes nobody **and it is now a demand rather than an
// observation**, which is the one thing publishing the seam changed
// about it. While the codings were a compiled-in table this sentence
// read *"a fact about four types in this crate rather than a demand on
// anybody outside it"*; [`ContentCoding::decoder`](super::ContentCoding::decoder)
// hands back this type, so a third-party decoder holding an [`Rc`](std::rc::Rc)
// is `E0277` where it is boxed. That is the right direction and the
// alternative is worse: a `Client` whose response body stopped being
// `Send` because of a coding somebody installed would break
// `tokio::spawn` for every body it never touched.
/// The decoder for one response body.
///
/// A boxed [`Decode`], and `Send`:
/// [`ContentCoding::decoder`](super::ContentCoding::decoder)
/// hands back this type, so a third-party decoder holding an [`Rc`](std::rc::Rc)
/// is `E0277` where it is boxed. That is the right direction and the
/// alternative is worse: a `Client` whose response body stopped being
/// `Send` because of a coding somebody installed would break
/// `tokio::spawn` for every body it never touched.
pub type Decoder = Box<dyn Decode + Send>; // send-bound-exception: amendment-C14

/// The output buffer a push-shaped decoder writes into.
///
/// **[`BytesMut`] behind [`bytes::buf::Writer`], not a `Vec<u8>`, and the
/// difference is one allocation per body rather than one per frame.**
/// `BytesMut` is arena-like: [`split`](BytesMut::split) hands the filled
/// prefix to the caller as a `Bytes` and leaves this buffer owning the
/// rest of the same allocation, so the next frame writes into capacity
/// that already exists. A `Vec` cannot do that — the only way to hand its
/// bytes over as `Bytes` is to give the allocation away
/// (`Bytes::from(mem::take(..))`), so every frame started from nothing.
///
/// Measured twice. In isolation, over 1,000 frames of 4 KiB: **1,000
/// allocations for `Vec::take` + `Bytes::from`, 2 for this** — the second
/// being the arena growing to fit the largest frame it has seen, after
/// which there are none.
///
/// And through the whole client, counting every allocation a decoded
/// response makes, from a consumer outside this crate (`#![forbid(
/// unsafe_code)]` reaches test targets, so a counting allocator cannot
/// live in one):
///
/// | 256 KiB gzip body | allocations |
/// |---|---|
/// | 20 frames, before | 69 |
/// | 20 frames, after | **52** |
/// | 78 frames, before | 127 |
/// | 78 frames, after | **54** |
///
/// The shape is the claim rather than either number: the old cost grew
/// with the frame count and this one is nearly flat in it, because after
/// the arena has reached its high-water mark a frame costs no allocation
/// at all.
///
/// The `Writer` is what lets a decoder that wants an [`std::io::Write`]
/// — which `flate2` and `brotli-decompressor` both do, since they own
/// their sink — write into a `BufMut`. `BytesMut` is not an `io::Write`
/// itself, checked rather than assumed.
pub(super) type Out = bytes::buf::Writer<bytes::BytesMut>;

/// A fresh output buffer.
///
/// **No capacity up front.** The isolated measurement above shows one
/// allocation saved by pre-sizing, and picking a number would mean
/// guessing a body size for every response — including the ones that
/// decode to nothing. The arena reaches its own high-water mark on the
/// second frame either way.
#[allow(
    dead_code,
    reason = "a build with no coding features has no caller, exactly as `take` has none"
)]
pub(super) fn out() -> Out {
    bytes::BufMut::writer(bytes::BytesMut::new())
}

/// Takes the plaintext accumulated so far, leaving the buffer empty and
/// **its capacity intact** for the next frame.
///
/// That is the whole of what [`Out`] buys over a `Vec`, and it is why
/// this is `split` rather than `mem::take`.
#[allow(
    dead_code,
    reason = "a build with no coding features has no caller; checked by removing it and watching `--no-default-features` warn"
)]
pub(super) fn take(out: &mut Out) -> Bytes {
    out.get_mut().split().freeze()
}
