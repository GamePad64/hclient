//! A datagram path through a filter: one peer, whole datagrams.

use std::fmt::Debug;
use std::io;
use std::io::IoSliceMut;
use std::net::SocketAddr;
use std::task::{Context, Poll};

use hclient_rt::{Datagrams, RecvMeta, UdpDatagrams, UdpSupport};

// Maintainer notes (not rendered):
// `Send + Sync` here and nowhere else in the seam, because the one
// consumer of a path is a QUIC stack and `quinn::AsyncUdpSocket` demands
// both of every socket. A bound that is paid anyway costs nothing to state,
// and stating it is what lets the path be one concrete `BoxPath`. The shape
// is quinn's own — `try_send` plus `poll_writable` — so an adapter in
// either direction is a forward.
/// A connected datagram path to one peer, as a filter opens it.
///
/// Whole datagrams only: no addresses, no segmentation offload, no ECN. A
/// relay carries none of those, and a QUIC stack over a path is told so.
// Maintainer notes (not rendered):
// The bound is a `where` clause rather than an inline supertrait list,
// and that is a formatting accommodation rather than a design choice: a
// `send-bound-exception` marker trailing an inline `pub trait X: Send +
// Sync + Debug {` line is deleted by a `cargo fmt` reflow, reproduced on
// this trait, the same finding this workspace recorded when the
// decompression codings and `auth`'s two traits met it. A `where` clause
// whose last predicate abuts the opening brace loses its trailing
// comment the same way; putting the unmarked `Debug` predicate last
// keeps the marked `Send + Sync` predicate away from the brace, and the
// bound is otherwise identical to `DatagramPath: Send + Sync + Debug`.
pub trait DatagramPath
where
    Self: Send + Sync, // send-bound-exception: amendment-C16
    Self: Debug,
{
    /// Send one datagram, or answer [`io::ErrorKind::WouldBlock`] — a real
    /// answer that obliges the caller to [`poll_writable`](Self::poll_writable)
    /// first.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] for a datagram longer than
    /// [`max_datagram_size`](Self::max_datagram_size) — never truncated —
    /// and whatever the path's carrier answers.
    fn try_send(&self, datagram: &[u8]) -> io::Result<()>;

    /// Ready when [`try_send`](Self::try_send) may succeed.
    ///
    /// # Errors
    ///
    /// The path has ended.
    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>>;

    /// The next datagram, copied into `buf`, its length returned.
    ///
    /// A datagram longer than `buf` is **truncated** to it, and the rest
    /// of it is lost: the length returned is what was copied, and nothing
    /// says a datagram was cut. That is what a UDP socket does, and every
    /// path in this crate and in the transports that use it does the same.
    /// A caller that must not lose bytes passes a buffer of at least
    /// [`max_datagram_size`](Self::max_datagram_size) — a QUIC stack always
    /// does, since it receives into buffers sized for the largest UDP
    /// payload.
    ///
    /// # Errors
    ///
    /// The path has ended — its carrier closed or failed.
    fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>>;

    /// The largest datagram [`try_send`](Self::try_send) accepts.
    ///
    /// A **ceiling**, not a promise about the link beneath it: a datagram
    /// of this size or smaller is accepted by the path, and may still be
    /// lost on its way — a relay's own link can be narrower than the path
    /// knows, behind a VPN or a tunnel, and a datagram over it disappears
    /// without an error. A transport treats this as the upper bound of what
    /// it discovers, starting from its own floor, rather than as a size it
    /// may send from the start.
    fn max_datagram_size(&self) -> usize;
}

/// A path whose type is gone.
#[derive(Debug)]
pub struct BoxPath(Box<dyn DatagramPath>);

impl BoxPath {
    /// Erase a path.
    pub fn new<P: DatagramPath + 'static>(path: P) -> Self {
        Self(Box::new(path))
    }
}

