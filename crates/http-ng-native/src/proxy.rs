//! Proxy support: the seam, and the two protocols that prove it.
//!
//! Behind the `proxy` feature, off by default. Neither protocol here needs
//! a third-party crate, so the argument that moved the WebSocket framing
//! into a crate of its own — a feature is additive, and `tungstenite`
//! would land in every build in any graph that switched it on — has no
//! subject; what the feature does buy is that a constrained-target build
//! is byte for byte what it was. `docs/proxy-design.md` has the rest.
//!
//! # Why this is not a `TcpConnect` wrapper, which would have cost nothing
//!
//! [`TcpConnect::connect`](http_ng_rt::TcpConnect::connect) takes a
//! `SocketAddr` and nothing else, so a wrapper implementing it could never
//! hand the proxy the origin's **name** — the client would resolve it
//! locally and leak exactly the DNS a proxy user is often there to hide,
//! and `http://` could never take absolute-form, because that is decided
//! where the request head is written. A proxy replaces the resolve →
//! Happy-Eyeballs → connect block; it does not decorate the socket.
//!
//! # Why a type parameter rather than `Box<dyn ProxyProtocol>`
//!
//! Erasing the protocol means erasing the IO with it, and a
//! `Box<dyn Read + Write>` needs `Send` to be useful — which is the
//! objection that disqualified `hyper::upgrade::Upgraded` for the
//! WebSocket work (`docs/w4-upgrade-seam.md`) and `hyper/http2` before
//! that: *"single-threaded runtimes shut out"*, the thing this crate
//! exists to avoid. So the protocol is a parameter, defaulted to
//! [`NoProxy`], and `.proxy(..)` changes it the way `.hooks(..)` changes
//! `H`.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::Poll;

use bytes::Bytes;
use http_ng_core::{Error, ErrorKind};
use hyper::rt::{Read, Write};

/// What a proxy does for one origin, which is not the same question for
/// the two protocols here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approach {
    /// The proxy carries bytes. The request is written exactly as it would
    /// be to the origin, because as far as the request is concerned it is.
    Tunnel,
    /// The proxy is an HTTP origin server for this request: the request
    /// line takes absolute-form (`GET http://example.com/x HTTP/1.1`).
    /// Only an HTTP proxy answers this, and only for `http://`.
    Absolute,
}

/// A proxy protocol: how to turn a connection **to the proxy** into a
/// connection **to the origin**.
///
/// Two implementations ship here and they share no bytes — one is HTTP,
/// one is RFC 1928 — which is the evidence that this shape is general
/// rather than the shape of its first caller. A seam with one implementer
/// is a claim; `Transport` earned its shape by a live consumer ported onto
/// it, and `WebSocketConnect` by the browser fitting it unchanged.
pub trait ProxyProtocol {
    /// Asked once per connection, before anything is dialled.
    fn approach(&self, use_tls: bool) -> Approach;

    /// Establish the tunnel over `io`, which is already connected to the
    /// proxy. Called only where [`approach`](ProxyProtocol::approach)
    /// answered [`Approach::Tunnel`].
    ///
    /// Returns the stream and **whatever was read past the end of the
    /// tunnel handshake** — `Upgrading::finish`'s `read_buf`, and the same
    /// rule applies: a caller that drops it loses the peer's first bytes
    /// for good.
    fn tunnel<S>(
        &self,
        io: S,
        host: &str,
        port: u16,
    ) -> impl Future<Output = Result<(S, Bytes), Error>>
    where
        S: Read + Write + Unpin + 'static;

    /// `Proxy-Authorization` for a request written in absolute-form, if
    /// this proxy wants one.
    ///
    /// Defaulted to `None`, and SOCKS5 leaves it there — it never answers
    /// [`Approach::Absolute`], and its credentials are a sub-negotiation
    /// on the socket rather than a header. A tunnelled request carries no
    /// such header either: it is addressed to the origin, and
    /// `Proxy-Authorization` belongs to the hop, which is why
    /// `http-ng-proto`'s redirect logic already strips it across origins.
    fn proxy_authorization(&self) -> Option<&http::HeaderValue> {
        None
    }
}

/// No proxy — and it is an **empty enum**, so `Proxy<NoProxy>` cannot be
/// constructed and the `Option` holding one is `None` by construction
/// rather than by discipline. A unit struct with `unreachable!()` bodies
/// would be a value that exists only to be absent, which is the shape
/// this workspace deleted `UpgradeSupport`'s spare variants for.
#[derive(Debug, Clone, Copy)]
pub enum NoProxy {}

impl ProxyProtocol for NoProxy {
    fn approach(&self, _: bool) -> Approach {
        match *self {}
    }

