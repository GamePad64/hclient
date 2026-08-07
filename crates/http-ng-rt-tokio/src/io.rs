use hyper::rt::ReadBufCursor;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

/// Bridges `tokio::net::TcpStream` → `hyper::rt`. `unsafe`-free: reads into
/// a scratch buffer and copies out with the safe `put_slice` — the same
/// technique as `http_ng_rt::FuturesIo` (Task 2), but here directly on top
/// of `tokio::io::{AsyncRead, AsyncWrite}` rather than `futures_io`: tokio
/// has its own IO traits, and an extra layer through `FuturesIo` would only
/// add a copy.
pub struct TokioIo {
    inner: tokio::net::TcpStream,
    /// The buffer is allocated and zeroed ONCE, in [`TokioIo::new`] — not
    /// on every `poll_read`.
    ///
    /// An earlier draft of this task kept it as `[0u8; SCRATCH]` on the
    /// stack inside `poll_read`. Task 2 already measured the cost of this
    /// pattern (`rustc -O --emit=asm`, see the comment on
    /// `FuturesIo::scratch`): the stack variant calls `memset` on EVERY
    /// invocation, whereas a struct field allocated once in the
    /// constructor does not — `poll_read` is called directly on an
    /// already-ready buffer. `poll_read` is the hot path of every request;
    /// a stray `memset` there is not a hypothetical cost, but a measured
    /// one.
    scratch: Box<[u8]>,
}

/// Buffer size. 8 KiB is hyper's typical read size, so no extra iterations
/// result. Matches `http_ng_rt::FuturesIo::SCRATCH`.
const SCRATCH: usize = 8 * 1024;

impl TokioIo {
    pub(crate) fn new(inner: tokio::net::TcpStream) -> Self {
        Self {
            inner,
            scratch: vec![0u8; SCRATCH].into_boxed_slice(),
        }
    }

    /// A reference to the underlying `tokio::net::TcpStream` — for example,
    /// to read applied `TcpOpts` back (`nodelay()`, …) in tests or
    /// diagnostics.
    pub fn get_ref(&self) -> &tokio::net::TcpStream {
        &self.inner
    }

    pub fn into_inner(self) -> tokio::net::TcpStream {
        self.inner
    }
}

// A hand-written `Debug`, not `#[derive]`: `derive` would dump all 8 KiB of
// `scratch` as a list of numbers on every format call — useless and noisy
// in logs. The same technique is already used in `http_ng_rt::FuturesIo`.
impl std::fmt::Debug for TokioIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioIo")
            .field("inner", &self.inner)
            .field("scratch_len", &self.scratch.len())
            .finish()
    }
}

impl hyper::rt::Read for TokioIo {
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
        let mut rb = tokio::io::ReadBuf::new(&mut scratch[..want]);
        std::task::ready!(Pin::new(inner).poll_read(cx, &mut rb))?;
        let filled = rb.filled().len();
        buf.put_slice(&scratch[..filled]);
        Poll::Ready(Ok(()))
    }
}

impl hyper::rt::Write for TokioIo {
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
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    /// Unlike `FuturesIo` (where `futures_io::AsyncWrite` gives no way to
    /// ask `S` at all), `tokio::io::AsyncWrite` carries
    /// `is_write_vectored` as a trait method — here it's an honest
    /// delegation to the underlying `TcpStream`, not a decision made on
    /// its behalf.
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::rt::Read as _;
    use std::io::Write as _;

    fn connected_pair() -> (TokioIo, std::net::TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || listener.accept().unwrap().0);
        let std_client = std::net::TcpStream::connect(addr).unwrap();
        std_client.set_nonblocking(true).unwrap();
        let client = TokioIo::new(tokio::net::TcpStream::from_std(std_client).unwrap());
        (client, server.join().unwrap())
    }

    #[tokio::test]
    async fn reads_bytes_larger_than_the_scratch_buffer() {
        // Same as `read_request_larger_than_scratch_buffer_does_not_panic`
        // in `http_ng_rt::FuturesIo`: `buf.remaining()` is controlled by
        // the caller and can be larger than `scratch.len()` — without
        // `.min(..)`, `&mut scratch[..want]` indexes out of bounds and
        // panics.
        let (mut client, mut server) = connected_pair();
        let len = SCRATCH + 137;
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let writer = {
            let data = data.clone();
            std::thread::spawn(move || server.write_all(&data).unwrap())
        };

        let mut out = Vec::new();
        let mut store = vec![0u8; len];
        loop {
            let mut rb = hyper::rt::ReadBuf::new(&mut store);
            std::future::poll_fn(|cx| Pin::new(&mut client).poll_read(cx, rb.unfilled()))
                .await
                .unwrap();
            let filled = rb.filled().to_vec();
            if filled.is_empty() {
                break;
            }
            out.extend_from_slice(&filled);
            if out.len() >= len {
                break;
            }
        }
        writer.join().unwrap();
        assert_eq!(out, data);
    }

    #[tokio::test]
    async fn is_write_vectored_delegates_to_the_inner_stream() {
        let (client, _server) = connected_pair();
        // `TcpStream::is_write_vectored` is an honest delegation, not a
        // conservative stub: compare against `TcpStream` itself, not a
        // hardcoded value, so the test doesn't drift from the platform
        // independently of `TokioIo`.
        assert_eq!(
            hyper::rt::Write::is_write_vectored(&client),
            client.get_ref().is_write_vectored()
        );
    }
}
