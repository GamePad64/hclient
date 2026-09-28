//! RFC 1928's UDP ASSOCIATE (§4 `CMD=0x03`) and its datagram header (§7).
//!
//! The exchange shares its first two messages with [`Socks5`](crate::Socks5)'s
//! CONNECT — the greeting and, where configured, RFC 1929's
//! username/password sub-negotiation — and diverges only at the request
//! itself: `CMD=0x03` rather than `0x01`, and an address the client does
//! not yet know, sent as `0.0.0.0:0` per §4. What comes back names where
//! datagrams are actually relayed, which may be the proxy's own address
//! rather than the one asked for.

use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use hclient_rt::UdpDatagrams as _;

use bytes::{BufMut, Bytes, BytesMut};
use hclient_core::error::{Error, ErrorKind};

use crate::error::{AssociateError, Socks5HandshakeError, Socks5Refused};
use crate::socks5::{METHOD_NONE, METHOD_PASSWORD, METHOD_UNACCEPTABLE, SOCKS5_VERSION};
use crate::take;

/// Where a SOCKS5 proxy relays datagrams.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RelayAddr {
    /// An address to send to.
    Ip(SocketAddr),
    /// `0.0.0.0` or `::` — the proxy's own address, at this port: §4 lets
    /// a proxy answer this way rather than name itself, so the caller must
    /// stand in the address it already dialled.
    Unspecified(u16),
    /// A name the proxy gave, at this port.
    Name(String, u16),
}

/// What an association wants next.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssociateStep {
    /// Send these bytes, then ask again.
    Write(Bytes),
    /// More bytes are needed; nothing was consumed.
    NeedMore,
    /// The association is open.
    Associated(RelayAddr),
}

/// A SOCKS5 UDP ASSOCIATE (RFC 1928 §4) as a state machine, the same
/// contract as [`Handshake`](crate::Handshake): `advance` consumes only
/// complete frames and answers `NeedMore` without consuming a partial one.
///
/// **SOCKS5's alone, and sealed.** What an association opens is read with
/// §7's datagram header, which the built-in rules write and strip
/// themselves, so an association for any other protocol would open a path
/// framed wrongly. The only implementation is the one
/// [`Socks5::with_udp`](crate::Socks5::with_udp) returns through
/// [`Handshake::associate`](crate::Handshake::associate); a type outside
/// this crate cannot implement the trait:
///
/// ```compile_fail,E0277
/// struct Mine;
/// impl hclient_proxy::Associate for Mine {
///     fn begin(&mut self) -> bytes::Bytes {
///         bytes::Bytes::new()
///     }
///     fn advance(
///         &mut self,
///         _: &mut bytes::BytesMut,
///     ) -> Result<hclient_proxy::AssociateStep, hclient_proxy::AssociateError> {
///         Ok(hclient_proxy::AssociateStep::NeedMore)
///     }
/// }
/// ```
pub trait Associate: sealed::Sealed + Send // send-bound-exception: amendment-C16
{
    /// The first bytes to send.
    fn begin(&mut self) -> Bytes;

    /// Consume what has arrived; answer what happens next.
    ///
    /// # Errors
    ///
    /// [`AssociateError`], saying whether the proxy lacks UDP or failed.
    fn advance(&mut self, from_peer: &mut BytesMut) -> Result<AssociateStep, AssociateError>;
}

/// What keeps [`Associate`] this crate's: public so it may be named in a
/// public bound, in a module nothing outside the crate can reach.
mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Socks5Associate {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Fresh,
    AwaitingMethod,
    AwaitingAuthReply,
    AwaitingReply,
    Done,
}

/// The built-in [`Associate`]: SOCKS5's own §4 exchange.
pub(crate) struct Socks5Associate {
    auth: Option<(Box<str>, Box<str>)>,
    offered: Vec<u8>,
    state: State,
}

