//! HTTP `CONNECT`, RFC 9110 §9.3.6 — and it needs no HTTP client.
//!
//! This used to drive `hyper`'s h1 dispatcher through `hclient-native`'s
//! upgrade seam, which is what tied the whole proxy family to hyper, to
//! hyper's IO traits, and to a transport. What replaced it is forty lines
//! over [`hclient_proto::head`], and the reason the trade is even
//! available is that **a `CONNECT` response has no body under any framing
//! rule** (§9.3.6): there is no `Content-Length` to honour, no chunked
//! decoding, and no interaction between the two — the hard half of HTTP/1
//! has no subject here.
//!
//! What is left is one request head out and one response head in. The
//! request head is written directly rather than through `http::Request`,
//! because the only thing a `Request` would buy is a renderer, and
//! authority-form (§3.2.3) is three fields.

use bytes::{BufMut, Bytes, BytesMut};
use hclient_core::{Error, ErrorKind};
use hclient_proto::head;

use crate::{Approach, Handshake, Step};

/// The proxy refused the tunnel. Deliberately **not** a response: a `407`
/// is the proxy's answer to us, not the origin's answer to the caller,
/// and handing it back as one would report a refusal to connect as an
/// HTTP result the caller could act on.
#[derive(Debug, thiserror::Error)]
#[error("the proxy refused CONNECT with {0}")]
pub struct ProxyRefused(pub http::StatusCode);

/// The proxy answered something that is not an HTTP response, or too much
/// of one.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
    #[error("the proxy's answer to CONNECT is not an HTTP response head: {0}")]
    Malformed(#[from] head::HeadError),
    /// A head that never ends is a proxy holding the connection open at
    /// our expense, and the bound is ours because HTTP states none.
    #[error("the proxy's response head passed {0} bytes without ending")]
    HeadTooLong(usize),
    #[error("`{0}:{1}` cannot be written as an authority")]
    BadAuthority(Box<str>, u16),
}

/// A response head this large is a proxy that has stopped answering and
/// started talking. 64 KiB is `H1Opts`'s own default one crate over, and
/// the number matters less than there being one.
const MAX_HEAD: usize = 64 * 1024;

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
    awaiting_head: bool,
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

impl Handshake for HttpConnect {
    /// The one implementation that answers anything but
    /// [`Approach::Tunnel`].
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

    fn begin(&mut self, host: &str, port: u16) -> Result<Bytes, Error> {
        // Authority-form, RFC 9112 §3.2.3. Checked through `http::Uri`
        // rather than written blind: a host carrying a space or a CR would
        // otherwise put a second request line on the wire, which is the
        // request-smuggling shape this workspace refuses to be one end of.
        let authority = format!("{host}:{port}");
        http::uri::Authority::try_from(authority.as_str()).map_err(|_| {
            Error::new(
                ErrorKind::Connect,
                ConnectError::BadAuthority(host.into(), port),
            )
        })?;

        let mut req = BytesMut::with_capacity(64 + 2 * authority.len());
        req.put_slice(b"CONNECT ");
        req.put_slice(authority.as_bytes());
        req.put_slice(b" HTTP/1.1\r\nHost: ");
        req.put_slice(authority.as_bytes());
        req.put_slice(b"\r\n");
        if let Some(auth) = &self.auth {
            req.put_slice(b"Proxy-Authorization: ");
            req.put_slice(auth.as_bytes());
            req.put_slice(b"\r\n");
        }
        req.put_slice(b"\r\n");

        self.awaiting_head = true;
        Ok(req.freeze())
    }

