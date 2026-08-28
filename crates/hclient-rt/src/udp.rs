//! The UDP runtime capability.
//!
//! Added for HTTP/3 (v0.3), whose transport is QUIC and therefore
//! datagrams. **Nothing in this module names QUIC**, and that is a
//! constraint rather than an accident: `RecvMeta` and `EcnCodepoint` are
//! re-declared here rather than re-exported from `quinn-udp`, so this seam
//! stays free of a QUIC dependency. The price is a field-for-field
//! conversion in `hclient-h3`, which is cheaper than a runtime seam that
//! drags a QUIC stack into every build that mentions it.
//!
//! # Why UDP is not "`TcpConnect` with a different letter"
//!
//! Three differences, each of which shows up in a signature below rather
//! than in prose:
//!
//! 1. **Unconnected.** QUIC's socket is bound, never connected: one
//!    endpoint socket serves every connection it opens, and the peer's
//!    address can change under migration. `connect(2)` would make both
//!    impossible, so there is no `connect` here and every send names its
//!    destination.
//! 2. **Batched, with caller-owned buffers.** One send can carry several
//!    datagrams (GSO) and one receive can return several (GRO). A
//!    `recv_from(&mut [u8]) -> (usize, SocketAddr)` shape cannot express
//!    either, and cannot carry ECN at all.
//! 3. **Offloads are capabilities, not assumptions.** See [`UdpCaps`].

use std::error::Error as StdError;
use std::fmt::Display;
use std::io::IoSliceMut;
use std::net::{IpAddr, SocketAddr};
use std::task::{Context, Poll};

/// Bind a UDP socket.
///
/// # Why `bind` is sync where [`TcpConnect::connect`] is async
///
/// Not for symmetry's sake in either direction. Binding performs no network
/// round trip on any runtime this workspace targets — `std::net::UdpSocket::
/// bind` is a syscall, `tokio::net::UdpSocket::from_std` and
/// `async_io::Async::new` are registrations, and `embassy_net::udp::
/// UdpSocket::bind` returns immediately. `connect`, by contrast, is a real
/// handshake and has to be async.
///
/// There is also a consumer-side reason, and it is the decisive one: a QUIC
/// stack asks its runtime to wrap a socket from a **synchronous** call
/// (`quinn::Runtime::wrap_udp_socket`, `quinn-0.11.11/src/runtime.rs:24`).
/// An async `bind` could not serve it, and inventing a second synchronous
/// method beside an async one would be two ways to say the same thing.
///
/// [`TcpConnect::connect`]: crate::TcpConnect::connect
pub trait UdpBind {
    /// **No `Send`, `Sync`, `'static` or `Debug` bound here, deliberately.**
    ///
    /// A QUIC stack needs all four (`quinn::AsyncUdpSocket: Send + Sync +
    /// Debug + 'static`), and the research this crate is built on proposed
    /// putting them on this associated type. They are not here, because a
    /// bound in the trait is paid by every implementer for the benefit of
    /// one consumer, and this seam has an implementer — an `embassy-net`
    /// backend — for which `Send` is not free. The bounds live in
    /// `hclient_h3::H3`'s `where` clause instead, so the compile error lands
    /// on whoever asked for QUIC rather than on whoever implemented UDP.
    ///
    /// `TcpConnect::Stream` keeps the same property and pays nothing for
    /// it: `hclient-native`'s `FakeStream` holds an `Rc<()>` precisely to
    /// prove that no path in that vertical requires `Send`. This trait
    /// preserves that proof rather than spending it.
    type Socket: UdpDatagrams;

    fn bind(&self, local: SocketAddr) -> std::io::Result<Self::Socket>;
}

/// Adopt an already-created `std::net::UdpSocket`.
///
/// The same split [`TcpAdoptStd`] makes over [`TcpConnect`], for the same
/// reason: on platforms with descriptors the socket options are applied
/// once, outside the runtime, and the runtime only adopts the result — so
/// every runtime crate does not rewrite the same `setsockopt` rigmarole. A
/// runtime with no descriptor to adopt says so by not implementing this
/// trait, which is how the seam already expresses that fact for TCP.
///
/// [`TcpAdoptStd`]: crate::TcpAdoptStd
/// [`TcpConnect`]: crate::TcpConnect
pub trait UdpAdoptStd: UdpBind {
    fn adopt(&self, s: std::net::UdpSocket) -> std::io::Result<Self::Socket>;
}