/// §4's UDP ASSOCIATE request: `CMD=0x03`, `ATYP=1`, address `0.0.0.0:0` —
/// the client does not yet know where it will send from, and §4 says this
/// is answered anyway.
const REQUEST: [u8; 10] = [SOCKS5_VERSION, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
const REP_COMMAND_NOT_SUPPORTED: u8 = 0x07;

impl Socks5Associate {
    pub(crate) fn new(auth: Option<(Box<str>, Box<str>)>) -> Self {
        Self {
            auth,
            offered: Vec::new(),
            state: State::Fresh,
        }
    }
}

fn failed(e: Socks5HandshakeError) -> AssociateError {
    AssociateError::Failed(Error::new(ErrorKind::Connect, e))
}

impl Associate for Socks5Associate {
    fn begin(&mut self) -> Bytes {
        self.offered = if self.auth.is_some() {
            vec![METHOD_PASSWORD, METHOD_NONE]
        } else {
            vec![METHOD_NONE]
        };
        let mut g = BytesMut::with_capacity(2 + self.offered.len());
        g.put_u8(SOCKS5_VERSION);
        // Bounded by construction: `offered` is built two lines above and
        // holds at most two methods.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Bounded by construction: `offered` is built two lines above and holds at most two methods."
        )]
        g.put_u8(self.offered.len() as u8);
        g.put_slice(&self.offered);
        self.state = State::AwaitingMethod;
        g.freeze()
    }

    fn advance(&mut self, buf: &mut BytesMut) -> Result<AssociateStep, AssociateError> {
        match self.state {
            State::Fresh | State::Done => Ok(AssociateStep::NeedMore),

            State::AwaitingMethod => {
                let Some(m) = take(buf, 2) else {
                    return Ok(AssociateStep::NeedMore);
                };
                if m[0] != SOCKS5_VERSION {
                    return Err(failed(Socks5HandshakeError::BadVersion(m[0])));
                }
                match m[1] {
                    METHOD_UNACCEPTABLE => Err(failed(Socks5HandshakeError::NoAcceptableMethods)),
                    x if !self.offered.contains(&x) => {
                        Err(failed(Socks5HandshakeError::UnofferedMethod(x)))
                    }
                    METHOD_PASSWORD => {
                        let (u, p) = self
                            .auth
                            .as_ref()
                            .expect("METHOD_PASSWORD is offered only when credentials exist");
                        let mut msg = BytesMut::with_capacity(3 + u.len() + p.len());
                        msg.put_u8(0x01);
                        // Both bounded: `Socks5::password_auth` refuses
                        // either over 255 bytes as `CredentialTooLong`, at
                        // the setter rather than here.
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "Both bounded: `Socks5::password_auth` refuses either over 255 bytes as `CredentialTooLong`, at the setter rather than here."
                        )]
                        msg.put_u8(u.len() as u8);
                        msg.put_slice(u.as_bytes());
                        // Bounded at the setter too — see the pair above.
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "Bounded at the setter too — see the pair above."
                        )]
                        msg.put_u8(p.len() as u8);
                        msg.put_slice(p.as_bytes());
                        self.state = State::AwaitingAuthReply;
                        Ok(AssociateStep::Write(msg.freeze()))
                    }
                    _ => {
                        self.state = State::AwaitingReply;
                        Ok(AssociateStep::Write(Bytes::from_static(&REQUEST)))
                    }
                }
            }

            State::AwaitingAuthReply => {
                let Some(r) = take(buf, 2) else {
                    return Ok(AssociateStep::NeedMore);
                };
                if r[1] != 0 {
                    return Err(failed(Socks5HandshakeError::BadCredentials));
                }
                self.state = State::AwaitingReply;
                Ok(AssociateStep::Write(Bytes::from_static(&REQUEST)))
            }

            State::AwaitingReply => {
                if buf.len() < 4 {
                    return Ok(AssociateStep::NeedMore);
                }
                if buf[0] != SOCKS5_VERSION {
                    return Err(failed(Socks5HandshakeError::BadVersion(buf[0])));
                }
                if buf[1] != 0 {
                    let e = Error::new(ErrorKind::Connect, Socks5Refused { rep: buf[1] });
                    return Err(if buf[1] == REP_COMMAND_NOT_SUPPORTED {
                        AssociateError::Unsupported(e)
                    } else {
                        AssociateError::Failed(e)
                    });
                }
                match parse_addr(&buf[3..]) {
                    AddrParse::NeedMore => Ok(AssociateStep::NeedMore),
                    AddrParse::Invalid(reason) => Err(failed(reason)),
                    AddrParse::Ready(relay, total) => {
                        let _ = buf.split_to(3 + total);
                        self.state = State::Done;
                        Ok(AssociateStep::Associated(relay))
                    }
                }
            }
        }
    }
}

/// What [`parse_addr`] found at the start of a buffer.
enum AddrParse {
    /// A complete `ATYP ADDR PORT`, and how many bytes it took.
    Ready(RelayAddr, usize),
    /// The buffer does not yet hold a complete frame.
    NeedMore,
    /// The buffer holds a complete frame that is not a legal one — an
    /// `ATYP` RFC 1928 does not define, or (for `ATYP=0x03`, once every
    /// byte the length names has arrived) a name that is not valid UTF-8.
    /// Neither is fixed by reading more, so neither is `NeedMore`.
    Invalid(Socks5HandshakeError),
}

