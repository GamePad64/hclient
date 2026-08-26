//! SOCKS5, RFC 1928, with RFC 1929's username/password sub-negotiation.
//!
//! The answer unless a server forces otherwise, for reasons that belong
//! to the other protocol and are written in [`crate::socks4`]: no IPv6,
//! because SOCKS4's address field is four bytes, and no authentication,
//! only a `USERID` a proxy may check against an identd.

use bytes::{BufMut, Bytes, BytesMut};
use hclient_core::{Error, ErrorKind};

use crate::{Approach, Handshake, Step, take};

/// RFC 1928 §6's `REP`, which is one byte and has no HTTP meaning at all.
#[derive(Debug, thiserror::Error)]
#[error("the SOCKS5 proxy refused with REP={rep:#04x} ({})", socks5_reply(*rep))]
#[non_exhaustive]
pub struct Socks5Refused {
    pub rep: u8,
}

fn socks5_reply(rep: u8) -> &'static str {
    match rep {
        0x01 => "general failure",
        0x02 => "connection not allowed by ruleset",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "TTL expired",
        0x07 => "command not supported",
        0x08 => "address type not supported",
        _ => "unassigned",
    }
}

/// The proxy would not agree to any method we offered, or refused the
/// credentials. `0xFF` is RFC 1928 §3's "no acceptable methods".
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Socks5HandshakeError {
    #[error("the SOCKS5 proxy accepted none of the authentication methods offered")]
    NoAcceptableMethods,
    #[error("the SOCKS5 proxy chose method {0:#04x}, which was not offered")]
    UnofferedMethod(u8),
    #[error("the SOCKS5 proxy rejected the username and password")]
    BadCredentials,
    #[error("the SOCKS5 proxy answered version {0} rather than 5")]
    BadVersion(u8),
    #[error("a SOCKS5 host name must be at most 255 bytes, this one is {0}")]
    HostTooLong(usize),
    #[error("a SOCKS5 username and password must each be at most 255 bytes")]
    CredentialTooLong,
}

/// SOCKS5, RFC 1928, with the username/password sub-negotiation of
/// RFC 1929.
///
/// The origin goes out as `ATYP=0x03 DOMAINNAME` — a name, never an
/// address — which is what `socks5h` names in other clients' URL schemes
/// and is why proxying is not a decorator over a seam that carries only a
/// `SocketAddr`: the DNS leak is a property of that seam.
#[derive(Debug, Clone, Default)]
pub struct Socks5 {
    auth: Option<(Box<str>, Box<str>)>,
    state: State,
    /// What the greeting offered, kept because a reply naming a method we
    /// never offered is a different failure from one refusing us all —
    /// and no state machine can tell them apart without remembering.
    offered: Vec<u8>,
    request: Bytes,
}

/// Where the exchange has got to. `Default` is the state a fresh
/// handshake is in, before [`Handshake::begin`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Fresh,
    /// §3's method-selection reply: two bytes.
    AwaitingMethod,
    /// RFC 1929's reply: two bytes, with a version of its own.
    AwaitingAuthReply,
    /// §6's reply: four bytes, then a variable address, then a port.
    AwaitingReply,
    Done,
}

impl Socks5 {
    pub fn new() -> Self {
        Self::default()
    }

    /// RFC 1929. Each of the two is length-prefixed with a single byte, so
    /// neither may exceed 255 bytes — refused here rather than truncated
    /// on the wire.
    pub fn password_auth(mut self, user: &str, password: &str) -> Result<Self, Error> {
        if user.len() > 255 || password.len() > 255 {
            return Err(handshake(Socks5HandshakeError::CredentialTooLong));
        }
        self.auth = Some((user.into(), password.into()));
        Ok(self)
    }
}

const SOCKS5_VERSION: u8 = 0x05;
const METHOD_NONE: u8 = 0x00;
const METHOD_PASSWORD: u8 = 0x02;
const METHOD_UNACCEPTABLE: u8 = 0xFF;

impl Handshake for Socks5 {
    /// Always. SOCKS5 is a byte tunnel with no idea that HTTP exists, so
    /// there is no absolute-form question to answer — the request is
    /// written exactly as it would be to the origin.
    fn approach(&self, _use_tls: bool) -> Approach {
        Approach::Tunnel
    }