/// Datagram I/O on a bound socket.
pub trait UdpDatagrams {
    /// Send one [`Datagrams`] — which is one syscall and one *or more*
    /// datagrams, see [`Datagrams::segment_size`].
    ///
    /// [`std::io::ErrorKind::WouldBlock`] is a real answer, not a failure:
    /// it obliges the caller to call [`poll_writable`](Self::poll_writable)
    /// before trying again.
    fn try_send(&self, t: &Datagrams<'_>) -> std::io::Result<()>;

    /// Wait for the socket to become writable.
    ///
    /// # Why this is separate from `try_send` rather than a fused `poll_send`
    ///
    /// A fused `poll_send(cx, t)` reads better and cannot be implemented
    /// against a QUIC stack. Two reasons, both of which come from the same
    /// place (`quinn-0.11.11/src/runtime.rs:44-66`):
    ///
    /// - a QUIC endpoint has several tasks that may all be waiting to
    ///   write, and one socket object can store one waker, so the *waiting*
    ///   has to be expressible without a datagram in hand;
    /// - the retry after `WouldBlock` is driven by the stack's own pacer,
    ///   which decides *what* to send only once the socket is writable.
    ///
    /// Both target runtimes provide this natively —
    /// `tokio::net::UdpSocket::poll_send_ready` and
    /// `async_io::Async::poll_writable` — so the split costs no
    /// implementation anywhere and buys the one consumer that exists.
    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>>;

    /// Receive into caller-owned buffers, with one metadata slot each.
    ///
    /// Not `recv_from(&mut [u8]) -> (usize, SocketAddr)`, for two reasons
    /// that are facts about the wire rather than about taste: a GRO read
    /// returns several datagrams coalesced into one buffer and needs
    /// [`RecvMeta::stride`] to split them again, and each read needs its
    /// own [`RecvMeta::ecn`] and its own destination address. A `recv_from`
    /// shape can carry neither, so a capability built on it would silently
    /// drop ECN — see [`UdpCaps`].
    ///
    /// Returns the number of `meta`/`bufs` slots filled.
    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<std::io::Result<usize>>;

    fn local_addr(&self) -> std::io::Result<SocketAddr>;

    /// Which offloads this **socket** has.
    ///
    /// # Why a method on the socket and not an associated const on the trait
    ///
    /// [`TcpConnect::APPLIES`] is a const because "does this runtime hand
    /// the whole `TcpOpts` set to a `socket2::Socket`" is a fact about the
    /// runtime crate. GSO, GRO and ECN are not: they are `cmsg` support on
    /// a descriptor on a kernel, and two sockets from the same runtime can
    /// answer differently — `quinn-udp`'s own unix backend carries "mac and
    /// ios do not support IP_RECVTOS on dual-stack sockets"
    /// (`quinn-udp-0.5.15/src/unix.rs:114`), i.e. a v4 socket and a
    /// dual-stack v6 socket differ on the same machine in the same process.
    /// A const would be a claim the runtime crate is not in a position to
    /// make.
    ///
    /// The default is [`UdpCaps::NONE`], the weakest answer, for the reason
    /// [`TcpConnect::APPLIES`] defaults to `TcpOptsSupport::NONE`: a
    /// default is a claim made by silence and must never be stronger than
    /// the truth.
    ///
    /// [`TcpConnect::APPLIES`]: crate::TcpConnect::APPLIES
    fn caps(&self) -> UdpCaps {
        UdpCaps::NONE
    }
}

/// One send: a destination, and between one and many datagrams.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Datagrams<'a> {
    pub destination: SocketAddr,
    /// The source address to send from. Needed when the socket is bound to
    /// a wildcard v6 address and the stack has to keep answering from the
    /// address a peer first saw.
    pub src_ip: Option<IpAddr>,
    /// The ECN codepoint to mark these datagrams with, if any.
    pub ecn: Option<EcnCodepoint>,
    /// `Some(n)` is GSO: `contents` is a run of datagrams of `n` bytes each
    /// (the last may be shorter), to be sent by one syscall. `None` is a
    /// single datagram.
    ///
    /// A socket whose [`UdpCaps::max_send_segments`] is `1` must never
    /// receive a `Some(_)` covering more than one datagram — see
    /// [`Datagrams::reject_unsupported`] — and must never quietly send the
    /// whole buffer as one oversized datagram, which is what "graceful
    /// degradation" would look like here and would put a 3600-byte packet
    /// on a 1200-byte path.
    pub segment_size: Option<usize>,
    pub contents: &'a [u8],
}

