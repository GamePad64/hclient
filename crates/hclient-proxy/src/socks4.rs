//! SOCKS4, and 4a — one type, because the wire says so.
//!
//! 4a is 4's own extension, signalled *inside* a SOCKS4 request by a
//! `DSTIP` of `0.0.0.x`: invalid as an address, therefore meaning *a
//! hostname follows the userid*. There is no version byte to tell them
//! apart and no handshake to negotiate in, so a second type would be a
//! choice nobody can make.
//!
//! [`Socks5`](crate::Socks5) is the answer unless a server forces
//! otherwise, for two reasons that are this protocol's own: **no IPv6**,
//! the address field being four bytes, and **no authentication**, only a
//! `USERID` the proxy may check against an identd.

use bytes::{BufMut, Bytes, BytesMut};
use hclient_core::{Error, ErrorKind};

use crate::error::{Socks4HandshakeError, Socks4Refused};
use crate::{Approach, Handshake, Step, take};

/// SOCKS4 grants with `CD = 90`. Not `0`, unlike SOCKS5's `REP`.
const SOCKS4_GRANTED: u8 = 90;

/// SOCKS4a: `CONNECT` by name, through a protocol that predates IPv6.
#[derive(Debug, Clone, Default)]
pub struct Socks4 {
    userid: Box<str>,
    awaiting_reply: bool,
}

impl Socks4 {
    pub fn new() -> Self {
        Self::default()
    }

    /// The `USERID` field, which a proxy may check against an identd.
    ///
    /// **Deliberately not marked sensitive anywhere**, unlike a password:
    /// the protocol gives it no secrecy — it travels in the clear and is
    /// checked, if at all, against a service on the client's own host —
    /// and marking it would claim a property it does not have.
    pub fn userid(mut self, userid: impl Into<Box<str>>) -> Result<Self, Socks4HandshakeError> {
        let userid = userid.into();
        if userid.as_bytes().contains(&0) {
            return Err(Socks4HandshakeError::NulInUserid);
        }
        self.userid = userid;
        Ok(self)
    }
}

impl Handshake for Socks4 {
    /// Always, for [`Socks5`](crate::Socks5)'s reason: a byte tunnel has
    /// no idea that HTTP exists.
    fn approach(&self, _use_tls: bool) -> Approach {
        Approach::Tunnel
    }

    fn begin(&mut self, host: &str, port: u16) -> Result<Bytes, Error> {
        let host_bytes = host.as_bytes();
        if host_bytes.contains(&0) {
            return Err(socks4(Socks4HandshakeError::NulInHost));
        }
        // The field has no length prefix, so nothing on the wire would
        // report this — the bound is ours, and generous: a DNS name is at
        // most 253 bytes and this refuses at 255 for the same reason
        // `Socks5` does.
        if host_bytes.len() > 255 {
            return Err(socks4(Socks4HandshakeError::HostTooLong(host_bytes.len())));
        }

        // VN=4, CD=1 (CONNECT), DSTPORT, DSTIP, USERID, NUL, host, NUL.
        //
        // `DSTIP = 0.0.0.1` is SOCKS4a's signal — an address that cannot
        // be a real one, so a 4a proxy reads the hostname appended after
        // the userid, and a plain SOCKS4 proxy fails rather than dialling
        // something wrong.
        let mut req = BytesMut::with_capacity(10 + self.userid.len() + host_bytes.len());
        req.put_slice(&[0x04, 0x01]);
        req.put_u16(port);
        req.put_slice(&[0, 0, 0, 1]);
        req.put_slice(self.userid.as_bytes());
        req.put_u8(0);
        req.put_slice(host_bytes);
        req.put_u8(0);

        self.awaiting_reply = true;
        Ok(req.freeze())
    }

    fn advance(&mut self, from_peer: &mut BytesMut) -> Result<Step, Error> {
        if !self.awaiting_reply {
            return Ok(Step::Done);
        }
        // The reply is exactly eight bytes and never more: VN, CD,
        // DSTPORT, DSTIP. There is no variable-length tail, which is the
        // one place this protocol is simpler than its successor.
        let Some(reply) = take(from_peer, 8) else {
            return Ok(Step::NeedMore);
        };
        if reply[0] != 0x00 {
            return Err(socks4(Socks4HandshakeError::BadReplyVersion(reply[0])));
        }
        if reply[1] != SOCKS4_GRANTED {
            return Err(Error::new(
                ErrorKind::Connect,
                Socks4Refused { cd: reply[1] },
            ));
        }
        self.awaiting_reply = false;
        Ok(Step::Done)
    }
}

