//! SOCKS4 and SOCKS4a, which are one type on purpose.
//!
//! The wire decides that, not this crate: 4a is 4's own extension,
//! signalled inside a SOCKS4 request by a `DSTIP` of `0.0.0.x` — invalid
//! as an address, and therefore meaning *a hostname follows the userid*.
//! There is no version byte to tell them apart and no handshake to
//! negotiate in, so a second type would be a choice nobody can make.

use super::frames::{read_exact, write_all};
use super::{Approach, ProxyProtocol};
use bytes::Bytes;
use hclient_core::{Error, ErrorKind};
use hyper::rt::{Read, Write};

/// SOCKS4 and SOCKS4a, the protocol RFC 1928 replaced.
///
/// # Why it is here at all, and why `Socks5` is the answer unless a server
/// forces otherwise
///
/// It is thirty years old and it shows: **no IPv6** — the address field is
/// four bytes and there is nowhere to put a v6 one — and **no
/// authentication**, only a `USERID` string the proxy may or may not check
/// against an identd. Anything a caller can choose freely should be
/// [`Socks5`](super::Socks5). What this exists for is a server that offers nothing else,
/// which is the same reason `hclient-tls-native-tls` exists one seam over:
/// a fact about a deployment rather than a preference.
///
/// # SOCKS4 and SOCKS4a are one type, and the choice is made per request
///
/// SOCKS4a is SOCKS4's own extension, signalled inside a SOCKS4 request:
/// an address of `0.0.0.x` with `x` non-zero is invalid as an address and
/// therefore means *a hostname follows the userid*. There is no version
/// byte to distinguish them and no handshake in which to negotiate, so a
/// separate type would be a choice the wire does not offer.
///
/// This connector always sends the hostname form, because that is what
/// [`ProxyProtocol::tunnel`] is given: the host is not resolved locally,
/// which is the whole point of proxying by name, and the leak that keeps
/// a proxy from being a `TcpConnect` decorator. A caller pointed at a
/// SOCKS4 server with no
/// 4a support gets that server's refusal, which is the honest failure.
#[derive(Debug, Clone, Default)]
pub struct Socks4 {
    userid: Box<str>,
}

impl Socks4 {
    /// An empty `USERID`, which is what a proxy that does not check one
    /// expects.
    pub fn new() -> Self {
        Self::default()
    }

    /// The `USERID` field.
    ///
    /// **Not a credential**, whatever it looks like: the protocol has no
    /// password and the proxy's only means of checking it is an identd
    /// query back to the client host. It is not marked sensitive for that
    /// reason — marking it would claim a secrecy the protocol does not
    /// have.
    ///
    /// A `NUL` is refused rather than escaped: it terminates the field, so
    /// a userid containing one would be silently truncated and the bytes
    /// after it read as a hostname.
    pub fn userid(mut self, userid: impl Into<Box<str>>) -> Result<Self, Socks4HandshakeError> {
        let userid = userid.into();
        if userid.as_bytes().contains(&0) {
            return Err(Socks4HandshakeError::NulInUserid);
        }
        self.userid = userid;
        Ok(self)
    }
}

/// The proxy refused, or answered something SOCKS4 does not define.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum Socks4HandshakeError {
    /// A `USERID` containing a `NUL`, which is the field's terminator.
    #[error("the SOCKS4 USERID contains a NUL, which terminates the field")]
    NulInUserid,
    /// The reply's first byte is not `0`. SOCKS4's reply carries a version
    /// of **zero**, not four — a detail every implementation gets wrong
    /// once.
    #[error("the SOCKS4 proxy replied with VN={0:#04x}, where the protocol specifies 0x00")]
    BadReplyVersion(u8),
    /// A hostname too long for the field, which has no length prefix and
    /// is `NUL`-terminated — so this is a bound on the whole request
    /// rather than on a byte.
    #[error("the host name is {0} bytes, past what a SOCKS4a request can carry")]
    HostTooLong(usize),
    /// A `NUL` in the hostname, for `NulInUserid`'s reason.
    #[error("the host name contains a NUL, which terminates the field")]
    NulInHost,
}

/// SOCKS4's `CD`, which is one byte and has no HTTP meaning at all.
#[derive(Debug, thiserror::Error)]
#[error("the SOCKS4 proxy refused with CD={cd} ({})", socks4_reply(*cd))]
#[non_exhaustive]
pub struct Socks4Refused {
    pub cd: u8,
}

/// The four `CD` values the protocol defines, by name.
fn socks4_reply(cd: u8) -> &'static str {
    match cd {
        90 => "request granted",
        91 => "request rejected or failed",
        92 => "rejected: identd unreachable from the proxy",
        93 => "rejected: identd reported a different user",
        _ => "unassigned",
    }
}

/// SOCKS4 grants with `CD = 90`. Not `0`, unlike SOCKS5's `REP`.
const SOCKS4_GRANTED: u8 = 90;

impl ProxyProtocol for Socks4 {
    /// Always, for [`Socks5`](super::Socks5)'s reason: a byte tunnel has no idea that
    /// HTTP exists.
    fn approach(&self, _use_tls: bool) -> Approach {
        Approach::Tunnel
    }

    async fn tunnel<S>(&self, mut io: S, host: &str, port: u16) -> Result<(S, Bytes), Error>
    where
        S: Read + Write + Unpin + 'static,
    {
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

        // The request: VN=4, CD=1 (CONNECT), DSTPORT, DSTIP, USERID, NUL.
        //
        // `DSTIP = 0.0.0.1` is SOCKS4a's signal — an address that cannot
        // be a real one, so a 4a proxy reads the hostname appended after
        // the userid and a plain SOCKS4 proxy fails rather than dialling
        // something wrong.
        let mut req = Vec::with_capacity(10 + self.userid.len() + host_bytes.len());
        req.extend_from_slice(&[0x04, 0x01]);
        req.extend_from_slice(&port.to_be_bytes());
        req.extend_from_slice(&[0, 0, 0, 1]);
        req.extend_from_slice(self.userid.as_bytes());
        req.push(0);
        req.extend_from_slice(host_bytes);
        req.push(0);
        write_all(&mut io, &req).await?;

        // The reply is exactly eight bytes and never more: VN, CD,
        // DSTPORT, DSTIP. There is no variable-length tail, which is the
        // one place this protocol is simpler than its successor.
        let mut reply = [0u8; 8];
        read_exact(&mut io, &mut reply).await?;
        if reply[0] != 0x00 {
            return Err(socks4(Socks4HandshakeError::BadReplyVersion(reply[0])));
        }
        if reply[1] != SOCKS4_GRANTED {
            return Err(Error::new(
                ErrorKind::Connect,
                Socks4Refused { cd: reply[1] },
            ));
        }
        // Nothing may follow, for `ProxySpokeFirst`'s reason one type up:
        // the origin has not been written to, so any bytes here are the
        // proxy's and feeding them on would hand them to TLS or to hyper
        // as if the origin had sent them. SOCKS4's fixed-size reply makes
        // this a simple statement rather than a length calculation.
        Ok((io, Bytes::new()))
    }
}

fn socks4(e: Socks4HandshakeError) -> Error {
    Error::new(ErrorKind::Connect, e)
}
