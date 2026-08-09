//! `UdpBind`/`UdpAdoptStd` on tokio, behind the `udp` feature.
//!
//! # Why `quinn-udp` and not our own `cmsg` code
//!
//! GSO, GRO and ECN are control messages on a descriptor, with a different
//! spelling on every platform — `UDP_SEGMENT`, `UDP_GRO`, `IP_TOS`/
//! `IPV6_TCLASS`, `IP_RECVTOS`/`IPV6_RECVTCLASS`, plus a Windows backend
//! that shares none of it. `quinn-udp` has that code for six platforms and
//! is maintained by people who run it at scale. Writing a second copy here
//! would be the same trade v0.2 W7 rejected for HTTP/1: a second
//! implementation of the hardest part, in a crate whose job is to adapt a
//! runtime.
//!
//! The seam stays clean anyway: `http-ng-rt` names no QUIC crate, and the
//! conversion between its `Datagrams`/`RecvMeta` and `quinn-udp`'s lives
//! here.
//!
//! # The one thing this module does that `quinn-udp` will not
//!
//! **It asks the kernel whether ECN actually works, and reports the
//! answer.** `quinn-udp` sets `IP_RECVTOS` and, if the call fails, logs at
//! debug and carries on — *"When support is unavailable, functionality will
//! gracefully degrade"* (`quinn-udp-0.5.15/src/lib.rs:22`). That is the
//! right default for a QUIC library and the wrong one here: a client
//! without ECN is a client whose congestion controller cannot tell
//! congestion from loss, and nothing about that failure is visible. So
//! [`caps`](TokioUdpSocket::caps) reads the option back with
//! `getsockopt` after the fact and reports what it finds, rather than what
//! was attempted.

use http_ng_rt::{Datagrams, RecvMeta, UdpAdoptStd, UdpBind, UdpCaps, UdpDatagrams};
use std::io;
use std::io::IoSliceMut;
use std::net::SocketAddr;
use std::task::{Context, Poll};
use tokio::io::Interest;

/// A bound UDP socket on tokio's reactor.
#[derive(Debug)]
pub struct TokioUdpSocket {
    io: tokio::net::UdpSocket,
    state: quinn_udp::UdpSocketState,
    caps: UdpCaps,
}

impl TokioUdpSocket {
    fn from_std(std: std::net::UdpSocket) -> io::Result<Self> {
        std.set_nonblocking(true)?;
        let io = tokio::net::UdpSocket::from_std(std)?;
        let state = quinn_udp::UdpSocketState::new((&io).into())?;
        let caps = UdpCaps {
            max_send_segments: state.max_gso_segments(),
            max_recv_segments: state.gro_segments(),
            ecn: ecn_is_really_on(&io),
            may_fragment: state.may_fragment(),
        };
        Ok(Self { io, state, caps })
    }
}

/// Whether the socket will actually report the ECN bits of what it
/// receives.
///
/// Read back with `getsockopt`, not inferred from the `setsockopt` that
/// `quinn_udp::UdpSocketState::new` already attempted and swallowed. The
/// distinction is the whole point: an attempt that failed and an attempt
/// that succeeded look identical from the outside, and only one of them
/// means the congestion controller will see marks.
///
/// **A dual-stack v6 socket must satisfy both options, not either.** It
/// receives v4-mapped traffic too, whose marks arrive through `IP_RECVTOS`
/// — and `IP_RECVTOS` on a dual-stack socket is exactly what macOS and iOS
/// do not support (`quinn-udp-0.5.15/src/unix.rs:114`). Asking for both is
/// what makes this report `false` there instead of a half-truth.
///
/// Platforms where the options are not reachable through `socket2` report
/// `false` — an understatement, which is the direction a capability is
/// allowed to be wrong in.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    windows
))]
fn ecn_is_really_on(io: &tokio::net::UdpSocket) -> bool {
    let sock = socket2::SockRef::from(io);
    let Ok(local) = io.local_addr() else {
        return false;
    };
    match local {
        SocketAddr::V4(_) => sock.recv_tos_v4().unwrap_or(false),
        SocketAddr::V6(_) => {
            let v6 = sock.recv_tclass_v6().unwrap_or(false);
            // `only_v6()` failing is not "it is v6-only": it is "we do not
            // know", and the conservative reading of not knowing is that
            // v4-mapped traffic can arrive.
            let dual = !sock.only_v6().unwrap_or(false);
            v6 && (!dual || sock.recv_tos_v4().unwrap_or(false))
        }
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    windows
)))]
fn ecn_is_really_on(_io: &tokio::net::UdpSocket) -> bool {
    false
}

