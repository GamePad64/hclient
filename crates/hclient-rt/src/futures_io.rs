use hyper::rt::ReadBufCursor;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Bridges `futures_io::{AsyncRead, AsyncWrite}` → `hyper::rt::{Read, Write}`.
///
/// hyper-util only ships `TokioIo`; `smol-hyper` 0.1.1 has been dead since
/// 2023-12-29 and bridges in the opposite direction. Without this bridge,
/// the smol backend (Task 4) doesn't exist.
///
/// The implementation is **`unsafe`-free**: it reads into a scratch buffer
/// and copies out through the safe `ReadBufCursor::put_slice` — exactly the
/// technique `hyper::rt::Read`'s documentation recommends. The cost is one
/// copy per read; zero-copy would require `unsafe`
/// (`ReadBufCursor::as_mut`/`advance`), and the crate declares
/// `#![forbid(unsafe_code)]` — deliberately deferred.
pub struct FuturesIo<S> {
    inner: S,
    /// The buffer is allocated and zeroed ONCE, in [`FuturesIo::new`] — not
    /// on every `poll_read`.
    ///
    /// An earlier draft of this task kept it as `[0u8; SCRATCH]` on the
    /// stack inside `poll_read`. Measured (`rustc -O --emit=asm` on an
    /// isolated reproduction of both versions outside the rest of the
    /// crate): the stack variant calls `memset` on EVERY invocation
    /// (`subq $4096,%rsp` twice — that's the 8192 bytes — then `callq
    /// *memset@GOTPCREL`), whereas the struct-field version makes no such
    /// call at all — the reading function is called directly on an
    /// already-ready buffer. The allocation and the single zeroing happen
    /// in `new()` (`__rust_alloc_zeroed`), once per connection, not once
    /// per read. `poll_read` is the hot path of every request on smol; a
    /// stray `memset` there is not a hypothetical cost.
    scratch: Box<[u8]>,
}

/// Buffer size. 8 KiB is hyper's typical read size, so no extra iterations
/// result.
const SCRATCH: usize = 8 * 1024;

impl<S> FuturesIo<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            scratch: vec![0u8; SCRATCH].into_boxed_slice(),
        }
    }

    pub fn into_inner(self) -> S {
        self.inner
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }
}

// A hand-written `Debug`, not `#[derive]`: `derive` would dump all 8 KiB of
// `scratch` as a list of numbers on every format call — useless and noisy
// in logs. The same technique is already used in
// `hclient_core::RequestBody` (length instead of contents).
impl<S: std::fmt::Debug> std::fmt::Debug for FuturesIo<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuturesIo")
            .field("inner", &self.inner)
            .field("scratch_len", &self.scratch.len())
            .finish()
    }
}

impl<S: futures_io::AsyncRead + Unpin> hyper::rt::Read for FuturesIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        let want = buf.remaining().min(self.scratch.len());
        if want == 0 {
            return Poll::Ready(Ok(()));
        }
        // Destructure into disjoint fields explicitly: `inner` and
        // `scratch` are borrowed at the same time, but independently of
        // each other.
        let Self { inner, scratch } = &mut *self;
        let n = std::task::ready!(Pin::new(inner).poll_read(cx, &mut scratch[..want]))?;
        buf.put_slice(&scratch[..n]);
        Poll::Ready(Ok(()))
    }
}

