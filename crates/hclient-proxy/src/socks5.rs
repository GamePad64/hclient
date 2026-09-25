//! SOCKS5, RFC 1928, with RFC 1929's username/password sub-negotiation.
//!
//! The answer unless a server forces otherwise, for reasons that belong
//! to the other protocol and are written in [`crate::socks4`]: no IPv6,
//! because SOCKS4's address field is four bytes, and no authentication,
//! only a `USERID` a proxy may check against an identd.

use bytes::{BufMut, Bytes, BytesMut};
use hclient_core::error::{Error, ErrorKind};

use crate::error::{Socks5HandshakeError, Socks5Refused};
use crate::{Approach, Handshake, Step, take};

/// SOCKS5, RFC 1928, with the username/password sub-negotiation of
/// RFC 1929.
///
/// The origin goes out as `ATYP=0x03 DOMAINNAME` — a name, never an
/// address — which is what `socks5h` names in other clients' URL schemes
/// and is why proxying is not a decorator over a seam that carries only a
/// `SocketAddr`: the DNS leak is a property of that seam.
///
/// ```
/// use hclient_proxy::{Proxy, Socks5};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let socks = Socks5::new().password_auth("alice", "hunter2")?;
/// let proxy = Proxy::new(socks, "socks.corp", 1080);
/// # let _ = proxy;
/// # Ok(())
/// # }
/// ```
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
    /// A handshake with no credentials configured.
    pub fn new() -> Self {
        Self::default()
    }

    /// RFC 1929. Each of the two is length-prefixed with a single byte, so
    /// neither may exceed 255 bytes — refused here rather than truncated
    /// on the wire.
    ///
    /// # Errors
    ///
    /// Returns [`Socks5HandshakeError::CredentialTooLong`] when `user` or
    /// `password` is longer than 255 bytes.
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
        // Bounded: `host_bytes.len() > 255` is refused as `HostTooLong`
        // a dozen lines above, so this cast is exact.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Bounded: `host_bytes.len() > 255` is refused as `HostTooLong` a dozen lines above, so this cast is exact."
        )]
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
        // Bounded by construction: `offered` is built one line above and
        // holds at most two methods.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Bounded by construction: `offered` is built one line above and holds at most two methods."
        )]
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
                // Which method the proxy picked out of what was offered
                // is the branch point of the whole handshake, and the
                // three outcomes below are indistinguishable from the
                // socket: one sends credentials, one sends the request
                // straight away, and two are refusals.
                tracing::trace!(
                    "proxy: socks5 offered {:?}, peer chose method {:#04x}",
                    self.offered,
                    chosen[1],
                );
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
                        // Both bounded: `password_auth` refuses either
                        // over 255 bytes as `CredentialTooLong`, at the
                        // setter rather than here, so RFC 1929's
                        // one-octet lengths are exact.
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "Both bounded: `password_auth` refuses either over 255 bytes as `CredentialTooLong`, at the setter rather than here, so RFC 1929's one-octet lengths are exact."
                        )]
                        msg.put_u8(user.len() as u8);
                        msg.put_slice(user.as_bytes());
                        // Bounded at the setter too — see the pair above.
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "Bounded at the setter too — see the pair above."
                        )]
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
                let atyp = from_peer[3];
                let addr_len = match atyp {
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
                // The bound address is read and discarded, so its length
                // is the only evidence that the reply was framed the way
                // `ATYP` said — which is what decides where the origin's
                // first byte begins.
                tracing::trace!(
                    "proxy: socks5 granted, atyp {:#04x}, {} reply bytes consumed",
                    atyp,
                    total,
                );
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
    fn a_domain_bound_reply_arriving_one_byte_at_a_time_asks_for_more_at_every_prefix() {
        // The test above feeds an `ATYP=1` reply, whose length is known
        // the moment the fixed four bytes are in hand. `ATYP=3` is the
        // shape that needs a **fifth** byte before its length can be
        // computed at all, and it is the only path through the
        // `from_peer.len() < 5` check — so without this, nothing asked
        // that check anything at a length where it decides.
        //
        // Measured: `< 5` weakened to `== 5` left the whole suite at
        // 134 passing, and it is the shape that would consume a partial
        // frame — at four bytes it stops asking for more and reads
        // `from_peer[4]`, which has not arrived.
        //
        // **`<= 5` is equivalent rather than unkilled**, which is why
        // nothing here chases it: at exactly five bytes the original
        // reads the length byte, computes `total = 4 + (len + 1) + 2`,
        // which is at least seven, and answers `NeedMore` anyway.
        // Checked exhaustively over every (buffer length, length byte)
        // pair from 4 to 40 and 0 to 255 — 0 differing pairs.
        let reply = [
            &[0x05, 0x00, 0x00, 0x03, 5][..],
            b"proxy",
            &[0x1f, 0x90][..],
        ]
        .concat();

        let mut h = Socks5::new();
        let mut buf = BytesMut::new();
        let _ = h.begin("example.com", 443).unwrap();
        buf.extend_from_slice(&[0x05, METHOD_NONE]);
        assert!(matches!(h.advance(&mut buf).unwrap(), Step::Write(_)));

        for fed in 1..reply.len() {
            buf.clear();
            buf.extend_from_slice(&reply[..fed]);
            assert_eq!(
                h.advance(&mut buf).unwrap(),
                Step::NeedMore,
                "answered something other than NeedMore at {fed} of {} bytes",
                reply.len()
            );
            assert_eq!(buf.len(), fed, "a partial reply was consumed at {fed}");
        }
        buf.clear();
        buf.extend_from_slice(&reply);
        assert_eq!(h.advance(&mut buf).unwrap(), Step::Done);
        assert!(buf.is_empty());
    }

    #[test]
    fn four_bytes_are_enough_to_read_a_refusal_and_never_enough_to_read_a_grant() {
        // **The `from_peer.len() < 4` boundary, from the only side that
        // can see it.** An `ATYP=3` grant answers `NeedMore` at four
        // bytes whatever that comparison says, because the length byte
        // it needs is the fifth — so the byte-at-a-time test above
        // cannot tell `< 4` from `<= 4`, and measured, it does not:
        // with `<= 4` the suite stayed green.
        //
        // A **refusal** is the case decided at exactly four, because
        // `REP` is the second octet and nothing after it is read. So
        // this is the pair: four bytes of a refusal are an error, and
        // four bytes of a grant are still a question.
        let head = |h: &mut Socks5| {
            let mut buf = BytesMut::from(&[0x05u8, METHOD_NONE][..]);
            assert!(matches!(h.advance(&mut buf).unwrap(), Step::Write(_)));
        };

        let mut h = Socks5::new();
        let _ = h.begin("example.com", 443).unwrap();
        head(&mut h);
        // `REP = 0x02`, and the reply is cut off after `ATYP` — a
        // conforming proxy sends more, and this client must not need it
        // to learn that it was refused.
        let mut buf = BytesMut::from(&[0x05u8, 0x02, 0x00, 0x01][..]);
        let err = h
            .advance(&mut buf)
            .expect_err("a refusal is readable at four bytes");
        assert!(err.to_string().contains("not allowed by ruleset"), "{err}");

        // The control, at the same length: a grant is not.
        let mut h = Socks5::new();
        let _ = h.begin("example.com", 443).unwrap();
        head(&mut h);
        let mut buf = BytesMut::from(&[0x05u8, 0x00, 0x00, 0x01][..]);
        assert_eq!(h.advance(&mut buf).unwrap(), Step::NeedMore);
        assert_eq!(buf.len(), 4, "a partial grant was consumed");
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
    fn every_rep_the_rfc_defines_is_rendered_by_its_own_name() {
        // RFC 1928 §6 names eight values, and a caller reading the error
        // is the only consumer of any of them. Two were probed — `0x02`
        // and `0x05` — so deleting any of the other six arms left the
        // whole suite green, and the failure that would cause is a
        // diagnostic that reports *unassigned* for a code the protocol
        // does define: a deployment debugging a proxy is then told the
        // one thing that is not true.
        //
        // The wrong answer is a real string rather than a panic, which
        // is why a table is the shape here: each row is checked against
        // its own name, so a deleted arm fails exactly its own row.
        for (rep, text) in [
            (0x01u8, "general failure"),
            (0x02, "connection not allowed by ruleset"),
            (0x03, "network unreachable"),
            (0x04, "host unreachable"),
            (0x05, "connection refused"),
            (0x06, "TTL expired"),
            (0x07, "command not supported"),
            (0x08, "address type not supported"),
        ] {
            let rendered = Socks5Refused { rep }.to_string();
            assert!(rendered.contains(text), "REP={rep:#04x}: {rendered}");
        }
        // The control: a code the RFC does not define says so, rather
        // than borrowing the name of one that is next to it.
        let rendered = Socks5Refused { rep: 0x09 }.to_string();
        assert!(rendered.contains("unassigned"), "{rendered}");
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

        // **255 is the longest a host may be, not the first refused**,
        // and the bound is only a bound if both sides of it are pinned:
        // the test above passes for a refusal at 255 as well, and
        // weakening `len() > 255` to `>= 255` kept the whole suite at
        // 134 passing. A single length byte carries exactly 255, so
        // refusing one would refuse a host the wire can state.
        let mut h = Socks5::new();
        let host = "a".repeat(255);
        let greeting = h
            .begin(&host, 443)
            .expect("255 bytes is what the length byte holds");
        assert_eq!(&greeting[..], &[0x05, 0x01, 0x00]);
        // And the length byte really says 255 rather than having wrapped.
        let mut buf = BytesMut::from(&[0x05u8, METHOD_NONE][..]);
        let Step::Write(request) = h.advance(&mut buf).unwrap() else {
            panic!("the method reply is answered with the CONNECT request")
        };
        assert_eq!(request[3..5], [0x03, 255]);
        assert_eq!(&request[5..5 + 255], host.as_bytes());
    }

    #[test]
    fn a_credential_too_long_for_its_length_byte_is_refused_at_configuration() {
        assert!(Socks5::new().password_auth(&"a".repeat(256), "p").is_err());
        assert!(Socks5::new().password_auth("u", &"p".repeat(256)).is_err());

        // **255 is what RFC 1929's one-octet length carries**, so it is
        // the longest accepted rather than the first refused — and each
        // side needs its own row, because the two are separate
        // comparisons: weakening either `> 255` to `>= 255` left the
        // suite at 134 passing, and each would refuse a credential the
        // wire can state while the other went on accepting one.
        let long = "a".repeat(255);
        assert!(Socks5::new().password_auth(&long, "p").is_ok());
        assert!(Socks5::new().password_auth("u", &long).is_ok());

        // And the length bytes on the wire really say 255 rather than
        // having wrapped — the refusal is at the setter precisely so
        // this cast cannot truncate.
        let mut h = Socks5::new().password_auth(&long, &long).unwrap();
        let _ = h.begin("example.com", 443).unwrap();
        let mut buf = BytesMut::from(&[0x05u8, METHOD_PASSWORD][..]);
        let Step::Write(msg) = h.advance(&mut buf).unwrap() else {
            panic!("a password method is answered with the sub-negotiation")
        };
        assert_eq!(msg[0], 0x01, "RFC 1929's own version byte");
        assert_eq!(msg[1], 255);
        assert_eq!(msg[2 + 255], 255);
        assert_eq!(msg.len(), 3 + 255 + 255);
    }

    /// A collector that counts events at `TRACE`.
    ///
    /// `tracing` alone installs no subscriber, so without one every
    /// `trace!` is a no-op and a green suite says nothing about whether
    /// the lines exist at all. The assertion is a **count** rather than a
    /// rendering, for `hclient-tls-rustls`'s reason: the text is a
    /// diagnostic and may be reworded, where *this path emits* is the
    /// property worth pinning.
    struct Counting(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl tracing::subscriber::Subscriber for Counting {
        fn enabled(&self, m: &tracing::Metadata<'_>) -> bool {
            *m.level() == tracing::Level::TRACE
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::Id {
            tracing::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::Id, _: &tracing::Id) {}
        fn event(&self, _: &tracing::Event<'_>) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn enter(&self, _: &tracing::Id) {}
        fn exit(&self, _: &tracing::Id) {}
    }

    /// **The handshake itself is driven, not the macro.**
    ///
    /// `hclient-tls-rustls`'s own trace test writes the `trace!` out a
    /// second time inside the subscriber, which pins that the macro
    /// reaches a collector and says nothing about the site. Here the
    /// protocol is sans-io, so a real exchange is a function call: this
    /// drives one to `Step::Done` and counts, which is what makes the
    /// count evidence about `advance` rather than about `tracing`.
    ///
    /// Two lines, and they are the two decision points: which method the
    /// peer chose out of what was offered, and the `ATYP` framing of the
    /// grant. A bound rather than an equality would pass for a machine
    /// that emitted neither and something else twice.
    #[test]
    fn a_completed_handshake_emits_a_line_for_the_method_and_for_the_grant() {
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sub = Counting(std::sync::Arc::clone(&seen));
        tracing::subscriber::with_default(sub, || {
            let mut h = Socks5::new();
            drive_for_test(
                &mut h,
                "example.com",
                443,
                scripted(vec![vec![0x05, METHOD_NONE], GRANTED.to_vec()]),
            )
            .expect("granted");
        });
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a handshake that reaches `Step::Done` passes both trace sites exactly once"
        );
    }
}