    fn begin(&mut self, host: &str, port: u16) -> Result<Bytes, Error> {
        let host_bytes = host.as_bytes();
        if host_bytes.len() > 255 {
            return Err(handshake(Socks5HandshakeError::HostTooLong(
                host_bytes.len(),
            )));
        }

        // §4's CONNECT, built now and sent once the method is settled:
        // building it here is what makes `HostTooLong` a failure of
        // `begin` rather than one discovered three round trips in.
        let mut request = BytesMut::with_capacity(7 + host_bytes.len());
        request.put_slice(&[SOCKS5_VERSION, 0x01, 0x00, 0x03]);
        request.put_u8(host_bytes.len() as u8);
        request.put_slice(host_bytes);
        request.put_u16(port);
        self.request = request.freeze();

        // §3: greeting. The offer is exactly what we can perform, so a
        // proxy that picks anything else picked something we never made
        // available — a distinct error from "no acceptable methods",
        // because one is the proxy refusing us and the other is the proxy
        // being wrong.
        self.offered = if self.auth.is_some() {
            vec![METHOD_PASSWORD, METHOD_NONE]
        } else {
            vec![METHOD_NONE]
        };
        let mut greeting = BytesMut::with_capacity(2 + self.offered.len());
        greeting.put_u8(SOCKS5_VERSION);
        greeting.put_u8(self.offered.len() as u8);
        greeting.put_slice(&self.offered);

        self.state = State::AwaitingMethod;
        Ok(greeting.freeze())
    }

    fn advance(&mut self, from_peer: &mut BytesMut) -> Result<Step, Error> {
        match self.state {
            State::Fresh => Ok(Step::NeedMore),
            State::Done => Ok(Step::Done),

            State::AwaitingMethod => {
                let Some(chosen) = take(from_peer, 2) else {
                    return Ok(Step::NeedMore);
                };
                expect_version(chosen[0])?;
                match chosen[1] {
                    METHOD_UNACCEPTABLE => {
                        Err(handshake(Socks5HandshakeError::NoAcceptableMethods))
                    }
                    m if !self.offered.contains(&m) => {
                        Err(handshake(Socks5HandshakeError::UnofferedMethod(m)))
                    }
                    METHOD_PASSWORD => {
                        let (user, password) = self
                            .auth
                            .as_ref()
                            .expect("METHOD_PASSWORD is offered only when credentials exist");
                        // RFC 1929's own version byte is `0x01` and is NOT
                        // the SOCKS version — a sub-negotiation with a
                        // numbering of its own, which is why
                        // `expect_version` is not used on its reply.
                        let mut msg = BytesMut::with_capacity(3 + user.len() + password.len());
                        msg.put_u8(0x01);
                        msg.put_u8(user.len() as u8);
                        msg.put_slice(user.as_bytes());
                        msg.put_u8(password.len() as u8);
                        msg.put_slice(password.as_bytes());
                        self.state = State::AwaitingAuthReply;
                        Ok(Step::Write(msg.freeze()))
                    }
                    _ => {
                        self.state = State::AwaitingReply;
                        Ok(Step::Write(self.request.clone()))
                    }
                }
            }

            State::AwaitingAuthReply => {
                let Some(reply) = take(from_peer, 2) else {
                    return Ok(Step::NeedMore);
                };
                if reply[1] != 0x00 {
                    return Err(handshake(Socks5HandshakeError::BadCredentials));
                }
                self.state = State::AwaitingReply;
                Ok(Step::Write(self.request.clone()))
            }

            State::AwaitingReply => {
                // §6's reply is fixed for four bytes and then variable, so
                // its length cannot be known until `ATYP` has arrived —
                // which is why this looks at the buffer before consuming
                // any of it. Consuming the four and then asking for more
                // would work here and would be wrong in principle: a
                // partial frame must stay in the driver's buffer.
                if from_peer.len() < 4 {
                    return Ok(Step::NeedMore);
                }
                expect_version(from_peer[0])?;
                if from_peer[1] != 0x00 {
                    return Err(Error::new(
                        ErrorKind::Connect,
                        Socks5Refused { rep: from_peer[1] },
                    ));
                }
                // The bound address is read and discarded — it is the
                // proxy's outbound socket, not anything a caller of this
                // client can act on — but it must be *consumed*, or its
                // bytes would be mistaken for the origin's first ones.
                let addr_len = match from_peer[3] {
                    0x01 => 4,
                    0x04 => 16,
                    0x03 => {
                        if from_peer.len() < 5 {
                            return Ok(Step::NeedMore);
                        }
                        usize::from(from_peer[4]) + 1
                    }
                    other => {
                        return Err(Error::new(ErrorKind::Connect, Socks5Refused { rep: other }));
                    }
                };
                let total = 4 + addr_len + 2;
                if from_peer.len() < total {
                    return Ok(Step::NeedMore);
                }
                let _ = from_peer.split_to(total);
                self.state = State::Done;
                Ok(Step::Done)
            }
        }
    }
}

