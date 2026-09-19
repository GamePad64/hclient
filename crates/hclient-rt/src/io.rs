//! Half-close, which is the one thing `futures-io` does not say.
//!
//! # Why the byte-stream seam is `futures-io` and this is not
//!
//! [`futures_io::AsyncRead`] and [`AsyncWrite`](futures_io::AsyncWrite) are
//! the seam's read and write halves, and they are somebody else's traits on
//! purpose. What this workspace needs from a byte stream is
//! `poll_read(&mut [u8]) -> Poll<Result<usize>>` and its write counterpart,
//! which is exactly what they are — so declaring our own would have been a
//! second spelling of a stable vocabulary, and one more thing an outside
//! implementor has to learn.
//!
//! **They were `hyper::rt::Read`/`Write` until this module existed**, and
//! the argument for those was real: `hyper::rt` is where every `S` in this
//! vertical ends up anyway, since `hyper::client::conn::http1::handshake`
//! accepts nothing else. What it did not cover is *whose major version the
//! seam promises*. A public bound naming `hyper::rt::Read` puts hyper's
//! major in the manifest of every implementor — nine crates here, and
//! anybody outside who writes a runtime or a TLS backend. This workspace
//! has paid that once and repaired it: `hclient-dns` leaked `domain`
//! through one `pub fn`, recorded as *the leak outlived the decoder it
//! leaked*. hyper is a dependency this workspace may one day replace;
//! `http`, `bytes` and `futures-io` are not.
//!
//! The uninitialised-buffer machinery went with it and is not missed:
//! `hyper::rt::ReadBufCursor` exists to let an implementation fill memory
//! that was never zeroed, and **measured before the change, no
//! implementation in this workspace took that path** — 39 `put_slice` call
//! sites, zero uses of the cursor's `unsafe` `as_mut`/`advance`. Both
//! shipped runtimes already read into a scratch buffer and copied out. So
//! `&mut [u8]` costs nothing that was being collected.
//!
//! # And half-close is the one thing that had to stay ours
//!
//! `futures_io::AsyncWrite` ends a stream with `poll_close`, which is
//! *close the writer*. An HTTP client needs the narrower promise: **send
//! FIN and go on reading the response**, which is what
//! `hyper::rt::Write::poll_shutdown` meant and what `TcpStream::shutdown`
//! does. Folding the two would lose a distinction this workspace treats as
//! load-bearing — `CLAUDE.md` records a half-close as *blocker two* against
//! an `embedded-nal-async` adapter, and records the W7 spike forwarding
//! `poll_shutdown` to `flush` as *"a half-close hyper believes it performed
//! and did not"*. `hclient-rt-embassy` owns its socket rather than a NAL
//! `Connection` precisely so that it can send a real one.
//!
//! So the seam is `AsyncRead + AsyncWrite + Shutdown`: two traits that
//! already exist for what they already say, and one of ours for the thing
//! only this vertical asks about.

use std::io::Result;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Send FIN while remaining able to read, and say whether vectored writes
/// are worth issuing.
///
/// This is not [`futures_io::AsyncWrite::poll_close`], and the difference
/// is the whole reason the trait exists: `poll_close` ends the stream,
/// where this ends only the writing half. An HTTP/1 client sends its
/// request, half-closes, and reads the response off a connection the peer
/// has not closed — so a `poll_shutdown` that forwarded to `poll_close`
/// would break the exchange, and one that forwarded to `flush` would
/// silently do nothing at all.
///
/// **A runtime that cannot half-close should say so rather than pretend**,
/// which is what the `embedded-nal-async` note in this module's
/// documentation is about: `embedded_io_async::Write` is `write` and
/// `flush` and nothing else, so an adapter over it has no honest body for
/// this method.
pub trait Shutdown {
    /// Send FIN. The read half stays open.
    ///
    /// # Errors
    ///
    /// Whatever the OS's `shutdown(2)` answers — with one convention this
    /// workspace already applies at its call sites: `ENOTCONN` on a peer
    /// that has already gone is **not** an error, and only the unixes
    /// report it, which `hclient-rt-tokio` documents where it maps the
    /// result.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>>;

    /// Whether `poll_write_vectored` reaches a real vectored write.
    ///
    /// **`futures_io::AsyncWrite` has no such method and `hyper::rt::Write`
    /// does**, which is the one capability that did not survive the move to
    /// somebody else's traits — so it lives here, beside the other thing
    /// only this vertical asks about. Without it a writer cannot tell a
    /// sink that coalesces from one that issues a syscall per slice, and
    /// `hclient-native` forwards the answer to hyper, which chooses its
    /// write strategy from it.
    ///
    /// Defaulted to `false`, the understating direction this workspace
    /// applies to every capability constant: a caller that believes a
    /// `false` merely writes one buffer at a time, where one that believes
    /// a wrong `true` pays a syscall per slice.
    fn is_write_vectored(&self) -> bool {
        false
    }
}
