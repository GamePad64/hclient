use hyper::rt::ReadBufCursor;
use std::fmt::Debug;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

/// Bridges `tokio::net::TcpStream` → `hyper::rt`. `unsafe`-free: reads into
/// a scratch buffer and copies out with the safe `put_slice` — the same
/// technique as `hclient_rt::FuturesIo`, but here directly on top
/// of `tokio::io::{AsyncRead, AsyncWrite}` rather than `futures_io`: tokio
/// has its own IO traits, and an extra layer through `FuturesIo` would only
/// add a copy.
pub struct TokioIo {
    inner: Socket,
    /// The buffer is allocated and zeroed ONCE, in [`TokioIo::new`] — not
    /// on every `poll_read`.
    ///
    /// A `[0u8; SCRATCH]` on the stack inside `poll_read` is the obvious
    /// alternative, and its cost is measured (`rustc -O --emit=asm`, see
    /// the comment on `FuturesIo::scratch`): the stack variant calls
    /// `memset` on EVERY
    /// invocation, whereas a struct field allocated once in the
    /// constructor does not — `poll_read` is called directly on an
    /// already-ready buffer. `poll_read` is the hot path of every request;
    /// a stray `memset` there is not a hypothetical cost, but a measured
    /// one.
    scratch: Box<[u8]>,
}

/// Buffer size. 8 KiB is hyper's typical read size, so no extra iterations
/// result. Matches `hclient_rt::FuturesIo::SCRATCH`.
const SCRATCH: usize = 8 * 1024;

impl TokioIo {
    pub(crate) fn new(inner: tokio::net::TcpStream) -> Self {
        Self::over(Socket::Tcp(inner))
    }

    /// The same, over a Unix-domain stream — `TcpConnect::connect_unix`.
    #[cfg(unix)]
    pub(crate) fn unix(inner: tokio::net::UnixStream) -> Self {
        Self::over(Socket::Unix(inner))
    }

    fn over(inner: Socket) -> Self {
        Self {
            inner,
            scratch: vec![0u8; SCRATCH].into_boxed_slice(),
        }
    }

    /// A reference to the underlying `tokio::net::TcpStream` — for example,
    /// to read applied `TcpOpts` back (`nodelay()`, …) in tests or
    /// diagnostics.
    ///
    /// # Panics
    ///
    /// On a Unix-domain stream, where there is no `TcpStream` to hand
    /// back and every `TcpOpts` field this accessor exists to read has no
    /// meaning. A `Result` or an `Option` was the alternative and is
    /// worse: every caller of this method today holds a connection it made
    /// with [`TcpConnect::connect`](hclient_rt::TcpConnect::connect), and
    /// `AF_UNIX` cannot be reached from there — so the failing arm would
    /// be unreachable noise at each of them.
    pub fn get_ref(&self) -> &tokio::net::TcpStream {
        match &self.inner {
            Socket::Tcp(s) => s,
            #[cfg(unix)]
            Socket::Unix(_) => panic!("get_ref on a Unix-domain stream: there is no TcpStream"),
        }
    }

    /// # Panics
    ///
    /// On a Unix-domain stream, for [`get_ref`](Self::get_ref)'s reason.
    pub fn into_inner(self) -> tokio::net::TcpStream {
        match self.inner {
            Socket::Tcp(s) => s,
            #[cfg(unix)]
            Socket::Unix(_) => panic!("into_inner on a Unix-domain stream: there is no TcpStream"),
        }
    }
}

/// What a [`TokioIo`] is actually over.
///
/// An enum here rather than a type parameter on `TokioIo`, because
/// `TcpConnect::Stream` is one associated type and both connects must
/// produce it — see `TcpConnect::connect_unix` for why that seam is one
/// trait rather than two. The cost is one branch per `poll_*`, against a
/// syscall.
enum Socket {
    Tcp(tokio::net::TcpStream),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

/// Delegates one method to whichever socket is underneath.
///
/// A macro rather than six hand-written matches: they differ only in the
/// method name and its arguments, and a hand-written set is where one arm
/// eventually gets a different body by accident.
macro_rules! either {
    ($self:expr, $io:ident => $call:expr) => {
        match &mut $self.inner {
            Socket::Tcp($io) => $call,
            #[cfg(unix)]
            Socket::Unix($io) => $call,
        }
    };
}

impl Debug for Socket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Socket::Tcp(s) => s.fmt(f),
            #[cfg(unix)]
            Socket::Unix(s) => s.fmt(f),
        }
    }
}

