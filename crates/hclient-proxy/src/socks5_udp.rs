//! RFC 1928's UDP ASSOCIATE (§4 `CMD=0x03`) and its datagram header (§7).
//!
//! The exchange shares its first two messages with [`Socks5`](crate::Socks5)'s
//! CONNECT — the greeting and, where configured, RFC 1929's
//! username/password sub-negotiation — and diverges only at the request
//! itself: `CMD=0x03` rather than `0x01`, and an address the client does
//! not yet know, sent as `0.0.0.0:0` per §4. What comes back names where
//! datagrams are actually relayed, which may be the proxy's own address
//! rather than the one asked for.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use bytes::{BufMut, Bytes, BytesMut};
use hclient_core::error::{Error, ErrorKind};

use crate::error::{Socks5HandshakeError, Socks5Refused};
use crate::socks5::{METHOD_NONE, METHOD_PASSWORD, METHOD_UNACCEPTABLE, SOCKS5_VERSION};
use crate::take;

/// Where a SOCKS5 proxy relays datagrams.
#[derive(Debug, Clone, PartialEq, Eq)]
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
pub enum AssociateStep {
    /// Send these bytes, then ask again.
    Write(Bytes),
    /// More bytes are needed; nothing was consumed.
    NeedMore,
    /// The association is open.
    Associated(RelayAddr),
}

/// How an association failed.
#[derive(Debug)]
pub enum AssociateError {
    /// The proxy does not relay UDP (`REP=0x07`, command not supported).
    Unsupported(Error),
    /// Any other failure: a malformed reply, a refusal, bad credentials.
    Failed(Error),
}

/// A UDP association as a state machine, the same contract as
/// [`Handshake`](crate::Handshake): `advance` consumes only complete
/// frames and answers `NeedMore` without consuming a partial one.
pub trait Associate: Send // send-bound-exception: amendment-C16
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
#[allow(
    dead_code,
    reason = "used by the SOCKS5 datagram path, which the next change adds"
)]
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
#[allow(
    dead_code,
    reason = "used by the SOCKS5 datagram path, which the next change adds"
)]
pub(crate) fn encode_datagram(prefix: &[u8], payload: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(prefix);
    out.extend_from_slice(payload);
}

/// The payload of a relayed datagram, or `None` for one this client
/// drops: a fragment (`FRAG != 0`, which RFC 1928 lets an implementation
/// refuse), a reserved field that is not zero, or a header that does not
/// parse.
#[allow(
    dead_code,
    reason = "used by the SOCKS5 datagram path, which the next change adds"
)]
pub(crate) fn decode_datagram(d: &[u8]) -> Option<&[u8]> {
    if d.len() < 4 || d[0] != 0 || d[1] != 0 || d[2] != 0 {
        return None;
    }
    match parse_addr(&d[3..]) {
        AddrParse::Ready(_, n) => d.get(3 + n..),
        AddrParse::NeedMore | AddrParse::Invalid(_) => None,
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
}
