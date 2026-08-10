//! `UdpBind`/`UdpAdoptStd` on smol, behind the `udp` feature.
//!
//! The second implementation of the UDP seam, and the reason it exists is
//! not that anyone needed UDP on smol: it is that a seam with one
//! implementation is a design. `crates/http-ng/tests/two_runtimes.rs` is
//! what makes "the runtime seam is real" a measurement rather than a claim
//! for TCP; `crates/http-ng-rt-pair-check/tests/udp_pair_property.rs` and
//! `crates/http-ng-h3/tests/two_runtimes.rs` are what make it one for UDP
//! and for HTTP/3.
//!
//! # What was in doubt before this file, and what it answered
//!
//! [`UdpDatagrams`] splits `try_send` from
//! [`poll_writable`](UdpDatagrams::poll_writable) — waiting for writability
//! has to be expressible **without a datagram in hand**, because a QUIC
//! endpoint has several tasks that may all be waiting to write. That split
//! was justified entirely by what `quinn` needs and never by what a second
//! runtime can express, so it was the one place this backend could have
//! turned into a seam change.
//!
//! It did not. `async_io::Async::poll_writable(&self, cx) -> Poll<io::
//! Result<()>>` (`async-io-2.6.0/src/lib.rs:1001`) is the signature the
//! seam asks for, argument for argument, and it registers a waker with no
//! datagram anywhere in sight. The seam is untouched.
//!
//! # One real difference from the tokio backend, and why it needs no code
//!
//! `http-ng-rt-tokio` sends through `tokio::net::UdpSocket::try_io(Interest::
//! WRITABLE, ..)`, which performs the syscall *and* clears tokio's cached
//! readiness when it comes back `WouldBlock` — without that, tokio would go
//! on believing the socket is writable and `poll_send_ready` would return
//! `Ready` for ever.
//!
//! `async-io` caches nothing to clear. `Source::poll_ready`
//! (`async-io-2.6.0/src/reactor.rs:440`) answers `Ready` only when the
//! reactor's tick has moved past the one recorded at the *caller's* last
//! `Pending`, and re-arms interest on every registration; a first call
//! always registers and returns `Pending`. So a bare `try_send` here is not
//! the tokio version with a safety step dropped — there is no step to drop,
//! and adding a `try_io`-shaped dance would be inventing state this runtime
//! deliberately does not keep.
//!
//! It has a **visible consequence**, measured rather than reasoned about,
//! by replacing the `WouldBlock` retry in
//! `crates/http-ng-rt-pair-check/tests/udp_pair_property.rs` with a
//! `panic!` and running both arms: **the tokio backend takes that path on
//! its very first send and this one never does.** tokio's `try_io` refuses
//! *before the syscall* when it holds no cached WRITABLE readiness, which
//! is the state a freshly bound socket is in; here the `sendmsg` happens
//! and a loopback socket with an empty send buffer accepts it. Both are
//! within the seam's contract — `WouldBlock` is a permission to return it,
//! not an obligation — and the asymmetry is written down in that test,
//! because a caller written against this backend alone would have a bug
//! only the other one finds.

use http_ng_rt::{Datagrams, RecvMeta, UdpAdoptStd, UdpBind, UdpCaps, UdpDatagrams};
use std::io;
use std::io::IoSliceMut;
use std::net::SocketAddr;
use std::task::{Context, Poll};

/// A bound UDP socket on smol's reactor.
#[derive(Debug)]
pub struct SmolUdpSocket {
    io: async_io::Async<std::net::UdpSocket>,
    state: quinn_udp::UdpSocketState,
    caps: UdpCaps,
}

