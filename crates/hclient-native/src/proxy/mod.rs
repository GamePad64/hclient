//! Proxy support: the seam, and the two protocols that prove it.
//!
//! Behind the `proxy` feature, off by default. Neither protocol here needs
//! a third-party crate, so the argument that moved the WebSocket framing
//! into a crate of its own — a feature is additive, and `tungstenite`
//! would land in every build in any graph that switched it on — has no
//! subject; what the feature does buy is that a constrained-target build
//! is byte for byte what it was.
//!
//! # Why this is not a `TcpConnect` wrapper, which would have cost nothing
//!
//! [`TcpConnect::connect`](hclient_rt::TcpConnect::connect) takes a
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
//! WebSocket work and `hyper/http2` before
//! that: *"single-threaded runtimes shut out"*, the thing this crate
//! exists to avoid. So the protocol is a parameter, defaulted to
//! [`NoProxy`], and `.proxy(..)` changes it the way `.hooks(..)` changes
//! `H`.

use std::future::Future;

use bytes::Bytes;
use hclient_core::Error;
use hyper::rt::{Read, Write};

mod connect;
mod frames;
mod socks4;
mod socks5;

pub use connect::{HttpConnect, ProxyRefused};
pub use socks4::{Socks4, Socks4HandshakeError, Socks4Refused};
pub use socks5::{Socks5, Socks5HandshakeError, Socks5Refused};

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
    /// `hclient-proto`'s redirect logic already strips it across origins.
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

/// Which request scheme a proxy serves, for a caller who has more than
/// one.
///
/// The distinction that motivates it is the ordinary corporate one — an
/// `HTTP_PROXY` and an `HTTPS_PROXY` pointing at different hosts — not two
/// different proxy *protocols*: `Native` has one `P`, so every proxy on
/// one transport speaks the same one. That is a real limit and it is
/// stated rather than worked around, because erasing `P` to lift it would
/// erase the IO with it, which is the objection `proxy`'s own module doc
/// records against `Box<dyn ProxyProtocol>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    Http,
    Https,
}

/// Where a proxy lives, and which protocol it speaks.
#[derive(Debug, Clone)]
pub struct Proxy<P> {
    protocol: P,
    host: Box<str>,
    port: u16,
    bypass: Vec<Box<str>>,
    /// `None` — the default — means both schemes, which is what a caller
    /// with one proxy wants and what `Proxy::new` gives them.
    only: Option<ProxyScheme>,
}

impl<P> Proxy<P> {
    pub fn new(protocol: P, host: impl Into<Box<str>>, port: u16) -> Self {
        Self {
            protocol,
            host: host.into(),
            port,
            bypass: Vec::new(),
            only: None,
        }
    }

    /// Origins this proxy does **not** serve, which go direct instead.
    ///
    /// # Why there is no default, and why nothing is read from the
    /// environment
    ///
    /// Excluding loopback by default would be this crate deciding, on a
    /// caller's behalf, that a request they asked to proxy should not be —
    /// a default that changes what goes on the wire without being asked,
    /// which is the shape `TcpOpts`' every-field-off default exists to
    /// avoid. So `Native::proxy` proxies everything until told otherwise,
    /// and a caller who also talks to `127.0.0.1` says so here.
    ///
    /// `HTTP_PROXY`/`NO_PROXY` are a different question and stay out:
    /// *which* variables, whose matching dialect, and whether a library
    /// may read the environment at all are policy, and policy belongs to
    /// whoever builds the transport. **This** list is not policy — the
    /// caller wrote it down.
    ///
    /// # The rules, which are small on purpose
    ///
    /// `NO_PROXY` has no specification and every implementation disagrees
    /// about the corners. Rather than pick one dialect and be subtly
    /// wrong, these are the forms this accepts, matched
    /// case-insensitively against the request's host:
    ///
    /// - `example.com` — that host exactly, at any port.
    /// - `.example.com` — that host **and** any subdomain of it.
    /// - `example.com:8080` — that host at that port alone.
    /// - `127.0.0.1`, `::1` — an address literal is just a host. A v6 one
    ///   takes RFC 3986 brackets to carry a port: `[::1]:8080`.
    ///
    /// No CIDR and no wildcard. A pattern in no accepted shape matches
    /// nothing rather than approximately something.
    ///
    /// # A bypass belongs to the proxy that carries it
    ///
    /// With one proxy — the overwhelming majority — a bypassed host goes
    /// direct, and that is `NO_PROXY`'s meaning. With several
    /// ([`Native::and_proxy`](crate::Native::and_proxy)) a bypassed host
    /// **falls through to the next proxy**, and only goes direct when the
    /// list runs out.
    ///
    /// The global reading is the worse one *because* the list exists: a
    /// host bypassed on an `https`-only proxy would take an `http://`
    /// request direct, past an `http` proxy that was never in the running
    /// and never mentioned it. A caller who wants the global rule writes
    /// the list on each proxy, which is honest because they wrote it.
    pub fn bypass<S: Into<Box<str>>>(mut self, patterns: impl IntoIterator<Item = S>) -> Self {
        self.bypass.extend(
            patterns
                .into_iter()
                .map(|p| p.into().to_ascii_lowercase().into_boxed_str()),
        );
        self
    }