/// `ATYP ADDR PORT` at the start of `b`.
fn parse_addr(b: &[u8]) -> AddrParse {
    let port_at = |at: usize| b.get(at..at + 2).map(|p| u16::from_be_bytes([p[0], p[1]]));
    let Some(&atyp) = b.first() else {
        return AddrParse::NeedMore;
    };
    match atyp {
        0x01 => {
            let Some(raw) = b.get(1..5) else {
                return AddrParse::NeedMore;
            };
            let ip = Ipv4Addr::from(<[u8; 4]>::try_from(raw).expect("checked length above"));
            let Some(p) = port_at(5) else {
                return AddrParse::NeedMore;
            };
            let relay = if ip.is_unspecified() {
                RelayAddr::Unspecified(p)
            } else {
                RelayAddr::Ip(SocketAddr::new(IpAddr::V4(ip), p))
            };
            AddrParse::Ready(relay, 7)
        }
        0x04 => {
            let Some(raw) = b.get(1..17) else {
                return AddrParse::NeedMore;
            };
            let ip = Ipv6Addr::from(<[u8; 16]>::try_from(raw).expect("checked length above"));
            let Some(p) = port_at(17) else {
                return AddrParse::NeedMore;
            };
            let relay = if ip.is_unspecified() {
                RelayAddr::Unspecified(p)
            } else {
                RelayAddr::Ip(SocketAddr::new(IpAddr::V6(ip), p))
            };
            AddrParse::Ready(relay, 19)
        }
        0x03 => {
            let Some(&len_byte) = b.get(1) else {
                return AddrParse::NeedMore;
            };
            let len = usize::from(len_byte);
            let Some(name_bytes) = b.get(2..2 + len) else {
                return AddrParse::NeedMore;
            };
            let Some(p) = port_at(2 + len) else {
                return AddrParse::NeedMore;
            };
            // Complete — the length byte and every byte it named, plus
            // the port, have all arrived — so a non-UTF-8 name from here
            // is malformed rather than merely not-yet-here: no further
            // bytes would ever make it valid.
            match std::str::from_utf8(name_bytes) {
                Ok(name) => AddrParse::Ready(RelayAddr::Name(name.to_owned(), p), 2 + len + 2),
                Err(_) => AddrParse::Invalid(Socks5HandshakeError::NonUtf8Name),
            }
        }
        other => AddrParse::Invalid(Socks5HandshakeError::BadAddressType(other)),
    }
}

/// §7's header for datagrams to `host:port`, the host **by name** —
/// `ATYP=0x03`, so the origin's name reaches the proxy rather than being
/// resolved locally, the same rule [`Socks5::begin`](crate::Socks5::begin)
/// follows for CONNECT.
pub(crate) fn header_for(host: &str, port: u16) -> Result<Bytes, Error> {
    let host = hclient_core::url::bare_host(host);
    if host.len() > 255 {
        return Err(Error::new(
            ErrorKind::Connect,
            Socks5HandshakeError::HostTooLong(host.len()),
        ));
    }
    let mut h = BytesMut::with_capacity(7 + host.len());
    h.put_slice(&[0, 0, 0, 0x03]);
    // Bounded: `host.len() > 255` is refused three lines above, so this
    // cast is exact.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "Bounded: `host.len() > 255` is refused three lines above, so this cast is exact."
    )]
    h.put_u8(host.len() as u8);
    h.put_slice(host.as_bytes());
    h.put_u16(port);
    Ok(h.freeze())
}

/// Write a relayed datagram: `prefix` (a §7 header, from [`header_for`])
/// followed by `payload`, into `out` — replacing whatever `out` held.
pub(crate) fn encode_datagram(prefix: &[u8], payload: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(prefix);
    out.extend_from_slice(payload);
}

/// The payload of a relayed datagram, or `None` for one this client
/// drops: a fragment (`FRAG != 0`, which RFC 1928 lets an implementation
/// refuse), a reserved field that is not zero, or a header that does not
/// parse.
pub(crate) fn decode_datagram(d: &[u8]) -> Option<&[u8]> {
    if d.len() < 4 || d[0] != 0 || d[1] != 0 || d[2] != 0 {
        return None;
    }
    match parse_addr(&d[3..]) {
        AddrParse::Ready(_, n) => d.get(3 + n..),
        AddrParse::NeedMore | AddrParse::Invalid(_) => None,
    }
}

/// Run an association over `io`, the control connection, until the proxy
/// names its relay.
pub(crate) async fn drive_associate<S: crate::Io>(
    io: &mut S,
    a: &mut dyn Associate,
) -> Result<RelayAddr, AssociateError> {
    crate::drive::write_all(io, &a.begin())
        .await
        .map_err(AssociateError::Failed)?;
    let mut buf = BytesMut::new();
    loop {
        match a.advance(&mut buf)? {
            AssociateStep::Associated(r) => return Ok(r),
            AssociateStep::Write(b) => crate::drive::write_all(io, &b)
                .await
                .map_err(AssociateError::Failed)?,
            AssociateStep::NeedMore => crate::drive::read_some(io, &mut buf)
                .await
                .map_err(AssociateError::Failed)?,
        }
    }
}