impl DatagramPath for BoxPath {
    fn try_send(&self, d: &[u8]) -> io::Result<()> {
        self.0.try_send(d)
    }
    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.poll_writable(cx)
    }
    fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        self.0.poll_recv(cx, buf)
    }
    fn max_datagram_size(&self) -> usize {
        self.0.max_datagram_size()
    }
}

/// A runtime's UDP socket, erased — what [`Dial::bind_udp`](crate::egress::Dial::bind_udp)
/// lends.
///
/// **A receive may be several datagrams.** Where the runtime does GRO, one
/// `poll_recv` can hand over a run of datagrams coalesced into one buffer,
/// [`RecvMeta::stride`](hclient_rt::RecvMeta::stride) bytes apart, up to
/// [`UdpSupport::max_recv_segments`](hclient_rt::UdpSupport::max_recv_segments)
/// of them. A path built on this socket must split each receive by its
/// stride before reading a datagram out of it, as the built-in SOCKS5
/// path does — reading the buffer as one datagram loses every one after
/// the first.
pub struct BoxUdp(Box<dyn UdpDatagrams + Send + Sync>); // send-bound-exception: amendment-C16

impl BoxUdp {
    /// Erase a socket.
    pub fn new<S>(socket: S) -> Self
    where
        S: UdpDatagrams + Send + Sync + 'static, // send-bound-exception: amendment-C16
    {
        Self(Box::new(socket))
    }
}

impl Debug for BoxUdp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BoxUdp")
    }
}

impl UdpDatagrams for BoxUdp {
    fn try_send(&self, t: &Datagrams<'_>) -> io::Result<()> {
        self.0.try_send(t)
    }
    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.poll_writable(cx)
    }
    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.0.poll_recv(cx, bufs, meta)
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }
    fn support(&self) -> UdpSupport {
        self.0.support()
    }
}

pub(crate) fn too_big(len: usize, max: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("a {len}-byte datagram does not fit a path of {max}"),
    )
}

/// Test doubles for a path, for filters' and transports' own tests,
/// behind the `test-util` feature.
#[cfg(any(test, feature = "test-util"))]
#[doc(hidden)]
pub mod testing {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::task::Waker;

    use super::{Context, DatagramPath, Debug, Poll, io, too_big};

    #[derive(Debug, Default)]
    struct Queue {
        items: VecDeque<Vec<u8>>,
        waker: Option<Waker>,
        closed: bool,
    }

    /// One end of an in-memory pair.
    #[derive(Debug)]
    pub struct ChannelPath {
        inbox: Arc<Mutex<Queue>>,
        outbox: Arc<Mutex<Queue>>,
        max: usize,
    }

    impl Drop for ChannelPath {
        fn drop(&mut self) {
            let mut q = self.outbox.lock().expect("path mutex");
            q.closed = true;
            if let Some(w) = q.waker.take() {
                w.wake();
            }
        }
    }

    impl DatagramPath for ChannelPath {
        fn try_send(&self, d: &[u8]) -> io::Result<()> {
            if d.len() > self.max {
                return Err(too_big(d.len(), self.max));
            }
            let mut q = self.outbox.lock().expect("path mutex");
            q.items.push_back(d.to_vec());
            if let Some(w) = q.waker.take() {
                w.wake();
            }
            Ok(())
        }
        fn poll_writable(&self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
            let mut q = self.inbox.lock().expect("path mutex");
            if let Some(d) = q.items.pop_front() {
                let n = d.len().min(buf.len());
                buf[..n].copy_from_slice(&d[..n]);
                return Poll::Ready(Ok(n));
            }
            if q.closed {
                return Poll::Ready(Err(io::ErrorKind::ConnectionAborted.into()));
            }
            q.waker = Some(cx.waker().clone());
            Poll::Pending
        }
        fn max_datagram_size(&self) -> usize {
            self.max
        }
    }