    /// Use this proxy for one scheme only.
    ///
    /// The default is both, and stays the honest default: a caller who
    /// names one proxy means it for everything, and narrowing it silently
    /// would send half their traffic direct.
    ///
    /// Ordering is the caller's, not a precedence rule of ours:
    /// [`Native::proxy`](crate::Native::proxy) and
    /// [`and_proxy`](crate::Native::and_proxy) build a list and **the
    /// first entry that serves a request wins**. So an unrestricted proxy
    /// placed first shadows everything after it, which is visible at the
    /// call site rather than hidden in a rule.
    #[must_use]
    pub fn only_for(mut self, scheme: ProxyScheme) -> Self {
        self.only = Some(scheme);
        self
    }

    /// Whether this proxy is used for a request to `host:port` under
    /// `use_tls`.
    ///
    /// Two questions in one, and they are asked in this order because they
    /// fail differently: a scheme this proxy does not serve means *try the
    /// next proxy*, where a bypassed host means *go direct* — and the
    /// caller of this function collapses them only because a list that
    /// runs out is itself "go direct".
    pub(crate) fn serves(&self, use_tls: bool, host: &str, port: u16) -> bool {
        let wanted = if use_tls {
            ProxyScheme::Https
        } else {
            ProxyScheme::Http
        };
        if self.only.is_some_and(|only| only != wanted) {
            return false;
        }
        let host = host.trim_start_matches('[').trim_end_matches(']');
        !self.bypass.iter().any(|p| matches_bypass(p, host, port))
    }

    /// The scheme this proxy is restricted to, if any — read by
    /// `Native`'s own tests and by nothing on the request path.
    pub fn scheme(&self) -> Option<ProxyScheme> {
        self.only
    }

    /// The first proxy in `list` that serves this request, or `None` for
    /// direct.
    ///
    /// First-match-wins rather than most-specific-wins: a precedence rule
    /// would have to be learned, where an ordered list is read off the
    /// builder chain that wrote it. `NO_PROXY` implementations that invent
    /// a precedence are exactly what `bypass`'s doc refuses to imitate.
    pub(crate) fn choose<'a>(
        list: &'a [Proxy<P>],
        use_tls: bool,
        host: &str,
        port: u16,
    ) -> Option<&'a Proxy<P>> {
        list.iter().find(|p| p.serves(use_tls, host, port))
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

/// One pattern against one origin. Separate from [`Proxy::serves`] so the
/// forms can be tested one at a time rather than through a list.
fn matches_bypass(pattern: &str, host: &str, port: u16) -> bool {
    let host = host.to_ascii_lowercase();
    let (p_host, p_port) = split_pattern(pattern);
    match p_port {
        Some(want) => want == port && host_matches(p_host, &host),
        None => host_matches(p_host, &host),
    }
}

