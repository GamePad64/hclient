//! A QUIC endpoint whose only peer is a filter's datagram path.

use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use hclient_proxy::{BoxPath, DatagramPath};

use crate::http3::H3Runtime;
use crate::http3::runtime::SeamRuntime;

// Maintainer notes (not rendered):
// quinn refuses `connect` to port 0 or an unspecified address
// (`quinn-proto` `endpoint.rs`, `InvalidRemoteAddress`), so the peer has
// to be a real-looking address; TEST-NET-1 can never be a real peer, which
// is what makes it safe to report every datagram as coming from it.
/// The address a path's peer is known by. Never dialled: every datagram
/// goes to the path, whatever quinn addresses it to.
pub(crate) const STAND_IN: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

/// QUIC's floor (RFC 9000 §14): a path that cannot carry this cannot carry
/// QUIC at all.
pub(crate) const MIN_PATH: usize = 1200;

/// QUIC's minimum datagram size (RFC 9000 §14), where a connection over a
/// path starts.
const QUIC_FLOOR: u16 = 1200;

/// A [`DatagramPath`], dressed as a `quinn::AsyncUdpSocket`.
///
/// One peer, whole datagrams, no ECN and no fragmentation — a filter's
/// path in quinn's shape, matching [`DatagramPath`]'s own contract rather
/// than adding anything to it.
#[derive(Debug)]
pub(crate) struct PathSocket {
    path: BoxPath,
    peer: SocketAddr,
}

impl PathSocket {
    pub(crate) fn new(path: BoxPath, port: u16) -> Self {
        Self {
            path,
            peer: SocketAddr::new(STAND_IN, port),
        }
    }
}

impl quinn::AsyncUdpSocket for PathSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn quinn::UdpPoller>> {
        Box::pin(PathPoller(self))
    }

    fn try_send(&self, t: &quinn::udp::Transmit) -> io::Result<()> {
        if t.segment_size.is_some_and(|s| s < t.contents.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a path carries one datagram per send",
            ));
        }
        self.path.try_send(t.contents)
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [quinn::udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let (Some(buf), Some(m)) = (bufs.first_mut(), meta.first_mut()) else {
            return Poll::Ready(Ok(0));
        };
        let n = std::task::ready!(self.path.poll_recv(cx, buf))?;
        *m = quinn::udp::RecvMeta {
            addr: self.peer,
            len: n,
            stride: n,
            ecn: None,
            dst_ip: None,
        };
        Poll::Ready(Ok(1))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::from(([0, 0, 0, 0], 0)))
    }

    fn max_transmit_segments(&self) -> usize {
        1
    }

    fn max_receive_segments(&self) -> usize {
        1
    }

    fn may_fragment(&self) -> bool {
        false
    }
}

/// One task's view of a [`PathSocket`]'s writability.
///
/// A path has exactly one peer and quinn creates at most one poller per
/// connection over this socket (the endpoint driver, in `endpoint_over`'s
/// case), so this forwards directly rather than fanning out the way
/// [`crate::http3::runtime::SeamSocket`]'s poller does for a socket shared
/// by many connections.
#[derive(Debug)]
struct PathPoller(Arc<PathSocket>);

impl quinn::UdpPoller for PathPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.path.poll_writable(cx)
    }
}

/// The transport config a QUIC connection over one path uses.
///
/// A path's `max_datagram_size` is a ceiling and not a measurement: a
/// capsule path's is exact, but a SOCKS relay's is a guess at a
/// 1500-byte link, and behind a VPN or a `PPPoE` link a datagram over the real
/// link is lost without a word. So the connection starts at QUIC's floor
/// and discovers upwards, with the path's ceiling as the upper bound —
/// no probe can exceed what the path accepts, and discovery keeps quinn's
/// black-hole detection honest about what it found.
pub(crate) fn transport_for(
    path_max: usize,
    keep_alive: Option<Duration>,
) -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to u16::MAX on the line above"
    )]
    let mtu = path_max.min(usize::from(u16::MAX)) as u16;
    t.initial_mtu(QUIC_FLOOR).min_mtu(QUIC_FLOOR);
    if mtu > QUIC_FLOOR {
        let mut discovery = quinn::MtuDiscoveryConfig::default();
        discovery.upper_bound(mtu);
        t.mtu_discovery_config(Some(discovery));
    } else {
        t.mtu_discovery_config(None);
    }
    if let Some(d) = keep_alive {
        t.keep_alive_interval(Some(d));
    }
    t
}