/// A UDP payload that fits an ordinary 1500-byte Ethernet MTU under IPv6.
const UDP_PAYLOAD: usize = 1452;

/// The longest §7 header a relay can put in front of a payload:
/// `RSV FRAG ATYP`, a length byte and a 255-byte name, and a port.
const LONGEST_HEADER: usize = 3 + 1 + 1 + 255 + 2;

/// What one segment of a receive is given room for.
const SEGMENT: usize = UDP_PAYLOAD + LONGEST_HEADER;

/// A datagram path through a SOCKS5 relay.
///
/// Holds the control connection for as long as the association lives:
/// RFC 1928 ends the association when that connection closes, so the path
/// watches it and ends too.
pub(crate) struct Socks5Path<S> {
    control: Mutex<S>,
    udp: crate::BoxUdp,
    relay: SocketAddr,
    header: Bytes,
    max: usize,
    /// The outgoing datagram, header and payload, reused across sends.
    scratch: Mutex<Vec<u8>>,
    /// The receive buffer, room for `segments` datagrams, reused.
    raw: Mutex<Vec<u8>>,
    /// Payloads already received and decoded but not yet handed over: a
    /// receive coalesced by GRO carries several, and `poll_recv` hands
    /// over one.
    queued: Mutex<VecDeque<Vec<u8>>>,
}

impl<S> std::fmt::Debug for Socks5Path<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Socks5Path")
            .field("relay", &self.relay)
            .finish_non_exhaustive()
    }
}

impl<S> Socks5Path<S> {
    pub(crate) fn new(control: S, udp: crate::BoxUdp, relay: SocketAddr, header: Bytes) -> Self {
        let segments = udp.support().max_recv_segments.max(1);
        Self {
            control: Mutex::new(control),
            max: UDP_PAYLOAD.saturating_sub(header.len()),
            udp,
            relay,
            header,
            scratch: Mutex::new(Vec::new()),
            raw: Mutex::new(vec![0; SEGMENT * segments]),
            queued: Mutex::new(VecDeque::new()),
        }
    }
}

impl<S> crate::DatagramPath for Socks5Path<S>
where
    // The path is `Send + Sync`, and the control connection is held in it
    // for the association's life; `Mutex<S>` is `Sync` for any `Send` `S`.
    S: crate::Io + Send + 'static, // send-bound-exception: amendment-C16
{
    fn try_send(&self, d: &[u8]) -> io::Result<()> {
        if d.len() > self.max {
            return Err(crate::datagram::too_big(d.len(), self.max));
        }
        let mut out = self.scratch.lock().expect("scratch mutex");
        encode_datagram(&self.header, d, &mut out);
        self.udp
            .try_send(&hclient_rt::Datagrams::new(self.relay, &out))
    }

    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.udp.poll_writable(cx)
    }

    fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let mut queued = self.queued.lock().expect("queue mutex");
        if let Some(p) = queued.pop_front() {
            return Poll::Ready(Ok(copy_out(&p, buf)));
        }
        // The control connection first: the proxy ends the association by
        // closing it, and a path that only watched UDP would wait for the
        // QUIC stack's idle timeout to learn that. Nothing follows the
        // proxy's reply on that connection, so a byte is as final as an
        // end of stream or an error.
        {
            let mut c = self.control.lock().expect("control mutex");
            let mut probe = [0u8; 1];
            if let Poll::Ready(r) = Pin::new(&mut *c).poll_read(cx, &mut probe) {
                tracing::trace!("proxy: socks5 association's control connection answered {r:?}");
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "the SOCKS5 proxy closed the association's control connection",
                )));
            }
        }
        let mut raw = self.raw.lock().expect("receive buffer mutex");
        loop {
            let mut meta = [hclient_rt::RecvMeta::default()];
            let n = {
                let mut bufs = [io::IoSliceMut::new(&mut raw)];
                std::task::ready!(self.udp.poll_recv(cx, &mut bufs, &mut meta))?
            };
            let m = &meta[0];
            if n == 0 || m.addr != self.relay {
                continue;
            }
            // A GRO receive is several datagrams `stride` apart; `0`, or a
            // stride at least `len`, is one.
            let stride = if m.stride == 0 || m.stride >= m.len {
                m.len
            } else {
                m.stride
            };
            queued.extend(
                raw[..m.len]
                    .chunks(stride.max(1))
                    .filter_map(decode_datagram)
                    .map(<[u8]>::to_vec),
            );
            if let Some(p) = queued.pop_front() {
                return Poll::Ready(Ok(copy_out(&p, buf)));
            }
        }
    }

    fn max_datagram_size(&self) -> usize {
        self.max
    }
}