    fn advance(&mut self, from_peer: &mut BytesMut) -> Result<Step, Error> {
        if !self.awaiting_head {
            return Ok(Step::Done);
        }
        let parsed = head::parse_response(from_peer)
            .map_err(|e| Error::new(ErrorKind::Connect, ConnectError::Malformed(e)))?;
        let Some((head, len)) = parsed else {
            if from_peer.len() > MAX_HEAD {
                return Err(Error::new(
                    ErrorKind::Connect,
                    ConnectError::HeadTooLong(from_peer.len()),
                ));
            }
            return Ok(Step::NeedMore);
        };

        // Any `2xx`, which is what hyper's own h1 client treats as an
        // upgrade for a `CONNECT`. A `407` is the proxy's refusal to
        // connect us and becomes `ErrorKind::Connect`, never a response —
        // handing it back as one would report the proxy's answer as the
        // origin's.
        if !head.status.is_success() {
            return Err(Error::new(ErrorKind::Connect, ProxyRefused(head.status)));
        }
        // Consumed only now: a refusal leaves the buffer as it was, which
        // costs nothing and keeps the failure path from being the one
        // path that mutates state on the way out.
        let _ = from_peer.split_to(len);
        self.awaiting_head = false;
        Ok(Step::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive_for_test;

    fn answering(reply: &'static [u8]) -> impl FnMut(&[u8]) -> Vec<u8> {
        move |_sent| reply.to_vec()
    }

    #[test]
    fn the_request_is_authority_form_with_a_host_header() {
        let mut h = HttpConnect::new();
        let (written, leftover) = drive_for_test(
            &mut h,
            "example.com",
            443,
            answering(b"HTTP/1.1 200 Connection established\r\n\r\n"),
        )
        .expect("granted");

        assert_eq!(
            String::from_utf8(written[0].clone()).unwrap(),
            "CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n"
        );
        assert!(leftover.is_empty());
    }

    #[test]
    fn credentials_go_out_as_a_proxy_authorization_header() {
        let mut h = HttpConnect::new().basic_auth("alice", "hunter2").unwrap();
        let (written, _) = drive_for_test(
            &mut h,
            "example.com",
            443,
            answering(b"HTTP/1.1 200 OK\r\n\r\n"),
        )
        .expect("granted");

        let sent = String::from_utf8(written[0].clone()).unwrap();
        // RFC 7617: `alice:hunter2`, base64.
        assert!(
            sent.contains("Proxy-Authorization: Basic YWxpY2U6aHVudGVyMg==\r\n"),
            "{sent}"
        );
    }

    #[test]
    fn the_origins_first_bytes_survive_the_head() {
        // The `read_buf` property, which is why this handshake reports how
        // much of the buffer was the head rather than draining it: a proxy
        // may send its `200` and the origin's opening bytes in one flight.
        let mut h = HttpConnect::new();
        let (_, leftover) = drive_for_test(
            &mut h,
            "example.com",
            443,
            answering(b"HTTP/1.1 200 OK\r\n\r\n\x16\x03\x01 a TLS ServerHello"),
        )
        .expect("granted");

        assert_eq!(&leftover[..], b"\x16\x03\x01 a TLS ServerHello");
    }

    #[test]
    fn any_2xx_opens_the_tunnel() {
        // hyper's own rule, kept: the RFC says 2xx, not 200.
        for status in ["200 OK", "201 Created", "299 Whatever"] {
            let mut h = HttpConnect::new();
            let reply = format!("HTTP/1.1 {status}\r\n\r\n");
            let mut buf = BytesMut::new();
            let _ = h.begin("example.com", 443).unwrap();
            buf.extend_from_slice(reply.as_bytes());
            assert_eq!(h.advance(&mut buf).unwrap(), Step::Done, "{status}");
        }
    }

    #[test]
    fn a_407_is_a_connect_failure_and_never_a_response() {
        let mut h = HttpConnect::new();
        let err = drive_for_test(
            &mut h,
            "example.com",
            443,
            answering(
                b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic\r\n\r\n",
            ),
        )
        .expect_err("refused");

        assert_eq!(*err.kind(), ErrorKind::Connect);
        assert!(err.to_string().contains("407"), "{err}");
    }

    #[test]
    fn a_head_arriving_in_pieces_is_not_consumed_until_it_is_whole() {
        let mut h = HttpConnect::new();
        let mut buf = BytesMut::new();
        let _ = h.begin("example.com", 443).unwrap();

        let reply = b"HTTP/1.1 200 OK\r\nVia: p\r\n\r\n";
        for n in 0..reply.len() {
            buf.clear();
            buf.extend_from_slice(&reply[..n]);
            assert_eq!(h.advance(&mut buf).unwrap(), Step::NeedMore, "at {n}");
            assert_eq!(buf.len(), n, "a partial head was consumed at {n}");
        }
        buf.clear();
        buf.extend_from_slice(reply);
        assert_eq!(h.advance(&mut buf).unwrap(), Step::Done);
        assert!(buf.is_empty());
    }

    #[test]
    fn a_proxy_that_answers_with_something_else_entirely_is_named() {
        let mut h = HttpConnect::new();
        let err = drive_for_test(&mut h, "example.com", 443, answering(b"\0\0\0\0\r\n\r\n"))
            .expect_err("not HTTP");
        assert!(
            err.to_string().contains("not an HTTP response head"),
            "{err}"
        );
    }

    #[test]
    fn a_head_that_never_ends_is_refused_rather_than_buffered_for_ever() {
        let mut h = HttpConnect::new();
        let mut buf = BytesMut::new();
        let _ = h.begin("example.com", 443).unwrap();
        buf.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
        // Just under, then just over.
        buf.extend_from_slice(&vec![b'X'; MAX_HEAD - buf.len()]);
        assert_eq!(h.advance(&mut buf).unwrap(), Step::NeedMore);
        buf.extend_from_slice(b"XX");
        let err = h.advance(&mut buf).expect_err("bounded");
        assert!(err.to_string().contains("without ending"), "{err}");
    }

    #[test]
    fn a_host_that_would_forge_a_second_request_line_is_refused() {
        // The request head is written directly, so the check that a host
        // is an authority is this crate's own — and it is the one that
        // keeps a caller-supplied name from putting CRLF on the wire.
        let mut h = HttpConnect::new();
        assert!(h.begin("example.com\r\nX-Evil: 1", 443).is_err());
        assert!(h.begin("exa mple.com", 443).is_err());
    }

    #[test]
    fn http_takes_absolute_form_and_https_takes_a_tunnel() {
        let h = HttpConnect::new();
        assert_eq!(h.approach(true), Approach::Tunnel);
        assert_eq!(h.approach(false), Approach::Absolute);
    }
}