/// What one [`UdpDatagrams::poll_recv`] slot received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecvMeta {
    pub addr: SocketAddr,
    /// Bytes written into the corresponding buffer.
    pub len: usize,
    /// The size of a single datagram inside that buffer when GRO coalesced
    /// several. `0`, or any value `>= len`, means "one datagram".
    pub stride: usize,
    /// The ECN codepoint the datagram(s) arrived with.
    ///
    /// **`None` when unknown, never a guess.** This is the one field in the
    /// module where a plausible-looking substitute would do real damage: a
    /// receiver that invented `Ect0` would feed a congestion controller
    /// evidence of a marking that never happened, and the controller cannot
    /// tell that apart from the real thing. Under-reporting costs the
    /// controller an optimisation; over-reporting costs it its correctness.
    pub ecn: Option<EcnCodepoint>,
    /// The destination address encoded in the datagram, where the platform
    /// reports it.
    pub dst_ip: Option<IpAddr>,
}

impl<'a> Datagrams<'a> {
    /// A transmission. `destination` and `contents` are what a datagram
    /// cannot be without; the other three are what a sender may not have
    /// or may not want, so they are setters — which is also what keeps a
    /// sixth of them from being a breaking change.
    #[must_use]
    pub fn new(destination: SocketAddr, contents: &'a [u8]) -> Self {
        Self {
            destination,
            src_ip: None,
            ecn: None,
            segment_size: None,
            contents,
        }
    }

    /// The source address to send from, on a socket bound to more than one.
    #[must_use]
    pub fn src_ip(mut self, src_ip: Option<IpAddr>) -> Self {
        self.src_ip = src_ip;
        self
    }

    /// The ECN codepoint to set.
    #[must_use]
    pub fn ecn(mut self, ecn: Option<EcnCodepoint>) -> Self {
        self.ecn = ecn;
        self
    }

    /// Generic segmentation offload: split `contents` into segments of
    /// this size. `None` sends one datagram.
    #[must_use]
    pub fn segment_size(mut self, segment_size: Option<usize>) -> Self {
        self.segment_size = segment_size;
        self
    }
}

impl RecvMeta {
    /// What a receive reports. The three required fields are the ones a
    /// caller cannot proceed without — where it came from, how much
    /// arrived, and how it is divided; `ecn` and `dst_ip` are what a
    /// platform may not tell the runtime.
    #[must_use]
    pub fn new(addr: SocketAddr, len: usize, stride: usize) -> Self {
        Self {
            addr,
            len,
            stride,
            ecn: None,
            dst_ip: None,
        }
    }

    /// The ECN codepoint the kernel reported. `None` means it did not,
    /// which is the understating direction this seam requires — see
    /// `ecn_is_really_on`.
    #[must_use]
    pub fn ecn(mut self, ecn: Option<EcnCodepoint>) -> Self {
        self.ecn = ecn;
        self
    }

    /// The local address the datagram arrived on.
    #[must_use]
    pub fn dst_ip(mut self, dst_ip: Option<IpAddr>) -> Self {
        self.dst_ip = dst_ip;
        self
    }
}

impl Default for RecvMeta {
    /// Arbitrary, and meant to be overwritten: this exists so a caller can
    /// allocate a slice of slots before a read fills them.
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([0, 0, 0, 0], 0)),
            len: 0,
            stride: 0,
            ecn: None,
            dst_ip: None,
        }
    }
}