/// Copy a payload into the caller's buffer, truncating where it is short.
fn copy_out(p: &[u8], buf: &mut [u8]) -> usize {
    let k = p.len().min(buf.len());
    buf[..k].copy_from_slice(&p[..k]);
    k
}

/// Test doubles shared with `rules.rs`'s tests: a UDP socket over an
/// in-memory queue, and a control connection that stays open or is closed.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::VecDeque;
    use std::io;
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use hclient_rt::{Datagrams, RecvMeta, UdpDatagrams, UdpSupport};

    #[derive(Debug, Default)]
    struct Inner {
        sent: Mutex<Vec<(SocketAddr, Vec<u8>)>>,
        /// Each receive: who sent it, the bytes, and the GRO stride.
        inbox: Mutex<VecDeque<(SocketAddr, Vec<u8>, usize)>>,
    }

    /// A UDP socket whose sends are kept and whose receives are queued in
    /// advance. Clones share one socket, so a test keeps a handle to what
    /// a path owns.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct FakeUdp {
        inner: Arc<Inner>,
        segments: usize,
        /// Whether `poll_writable` pends rather than answering at once.
        blocked: bool,
    }

    impl FakeUdp {
        /// A socket reporting GRO of up to `segments` datagrams per receive.
        pub(crate) fn with_gro(segments: usize) -> Self {
            Self {
                segments,
                ..Self::default()
            }
        }

        /// A socket that is never writable.
        pub(crate) fn blocked() -> Self {
            Self {
                blocked: true,
                ..Self::default()
            }
        }

        pub(crate) fn sent(&self) -> Vec<(SocketAddr, Vec<u8>)> {
            self.inner.sent.lock().unwrap().clone()
        }

        pub(crate) fn push_from(&self, from: &str, d: &[u8]) {
            self.inner
                .inbox
                .lock()
                .unwrap()
                .push_back((from.parse().unwrap(), d.to_vec(), 0));
        }

        /// Several equal-sized datagrams coalesced into one receive, the
        /// way GRO hands them over.
        pub(crate) fn push_coalesced(&self, from: &str, segments: &[&[u8]]) {
            let stride = segments[0].len();
            assert!(
                segments[..segments.len() - 1]
                    .iter()
                    .all(|s| s.len() == stride)
            );
            self.inner.inbox.lock().unwrap().push_back((
                from.parse().unwrap(),
                segments.concat(),
                stride,
            ));
        }
    }

    impl UdpDatagrams for FakeUdp {
        fn try_send(&self, t: &Datagrams<'_>) -> io::Result<()> {
            self.inner
                .sent
                .lock()
                .unwrap()
                .push((t.destination, t.contents.to_vec()));
            Ok(())
        }
        fn poll_writable(&self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            if self.blocked {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        }
        fn poll_recv(
            &self,
            _: &mut Context<'_>,
            bufs: &mut [io::IoSliceMut<'_>],
            meta: &mut [RecvMeta],
        ) -> Poll<io::Result<usize>> {
            let Some((from, d, stride)) = self.inner.inbox.lock().unwrap().pop_front() else {
                return Poll::Pending;
            };
            let n = d.len().min(bufs[0].len());
            bufs[0][..n].copy_from_slice(&d[..n]);
            meta[0] = RecvMeta::new(from, n, stride);
            Poll::Ready(Ok(1))
        }
        fn local_addr(&self) -> io::Result<SocketAddr> {
            Ok("0.0.0.0:0".parse().unwrap())
        }
        fn support(&self) -> UdpSupport {
            UdpSupport::NONE.max_recv_segments(self.segments.max(1))
        }
    }

    /// A control connection: open reads pend, closed reads end.
    #[derive(Debug)]
    pub(crate) struct Control {
        pub(crate) open: bool,
    }

    impl futures_io::AsyncRead for Control {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            if self.open {
                Poll::Pending
            } else {
                Poll::Ready(Ok(0))
            }
        }
    }
    impl futures_io::AsyncWrite for Control {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            b: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(b.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    impl hclient_rt::Shutdown for Control {
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    use crate::{Handshake, Socks5};

    fn assoc(auth: bool) -> Box<dyn Associate> {
        let s = if auth {
            Socks5::new().password_auth("u", "p").unwrap()
        } else {
            Socks5::new()
        };
        s.with_udp().associate().expect("with_udp answers Some")
    }

    #[test]
    fn a_proxy_without_with_udp_offers_no_association() {
        assert!(Socks5::new().associate().is_none());
    }

    #[test]
    fn the_exchange_is_greeting_then_associate_with_a_zero_address() {
        let mut a = assoc(false);
        assert_eq!(&a.begin()[..], &[0x05, 0x01, 0x00]);
        let mut buf = BytesMut::from(&[0x05, 0x00][..]);
        let AssociateStep::Write(req) = a.advance(&mut buf).unwrap() else {
            panic!()
        };
        assert_eq!(&req[..], &[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        let mut buf = BytesMut::from(&[0x05, 0x00, 0x00, 0x01, 10, 0, 0, 7, 0x1f, 0x90][..]);
        let AssociateStep::Associated(r) = a.advance(&mut buf).unwrap() else {
            panic!()
        };
        assert_eq!(r, RelayAddr::Ip("10.0.0.7:8080".parse().unwrap()));
        assert!(buf.is_empty());
    }

    #[test]
    fn an_unspecified_relay_is_reported_so_the_proxys_address_can_stand_in() {
        let mut a = assoc(false);
        a.begin();
        a.advance(&mut BytesMut::from(&[0x05, 0x00][..])).unwrap();
        let mut buf = BytesMut::from(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0x04, 0x38][..]);
        let AssociateStep::Associated(r) = a.advance(&mut buf).unwrap() else {
            panic!()
        };
        assert_eq!(r, RelayAddr::Unspecified(1080));
    }

    #[test]
    fn command_not_supported_is_unsupported_and_every_other_refusal_is_failed() {
        for (rep, unsupported) in [(0x07, true), (0x01, false), (0x02, false), (0x05, false)] {
            let mut a = assoc(false);
            a.begin();
            a.advance(&mut BytesMut::from(&[0x05, 0x00][..])).unwrap();
            let e = a
                .advance(&mut BytesMut::from(
                    &[0x05, rep, 0x00, 0x01, 0, 0, 0, 0, 0, 0][..],
                ))
                .unwrap_err();
            assert_eq!(
                matches!(e, AssociateError::Unsupported(_)),
                unsupported,
                "rep {rep:#04x}"
            );
        }
    }

    #[test]
    fn a_reply_arriving_one_byte_at_a_time_is_never_consumed_early() {
        let reply = [
            0x05, 0x00, 0x00, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0x04, 0x38,
        ];
        let mut a = assoc(false);
        a.begin();
        a.advance(&mut BytesMut::from(&[0x05, 0x00][..])).unwrap();
        let mut buf = BytesMut::new();
        for (i, b) in reply.iter().enumerate() {
            buf.extend_from_slice(&[*b]);
            let step = a.advance(&mut buf).unwrap();
            if i + 1 < reply.len() {
                assert_eq!(step, AssociateStep::NeedMore);
                assert_eq!(buf.len(), i + 1, "consumed early at {i}");
            } else {
                assert_eq!(
                    step,
                    AssociateStep::Associated(RelayAddr::Ip("[::1]:1080".parse().unwrap()))
                );
            }
        }
    }

    #[test]
    fn the_password_sub_negotiation_precedes_the_association() {
        let mut a = assoc(true);
        assert_eq!(&a.begin()[..], &[0x05, 0x02, 0x02, 0x00]);
        let AssociateStep::Write(auth) = a.advance(&mut BytesMut::from(&[0x05, 0x02][..])).unwrap()
        else {
            panic!()
        };
        assert_eq!(&auth[..], &[0x01, 1, b'u', 1, b'p']);
        let AssociateStep::Write(req) = a.advance(&mut BytesMut::from(&[0x01, 0x00][..])).unwrap()
        else {
            panic!()
        };
        assert_eq!(req[1], 0x03);
    }

    #[test]
    fn a_datagram_carries_the_origin_by_name() {
        let h = header_for("origin.test", 443).unwrap();
        assert_eq!(&h[..5], &[0, 0, 0, 0x03, 11]);
        assert_eq!(&h[5..16], b"origin.test");
        assert_eq!(&h[16..], &443u16.to_be_bytes());
        let mut out = Vec::new();
        encode_datagram(&h, b"quic", &mut out);
        assert_eq!(&out[out.len() - 4..], b"quic");
    }

    #[test]
    fn decode_strips_any_address_type_and_drops_fragments_and_garbage() {
        let v4 = [0, 0, 0, 0x01, 1, 2, 3, 4, 0, 80, b'x'];
        assert_eq!(decode_datagram(&v4), Some(&b"x"[..]));
        let name = [0, 0, 0, 0x03, 1, b'a', 0, 80, b'y'];
        assert_eq!(decode_datagram(&name), Some(&b"y"[..]));
        let frag = [0, 0, 1, 0x01, 1, 2, 3, 4, 0, 80, b'x'];
        assert_eq!(decode_datagram(&frag), None);
        assert_eq!(decode_datagram(&[0, 0, 0, 0x09]), None);
        assert_eq!(decode_datagram(&[0, 0, 0, 0x01, 1]), None);
    }

    #[test]
    fn a_host_too_long_for_the_length_byte_is_refused() {
        assert!(header_for(&"a".repeat(256), 1).is_err());
        // The control: the longest name the byte can hold is accepted.
        let h = header_for(&"a".repeat(255), 1).unwrap();
        assert_eq!(h[4], 255);
    }

    #[test]
    fn a_datagram_with_either_reserved_byte_set_or_too_short_to_hold_one_is_dropped() {
        let ok = [0, 0, 0, 0x01, 1, 2, 3, 4, 0, 80, b'x'];
        assert_eq!(decode_datagram(&ok), Some(&b"x"[..]));
        let mut rsv0 = ok;
        rsv0[0] = 1;
        assert_eq!(decode_datagram(&rsv0), None);
        let mut rsv1 = ok;
        rsv1[1] = 1;
        assert_eq!(decode_datagram(&rsv1), None);
        for short in [&[][..], &[0][..], &[0, 0][..], &[0, 0, 0][..]] {
            assert_eq!(decode_datagram(short), None, "{short:?}");
        }
    }

    fn method_error(chosen: u8) -> Socks5HandshakeError {
        let mut a = assoc(false);
        a.begin();
        let err = a
            .advance(&mut BytesMut::from(&[0x05, chosen][..]))
            .expect_err("a method this client cannot use is a refusal");
        let AssociateError::Failed(e) = err else {
            panic!("a method refusal is not `Unsupported`: {err:?}")
        };
        let source = std::error::Error::source(&e).expect("a source");
        match source.downcast_ref::<Socks5HandshakeError>() {
            Some(Socks5HandshakeError::NoAcceptableMethods) => {
                Socks5HandshakeError::NoAcceptableMethods
            }
            Some(Socks5HandshakeError::UnofferedMethod(m)) => {
                Socks5HandshakeError::UnofferedMethod(*m)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_acceptable_method_is_named_as_such_and_an_unoffered_one_by_its_number() {
        assert!(matches!(
            method_error(0xFF),
            Socks5HandshakeError::NoAcceptableMethods
        ));
        // Password was not offered — no credentials — so a proxy choosing
        // it is refused, not answered with credentials this client has not
        // got.
        assert!(matches!(
            method_error(0x02),
            Socks5HandshakeError::UnofferedMethod(0x02)
        ));
    }

    #[test]
    fn a_complete_non_utf8_domain_name_is_refused_rather_than_awaited_forever() {
        // `ATYP=3`, length 1, one byte that is not valid UTF-8 on its
        // own (a lone continuation byte), then a port. The frame is
        // complete — no further byte would ever make `0x80` a valid
        // UTF-8 string — so this must be a definite failure rather than
        // `NeedMore`, which would stall a real driver forever.
        let mut a = assoc(false);
        a.begin();
        a.advance(&mut BytesMut::from(&[0x05, 0x00][..])).unwrap();
        let reply = [0x05, 0x00, 0x00, 0x03, 1, 0x80, 0x00, 0x50];
        let err = a
            .advance(&mut BytesMut::from(&reply[..]))
            .expect_err("a non-UTF-8 name is malformed, not incomplete");
        let AssociateError::Failed(e) = err else {
            panic!("a malformed name is not an `Unsupported` refusal: {err:?}")
        };
        let source = std::error::Error::source(&e).expect("a source");
        assert!(
            matches!(
                source.downcast_ref::<Socks5HandshakeError>(),
                Some(Socks5HandshakeError::NonUtf8Name)
            ),
            "{e:?}"
        );
    }

    mod path {
        use std::task::{Context, Poll, Waker};

        use super::super::fake::{Control, FakeUdp};
        use super::super::{Socks5Path, header_for};
        use crate::{BoxUdp, DatagramPath};

        fn cx() -> Context<'static> {
            Context::from_waker(Waker::noop())
        }

        fn open_control() -> Control {
            Control { open: true }
        }

        fn closed_control() -> Control {
            Control { open: false }
        }

        fn over(relay: &str, control: Control, udp: FakeUdp) -> (Socks5Path<Control>, FakeUdp) {
            let path = Socks5Path::new(
                control,
                BoxUdp::new(udp.clone()),
                relay.parse().unwrap(),
                header_for("origin.test", 443).unwrap(),
            );
            (path, udp)
        }

        fn path_over_fake(relay: &str, control: Control) -> (Socks5Path<Control>, FakeUdp) {
            over(relay, control, FakeUdp::default())
        }

        #[test]
        fn a_path_prefixes_the_header_and_sends_to_the_relay() {
            let (path, udp) = path_over_fake("10.0.0.7:8080", open_control());
            path.try_send(b"quic").unwrap();
            let (to, d) = udp.sent()[0].clone();
            assert_eq!(to, "10.0.0.7:8080".parse().unwrap());
            assert_eq!(&d[..4], &[0, 0, 0, 0x03]);
            assert!(d.ends_with(b"quic"));
        }

        #[test]
        fn a_datagram_not_from_the_relay_is_dropped() {
            let (path, udp) = path_over_fake("10.0.0.7:8080", open_control());
            udp.push_from("10.0.0.8:8080", &[0, 0, 0, 1, 1, 2, 3, 4, 0, 80, b'x']);
            udp.push_from("10.0.0.7:8080", &[0, 0, 0, 1, 1, 2, 3, 4, 0, 80, b'y']);
            let mut buf = [0u8; 8];
            let Poll::Ready(Ok(n)) = path.poll_recv(&mut cx(), &mut buf) else {
                panic!()
            };
            assert_eq!(&buf[..n], b"y");
        }

        #[test]
        fn a_closed_control_stream_is_a_path_error() {
            let (path, _udp) = path_over_fake("10.0.0.7:8080", closed_control());
            let mut buf = [0u8; 8];
            let Poll::Ready(Err(e)) = path.poll_recv(&mut cx(), &mut buf) else {
                panic!("must not pend")
            };
            assert_eq!(e.kind(), std::io::ErrorKind::ConnectionAborted);
        }

        #[test]
        fn socks5_path_refuses_an_oversized_datagram() {
            let (path, _udp) = path_over_fake("10.0.0.7:8080", open_control());
            let max = path.max_datagram_size();
            assert_eq!(
                path.try_send(&vec![0; max + 1]).unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput
            );
            // The control: exactly the size it declares is carried.
            path.try_send(&vec![0; max]).unwrap();
        }

        #[test]
        fn a_path_waits_on_its_socket_to_be_writable() {
            let (path, _udp) = over("10.0.0.7:8080", open_control(), FakeUdp::blocked());
            assert!(path.poll_writable(&mut cx()).is_pending());
            let (open, _) = path_over_fake("10.0.0.7:8080", open_control());
            assert!(matches!(open.poll_writable(&mut cx()), Poll::Ready(Ok(()))));
        }

        #[test]
        fn a_full_datagram_behind_the_longest_header_is_received_whole() {
            // RSV FRAG, ATYP=3, a 255-byte name, a port — the longest §7
            // header — then a full 1452-byte payload: the most a relay can
            // send, and exactly what the receive buffer is sized for.
            let (path, udp) = path_over_fake("10.0.0.7:8080", open_control());
            let mut d = vec![0, 0, 0, 0x03, 255];
            d.extend(std::iter::repeat_n(b'a', 255));
            d.extend_from_slice(&443u16.to_be_bytes());
            d.extend(std::iter::repeat_n(7u8, 1452));
            udp.push_from("10.0.0.7:8080", &d);
            let mut buf = vec![0u8; 4096];
            let Poll::Ready(Ok(n)) = path.poll_recv(&mut cx(), &mut buf) else {
                panic!("the datagram")
            };
            assert_eq!(n, 1452);
        }

        #[test]
        fn a_coalesced_receive_is_handed_over_one_datagram_at_a_time() {
            // GRO: two relayed datagrams in one buffer, `stride` apart.
            // Read as one they would be glued together — the second
            // header inside the first payload.
            let (path, udp) = over("10.0.0.7:8080", open_control(), FakeUdp::with_gro(4));
            udp.push_coalesced(
                "10.0.0.7:8080",
                &[
                    &[0, 0, 0, 1, 1, 2, 3, 4, 0, 80, b'a', b'1'],
                    &[0, 0, 0, 1, 1, 2, 3, 4, 0, 80, b'b', b'2'],
                ],
            );
            let mut buf = [0u8; 64];
            let Poll::Ready(Ok(n)) = path.poll_recv(&mut cx(), &mut buf) else {
                panic!("first")
            };
            assert_eq!(&buf[..n], b"a1");
            let Poll::Ready(Ok(n)) = path.poll_recv(&mut cx(), &mut buf) else {
                panic!("second, from the queue")
            };
            assert_eq!(&buf[..n], b"b2");
            assert!(path.poll_recv(&mut cx(), &mut buf).is_pending());
        }
    }
}