impl SmolUdpSocket {
    fn from_std(std: std::net::UdpSocket) -> io::Result<Self> {
        std.set_nonblocking(true)?;
        // `new_nonblocking`, not `new`: the line above already put the
        // descriptor in non-blocking mode, and `Async::new` is literally
        // `set_nonblocking(io)` followed by `new_nonblocking(io)`
        // (`async-io-2.6.0/src/lib.rs:658-663`). The same choice, for the
        // same reason, as `Smol::connect` in this crate's `lib.rs`.
        let io = async_io::Async::new_nonblocking(std)?;
        let state = quinn_udp::UdpSocketState::new(io.get_ref().into())?;
        let caps = UdpCaps {
            max_send_segments: state.max_gso_segments(),
            max_recv_segments: state.gro_segments(),
            ecn: ecn_is_really_on(io.get_ref()),
            may_fragment: state.may_fragment(),
        };
        Ok(Self { io, state, caps })
    }
}

/// Whether the socket will actually report the ECN bits of what it
/// receives.
///
/// **The twin of `http_ng_rt_tokio`'s `ecn_is_really_on`, deliberately
/// identical in what it asks and in what order** — the same relationship
/// `build_socket` in this crate already has with its tokio counterpart, and
/// for the same reason: both implement one seam's contract, so a divergence
/// between them would mean one of the two runtimes is lying about the same
/// kernel. Copied rather than shared because neither runtime crate depends
/// on the other and the seam crate is not a place to put `getsockopt`.
/// `crates/http-ng-rt-pair-check/tests/udp_pair_property.rs` runs the same
/// assertions against both, which is what would catch a divergence.
///
/// Read back with `getsockopt`, not inferred from the `setsockopt` that
/// `quinn_udp::UdpSocketState::new` already attempted and swallowed —
/// *"When support is unavailable, functionality will gracefully degrade"*
/// (`quinn-udp-0.5.15/src/lib.rs:22`). An attempt that failed and an
/// attempt that succeeded look identical from the outside, and only one of
/// them means the congestion controller will see marks.
///
/// **A dual-stack v6 socket must satisfy both options, not either.** It
/// receives v4-mapped traffic too, whose marks arrive through `IP_RECVTOS`
/// — and `IP_RECVTOS` on a dual-stack socket is exactly what macOS and iOS
/// do not support (`quinn-udp-0.5.15/src/unix.rs:114`).
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
fn ecn_is_really_on(io: &std::net::UdpSocket) -> bool {
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
fn ecn_is_really_on(_io: &std::net::UdpSocket) -> bool {
    false
}

impl UdpBind for crate::Smol {
    type Socket = SmolUdpSocket;

    /// No runtime-context guard, unlike `http_ng_rt_tokio::TokioHandle`'s:
    /// `async_io::Async::new_nonblocking` registers with a process-global
    /// reactor that `async-io` starts on its own thread on first use, so
    /// there is no "inside the runtime" to be outside of. That is the same
    /// property `Smol`'s `Timer` and `TcpConnect` already have, and it is
    /// why this crate has no handle-carrying variant at all.
    fn bind(&self, local: SocketAddr) -> io::Result<Self::Socket> {
        SmolUdpSocket::from_std(std::net::UdpSocket::bind(local)?)
    }
}

impl UdpAdoptStd for crate::Smol {
    fn adopt(&self, s: std::net::UdpSocket) -> io::Result<Self::Socket> {
        SmolUdpSocket::from_std(s)
    }
}

impl UdpDatagrams for SmolUdpSocket {
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
        // `try_send`, not `send`: the two differ in exactly one thing —
        // `send` loops on `EINTR` and on a `WouldBlock` that follows one —
        // and this seam's contract is that `WouldBlock` comes back to the
        // caller as an obligation to `poll_writable`. `try_send` is the
        // spelling that says so.
        self.state.try_send(self.io.get_ref().into(), &transmit)
    }

    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.io.poll_writable(cx)
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
            std::task::ready!(self.io.poll_readable(cx))?;
            let attempt = self.state.recv(
                self.io.get_ref().into(),
                &mut bufs[..slots],
                &mut theirs[..slots],
            );
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
                // The readiness `poll_readable` reported was consumed by
                // someone else, or the datagram was dropped between the
                // event and the call; go round and register again.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.get_ref().local_addr()
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