fn handshake(e: Socks5HandshakeError) -> Error {
    Error::new(ErrorKind::Connect, e)
}

fn expect_version(v: u8) -> Result<(), Error> {
    if v == SOCKS5_VERSION {
        Ok(())
    } else {
        Err(handshake(Socks5HandshakeError::BadVersion(v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive_for_test;

    /// `REP=0`, `ATYP=1`, `0.0.0.0:0` — the reply a proxy sends when the
    /// tunnel is open.
    const GRANTED: &[u8] = &[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0];

    /// A proxy that answers each of our writes in order.
    fn scripted(replies: Vec<Vec<u8>>) -> impl FnMut(&[u8]) -> Vec<u8> {
        let mut replies = replies.into_iter();
        move |_sent| replies.next().unwrap_or_default()
    }

    #[test]
    fn the_unauthenticated_exchange_is_two_writes_and_the_bytes_are_exact() {
        let mut h = Socks5::new();
        let (written, leftover) = drive_for_test(
            &mut h,
            "example.com",
            443,
            scripted(vec![vec![0x05, 0x00], GRANTED.to_vec()]),
        )
        .expect("granted");

        // §3: one method offered, and it is `NONE`.
        assert_eq!(written[0], vec![0x05, 0x01, 0x00]);
        // §4: CONNECT, by NAME — `ATYP=3`, the length, the host, the port.
        assert_eq!(
            written[1],
            [
                &[0x05, 0x01, 0x00, 0x03, 11][..],
                b"example.com",
                &443u16.to_be_bytes()[..],
            ]
            .concat()
        );
        assert!(leftover.is_empty());
    }

    #[test]
    fn the_origins_first_bytes_survive_the_handshake() {
        // The property the whole `Step::Done` contract exists for: a
        // proxy may send the reply and the origin's first bytes in one
        // flight, and a handshake that consumed them would lose the
        // peer's opening frames for good.
        let mut h = Socks5::new();
        let mut granted = GRANTED.to_vec();
        granted.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
        let (_, leftover) = drive_for_test(
            &mut h,
            "example.com",
            443,
            scripted(vec![vec![0x05, 0x00], granted]),
        )
        .expect("granted");

        assert_eq!(&leftover[..], b"HTTP/1.1 200 OK\r\n");
    }

    #[test]
    fn a_reply_arriving_one_byte_at_a_time_is_never_consumed_early() {
        // The contract a driver depends on: `NeedMore` must leave the
        // buffer untouched, or the fragment is lost. Asserted by feeding
        // the reply byte by byte and checking the buffer still holds
        // everything until the frame is complete.
        let mut h = Socks5::new();
        let mut buf = BytesMut::new();
        let _ = h.begin("example.com", 443).unwrap();

        for (i, b) in [0x05u8, 0x00].iter().enumerate() {
            buf.extend_from_slice(&[*b]);
            let step = h.advance(&mut buf).unwrap();
            if i == 0 {
                assert_eq!(step, Step::NeedMore);
                assert_eq!(buf.len(), 1, "a partial frame was consumed");
            } else {
                assert!(matches!(step, Step::Write(_)));
                assert!(buf.is_empty());
            }
        }
        // The same again for the variable-length §6 reply, whose length
        // is not known until `ATYP` and its length byte have arrived.
        let mut fed = 0;
        for b in GRANTED {
            buf.extend_from_slice(&[*b]);
            fed += 1;
            let step = h.advance(&mut buf).unwrap();
            if fed < GRANTED.len() {
                assert_eq!(step, Step::NeedMore, "at {fed} bytes");
                assert_eq!(buf.len(), fed, "a partial reply was consumed at {fed}");
            } else {
                assert_eq!(step, Step::Done);
                assert!(buf.is_empty());
            }
        }
    }

    #[test]
    fn a_domain_bound_address_is_consumed_by_its_own_length() {
        // `ATYP=3` puts a length byte where the fixed forms put address
        // bytes, so getting this wrong leaves the tail of the proxy's
        // reply in the buffer, to be read as the origin's first bytes.
        let mut h = Socks5::new();
        let granted = [
            &[0x05, 0x00, 0x00, 0x03, 5][..],
            b"proxy",
            &[0x1f, 0x90][..],
            b"origin says hello",
        ]
        .concat();
        let (_, leftover) = drive_for_test(
            &mut h,
            "example.com",
            443,
            scripted(vec![vec![0x05, 0x00], granted]),
        )
        .expect("granted");

        assert_eq!(&leftover[..], b"origin says hello");
    }

    #[test]
    fn the_password_sub_negotiation_goes_out_when_the_proxy_asks_for_it() {
        let mut h = Socks5::new().password_auth("alice", "hunter2").unwrap();
        let (written, _) = drive_for_test(
            &mut h,
            "example.com",
            443,
            scripted(vec![
                vec![0x05, METHOD_PASSWORD],
                vec![0x01, 0x00],
                GRANTED.to_vec(),
            ]),
        )
        .expect("granted");

        // The greeting offers both, strongest first.
        assert_eq!(written[0], vec![0x05, 0x02, METHOD_PASSWORD, METHOD_NONE]);
        // RFC 1929: its own version byte is 1, not 5.
        assert_eq!(
            written[1],
            [&[0x01, 5][..], b"alice", &[7][..], b"hunter2"].concat()
        );
        assert_eq!(written.len(), 3);
    }

    #[test]
    fn a_proxy_that_chooses_a_method_we_never_offered_is_a_distinct_failure() {
        // Not the same as refusing us all: one is the proxy saying no,
        // the other is the proxy being wrong, and a caller diagnosing a
        // broken deployment needs to know which.
        let mut h = Socks5::new();
        let err = drive_for_test(
            &mut h,
            "example.com",
            443,
            scripted(vec![vec![0x05, METHOD_PASSWORD]]),
        )
        .expect_err("unoffered");
        assert!(err.to_string().contains("was not offered"), "{err}");

        let mut h = Socks5::new();
        let err = drive_for_test(
            &mut h,
            "example.com",
            443,
            scripted(vec![vec![0x05, METHOD_UNACCEPTABLE]]),
        )
        .expect_err("no acceptable methods");
        assert!(err.to_string().contains("accepted none"), "{err}");
    }

    #[test]
    fn a_refusal_carries_the_rep_byte_by_name() {
        for (rep, text) in [(0x02u8, "not allowed by ruleset"), (0x05, "refused")] {
            let mut h = Socks5::new();
            let err = drive_for_test(
                &mut h,
                "example.com",
                443,
                scripted(vec![
                    vec![0x05, 0x00],
                    vec![0x05, rep, 0x00, 0x01, 0, 0, 0, 0, 0, 0],
                ]),
            )
            .expect_err("refused");
            assert!(err.to_string().contains(text), "{err}");
        }
    }

    #[test]
    fn a_reply_with_the_wrong_version_is_refused() {
        let mut h = Socks5::new();
        let err = drive_for_test(&mut h, "example.com", 443, scripted(vec![vec![0x04, 0x00]]))
            .expect_err("bad version");
        assert!(err.to_string().contains("version 4"), "{err}");
    }

    #[test]
    fn a_host_too_long_for_the_length_byte_fails_before_anything_is_sent() {
        let mut h = Socks5::new();
        let err = h.begin(&"a".repeat(256), 443).expect_err("too long");
        assert!(err.to_string().contains("at most 255 bytes"), "{err}");
    }

    #[test]
    fn a_credential_too_long_for_its_length_byte_is_refused_at_configuration() {
        assert!(Socks5::new().password_auth(&"a".repeat(256), "p").is_err());
        assert!(Socks5::new().password_auth("u", &"p".repeat(256)).is_err());
    }
}
