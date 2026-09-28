//! The two datagram paths a CONNECT-UDP tunnel opens: over HTTP/3's own
//! datagrams, and over DATAGRAM capsules on the tunnel's stream.

use std::fmt;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use futures_io::{AsyncRead, AsyncWrite};
use hclient_proxy::{BoxIo, BoxPath, DatagramPath};

use crate::capsule::{self, Capsule, Decoder};

/// The largest UDP payload a capsule path accepts. Capsules have no limit
/// of their own; this one leaves room for a whole QUIC packet at the size
/// QUIC starts at, and a little more, without inviting a relay to send
/// what the path past it cannot carry.
pub const CAPSULE_MAX: usize = 1350;

/// The one byte context id 0 is on the wire (RFC 9298 §5).
const CONTEXT_ZERO: u8 = 0x00;

/// The largest datagram an HTTP/3 path can hand over — a QUIC packet
/// carries no more than this.
const RECV_SCRATCH: usize = 65_535;

// Maintainer notes (not rendered):
// The tunnel's stream is held rather than dropped because dropping it ends
// the tunnel: over HTTP/3 the proxy reads the request stream's end as the
// end of the association. It is never read — capsules on it are skipped
// by RFC 9297 §3.2 anyway, and none of this crate's own are sent there
// over HTTP/3.
/// A CONNECT-UDP path over an HTTP/3 tunnel's datagrams: each one carries
/// context id 0 ahead of the UDP payload, and one with any other context
/// id is dropped.
pub struct ContextPath {
    inner: BoxPath,
    scratch: Mutex<Vec<u8>>,
    _stream: Mutex<BoxIo>,
}

impl ContextPath {
    /// A path over `inner`, the tunnel's datagrams, holding `stream` open
    /// for as long as the path lives.
    #[must_use]
    pub fn new(inner: BoxPath, stream: BoxIo) -> Self {
        Self {
            inner,
            scratch: Mutex::new(Vec::new()),
            _stream: Mutex::new(stream),
        }
    }
}

impl fmt::Debug for ContextPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContextPath")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl DatagramPath for ContextPath {
    fn try_send(&self, datagram: &[u8]) -> io::Result<()> {
        if datagram.len() > self.max_datagram_size() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a datagram longer than the tunnel carries",
            ));
        }
        let mut frame = Vec::with_capacity(1 + datagram.len());
        frame.push(CONTEXT_ZERO);
        frame.extend_from_slice(datagram);
        self.inner.try_send(&frame)
    }

    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.inner.poll_writable(cx)
    }

    fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let mut scratch = self.scratch.lock().unwrap_or_else(PoisonError::into_inner);
        if scratch.len() < RECV_SCRATCH {
            scratch.resize(RECV_SCRATCH, 0);
        }
        loop {
            let n = std::task::ready!(self.inner.poll_recv(cx, &mut scratch))?;
            let Some((context, header)) = capsule::varint::decode(&scratch[..n]) else {
                continue;
            };
            if context != 0 {
                continue;
            }
            let payload = &scratch[header..n];
            let len = payload.len().min(buf.len());
            buf[..len].copy_from_slice(&payload[..len]);
            return Poll::Ready(Ok(len));
        }
    }

    fn max_datagram_size(&self) -> usize {
        self.inner.max_datagram_size().saturating_sub(1)
    }
}

/// Who is waiting for the stream to take more of a capsule: the last
/// sender that asked [`DatagramPath::poll_writable`], and the last
/// receiver, which drains a kept tail too so that the last capsule of a
/// burst reaches the stream whether or not anything is sent after it.
///
/// Every write is polled with a waker that wakes both, so neither side's
/// interest is replaced by the other's, and a send from a task that is not
/// waiting at all replaces neither.
#[derive(Default)]
struct Waiters {
    sender: Mutex<Option<Waker>>,
    receiver: Mutex<Option<Waker>>,
}

impl Waiters {
    fn register(slot: &Mutex<Option<Waker>>, waker: &Waker) {
        let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.as_ref() {
            Some(w) if w.will_wake(waker) => {}
            _ => *slot = Some(waker.clone()),
        }
    }
}

impl Wake for Waiters {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        for slot in [&self.sender, &self.receiver] {
            let taken = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(w) = taken {
                w.wake();
            }
        }
    }
}

/// What the receiving side of a [`CapsulePath`] keeps between calls.
struct Receiving {
    decoder: Decoder,
    buf: Box<[u8]>,
}

/// A CONNECT-UDP path over DATAGRAM capsules on a tunnel's stream, for a
/// proxy spoken to over HTTP/2.
///
/// Safe to receive in one task and send in another, which is how a QUIC
/// stack uses a socket: every wait registers its caller's waker, and a
/// write wakes every waiter rather than the last one to poll. A send never
/// blocks and never loses part of a capsule — what the stream would not
/// take yet is kept, written ahead of the next capsule, and pushed on by
/// the receiving side in the meantime, so a capsule accepted is a capsule
/// that reaches the stream.
pub struct CapsulePath {
    io: Mutex<BoxIo>,
    /// The part of the last capsule the stream has not taken yet. At most
    /// one capsule: a send finding it non-empty after trying to drain it
    /// answers `WouldBlock`.
    tail: Mutex<Vec<u8>>,
    receiving: Mutex<Receiving>,
    waiters: Arc<Waiters>,
    waker: Waker,
}