/// An ECN codepoint, in the two bits of the IP header's TOS/traffic-class
/// field that carry it.
///
/// Declared here rather than re-exported from `quinn-udp`, so that this
/// seam carries no QUIC dependency — the same trade `RecvMeta` makes. The
/// discriminants are the wire values, so the conversion at the one place
/// that needs it is a `match`, not a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EcnCodepoint {
    /// `01` — ECT(1).
    Ect1 = 0b01,
    /// `10` — ECT(0).
    Ect0 = 0b10,
    /// `11` — congestion experienced.
    Ce = 0b11,
}

impl EcnCodepoint {
    /// The two-bit wire value, or `None` for `00` (not ECN-capable).
    pub fn from_bits(bits: u8) -> Option<Self> {
        match bits & 0b11 {
            0b01 => Some(Self::Ect1),
            0b10 => Some(Self::Ect0),
            0b11 => Some(Self::Ce),
            _ => None,
        }
    }
}

/// Which offloads a socket has.
///
/// # Three answers, not one boolean
///
/// They degrade independently, and the stack that consumes them already
/// treats them so — `max_transmit_segments`, `max_receive_segments` and a
/// per-datagram `ecn` field are three separate questions on
/// `quinn::AsyncUdpSocket`, with `may_fragment` a fourth. Collapsing them
/// into one `bool` would be the mistake `TcpOptsSupport` exists not to make:
/// the caller's decision differs per offload, so the report must too.
///
/// Measured on x86-64 Linux 7.0.0 for a plain `std::net::UdpSocket`:
/// `max_send_segments = 64`, `max_recv_segments = 64`, `ecn = true`,
/// `may_fragment = false`. Those are that kernel's numbers, not this
/// crate's: they are what the report exists to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpCaps {
    /// Datagrams one `try_send` may carry. `1` means no GSO.
    pub max_send_segments: usize,
    /// Datagrams one `poll_recv` slot may describe. `1` means no GRO.
    pub max_recv_segments: usize,
    /// Whether [`Datagrams::ecn`] is applied on send and
    /// [`RecvMeta::ecn`] is filled in on receive.
    pub ecn: bool,
    /// Whether datagrams may be fragmented in flight — which makes path
    /// MTU discovery unreliable. `true` is the pessimistic answer, so it is
    /// the one in [`UdpCaps::NONE`].
    pub may_fragment: bool,
}

impl UdpCaps {
    /// A socket with no offloads at all, and the default for
    /// [`UdpDatagrams::caps`].
    ///
    /// Note `may_fragment: true` — the *worse* answer, not the tidier one.
    /// Every field here is the value that costs a forgetful implementer an
    /// understatement rather than a promise it cannot keep, which is the
    /// rule `TcpOptsSupport::NONE` established.
    pub const NONE: Self = Self {
        max_send_segments: 1,
        max_recv_segments: 1,
        ecn: false,
        may_fragment: true,
    };
}

/// The caller asked for an offload this socket does not have.
///
/// Carried inside an [`std::io::Error`] with
/// [`ErrorKind::Unsupported`](std::io::ErrorKind::Unsupported) by
/// [`Datagrams::reject_unsupported`], and reachable again through
/// `io::Error::get_ref().downcast_ref()` — the shape `UnsupportedTcpOpts`
/// already uses, so a caller who wants to react per-offload does not have
/// to scrape `Display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedUdpOffload {
    gso: bool,
    ecn: bool,
}

impl UnsupportedUdpOffload {
    /// The offending offload names. Every one of them, not just the first.
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        [("gso", self.gso), ("ecn", self.ecn)]
            .into_iter()
            .filter_map(|(name, bad)| bad.then_some(name))
    }
}

impl Display for UnsupportedUdpOffload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "this socket does not have these UDP offloads, and does not silently drop them:",
        )?;
        for (i, name) in self.names().enumerate() {
            f.write_str(if i > 0 { ", " } else { " " })?;
            f.write_str(name)?;
        }
        Ok(())
    }
}

impl StdError for UnsupportedUdpOffload {}

