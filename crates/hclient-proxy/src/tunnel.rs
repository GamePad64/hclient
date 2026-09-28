//! An extended CONNECT to a proxy, which a transport lends a filter.

use crate::{BoxIo, BoxPath, ProxyTls};

/// Which HTTP version a tunnel to a proxy is asked over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TunnelVersion {
    /// HTTP/3 only.
    Http3,
    /// HTTP/2 only.
    Http2,
    /// HTTP/3, and HTTP/2 where the proxy cannot be reached over it.
    Http3ThenHttp2,
}

/// A CONNECT or extended CONNECT to a proxy.
///
/// Built by a filter and read by a transport; `#[non_exhaustive]`, so build
/// one with [`TunnelRequest::new`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TunnelRequest<'a> {
    /// The proxy's host, resolved by the transport.
    pub proxy_host: &'a str,
    /// The proxy's port.
    pub proxy_port: u16,
    /// The TLS handshake to the proxy; always TLS, since h2 and h3 to a
    /// proxy are.
    pub tls: ProxyTls<'a>,
    /// `:authority` — for plain CONNECT the target `host:port`, for
    /// extended CONNECT the proxy's own authority.
    pub authority: &'a str,
    /// `:path`; `None` for plain CONNECT, which has none.
    pub path: Option<&'a str>,
    /// `:protocol` (RFC 8441, RFC 9220); `None` for plain CONNECT.
    pub protocol: Option<&'a str>,
    /// Further request fields, `capsule-protocol` among them.
    pub headers: http::HeaderMap,
    /// Which version to ask over.
    pub version: TunnelVersion,
}

impl<'a> TunnelRequest<'a> {
    /// A plain CONNECT for `authority` through the proxy at
    /// `proxy_host:proxy_port`, over HTTP/3 then HTTP/2.
    #[must_use]
    pub fn new(
        proxy_host: &'a str,
        proxy_port: u16,
        tls: ProxyTls<'a>,
        authority: &'a str,
    ) -> Self {
        Self {
            proxy_host,
            proxy_port,
            tls,
            authority,
            path: None,
            protocol: None,
            headers: http::HeaderMap::new(),
            version: TunnelVersion::Http3ThenHttp2,
        }
    }
    /// Set `:path`.
    #[must_use]
    pub fn path(mut self, path: Option<&'a str>) -> Self {
        self.path = path;
        self
    }
    /// Set `:protocol`, making this an extended CONNECT.
    #[must_use]
    pub fn protocol(mut self, protocol: Option<&'a str>) -> Self {
        self.protocol = protocol;
        self
    }
    /// Add a request field.
    #[must_use]
    pub fn header(mut self, name: http::HeaderName, value: http::HeaderValue) -> Self {
        self.headers.append(name, value);
        self
    }
    /// Ask over this version.
    #[must_use]
    pub fn version(mut self, version: TunnelVersion) -> Self {
        self.version = version;
        self
    }
}

/// An open tunnel: the proxy's answer, the stream both ways, and — over
/// HTTP/3 — the stream's datagrams.
///
/// The response is handed over unjudged: whether a `2xx` without
/// `capsule-protocol` is acceptable is the filter's protocol, not the
/// transport's.
#[derive(Debug)]
#[non_exhaustive]
pub struct Tunnel {
    /// The proxy's response head.
    pub response: http::response::Parts,
    /// The request and response bodies, as one byte stream.
    pub stream: BoxIo,
    /// HTTP datagrams (RFC 9297) bound to this stream, over HTTP/3 only;
    /// the quarter stream id is the transport's to write and strip.
    pub datagrams: Option<BoxPath>,
}

impl Tunnel {
    /// A tunnel from its parts.
    #[must_use]
    pub fn new(response: http::response::Parts, stream: BoxIo, datagrams: Option<BoxPath>) -> Self {
        Self {
            response,
            stream,
            datagrams,
        }
    }
}
