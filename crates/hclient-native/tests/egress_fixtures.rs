//! Shared fixtures for the egress-datagram tests: a filter's datagram path
//! as a real test would meet one, without a real relay in the way.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]
#![allow(
    dead_code,
    reason = "a fixture module included by `#[path]`; each including test file uses its own subset"
)]

use hclient_proxy::{BoxPath, DatagramPath};
use std::io;
use std::net::SocketAddr;
use std::task::{Context, Poll};

/// What a UDP relay's path to one peer carries: a common Ethernet MTU less
/// an IPv4 and a UDP header and a relay's own 20 bytes of framing.
pub const BRIDGE_MAX: usize = 1452;

/// A [`DatagramPath`] over a plain UDP socket aimed at one peer.
///
/// The simplest honest relay: every datagram the QUIC stack hands the
/// path leaves this host on a socket of its own, addressed to `peer`,
/// and every datagram `peer` sends back is handed up whole. What it
/// proves is that a transport addressed nothing itself — the only
/// address anything was sent to is the one this fixture was built with.
#[derive(Debug)]
pub struct UdpBridge {
    sock: tokio::net::UdpSocket,
}

impl DatagramPath for UdpBridge {
    fn try_send(&self, datagram: &[u8]) -> io::Result<()> {
        if datagram.len() > BRIDGE_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} bytes over a {BRIDGE_MAX}-byte path", datagram.len()),
            ));
        }
        self.sock.try_send(datagram).map(|_| ())
    }

    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.sock.poll_send_ready(cx)
    }

    fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let mut rb = tokio::io::ReadBuf::new(buf);
        std::task::ready!(self.sock.poll_recv(cx, &mut rb))?;
        Poll::Ready(Ok(rb.filled().len()))
    }

    fn max_datagram_size(&self) -> usize {
        BRIDGE_MAX
    }
}

/// A path to `peer` over a fresh loopback UDP socket.
///
/// # Panics
///
/// Outside a tokio runtime, or if loopback cannot bind a socket.
pub fn udp_bridge(peer: SocketAddr) -> BoxPath {
    let std = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind a loopback UDP socket");
    std.connect(peer).expect("connect the bridge to its peer");
    std.set_nonblocking(true).expect("non-blocking");
    let sock = tokio::net::UdpSocket::from_std(std).expect("inside a tokio runtime");
    BoxPath::new(UdpBridge { sock })
}