/// A pattern into its host and its optional port.
///
/// **An IPv6 literal is why this is not one `rsplit_once(':')`.** `::1`
/// splits into `("::", "1")`, and `1` parses as a port — so a bare v6
/// address would silently become "the host `::` at port 1", matching
/// nothing a caller meant. RFC 3986 §3.2.2's brackets are the
/// disambiguator and are required here for the same reason they are in an
/// authority: `[::1]:8080` binds a port, `::1` does not.
fn split_pattern(pattern: &str) -> (&str, Option<u16>) {
    if let Some(rest) = pattern.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((h, "")) => (h, None),
            Some((h, tail)) => (h, tail.strip_prefix(':').and_then(|p| p.parse().ok())),
            // An unclosed bracket is in no accepted shape, so it matches
            // nothing rather than approximately something.
            None => (pattern, None),
        };
    }
    if pattern.matches(':').count() > 1 {
        return (pattern, None);
    }
    match pattern.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(port) => (h, Some(port)),
            Err(_) => (pattern, None),
        },
        None => (pattern, None),
    }
}

fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix('.') {
        // `.example.com` is the domain and everything under it. The
        // leading dot is not part of the name, so `example.com` itself
        // matches — which is what a reader expects and what most
        // `NO_PROXY` implementations do.
        Some(domain) => host == domain || host.ends_with(pattern),
        None => host == pattern,
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
#[cfg(test)]
mod tests {
    use super::*;
    use hclient_core::ErrorKind;
    use std::error::Error as StdError;
    use std::io;
    use std::pin::Pin;
    use std::task::Context;
    use std::task::Poll;

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
            StdError::source(&err)
                .and_then(|s| s.downcast_ref::<ProxyRefused>())
                .is_some_and(|r| r.0 == http::StatusCode::PROXY_AUTHENTICATION_REQUIRED),
            "the status must be readable off the error: {err:?}"
        );
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

    /// The accepted forms match what they say and nothing beside it — a
    /// bypass list that matched approximately would send a request direct
    /// that the caller asked to be proxied.
    #[test]
    fn the_bypass_forms_match_what_they_say_and_nothing_beside_it() {
        let p = |pat: &str| Proxy::new(Socks5::new(), "px", 1080).bypass([pat]);

        // Exact host, at any port, case-insensitively.
        assert!(!p("example.com").serves(true, "example.com", 443));
        assert!(!p("example.com").serves(true, "EXAMPLE.COM", 8080));
        assert!(p("example.com").serves(true, "api.example.com", 443));
        assert!(p("example.com").serves(true, "notexample.com", 443));

        // Domain and everything under it.
        assert!(!p(".example.com").serves(true, "example.com", 443));
        assert!(!p(".example.com").serves(true, "api.example.com", 443));
        assert!(!p(".example.com").serves(true, "a.b.example.com", 443));
        assert!(p(".example.com").serves(true, "notexample.com", 443));

        // Host at one port alone.
        assert!(!p("example.com:8080").serves(true, "example.com", 8080));
        assert!(p("example.com:8080").serves(true, "example.com", 443));

        // An address literal is just a host, and a v6 one arrives here
        // wearing the brackets RFC 3986 gives the authority.
        assert!(!p("127.0.0.1").serves(true, "127.0.0.1", 80));
        assert!(p("127.0.0.1").serves(true, "127.0.0.2", 80));
        assert!(!p("::1").serves(true, "[::1]", 80));
        assert!(!p("::1").serves(true, "::1", 1), "`::1` binds no port");
        assert!(!p("[::1]:8080").serves(true, "[::1]", 8080));
        assert!(p("[::1]:8080").serves(true, "[::1]", 80));

        // No CIDR, no wildcard: a pattern in no accepted shape matches
        // nothing rather than approximately something.
        assert!(p("10.0.0.0/8").serves(true, "10.1.2.3", 80));
        assert!(p("*.example.com").serves(true, "api.example.com", 80));
    }

    /// Empty by default, which is the decision rather than an oversight:
    /// excluding loopback for a caller who asked to proxy everything
    /// would change what goes on the wire without being asked.
    #[test]
    fn nothing_is_bypassed_until_a_caller_says_so() {
        let p = Proxy::new(Socks5::new(), "px", 1080);
        assert!(p.serves(true, "127.0.0.1", 80));
        assert!(p.serves(true, "localhost", 80));
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

/// A proxy and a Unix-domain socket were both configured.
///
/// Both answer *where does this connection go*, and a rule about which one
/// wins would be a rule nobody could guess — so the second one is refused
/// where it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a proxy and a Unix socket both decide where a connection goes; configure at most one")]
pub struct ProxyAndUnixSocket;