    async fn tunnel<S>(&self, _: S, _: &str, _: u16) -> Result<(S, Bytes), Error>
    where
        S: Read + Write + Unpin + 'static,
    {
        match *self {}
    }
}

/// Where a proxy lives, and which protocol it speaks.
#[derive(Debug, Clone)]
pub struct Proxy<P> {
    protocol: P,
    host: Box<str>,
    port: u16,
}

impl<P> Proxy<P> {
    pub fn new(protocol: P, host: impl Into<Box<str>>, port: u16) -> Self {
        Self {
            protocol,
            host: host.into(),
            port,
        }
    }

    pub fn protocol(&self) -> &P {
        &self.protocol
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The pool-key component. Two proxies to one origin are two
    /// connections, and a tunnel reused through a *different* proxy would
    /// be a security defect rather than a redundancy — the same argument
    /// `PoolKey`'s TLS-identity field is already kept for.
    pub(crate) fn key(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

// --- errors -------------------------------------------------------------

/// How the *request* is written, which is the one thing a proxy changes
/// above the socket.
///
/// A **tunnelled** proxy is [`Via::Direct`] here, and that is the honest
/// answer rather than a shortcut: once the tunnel is up the request is
/// addressed to the origin and nothing about it differs. Only an HTTP
/// proxy serving `http://` is the other arm, and then the proxy is an
/// origin server for this request — RFC 9112 §3.2.2's absolute-form.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Via<'a> {
    Direct,
    AbsoluteForm(Option<&'a http::HeaderValue>),
}

/// The proxy sent bytes past the end of its own handshake.
///
/// Not feature-gated, unlike the two below: it is the **seam's** rule
/// rather than either protocol's. Nothing the origin might say can have
/// arrived yet — the client has not written to it — so these bytes are the
/// proxy's, and carrying them on would feed them to the TLS handshake, or
/// to hyper, as if the origin had sent them. A refusal to connect rather
/// than a rewind, because the rewind is the quieter failure and the worse
/// one.
#[derive(Debug, thiserror::Error)]
#[error("the proxy sent {0} bytes past its own handshake, before anything was sent to the origin")]
pub struct ProxySpokeFirst(pub usize);

/// The proxy refused the tunnel. Deliberately **not** a response: a `407`
/// is the proxy's answer to us, not the origin's answer to the caller, and
/// handing it back as one would report a refusal to connect as an HTTP
/// result the caller could act on.
#[derive(Debug, thiserror::Error)]
#[error("the proxy refused CONNECT with {0}")]
pub struct ProxyRefused(pub http::StatusCode);

/// RFC 1928 §6's `REP`, which is one byte and has no HTTP meaning at all.
#[derive(Debug, thiserror::Error)]
#[error("the SOCKS5 proxy refused with REP={rep:#04x} ({})", socks5_reply(*rep))]
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

// --- HTTP proxies -------------------------------------------------------

/// An HTTP proxy: `CONNECT` for `https://`, absolute-form for `http://`.
///
/// The asymmetry is the protocol's, not a simplification. Tunnelling a
/// plain `http://` request would work at proxies that allow `CONNECT` to
/// port 80, and many allow it to 443 alone — so absolute-form is both the
/// specified behaviour (RFC 9112 §3.2.2) and the one that reaches more
/// deployments.
#[derive(Debug, Clone, Default)]
pub struct HttpConnect {
    auth: Option<http::HeaderValue>,
}

impl HttpConnect {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Proxy-Authorization: Basic ..`, RFC 7617.
    ///
    /// Held as the finished header value rather than as the pair, so the
    /// password is encoded once at configuration time instead of on every
    /// connect — and so that a value which cannot be a header is refused
    /// here rather than at the first request.
    pub fn basic_auth(mut self, user: &str, password: &str) -> Result<Self, Error> {
        let raw = base64(format!("{user}:{password}").as_bytes());
        let mut v = http::HeaderValue::from_str(&format!("Basic {raw}"))
            .map_err(|e| Error::new(ErrorKind::Connect, e))?;
        v.set_sensitive(true);
        self.auth = Some(v);
        Ok(self)
    }
}

impl ProxyProtocol for HttpConnect {
    /// The one implementation that answers anything but [`Approach::Tunnel`].
    fn approach(&self, use_tls: bool) -> Approach {
        if use_tls {
            Approach::Tunnel
        } else {
            Approach::Absolute
        }
    }

    fn proxy_authorization(&self) -> Option<&http::HeaderValue> {
        self.auth.as_ref()
    }

    async fn tunnel<S>(&self, io: S, host: &str, port: u16) -> Result<(S, Bytes), Error>
    where
        S: Read + Write + Unpin + 'static,
    {
        // Authority-form, RFC 9112 §3.2.3, and hyper writes it verbatim:
        // its client renders `http::Uri`'s `Display` into the request line
        // (`role.rs:1212`), so the target is whatever `Uri` we hand it.
        let authority = format!("{host}:{port}");
        let uri: http::Uri = http::uri::Builder::new()
            .authority(authority.as_str())
            .build()
            .map_err(|e| Error::new(ErrorKind::Connect, e))?;

        let mut req = http::Request::new(http_body_util::Empty::<Bytes>::new());
        *req.method_mut() = http::Method::CONNECT;
        *req.version_mut() = http::Version::HTTP_11;
        *req.uri_mut() = uri;
        let value = http::HeaderValue::from_str(&authority)
            .map_err(|e| Error::new(ErrorKind::Connect, e))?;
        req.headers_mut().insert(http::header::HOST, value);
        if let Some(auth) = &self.auth {
            req.headers_mut()
                .insert(http::header::PROXY_AUTHORIZATION, auth.clone());
        }

        // Any `2xx`, which is what hyper's own h1 client treats as an
        // upgrade for a `CONNECT`. A `407` is the proxy's refusal to
        // connect us and becomes `ErrorKind::Connect`, never a response —
        // handing it back as one would report the proxy's answer as the
        // origin's.
        let upgrading = crate::upgrade::exchange(io, req, |status| {
            (!status.is_success()).then(|| Error::new(ErrorKind::Connect, ProxyRefused(status)))
        })
        .await?;
        upgrading.finish().await
    }
}

// --- SOCKS5 -------------------------------------------------------------

/// SOCKS5, RFC 1928, with the username/password sub-negotiation of
/// RFC 1929.
///
/// The origin goes out as `ATYP=0x03 DOMAINNAME` — a name, never an
/// address — which is what `socks5h` names in other clients' URL schemes
/// and is the reason §2 of `docs/proxy-design.md` rejects a wrapper over
/// [`TcpConnect`](http_ng_rt::TcpConnect): the DNS leak is a property of a
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

// --- byte-exact IO over hyper's traits ----------------------------------
//
// Written here rather than reached for: `http-ng-tls-native-tls`'s
// `HyperIo` would give `futures_util`'s `read_exact`, but it is that
// crate's private adapter, and these two helpers are shorter than moving
// it would be. Both are used only for fixed-size SOCKS5 frames.

async fn write_all<S: Write + Unpin>(io: &mut S, mut buf: &[u8]) -> Result<(), Error> {
    while !buf.is_empty() {
        let n = std::future::poll_fn(|cx| Pin::new(&mut *io).poll_write(cx, buf))
            .await
            .map_err(conn)?;
        if n == 0 {
            return Err(conn(io::Error::from(io::ErrorKind::WriteZero)));
        }
        buf = &buf[n..];
    }
    std::future::poll_fn(|cx| Pin::new(&mut *io).poll_flush(cx))
        .await
        .map_err(conn)
}

async fn read_exact<S: Read + Unpin>(io: &mut S, buf: &mut [u8]) -> Result<(), Error> {
    let mut at = 0;
    while at < buf.len() {
        let n = std::future::poll_fn(|cx| {
            let mut rb = hyper::rt::ReadBuf::new(&mut buf[at..]);
            match Pin::new(&mut *io).poll_read(cx, rb.unfilled()) {
                Poll::Ready(Ok(())) => Poll::Ready(Ok(rb.filled().len())),
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
        .map_err(conn)?;
        if n == 0 {
            // A short handshake is a failure to connect, never a partial
            // success: every frame read here is fixed-size by the RFC.
            return Err(conn(io::Error::from(io::ErrorKind::UnexpectedEof)));
        }
        at += n;
    }
    Ok(())
}

fn conn(e: io::Error) -> Error {
    Error::new(ErrorKind::Connect, e)
}

/// RFC 4648 §4, encode only.
///
/// Written here rather than taken from a crate, and the reason is the
/// module doc's own claim: neither protocol needs a third-party
/// dependency, so one arriving for a single header value would make that
/// sentence false. Twenty lines against a crate is the same trade
/// `http-ng-webtransport` made for its close capsule.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            // `chunk.len() + 1` output characters carry data; the rest is
            // padding, which is `=` rather than the alphabet's `A`.
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Context;

    /// A socket whose answers are decided in advance and whose writes are
    /// kept. Enough for a handshake, which is all `tunnel` does before it
    /// hands the stream back.
    #[derive(Debug)]
    struct ScriptIo {
        reply: Vec<u8>,
        at: usize,
        written: Vec<u8>,
        waiting: Option<std::task::Waker>,
    }

    impl ScriptIo {
        fn new(reply: &[u8]) -> Self {
            Self {
                reply: reply.to_vec(),
                at: 0,
                written: Vec::new(),
                waiting: None,
            }
        }
    }

    impl Read for ScriptIo {
        /// **Silent until the request has been written**, which is what a
        /// socket is: handing the reply back on the first poll made hyper
        /// see a response with no request in flight and fail the exchange
        /// with `UnexpectedMessage` — a fixture bug that looked exactly
        /// like a client one.
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            if self.written.is_empty() {
                self.waiting = Some(cx.waker().clone());
                return Poll::Pending;
            }
            let n = (self.reply.len() - self.at).min(buf.remaining());
            let at = self.at;
            let chunk = self.reply[at..at + n].to_vec();
            buf.put_slice(&chunk);
            self.at += n;
            Poll::Ready(Ok(()))
        }
    }

    impl Write for ScriptIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.written.extend_from_slice(buf);
            if let Some(w) = self.waiting.take() {
                w.wake();
            }
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// **The `CONNECT` line is authority-form and carries `Host:`.** hyper
    /// writes `http::Uri`'s `Display` into the request line verbatim, so
    /// an origin-form URI here would put `/` on the wire and the proxy
    /// would have nothing to dial.
    #[test]
    fn connect_asks_for_the_authority_and_a_200_hands_the_socket_back() {
        let io = ScriptIo::new(b"HTTP/1.1 200 Connection Established\r\n\r\n");
        let (io, rest) =
            futures_executor::block_on(HttpConnect::new().tunnel(io, "example.invalid", 443))
                .expect("a 2xx is a tunnel");
        let sent = String::from_utf8_lossy(&io.written).into_owned();
        assert!(
            sent.starts_with("CONNECT example.invalid:443 HTTP/1.1\r\n"),
            "authority-form, got: {:?}",
            sent.lines().next()
        );
        assert!(
            sent.to_ascii_lowercase()
                .contains("host: example.invalid:443\r\n"),
            "and `Host:`, got:\n{sent}"
        );
        assert!(rest.is_empty(), "nothing arrived past the 200");
    }

    /// **A `407` refusing the tunnel is a connect error**, never a
    /// response: it is the proxy's answer to us rather than the origin's
    /// to the caller. The control is the test above, whose only
    /// difference is the status.
    #[test]
    fn a_407_refusing_the_tunnel_is_a_connect_error() {
        let io = ScriptIo::new(
            b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
        );
        let err = futures_executor::block_on(HttpConnect::new().tunnel(io, "h", 443))
            .expect_err("a 407 is a refusal");
        assert_eq!(*err.kind(), ErrorKind::Connect);
        assert!(
            std::error::Error::source(&err)
                .and_then(|s| s.downcast_ref::<ProxyRefused>())
                .is_some_and(|r| r.0 == http::StatusCode::PROXY_AUTHENTICATION_REQUIRED),
            "the status must be readable off the error: {err:?}"
        );
    }

    /// RFC 4648 §10's own vectors, plus the `Aladdin` line from RFC 7617
    /// §2 — the one this function actually exists to produce.
    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("Aladdin:open sesame", "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
        ] {
            assert_eq!(base64(input.as_bytes()), want, "input {input:?}");
        }
    }

    /// The header is built once, at configuration time, and marked
    /// sensitive — so a `Debug` of the request does not print the
    /// password.
    #[test]
    fn basic_auth_is_encoded_once_and_marked_sensitive() {
        let p = HttpConnect::new()
            .basic_auth("Aladdin", "open sesame")
            .expect("a valid header value");
        let v = p.proxy_authorization().expect("set");
        assert_eq!(v, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        assert!(v.is_sensitive());
    }

    /// `http://` is the one origin an HTTP proxy does not tunnel, and
    /// SOCKS5 has no opinion about schemes at all. Asserted as a pair,
    /// because either alone reads as an accident.
    #[test]
    fn only_an_http_proxy_treats_the_two_schemes_differently() {
        let http = HttpConnect::new();
        assert_eq!(http.approach(true), Approach::Tunnel);
        assert_eq!(http.approach(false), Approach::Absolute);

        let socks = Socks5::new();
        assert_eq!(socks.approach(true), Approach::Tunnel);
        assert_eq!(socks.approach(false), Approach::Tunnel);
    }

    /// Both length prefixes are one byte, so both limits are the RFC's
    /// rather than ours, and both are refused at configuration time.
    #[test]
    fn socks5_refuses_a_credential_that_cannot_be_length_prefixed() {
        let long = "x".repeat(256);
        assert!(Socks5::new().password_auth(&long, "p").is_err());
        assert!(Socks5::new().password_auth("u", &long).is_err());
        assert!(Socks5::new().password_auth(&"x".repeat(255), "p").is_ok());
    }
}