    /// Two ends of an in-memory path, each carrying datagrams up to `max`.
    pub fn channel_pair(max: usize) -> (ChannelPath, ChannelPath) {
        let ab = Arc::new(Mutex::new(Queue::default()));
        let ba = Arc::new(Mutex::new(Queue::default()));
        (
            ChannelPath {
                inbox: Arc::clone(&ba),
                outbox: Arc::clone(&ab),
                max,
            },
            ChannelPath {
                inbox: ab,
                outbox: ba,
                max,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    fn cx() -> Context<'static> {
        Context::from_waker(Waker::noop())
    }

    #[test]
    fn a_channel_pair_delivers_whole_datagrams_both_ways() {
        let (a, b) = testing::channel_pair(1200);
        a.try_send(b"hello").unwrap();
        let mut buf = [0u8; 64];
        let Poll::Ready(Ok(n)) = b.poll_recv(&mut cx(), &mut buf) else {
            panic!("b recv")
        };
        assert_eq!(&buf[..n], b"hello");
        b.try_send(b"back").unwrap();
        let Poll::Ready(Ok(n)) = a.poll_recv(&mut cx(), &mut buf) else {
            panic!("a recv")
        };
        assert_eq!(&buf[..n], b"back");
    }

    #[test]
    fn a_path_refuses_an_oversized_datagram() {
        let (a, _b) = testing::channel_pair(10);
        let err = a.try_send(&[0u8; 11]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        a.try_send(&[0u8; 10]).unwrap();
    }

    #[test]
    fn an_empty_path_is_pending_not_zero() {
        let (a, _b) = testing::channel_pair(1200);
        let mut buf = [0u8; 8];
        assert!(a.poll_recv(&mut cx(), &mut buf).is_pending());
    }

    /// A path whose every answer is one no default would give: not
    /// writable, and a receive of seven bytes.
    #[derive(Debug)]
    struct Distinct;

    impl DatagramPath for Distinct {
        fn try_send(&self, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn poll_writable(&self, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
        fn poll_recv(&self, _: &mut Context<'_>, buf: &mut [u8]) -> Poll<std::io::Result<usize>> {
            buf[..7].copy_from_slice(b"seven!!");
            Poll::Ready(Ok(7))
        }
        fn max_datagram_size(&self) -> usize {
            1200
        }
    }

    #[test]
    fn a_boxed_path_forwards_writability_and_receives() {
        let boxed = BoxPath::new(Distinct);
        assert!(boxed.poll_writable(&mut cx()).is_pending());
        let mut buf = [0u8; 16];
        let Poll::Ready(Ok(n)) = boxed.poll_recv(&mut cx(), &mut buf) else {
            panic!("the inner path's answer")
        };
        assert_eq!(&buf[..n], b"seven!!");
    }

    #[test]
    fn a_boxed_socket_forwards_a_receive_and_its_metadata() {
        use hclient_rt::{RecvMeta, UdpDatagrams};
        let udp = crate::socks5_udp::fake::FakeUdp::default();
        udp.push_from("10.0.0.7:8080", b"hello");
        let boxed = BoxUdp::new(udp);
        let mut raw = [0u8; 16];
        let mut meta = [RecvMeta::default()];
        let mut bufs = [std::io::IoSliceMut::new(&mut raw)];
        let Poll::Ready(Ok(n)) = boxed.poll_recv(&mut cx(), &mut bufs, &mut meta) else {
            panic!("the inner socket's answer")
        };
        assert_eq!(n, 1);
        assert_eq!(meta[0].len, 5);
        assert_eq!(meta[0].addr, "10.0.0.7:8080".parse().unwrap());
        assert_eq!(&raw[..5], b"hello");
    }

    #[test]
    fn a_boxed_path_forwards_its_size() {
        let (a, _b) = testing::channel_pair(1337);
        assert_eq!(a.max_datagram_size(), 1337);
        let boxed = BoxPath::new(a);
        assert_eq!(boxed.max_datagram_size(), 1337);
    }
}