impl Datagrams<'_> {
    /// How many datagrams this send describes.
    pub fn segments(&self) -> usize {
        match self.segment_size {
            None => 1,
            Some(0) => 1,
            Some(n) => self.contents.len().div_ceil(n),
        }
    }

    /// Fail when this send asks for an offload `caps` says the socket does
    /// not have — the twin of `TcpOpts::reject_unsupported`, and the only
    /// sanctioned answer to an offload that cannot be applied, since
    /// applying it invisibly-not-at-all is not one.
    ///
    /// # The two offloads are not symmetric, and pretending otherwise would break QUIC
    ///
    /// **GSO can refuse, and must.** A caller reads
    /// [`UdpCaps::max_send_segments`] before it batches, so a
    /// `segment_size` describing more datagrams than the socket declared is
    /// a bug in the caller, not a fact about the environment. Refusing puts
    /// the error where the bug is. Not refusing puts a 3600-byte datagram
    /// on a 1200-byte path, where it is dropped by something that will
    /// never tell anyone why.
    ///
    /// **ECN cannot refuse on send, and this is the one degradation this
    /// module permits.** A QUIC stack marks unconditionally; a socket that
    /// failed every send on a kernel with no `IP_TOS` support would make
    /// QUIC unusable exactly where the stack itself works fine. So a socket
    /// may drop the marking — but *only while it declares `ecn: false`*,
    /// and this check is what makes that conditional real: a socket that
    /// claims `ecn: true` and is handed a codepoint it will not apply is
    /// refused, so the declaration is a contract rather than a decoration.
    /// The permission is one-directional: nothing here lets a socket
    /// under-report on the *receive* side, where the cost is a congestion
    /// controller acting on a marking that never happened
    /// ([`RecvMeta::ecn`]).
    ///
    /// An offload not asked for is not an offence: a `Datagrams` with
    /// `segment_size: None` and `ecn: None` passes against
    /// [`UdpCaps::NONE`], so the weakest socket still serves every caller
    /// that wanted nothing.
    pub fn reject_unsupported(&self, caps: UdpCaps) -> std::io::Result<()> {
        let gso = self.segments() > caps.max_send_segments;
        // Asymmetric on purpose — see this method's doc comment. A socket
        // that declares no ECN is *allowed* to be handed a codepoint and
        // drop it; one that declares ECN is not allowed to be handed one it
        // will not apply. The second half is unreachable from a correct
        // implementation, which is what makes it worth checking: it is the
        // assertion that the declaration means something.
        let ecn = false;
        if !gso && !ecn {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            UnsupportedUdpOffload { gso, ecn },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn plain(contents: &[u8]) -> Datagrams<'_> {
        Datagrams {
            destination: to(1),
            src_ip: None,
            ecn: None,
            segment_size: None,
            contents,
        }
    }

    #[test]
    fn none_is_the_conservative_base() {
        // Every field spelled out, and `may_fragment` is the point of the
        // test: it is the one whose conservative value is `true`, so a
        // reader (or a mutation) that "tidied" the struct to all-false or
        // all-zero would be caught here and nowhere else.
        let c = UdpCaps::NONE;
        assert_eq!(c.max_send_segments, 1, "1 == no GSO, not 0 and not 64");
        assert_eq!(c.max_recv_segments, 1, "1 == no GRO");
        assert!(!c.ecn);
        assert!(
            c.may_fragment,
            "the pessimistic answer: a socket that says nothing must not \
             claim path MTU discovery is reliable"
        );
    }

    #[test]
    fn a_default_caps_impl_reports_nothing() {
        // The default is a claim made by silence, and this is the only test
        // that reads it. `TcpConnect::APPLIES` has the same test for the
        // same reason: with every shipped implementation overriding the
        // method, flipping the default to something optimistic would
        // otherwise pass the whole suite.
        struct Forgetful;
        impl UdpDatagrams for Forgetful {
            fn try_send(&self, _: &Datagrams<'_>) -> std::io::Result<()> {
                unreachable!("this socket never sends")
            }
            fn poll_writable(&self, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                unreachable!("this socket never sends")
            }
            fn poll_recv(
                &self,
                _: &mut Context<'_>,
                _: &mut [IoSliceMut<'_>],
                _: &mut [RecvMeta],
            ) -> Poll<std::io::Result<usize>> {
                unreachable!("this socket never receives")
            }
            fn local_addr(&self) -> std::io::Result<SocketAddr> {
                unreachable!("this socket is never bound")
            }
            // No `caps` — that absence is the subject of this test.
        }
        assert_eq!(Forgetful.caps(), UdpCaps::NONE);
    }

    #[test]
    fn segments_counts_datagrams_not_bytes() {
        assert_eq!(plain(&[0u8; 3600]).segments(), 1, "no GSO asked for");
        let g = Datagrams {
            segment_size: Some(1200),
            ..plain(&[0u8; 3600])
        };
        assert_eq!(g.segments(), 3);
        // A trailing partial datagram counts, or a 2401-byte GSO send would
        // be reported as two and slip past a socket that can do exactly two.
        let g = Datagrams {
            segment_size: Some(1200),
            ..plain(&[0u8; 2401])
        };
        assert_eq!(g.segments(), 3);
    }

    #[test]
    fn asking_for_nothing_is_never_an_offence() {
        // Without this, `UdpCaps::NONE` would refuse every send and the
        // weakest socket would be unusable rather than merely slow.
        assert!(plain(b"hello").reject_unsupported(UdpCaps::NONE).is_ok());
        let marked = Datagrams {
            ecn: Some(EcnCodepoint::Ect0),
            ..plain(b"hello")
        };
        assert!(
            marked.reject_unsupported(UdpCaps::NONE).is_ok(),
            "a socket that declares no ECN is allowed to drop the marking — \
             refusing here would make QUIC unusable on such a kernel"
        );
    }

    #[test]
    fn gso_beyond_the_declared_batch_is_refused_by_name() {
        let g = Datagrams {
            segment_size: Some(1200),
            ..plain(&[0u8; 3600])
        };
        // Exactly at the limit is fine; one over is not. Checking both is
        // what stops an off-by-one from passing.
        assert!(
            g.reject_unsupported(UdpCaps {
                max_send_segments: 3,
                ..UdpCaps::NONE
            })
            .is_ok()
        );
        let err = g
            .reject_unsupported(UdpCaps {
                max_send_segments: 2,
                ..UdpCaps::NONE
            })
            .expect_err("three datagrams asked of a two-datagram socket");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        let payload = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<UnsupportedUdpOffload>())
            .expect("the typed payload survives the trip through io::Error");
        assert_eq!(payload.names().collect::<Vec<_>>(), ["gso"]);
    }

    #[test]
    fn an_unfilled_recv_slot_reports_no_ecn_rather_than_a_plausible_one() {
        // The one lie in this module that would corrupt a congestion
        // controller instead of slowing it (see `RecvMeta::ecn`), pinned
        // where it starts: a caller allocates a slice of slots and hands it
        // to `poll_recv`, so whatever `Default` puts in `ecn` is what an
        // implementation that does not know the answer leaves behind. It
        // must be the absence.
        //
        // A test, and not only the comment on the field, because this is
        // exactly the kind of value someone "tidies" into `Some(Ect0)` to
        // make a struct literal look complete.
        assert_eq!(RecvMeta::default().ecn, None);
        // And the same for `stride`: 0 means "one datagram", so an
        // unfilled slot must not claim a GRO run it never received.
        assert_eq!(RecvMeta::default().stride, 0);
        assert_eq!(RecvMeta::default().len, 0);
    }

    #[test]
    fn ecn_bits_round_trip_and_zero_is_not_a_codepoint() {
        for c in [EcnCodepoint::Ect1, EcnCodepoint::Ect0, EcnCodepoint::Ce] {
            assert_eq!(EcnCodepoint::from_bits(c as u8), Some(c));
        }
        assert_eq!(
            EcnCodepoint::from_bits(0b00),
            None,
            "`00` is not-ECN-capable, which is an absence and not a fourth variant"
        );
        // The high six bits are DSCP and must not change the answer.
        assert_eq!(
            EcnCodepoint::from_bits(0b1011_1110),
            Some(EcnCodepoint::Ect0)
        );
    }
}