impl UdpBind for crate::Tokio {
    type Socket = TokioUdpSocket;

    fn bind(&self, local: SocketAddr) -> io::Result<Self::Socket> {
        TokioUdpSocket::from_std(std::net::UdpSocket::bind(local)?)
    }
}

impl UdpAdoptStd for crate::Tokio {
    fn adopt(&self, s: std::net::UdpSocket) -> io::Result<Self::Socket> {
        TokioUdpSocket::from_std(s)
    }
}

impl UdpBind for crate::TokioHandle {
    type Socket = TokioUdpSocket;

    /// `tokio::net::UdpSocket::from_std` registers with the reactor and
    /// panics outside a runtime context, so a handle-carrying runtime has
    /// to enter its own — which is the whole reason `TokioHandle` exists
    /// (its `Spawn` is total where the ZST's panics). Without the guard
    /// this method would work from inside `block_on` and panic from the
    /// place a client is usually built.
    fn bind(&self, local: SocketAddr) -> io::Result<Self::Socket> {
        let _guard = self.handle().enter();
        TokioUdpSocket::from_std(std::net::UdpSocket::bind(local)?)
    }
}

impl UdpAdoptStd for crate::TokioHandle {
    fn adopt(&self, s: std::net::UdpSocket) -> io::Result<Self::Socket> {
        let _guard = self.handle().enter();
        TokioUdpSocket::from_std(s)
    }
}

impl UdpDatagrams for TokioUdpSocket {
    fn try_send(&self, t: &Datagrams<'_>) -> io::Result<()> {
        // Refuse rather than truncate: this socket's own report is the
        // contract, and a GSO batch beyond it would otherwise leave one
        // oversized datagram on a path that will drop it silently.
        t.reject_unsupported(self.caps)?;
        let transmit = quinn_udp::Transmit {
            destination: t.destination,
            ecn: t.ecn.map(to_quinn),
            contents: t.contents,
            segment_size: t.segment_size,
            src_ip: t.src_ip,
        };
        self.io.try_io(Interest::WRITABLE, || {
            self.state.send((&self.io).into(), &transmit)
        })
    }

    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.io.poll_send_ready(cx)
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let slots = meta.len().min(bufs.len()).min(quinn_udp::BATCH_SIZE);
        if slots == 0 {
            return Poll::Ready(Ok(0));
        }
        let mut theirs = [quinn_udp::RecvMeta::default(); quinn_udp::BATCH_SIZE];
        loop {
            std::task::ready!(self.io.poll_recv_ready(cx))?;
            let attempt = self.io.try_io(Interest::READABLE, || {
                self.state
                    .recv((&self.io).into(), &mut bufs[..slots], &mut theirs[..slots])
            });
            match attempt {
                Ok(n) => {
                    for (dst, src) in meta.iter_mut().zip(theirs.iter().take(n)) {
                        *dst = RecvMeta {
                            addr: src.addr,
                            len: src.len,
                            stride: src.stride,
                            // `quinn-udp` leaves this `None` when the
                            // platform did not report the bits, and it must
                            // stay `None` here — see `RecvMeta::ecn`.
                            ecn: src.ecn.map(from_quinn),
                            dst_ip: src.dst_ip,
                        };
                    }
                    return Poll::Ready(Ok(n));
                }
                // `try_io` cleared the readiness; go round and wait again.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }

    fn caps(&self) -> UdpCaps {
        self.caps
    }
}

fn to_quinn(c: http_ng_rt::EcnCodepoint) -> quinn_udp::EcnCodepoint {
    match c {
        http_ng_rt::EcnCodepoint::Ect0 => quinn_udp::EcnCodepoint::Ect0,
        http_ng_rt::EcnCodepoint::Ect1 => quinn_udp::EcnCodepoint::Ect1,
        http_ng_rt::EcnCodepoint::Ce => quinn_udp::EcnCodepoint::Ce,
    }
}

fn from_quinn(c: quinn_udp::EcnCodepoint) -> http_ng_rt::EcnCodepoint {
    match c {
        quinn_udp::EcnCodepoint::Ect0 => http_ng_rt::EcnCodepoint::Ect0,
        quinn_udp::EcnCodepoint::Ect1 => http_ng_rt::EcnCodepoint::Ect1,
        quinn_udp::EcnCodepoint::Ce => http_ng_rt::EcnCodepoint::Ce,
    }
}