// A hand-written `Debug`, not `#[derive]`: `derive` would dump all 8 KiB of
// `scratch` as a list of numbers on every format call — useless and noisy
// in logs. The same technique is already used in `hclient_rt::FuturesIo`.
impl Debug for TokioIo {
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
        let n = match inner {
            Socket::Tcp(s) => Pin::new(s).poll_read(cx, &mut rb),
            #[cfg(unix)]
            Socket::Unix(s) => Pin::new(s).poll_read(cx, &mut rb),
        };
        std::task::ready!(n)?;
        let filled = rb.filled().len();
        buf.put_slice(&scratch[..filled]);
        Poll::Ready(Ok(()))
    }
}

/// `shutdown` on a socket whose peer has already gone is **not an error**,
/// and only the unixes say otherwise.
///
/// `shutdown(2)` returns `ENOTCONN` on macOS and the BSDs for an
/// `AF_UNIX` socket the peer has closed, where Linux returns success.
/// What the caller asked for is "my write half is closed"; a socket that
/// is not connected has certainly reached that state, so reporting a
/// failure turns a **completed** exchange into an error.
///
/// It is not hypothetical and it was not cheap: `Native::unix_socket` was
/// unusable on macOS against any server that closes first — which is every
/// server answering `Connection: close` — and it surfaced as
/// `ErrorKind::Connect`, naming the phase that had already succeeded.
/// Found only when `test (macos-latest)` started finishing runs again.
///
/// Applied to every socket kind rather than to the unix arm alone: the
/// argument is about what `shutdown` means, not about which address
/// family is asking, and a narrower fix would invite the same report for
/// TCP on the next BSD.
fn shutdown_is_done(r: std::io::Result<()>) -> std::io::Result<()> {
    match r {
        Err(e) if e.kind() == std::io::ErrorKind::NotConnected => Ok(()),
        other => other,
    }
}

impl hyper::rt::Write for TokioIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        either!(self, s => Pin::new(s).poll_write(cx, buf))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        either!(self, s => Pin::new(s).poll_flush(cx))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let p = either!(self, s => Pin::new(s).poll_shutdown(cx));
        Poll::Ready(shutdown_is_done(std::task::ready!(p)))
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        either!(self, s => Pin::new(s).poll_write_vectored(cx, bufs))
    }

    /// Unlike `FuturesIo` (where `futures_io::AsyncWrite` gives no way to
    /// ask `S` at all), `tokio::io::AsyncWrite` carries
    /// `is_write_vectored` as a trait method — here it's an honest
    /// delegation to the underlying `TcpStream`, not a decision made on
    /// its behalf.
    fn is_write_vectored(&self) -> bool {
        match &self.inner {
            Socket::Tcp(s) => s.is_write_vectored(),
            #[cfg(unix)]
            Socket::Unix(s) => s.is_write_vectored(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::poll_fn;
    /// The `ENOTCONN`-on-shutdown decision, checked where it can be:
    /// Linux never produces the error, so the platform cannot be the test.
    /// What is testable is our own rule, in all three directions.
    #[test]
    fn a_shutdown_of_a_socket_that_is_already_gone_is_success() {
        use std::io::ErrorKind;
        assert!(super::shutdown_is_done(Ok(())).is_ok());
        assert!(
            super::shutdown_is_done(Err(std::io::Error::from(ErrorKind::NotConnected))).is_ok(),
            "macOS reports ENOTCONN for a peer that closed first, and the write \
             half it asks about is closed either way"
        );
        // The control, and the half that makes this more than
        // `|_| Ok(())`: every other error still travels.
        assert_eq!(
            super::shutdown_is_done(Err(std::io::Error::from(ErrorKind::BrokenPipe)))
                .unwrap_err()
                .kind(),
            ErrorKind::BrokenPipe
        );
    }

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
        // in `hclient_rt::FuturesIo`: `buf.remaining()` is controlled by
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
            poll_fn(|cx| Pin::new(&mut client).poll_read(cx, rb.unfilled()))
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