/// Bind a QUIC endpoint over a filter's datagram path.
///
/// # Errors
///
/// [`io::ErrorKind::Unsupported`] for a path whose
/// [`DatagramPath::max_datagram_size`] is under [`MIN_PATH`] — QUIC's own
/// floor, which nothing here can lower — and whatever
/// [`quinn::Endpoint::new_with_abstract_socket`] answers for a socket
/// whose [`quinn::AsyncUdpSocket::local_addr`] fails (it never does, for
/// [`PathSocket`]).
pub(crate) fn endpoint_over<R>(rt: &R, path: BoxPath, port: u16) -> io::Result<quinn::Endpoint>
where
    R: H3Runtime,
    R::Sleep: Send + 'static, // send-bound-exception: amendment-C10
    R::Socket: fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
{
    let max = path.max_datagram_size();
    if max < MIN_PATH {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("a path of {max} bytes cannot carry QUIC"),
        ));
    }
    quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        None,
        Arc::new(PathSocket::new(path, port)),
        Arc::new(SeamRuntime::new(rt.clone())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hclient_proxy::testing::channel_pair;

    #[test]
    fn a_path_under_1200_bytes_is_refused() {
        let rt = hclient_rt_tokio::Tokio;
        let (a, _b) = channel_pair(1199);
        let rt_ = tokio::runtime::Runtime::new().unwrap();
        let _g = rt_.enter();
        let e = endpoint_over(&rt, BoxPath::new(a), 443).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn a_datagram_from_the_path_is_reported_as_from_the_stand_in() {
        let (a, b) = channel_pair(1400);
        let sock = PathSocket::new(BoxPath::new(a), 443);
        b.try_send(b"x").unwrap();
        let mut buf = [0u8; 16];
        let mut bufs = [std::io::IoSliceMut::new(&mut buf)];
        let mut meta = [quinn::udp::RecvMeta::default()];
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let std::task::Poll::Ready(Ok(1)) =
            quinn::AsyncUdpSocket::poll_recv(&sock, &mut cx, &mut bufs, &mut meta)
        else {
            panic!()
        };
        assert_eq!(meta[0].addr, std::net::SocketAddr::new(STAND_IN, 443));
        assert_eq!(meta[0].len, 1);
        assert_eq!(meta[0].ecn, None);
    }

    #[test]
    fn the_socket_claims_one_segment_no_ecn_and_no_fragmentation() {
        let (a, _b) = channel_pair(1400);
        let sock = PathSocket::new(BoxPath::new(a), 443);
        assert_eq!(quinn::AsyncUdpSocket::max_transmit_segments(&sock), 1);
        assert_eq!(quinn::AsyncUdpSocket::max_receive_segments(&sock), 1);
        assert!(!quinn::AsyncUdpSocket::may_fragment(&sock));
    }

    #[test]
    fn a_transmit_with_a_segment_size_is_refused_rather_than_sent_whole() {
        let (a, _b) = channel_pair(1400);
        let sock = PathSocket::new(BoxPath::new(a), 443);
        let t = quinn::udp::Transmit {
            destination: std::net::SocketAddr::new(STAND_IN, 443),
            ecn: None,
            contents: &[0u8; 20],
            segment_size: Some(10),
            src_ip: None,
        };
        assert!(quinn::AsyncUdpSocket::try_send(&sock, &t).is_err());
    }
}
