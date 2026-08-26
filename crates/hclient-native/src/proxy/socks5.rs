//! SOCKS5, RFC 1928.
//!
//! The answer unless a server forces otherwise, for reasons that belong
//! to the other protocol and are written in [`super::socks4`]: no IPv6,
//! because SOCKS4's address field is four bytes, and no authentication,
//! only a `USERID` a proxy may check against an identd.

use super::frames::{read_exact, write_all};
use super::{Approach, ProxyProtocol};
use bytes::Bytes;
use hclient_core::{Error, ErrorKind};
use hyper::rt::{Read, Write};

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

// --- SOCKS5 -------------------------------------------------------------

/// SOCKS5, RFC 1928, with the username/password sub-negotiation of
/// RFC 1929.
///
/// The origin goes out as `ATYP=0x03 DOMAINNAME` — a name, never an
/// address — which is what `socks5h` names in other clients' URL schemes
/// and is why this is not a wrapper over
/// [`TcpConnect`](hclient_rt::TcpConnect): the DNS leak is a property of a
/// seam that carries only a `SocketAddr`, not of proxying.
#[derive(Debug, Clone, Default)]
pub struct Socks5 {
    auth: Option<(Box<str>, Box<str>)>,
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
            return Err(Error::new(
                ErrorKind::Connect,
                Socks5HandshakeError::CredentialTooLong,
            ));
        }
        self.auth = Some((user.into(), password.into()));
        Ok(self)
    }
}

const SOCKS5_VERSION: u8 = 0x05;
const METHOD_NONE: u8 = 0x00;
const METHOD_PASSWORD: u8 = 0x02;
const METHOD_UNACCEPTABLE: u8 = 0xFF;

impl ProxyProtocol for Socks5 {
    /// Always. SOCKS5 is a byte tunnel with no idea that HTTP exists, so
    /// there is no absolute-form question to answer — the request is
    /// written exactly as it would be to the origin.
    fn approach(&self, _use_tls: bool) -> Approach {
        Approach::Tunnel
    }

    async fn tunnel<S>(&self, mut io: S, host: &str, port: u16) -> Result<(S, Bytes), Error>
    where
        S: Read + Write + Unpin + 'static,
    {
        let host_bytes = host.as_bytes();
        if host_bytes.len() > 255 {
            return Err(Error::new(
                ErrorKind::Connect,
                Socks5HandshakeError::HostTooLong(host_bytes.len()),
            ));
        }

        // §3: greeting. The offer is exactly what we can perform, so a
        // proxy that picks anything else picked something we never made
        // available — a distinct error from "no acceptable methods",
        // because one is the proxy refusing us and the other is the proxy
        // being wrong.
        let methods: &[u8] = if self.auth.is_some() {
            &[METHOD_PASSWORD, METHOD_NONE]
        } else {
            &[METHOD_NONE]
        };
        let mut greeting = Vec::with_capacity(2 + methods.len());
        greeting.push(SOCKS5_VERSION);
        greeting.push(methods.len() as u8);
        greeting.extend_from_slice(methods);
        write_all(&mut io, &greeting).await?;

        let mut chosen = [0u8; 2];
        read_exact(&mut io, &mut chosen).await?;
        expect_version(chosen[0])?;
        match chosen[1] {
            METHOD_UNACCEPTABLE => {
                return Err(handshake(Socks5HandshakeError::NoAcceptableMethods));
            }
            m if !methods.contains(&m) => {
                return Err(handshake(Socks5HandshakeError::UnofferedMethod(m)));
            }
            METHOD_PASSWORD => {
                let (user, password) = self
                    .auth
                    .as_ref()
                    .expect("METHOD_PASSWORD is offered only when credentials exist");
                // RFC 1929's own version byte is `0x01` and is NOT the
                // SOCKS version — a sub-negotiation with a numbering of
                // its own, which is why `expect_version` is not used on
                // the reply below.
                let mut msg = Vec::with_capacity(3 + user.len() + password.len());
                msg.push(0x01);
                msg.push(user.len() as u8);
                msg.extend_from_slice(user.as_bytes());
                msg.push(password.len() as u8);
                msg.extend_from_slice(password.as_bytes());
                write_all(&mut io, &msg).await?;

                let mut reply = [0u8; 2];
                read_exact(&mut io, &mut reply).await?;
                if reply[1] != 0x00 {
                    return Err(handshake(Socks5HandshakeError::BadCredentials));
                }
            }
            _ => {}
        }

        // §4: CONNECT, by name.
        let mut request = Vec::with_capacity(7 + host_bytes.len());
        request.extend_from_slice(&[SOCKS5_VERSION, 0x01, 0x00, 0x03]);
        request.push(host_bytes.len() as u8);
        request.extend_from_slice(host_bytes);
        request.extend_from_slice(&port.to_be_bytes());
        write_all(&mut io, &request).await?;

        // §6: reply. The bound address is read and discarded — it is the
        // proxy's outbound socket, not anything a caller of this client
        // can act on — but it must be *read*, or its bytes would be
        // mistaken for the origin's first response bytes.
        let mut head = [0u8; 4];
        read_exact(&mut io, &mut head).await?;
        expect_version(head[0])?;
        if head[1] != 0x00 {
            return Err(Error::new(
                ErrorKind::Connect,
                Socks5Refused { rep: head[1] },
            ));
        }
        let bound = match head[3] {
            0x01 => 4,
            0x04 => 16,
            0x03 => {
                let mut len = [0u8; 1];
                read_exact(&mut io, &mut len).await?;
                usize::from(len[0])
            }
            other => {
                return Err(Error::new(ErrorKind::Connect, Socks5Refused { rep: other }));
            }
        };
        let mut discard = vec![0u8; bound + 2];
        read_exact(&mut io, &mut discard).await?;

        // Nothing is read past the handshake, so there is nothing to hand
        // on — unlike `CONNECT`, where hyper may have taken the origin's
        // first bytes off the socket in the same flight as the `200`.
        Ok((io, Bytes::new()))
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