fn socks4(e: Socks4HandshakeError) -> Error {
    Error::new(ErrorKind::Connect, e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive_for_test;

    const GRANTED: &[u8] = &[0x00, 90, 0, 0, 0, 0, 0, 0];

    fn scripted(replies: Vec<Vec<u8>>) -> impl FnMut(&[u8]) -> Vec<u8> {
        let mut replies = replies.into_iter();
        move |_sent| replies.next().unwrap_or_default()
    }

    #[test]
    fn the_request_is_socks4a_shaped_and_carries_the_host_by_name() {
        let mut h = Socks4::new();
        let (written, leftover) =
            drive_for_test(&mut h, "example.com", 443, scripted(vec![GRANTED.to_vec()]))
                .expect("granted");

        assert_eq!(
            written[0],
            [
                &[0x04, 0x01][..],
                &443u16.to_be_bytes()[..],
                // The 4a signal: an address that cannot be real.
                &[0, 0, 0, 1][..],
                // An empty USERID, then its NUL.
                &[0][..],
                b"example.com",
                &[0][..],
            ]
            .concat()
        );
        assert_eq!(written.len(), 1);
        assert!(leftover.is_empty());
    }

    #[test]
    fn a_userid_goes_between_the_address_and_the_host() {
        let mut h = Socks4::new().userid("alice").unwrap();
        let (written, _) =
            drive_for_test(&mut h, "example.com", 80, scripted(vec![GRANTED.to_vec()]))
                .expect("granted");
        assert!(
            written[0].windows(7).any(|w| w == b"alice\0e"),
            "{:?}",
            written[0]
        );
    }

    #[test]
    fn the_reply_version_is_zero_and_the_grant_is_ninety() {
        // Both are the details every implementation gets wrong once, so
        // both are pinned: a reply of `4` is refused, and a `CD` of `0`
        // is a refusal rather than a grant.
        let mut h = Socks4::new();
        let err = drive_for_test(
            &mut h,
            "example.com",
            443,
            scripted(vec![vec![0x04, 90, 0, 0, 0, 0, 0, 0]]),
        )
        .expect_err("VN must be 0");
        assert!(err.to_string().contains("VN=0x04"), "{err}");

        let mut h = Socks4::new();
        let err = drive_for_test(
            &mut h,
            "example.com",
            443,
            scripted(vec![vec![0x00, 0, 0, 0, 0, 0, 0, 0]]),
        )
        .expect_err("CD 0 is not a grant");
        assert!(err.to_string().contains("CD=0"), "{err}");
    }

    #[test]
    fn a_refusal_names_its_cd_value() {
        for (cd, text) in [(91u8, "rejected or failed"), (92, "identd unreachable")] {
            let mut h = Socks4::new();
            let err = drive_for_test(
                &mut h,
                "example.com",
                443,
                scripted(vec![vec![0x00, cd, 0, 0, 0, 0, 0, 0]]),
            )
            .expect_err("refused");
            assert!(err.to_string().contains(text), "{err}");
        }
    }

    #[test]
    fn the_origins_first_bytes_survive_the_handshake() {
        let mut h = Socks4::new();
        let mut granted = GRANTED.to_vec();
        granted.extend_from_slice(b"origin says hello");
        let (_, leftover) =
            drive_for_test(&mut h, "example.com", 443, scripted(vec![granted])).expect("granted");
        assert_eq!(&leftover[..], b"origin says hello");
    }

    #[test]
    fn a_partial_reply_is_left_in_the_buffer() {
        let mut h = Socks4::new();
        let mut buf = BytesMut::new();
        let _ = h.begin("example.com", 443).unwrap();
        buf.extend_from_slice(&GRANTED[..7]);
        assert_eq!(h.advance(&mut buf).unwrap(), Step::NeedMore);
        assert_eq!(buf.len(), 7, "a partial reply was consumed");
        buf.extend_from_slice(&GRANTED[7..]);
        assert_eq!(h.advance(&mut buf).unwrap(), Step::Done);
    }

    #[test]
    fn a_nul_in_either_field_is_refused_rather_than_truncated() {
        assert!(Socks4::new().userid("a\0b").is_err());
        let mut h = Socks4::new();
        assert!(h.begin("exa\0mple.com", 443).is_err());
    }
}