impl<S: futures_io::AsyncWrite + Unpin> hyper::rt::Write for FuturesIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    /// `futures_io::AsyncWrite` gives no way to ask `S` whether its
    /// vectored write is efficient: the trait has no `is_write_vectored`
    /// method at all — only `poll_write`, `poll_write_vectored`,
    /// `poll_flush`, `poll_close`. The default `poll_write_vectored`
    /// implementation in `futures-io` 0.3.33 writes only the first
    /// non-empty buffer (`futures_io::AsyncWrite::poll_write_vectored`,
    /// checked against the dependency's source); for any `S` that hasn't
    /// overridden it, a vectored write silently degrades into one ordinary
    /// write — more syscalls, not fewer.
    ///
    /// hyper documents this method as a promise of an "efficient"
    /// `poll_write_vectored` implementation and branches on it to decide
    /// whether to coalesce buffers before writing. Returning `true` here
    /// would assert something we have no way to back up — and a capability
    /// that lies is worse than one that's absent, because the calling code
    /// (here, hyper itself) branches on it. The honest, conservative
    /// answer is `false`.
    ///
    /// If a concrete `S` shows up for which vectored writes are provably
    /// efficient, the right path is a separate constructor with an
    /// explicit opt-in, not an optimistic default here. Today there is no
    /// such consumer — Task 4 (`hclient-rt-smol`) hasn't been written yet
    /// — so adding one now would be speculative API with no caller.
    fn is_write_vectored(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_executor::block_on;
    use std::pin::Pin;

    /// A source that hands out data in chunks, to catch partial reads.
    struct Chunked {
        data: Vec<u8>,
        at: usize,
        step: usize,
    }
    impl futures_io::AsyncRead for Chunked {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let n = self.step.min(buf.len()).min(self.data.len() - self.at);
            buf[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            std::task::Poll::Ready(Ok(n))
        }
    }

    fn read_all(mut io: FuturesIo<Chunked>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut store = [0u8; 8];
        loop {
            let mut rb = hyper::rt::ReadBuf::new(&mut store);
            let poll = block_on(std::future::poll_fn(|cx| {
                hyper::rt::Read::poll_read(Pin::new(&mut io), cx, rb.unfilled())
            }));
            poll.unwrap();
            let filled = rb.filled().to_vec();
            if filled.is_empty() {
                return out;
            }
            out.extend_from_slice(&filled);
        }
    }

    #[test]
    fn forwards_bytes_through_partial_reads() {
        let io = FuturesIo::new(Chunked {
            data: b"hello world".to_vec(),
            at: 0,
            step: 3,
        });
        assert_eq!(read_all(io), b"hello world");
    }

    #[test]
    fn never_writes_more_than_remaining() {
        // step is larger than the buffer's capacity: put_slice must not panic.
        let io = FuturesIo::new(Chunked {
            data: vec![7u8; 64],
            at: 0,
            step: 64,
        });
        assert_eq!(read_all(io).len(), 64);
    }

    #[test]
    fn into_inner_round_trips() {
        let io = FuturesIo::new(Chunked {
            data: vec![],
            at: 0,
            step: 1,
        });
        let c = io.into_inner();
        assert_eq!(c.step, 1);
    }

    #[test]
    fn read_request_larger_than_scratch_buffer_does_not_panic() {
        // `buf.remaining()` is controlled by the caller (hyper) and can be
        // larger than `scratch.len()` — `want` must be clamped to the
        // buffer's size, or `&mut scratch[..want]` indexes out of bounds
        // and panics. This was previously untested by anything:
        // `never_writes_more_than_remaining` uses an 8-byte caller buffer,
        // which is always smaller than `SCRATCH`, so removing the
        // `.min(..)` wouldn't have been caught there.
        let len = super::SCRATCH + 137;
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let mut io = FuturesIo::new(Chunked {
            data: data.clone(),
            at: 0,
            step: len,
        });
        let mut out = Vec::new();
        let mut store = vec![0u8; len];
        loop {
            let mut rb = hyper::rt::ReadBuf::new(&mut store);
            let poll = block_on(std::future::poll_fn(|cx| {
                hyper::rt::Read::poll_read(Pin::new(&mut io), cx, rb.unfilled())
            }));
            poll.unwrap();
            let filled = rb.filled().to_vec();
            if filled.is_empty() {
                break;
            }
            out.extend_from_slice(&filled);
        }
        assert_eq!(out, data);
    }

    /// An `AsyncWrite` stub that's always "successful" — needed only to
    /// check `is_write_vectored()`, not write behavior.
    struct NullWrite;
    impl futures_io::AsyncWrite for NullWrite {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn is_write_vectored_reports_false_not_an_unverifiable_claim() {
        // `futures_io::AsyncWrite` gives no way to ask `S` whether its
        // vectored write is efficient — so `true` here would be an
        // assertion we have no way to back up. See the doc comment on the
        // impl.
        let io = FuturesIo::new(NullWrite);
        assert!(!hyper::rt::Write::is_write_vectored(&io));
    }
}
