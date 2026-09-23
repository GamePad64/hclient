use std::fmt::Debug;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

/// Bridges `tokio::net::TcpStream` → [`futures_io::AsyncRead`]/
/// [`AsyncWrite`](futures_io::AsyncWrite) plus [`hclient_rt::Shutdown`].
///
/// **`unsafe`-free and, since the seam stopped being `hyper::rt`, also
/// copy-free.** The cursor version kept a per-connection scratch buffer
/// because `ReadBufCursor` hands over possibly-uninitialised memory;
/// `futures-io` hands over an initialised slice, so the read goes straight
/// into the caller's buffer and the field is gone. What remains is the
/// enum, because one associated `Stream` type has to cover both TCP and
/// Unix sockets.
pub struct TokioIo {
    inner: Socket,
}

impl TokioIo {
    pub(crate) fn new(inner: tokio::net::TcpStream) -> Self {
        Self::over(Socket::Tcp(inner))
    }

    /// The same, over a Unix-domain stream — `IpcConnect::connect_ipc`.
    #[cfg(unix)]
    pub(crate) fn unix(inner: tokio::net::UnixStream) -> Self {
        Self::over(Socket::Unix(inner))
    }

    fn over(inner: Socket) -> Self {
        Self { inner }
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
/// produce it — which is why `IpcConnect` extends `TcpConnect` rather than
/// naming a stream of its own. The cost is one branch per `poll_*`, against a
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
// in logs.
impl Debug for TokioIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioIo")
            .field("inner", &self.inner)
            .finish()
    }
}

impl futures_io::AsyncRead for TokioIo {
    /// **No scratch buffer, and that is what the seam change bought.**
    ///
    /// The cursor version read into a per-connection scratch and copied
    /// out with `put_slice`, because `hyper::rt::ReadBufCursor` hands over
    /// possibly-uninitialised memory and filling it directly is `unsafe`.
    /// `futures_io::AsyncRead` hands over an initialised `&mut [u8]`, so
    /// `tokio::io::ReadBuf::new` wraps the caller's own buffer and the
    /// copy — one per read, per connection, on the hot path — is gone
    /// with the field that held it.
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let mut rb = tokio::io::ReadBuf::new(buf);
        let n = match &mut self.inner {
            Socket::Tcp(s) => Pin::new(s).poll_read(cx, &mut rb),
            #[cfg(unix)]
            Socket::Unix(s) => Pin::new(s).poll_read(cx, &mut rb),
        };
        std::task::ready!(n)?;
        Poll::Ready(Ok(rb.filled().len()))
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

impl hclient_rt::Shutdown for TokioIo {
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let p = either!(self, s => Pin::new(s).poll_shutdown(cx));
        Poll::Ready(shutdown_is_done(std::task::ready!(p)))
    }
}

impl futures_io::AsyncWrite for TokioIo {
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

    /// `futures-io` spells the end of the stream `poll_close`, and for a
    /// socket that is the same half-close [`hclient_rt::Shutdown`] asks
    /// for — `tokio::io::AsyncWrite::poll_shutdown` sends FIN and leaves
    /// the read half open. So this forwards rather than stating it twice.
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        hclient_rt::Shutdown::poll_shutdown(self, cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        either!(self, s => Pin::new(s).poll_write_vectored(cx, bufs))
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
    use futures_io::AsyncRead as _;
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

    /// **A read far larger than one segment, across several polls.**
    ///
    /// It was `reads_bytes_larger_than_the_scratch_buffer`, and it pinned
    /// a defect that no longer has a subject: this bridge held an 8 KiB
    /// scratch buffer because `hyper::rt::ReadBufCursor` hands out
    /// possibly-uninitialised memory, and a caller's buffer larger than
    /// that indexed out of bounds without a `.min(..)`. The seam is
    /// `futures_io::AsyncRead` now, the socket reads straight into the
    /// caller's buffer, and there is no second buffer to overrun. What
    /// survives is the property rather than the defect: a body bigger
    /// than a segment arrives whole and in order, which is a claim about
    /// the loop below rather than about any constant.
    #[tokio::test]
    async fn reads_a_body_larger_than_one_segment_in_order() {
        let (mut client, mut server) = connected_pair();
        let len = 8 * 1024 + 137;
        // `i % 251` is always in 0..251, which fits `u8` — bounded by the
        // modulus, not by `len`.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "`i % 251` is always in 0..251, which fits `u8` — bounded by the modulus, not by `len`."
        )]
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let writer = {
            let data = data.clone();
            std::thread::spawn(move || server.write_all(&data).unwrap())
        };

        let mut out = Vec::new();
        let mut store = vec![0u8; len];
        loop {
            let n = poll_fn(|cx| Pin::new(&mut client).poll_read(cx, &mut store))
                .await
                .unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&store[..n]);
            if out.len() >= len {
                break;
            }
        }
        writer.join().unwrap();
        assert_eq!(out, data);
    }
}