impl CapsulePath {
    /// A path over `stream`, the tunnel's request and response bodies.
    #[must_use]
    pub fn new(stream: BoxIo) -> Self {
        let waiters = Arc::new(Waiters::default());
        Self {
            io: Mutex::new(stream),
            tail: Mutex::new(Vec::new()),
            receiving: Mutex::new(Receiving {
                decoder: Decoder::default(),
                buf: vec![0; 4096].into_boxed_slice(),
            }),
            waker: Waker::from(Arc::clone(&waiters)),
            waiters,
        }
    }

    /// Write as much of `tail` as the stream takes now, removing what it
    /// took, with the waker that wakes every waiter. `Ready(Ok)` once it is
    /// empty.
    fn drain(&self, tail: &mut Vec<u8>) -> Poll<io::Result<()>> {
        let mut cx = Context::from_waker(&self.waker);
        let mut io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        while !tail.is_empty() {
            match Pin::new(&mut *io).poll_write(&mut cx, tail) {
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
                }
                Poll::Ready(Ok(n)) => {
                    tail.drain(..n);
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        // Pushes what was written towards the socket now rather than on
        // the next poll; a stream that has nothing to flush answers at once.
        let _ = Pin::new(&mut *io).poll_flush(&mut cx);
        Poll::Ready(Ok(()))
    }
}

impl fmt::Debug for CapsulePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapsulePath").finish_non_exhaustive()
    }
}

impl DatagramPath for CapsulePath {
    fn try_send(&self, datagram: &[u8]) -> io::Result<()> {
        if datagram.len() > CAPSULE_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a datagram longer than the capsule path carries",
            ));
        }
        let mut tail = self.tail.lock().unwrap_or_else(PoisonError::into_inner);
        match self.drain(&mut tail) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Err(e),
            Poll::Pending => return Err(io::ErrorKind::WouldBlock.into()),
        }
        capsule::encode_datagram(datagram, &mut tail);
        match self.drain(&mut tail) {
            // Taken whole, or in part with the rest kept: accepted either
            // way, and the next send, wait or receive finishes it.
            Poll::Ready(Ok(())) | Poll::Pending => Ok(()),
            Poll::Ready(Err(e)) => Err(e),
        }
    }

    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Waiters::register(&self.waiters.sender, cx.waker());
        let mut tail = self.tail.lock().unwrap_or_else(PoisonError::into_inner);
        self.drain(&mut tail)
    }

    fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let mut receiving = self
            .receiving
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        {
            // A kept tail is pushed on from here too: a QUIC stack asks for
            // writability only when it has something more to send, so the
            // last capsule of a burst would otherwise wait for a send that
            // may never come. The receiver registers every time, before
            // looking at the tail, so it always owns the tail's write
            // interest — a tail a send leaves behind after this poll has
            // someone to wake, and waking a receiver costs one re-poll of a
            // pending read. Its error, if any, is the next send's to report.
            Waiters::register(&self.waiters.receiver, cx.waker());
            let mut tail = self.tail.lock().unwrap_or_else(PoisonError::into_inner);
            if !tail.is_empty() {
                let _ = self.drain(&mut tail);
            }
        }
        let Receiving {
            decoder,
            buf: scratch,
        } = &mut *receiving;
        loop {
            match decoder.next() {
                Ok(Some(Capsule::Datagram(payload))) => {
                    let len = payload.len().min(buf.len());
                    buf[..len].copy_from_slice(&payload[..len]);
                    return Poll::Ready(Ok(len));
                }
                Ok(Some(Capsule::Other { .. })) => continue,
                Ok(None) => {}
                Err(e) => return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
            }
            let n = {
                let mut io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
                std::task::ready!(Pin::new(&mut *io).poll_read(cx, scratch))?
            };
            if n == 0 {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "the proxy ended the tunnel",
                )));
            }
            decoder.push(&scratch[..n]);
        }
    }

    fn max_datagram_size(&self) -> usize {
        CAPSULE_MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hclient_proxy::testing::channel_pair;

    #[test]
    fn a_context_path_prepends_context_zero_and_is_one_byte_smaller() {
        let (a, b) = channel_pair(1300);
        let dummy = BoxIo::new(Never);
        let path = ContextPath::new(BoxPath::new(a), dummy);
        assert_eq!(path.max_datagram_size(), 1299);
        path.try_send(b"hello").unwrap();
        let mut buf = [0u8; 64];
        let n = poll_now(|cx| b.poll_recv(cx, &mut buf)).unwrap();
        assert_eq!(&buf[..n], b"\x00hello");
        let too_big = vec![0u8; 1300];
        assert_eq!(
            path.try_send(&too_big).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn a_context_path_drops_another_context_and_strips_its_own() {
        let (a, b) = channel_pair(1300);
        let path = ContextPath::new(BoxPath::new(a), BoxIo::new(Never));
        b.try_send(b"\x01other").unwrap();
        b.try_send(b"\x00mine").unwrap();
        let mut buf = [0u8; 64];
        let n = poll_now(|cx| path.poll_recv(cx, &mut buf)).unwrap();
        assert_eq!(&buf[..n], b"mine");
    }

    fn poll_now<T>(mut f: impl FnMut(&mut Context<'_>) -> Poll<T>) -> T {
        let mut cx = Context::from_waker(Waker::noop());
        match f(&mut cx) {
            Poll::Ready(t) => t,
            Poll::Pending => panic!("not ready"),
        }
    }

    /// A stream that never answers, for a path whose stream is only held.
    struct Never;
    impl AsyncRead for Never {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
    }
    impl AsyncWrite for Never {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }
    impl hclient_rt::Shutdown for Never {
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }
}
