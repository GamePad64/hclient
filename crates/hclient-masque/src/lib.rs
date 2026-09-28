//! An unpublished experiment: MASQUE — RFC 9298 CONNECT-UDP, and CONNECT
//! over h2/h3 — built as an `hclient-proxy` egress filter, to prove that
//! crate's datagram and tunnel seams against a proxy protocol neither
//! seam was written against.
//!
//! This crate never reaches crates.io. It exists to answer one question
//! by building the thing rather than arguing about it: can a proxy
//! protocol this workspace did not have in mind when it designed
//! `EgressFilter`, `Dial` and the datagram path actually be written as
//! one, with no seam changed underneath it? Beneath the filter are the
//! codecs a MASQUE implementation needs before it can dial anything —
//! [`template`] expands RFC 9298 §2's URI template, and [`capsule`] reads
//! and writes RFC 9297 §3.2's capsules, over [`capsule::varint`]'s RFC
//! 9000 §16 varints. [`Masque`] is the filter itself.
//!
//! [`Masque`] carries HTTP/3 over CONNECT-UDP (RFC 9298) — over a proxy's
//! own HTTP/3 datagrams when it is spoken to over HTTP/3, over DATAGRAM
//! capsules on the tunnel's stream when it is spoken to over HTTP/2 — and
//! every other request over a plain CONNECT.
//!
//! A plain CONNECT goes over HTTP/2 only. The HTTP/3 stack this workspace
//! uses writes `:scheme` and `:path` on every request, which RFC 9114 §4.4
//! forbids on a CONNECT, so the transport refuses one over HTTP/3 as
//! unsupported before a packet is sent: with [`TunnelVersion::Http3`] a
//! request that needs a byte stream fails, and with the default
//! [`TunnelVersion::Http3ThenHttp2`] it goes straight to HTTP/2.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod capsule;
mod error;
mod path;
pub mod template;

use hclient_core::error::{Error, ErrorKind};
use hclient_proxy::{
    Attempt, BoxDial, BoxIo, BoxOpening, BoxPath, BoxPathOpening, Decision, Dial, EgressFilter,
    FilterSupport, Io, Opened, ProxyTls, RequestForm, Route, SendEgressFilter, Target, Tunnel,
    TunnelRequest, TunnelVersion,
};

pub use error::{CapsuleError, Refused, TemplateError};
pub use path::{CAPSULE_MAX, CapsulePath, ContextPath};

/// RFC 9298 §3's well-known template, the one a proxy with no other
/// configuration serves.
pub const DEFAULT_TEMPLATE: &str = "/.well-known/masque/udp/{target_host}/{target_port}/";

/// An egress filter that sends every request through one MASQUE proxy:
/// HTTP/3 over CONNECT-UDP, everything else over a plain CONNECT.
///
/// The proxy is always spoken to over TLS, checked against `proxy_host`
/// with the transport's own trust.
#[derive(Debug, Clone)]
pub struct Masque {
    proxy_host: String,
    proxy_port: u16,
    template: String,
    version: TunnelVersion,
    key: String,
}

impl Masque {
    /// The proxy at `proxy_host:proxy_port`, with RFC 9298's default
    /// template, asked over HTTP/3 and then HTTP/2.
    #[must_use]
    pub fn new(proxy_host: impl Into<String>, proxy_port: u16) -> Self {
        let proxy_host = proxy_host.into();
        let key = format!("masque:{proxy_host}:{proxy_port}");
        Self {
            proxy_host,
            proxy_port,
            template: DEFAULT_TEMPLATE.to_owned(),
            version: TunnelVersion::Http3ThenHttp2,
            key,
        }
    }

    /// Use this RFC 9298 URI template, a path carrying `{target_host}` and
    /// `{target_port}`.
    #[must_use]
    pub fn template(mut self, template: String) -> Self {
        self.template = template;
        self
    }

    /// Ask for tunnels over this HTTP version.
    #[must_use]
    pub fn version(mut self, version: TunnelVersion) -> Self {
        self.version = version;
        self
    }

    fn proxy_tls(&self) -> ProxyTls<'_> {
        ProxyTls::new(&self.proxy_host)
    }
}

/// A `2xx`, or the refusal naming what was answered.
fn accepted(tunnel: &Tunnel) -> Result<(), Error> {
    let status = tunnel.response.status;
    if status.is_success() {
        Ok(())
    } else {
        Err(Error::new(ErrorKind::Connect, Refused { status }))
    }
}

/// `host:port`, bracketing an IPv6 literal that arrives bare.
fn authority(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

impl EgressFilter for Masque {
    type Wrapped<S: Io> = BoxIo;

    fn route(&self, _target: &Target<'_>) -> Decision<'_> {
        Decision::Filtered(Route::new(
            FilterSupport::STREAM.with_datagrams(),
            self.key.as_str(),
            RequestForm::Origin,
        ))
    }

    async fn open_stream<'a, C: Dial + 'a>(
        &'a self,
        target: Target<'a>,
        ctx: &'a C,
    ) -> Result<Opened<C::Stream, BoxIo>, Attempt>
    where
        Self: Sized,
    {
        let authority = authority(target.host, target.port);
        let req = TunnelRequest::new(
            &self.proxy_host,
            self.proxy_port,
            self.proxy_tls(),
            &authority,
        )
        .version(self.version);
        let tunnel = ctx.connect_tunnel(req).await.map_err(Attempt::Failed)?;
        accepted(&tunnel).map_err(Attempt::Failed)?;
        Ok(Opened::Wrapped(tunnel.stream))
    }

    async fn open_datagrams<'a, C: Dial + 'a>(
        &'a self,
        target: Target<'a>,
        ctx: &'a C,
    ) -> Result<BoxPath, Attempt>
    where
        Self: Sized,
        C::Stream: Send + 'static, // send-bound-exception: amendment-C16
    {
        let path = template::expand(&self.template, target.host, target.port)
            .map_err(|e| Attempt::Failed(Error::new(ErrorKind::Connect, e)))?;
        let authority = authority(&self.proxy_host, self.proxy_port);
        let req = TunnelRequest::new(
            &self.proxy_host,
            self.proxy_port,
            self.proxy_tls(),
            &authority,
        )
        .path(Some(&path))
        .protocol(Some("connect-udp"))
        .header(
            http::HeaderName::from_static("capsule-protocol"),
            http::HeaderValue::from_static("?1"),
        )
        .version(self.version);
        let tunnel = ctx.connect_tunnel(req).await.map_err(|e| {
            if *e.kind() == ErrorKind::Unsupported {
                Attempt::Unsupported(e)
            } else {
                Attempt::Failed(e)
            }
        })?;
        // A proxy that answers CONNECT-UDP with anything but `2xx` does not
        // carry UDP for this target, which is what lets the transport ask
        // for a byte stream instead.
        accepted(&tunnel).map_err(Attempt::Unsupported)?;
        Ok(match tunnel.datagrams {
            Some(d) => BoxPath::new(ContextPath::new(d, tunnel.stream)),
            None => BoxPath::new(CapsulePath::new(tunnel.stream)),
        })
    }
}

impl SendEgressFilter for Masque {
    fn open_stream_send<'a>(&'a self, target: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a> {
        Box::pin(async move {
            self.open_stream(target, ctx)
                .await
                .map(hclient_proxy::erase)
        })
    }

    fn open_datagrams_send<'a>(
        &'a self,
        target: Target<'a>,
        ctx: &'a BoxDial<'a>,
    ) -> BoxPathOpening<'a> {
        Box::pin(self.open_datagrams(target, ctx))
    }
}
