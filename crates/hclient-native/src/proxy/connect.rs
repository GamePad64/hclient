//! HTTP `CONNECT`, RFC 9110 §9.3.6.
//!
//! `connect` and not `http`: a submodule of that name shadows the `http`
//! crate for every `http::HeaderValue` in `mod.rs`, which is the same
//! collision `http3::runtime` was renamed out of one commit earlier.
//!
//! The tunnel is [`crate::upgrade`]'s seam with a different accepted
//! status, which is why those forty delicate lines exist once rather than
//! twice: hyper's h1 client sets `wants_upgrade` for `Method::CONNECT`
//! and skips the body for `CONNECT` + `is_success`, so `into_parts`
//! yields the tunnel and the bytes read past it exactly as it does for a
//! `101`.

use super::{Approach, ProxyProtocol};
use bytes::Bytes;
use hclient_core::{Error, ErrorKind};
use hyper::rt::{Read, Write};

/// The proxy refused the tunnel. Deliberately **not** a response: a `407`
/// is the proxy's answer to us, not the origin's answer to the caller, and
/// handing it back as one would report a refusal to connect as an HTTP
/// result the caller could act on.
#[derive(Debug, thiserror::Error)]
#[error("the proxy refused CONNECT with {0}")]
pub struct ProxyRefused(pub http::StatusCode);
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
        let raw = hclient_proto::encode::base64(format!("{user}:{password}").as_bytes());
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
