//! The transport: one WinHTTP session, one exchange per request.

use std::future::poll_fn;
use std::sync::Arc;

use hclient_core::unversioned::Transport;
use hclient_core::{
    CancelSupport, Capabilities, DecompressionSupport, Error, ErrorKind, RedirectSupport,
    RequestBody, ReuseSupport, TlsSupport,
};

use crate::body::{WinHttpBody, event_name};
use crate::error::{Win32Error, WinHttpError};
use crate::sys::{Event, Exchange, Session};

/// WinHTTP as a [`Transport`].
///
/// See the crate documentation for what it takes from the OS, what it
/// deliberately does not, and what has not been observed running.
#[derive(Debug)]
pub struct WinHttp {
    session: Arc<Session>,
    caps: Capabilities,
}

impl WinHttp {
    /// A session under this crate's own user agent.
    ///
    /// The name reaches the wire only where a caller sets no
    /// `User-Agent` of their own: `WinHttpOpen`'s agent is what WinHTTP
    /// falls back to, and a header added here replaces it.
    pub fn new() -> Result<Self, Error> {
        Self::with_user_agent(concat!("hclient-winhttp/", env!("CARGO_PKG_VERSION")))
    }

    /// A session under `agent`.
    pub fn with_user_agent(agent: &str) -> Result<Self, Error> {
        let session = Session::open(agent).map_err(|e| {
            Error::new(
                ErrorKind::Connect,
                WinHttpError::Call {
                    call: "WinHttpOpen",
                    source: e,
                },
            )
        })?;
        Ok(Self {
            session: Arc::new(session),
            caps: capabilities(),
        })
    }
}

/// What this backend can and cannot do, decided field by field.
fn capabilities() -> Capabilities {
    let mut c = Capabilities::default();
    // `WINHTTP_DISABLE_REDIRECTS` is set on every request, so a `3xx` is
    // an ordinary response and `Client`'s hop limit, its predicate and
    // its `Authorization` stripping all apply. This is the same answer
    // `hclient-urlsession` gives and for the same reason, and it is the
    // one place both are stronger than `hclient-fetch`, which has no way
    // to refuse a hop.
    c.redirects = RedirectSupport::Transparent;
    // Dropping the body closes the request handle, which is WinHTTP's
    // own cancellation. Nothing is spawned here, so there is no pump to
    // outlive the caller — see `body.rs`.
    c.cancel_on_drop = CancelSupport::Supported;
    // WinHTTP keeps connections in a per-session pool of its own. This
    // crate neither configures nor observes it; what the field says is
    // that a second request may reuse a connection, which is true.
    c.connection_reuse = ReuseSupport::Supported;
    // `WINHTTP_OPTION_DECOMPRESSION` is deliberately **not** set, so
    // nothing here decodes a `Content-Encoding` and `hclient`'s own
    // decompressor does the work on every backend alike. That is the
    // decision `hclient-fetch` and `hclient-urlsession` cannot make: both
    // must report `Internal` because their platform decodes underneath
    // them.
    c.response_decompression = DecompressionSupport::None;
    // SChannel's configuration is the machine's, and this crate exposes
    // no way to change it. `None` is the honest report of a stack whose
    // trust decisions are not the caller's to make here — which is the
    // whole reason somebody would choose this backend.
    c.tls_config = TlsSupport::None;
    // `WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY`: WPAD and any PAC script are
    // evaluated by the OS, per request. This is a *report* rather than a
    // gate, and it is the one this backend exists to be able to answer
    // `true` to.
    c.proxy = true;
    // `Response::version()` carries what the status line said, parsed
    // out of `WINHTTP_QUERY_RAW_HEADERS_CRLF` by the same parser
    // `hclient-proxy` uses. It is truthful because HTTP/2 is not enabled
    // — see the crate doc — so the connection really is the HTTP/1.x the
    // status line names.
    c.version_reported = true;
    c
}

impl Transport for WinHttp {
    type Body = WinHttpBody;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<WinHttpBody>, Error> {
        let (parts, body) = req.into_parts();
        let bytes = resolve_body(body)?;
        let (secure, host, port, target) = split_uri(&parts.uri)?;
        let headers = header_block(&parts.headers)?;

        let connect = self
            .session
            .connect(&host, port)
            .map_err(|e| setup("WinHttpConnect", e))?;
        let request = connect
            .open_request(parts.method.as_str(), &target, secure)
            .map_err(|e| setup("WinHttpOpenRequest", e))?;

        // Set first, so that every failure path below still reaches
        // `HANDLE_CLOSING` with a context to release — see `sys.rs`.
        let ex = Arc::new(Exchange::new());
        request
            .set_context(&ex)
            .map_err(|e| setup("WinHttpSetOption(CONTEXT_VALUE)", e))?;
        request
            .disable_redirects_and_cookies()
            .map_err(|e| setup("WinHttpSetOption(DISABLE_FEATURE)", e))?;
        request
            .add_headers(&headers)
            .map_err(|e| setup("WinHttpAddRequestHeaders", e))?;
        request
            .send(&ex, bytes)
            .map_err(|e| setup("WinHttpSendRequest", e))?;

        // From here to the head, dropping this future drops `request`,
        // which closes the handle and cancels whatever is in flight.
        expect(&ex, "SENDREQUEST_COMPLETE").await?;
        request
            .receive_response()
            .map_err(|e| setup("WinHttpReceiveResponse", e))?;
        expect(&ex, "HEADERS_AVAILABLE").await?;

        let raw = request
            .raw_headers()
            .map_err(|e| setup("WinHttpQueryHeaders", e))?;
        let head = match hclient_proto::head::parse_response(&raw)
            .map_err(|e| Error::new(ErrorKind::Body, WinHttpError::Head(e)))?
        {
            Some((head, _)) => head,
            // The head is WinHTTP's own copy of a message it has already
            // finished receiving, so an incomplete one is not "wait for
            // more" — there is no more.
            None => {
                return Err(Error::new(
                    ErrorKind::Body,
                    WinHttpError::Unsupported(
                        "WinHTTP handed back a response head that stops mid-message".to_owned(),
                    ),
                ));
            }
        };

        let mut resp = http::Response::new(WinHttpBody::new(
            request,
            connect,
            Arc::clone(&self.session),
            ex,
        ));
        *resp.status_mut() = head.status;
        *resp.version_mut() = head.version;
        *resp.headers_mut() = head.headers;
        Ok(resp)
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// The `Send` half of the seam.
///
/// A WinHTTP handle is not bound to the thread that made it, and the
/// completion callback is called on an arbitrary thread pool thread —
/// which is why the shared state is a `Mutex` pair rather than a cell. So
/// `execute`'s future is `Send` by inference and this is one line of
/// forwarding, exactly as it is for `hclient-urlsession`.
impl hclient_core::unversioned::SendTransport for WinHttp {
    fn execute_send(
        &self,
        req: http::Request<RequestBody>,
    ) -> hclient_core::unversioned::BoxSendExchange<'_, Self::Body, Self::Error> {
        Box::pin(<Self as Transport>::execute(self, req))
    }
}

/// A synchronous WinHTTP call that failed while setting the exchange up.
fn setup(call: &'static str, source: Win32Error) -> Error {
    Error::new(ErrorKind::Connect, WinHttpError::Call { call, source })
}

/// Wait for the next completion and insist it is the expected one.
async fn expect(ex: &Arc<Exchange>, expected: &'static str) -> Result<(), Error> {
    match poll_fn(|cx| ex.poll_next(cx)).await {
        Event::SendComplete if expected == "SENDREQUEST_COMPLETE" => Ok(()),
        Event::HeadersAvailable if expected == "HEADERS_AVAILABLE" => Ok(()),
        Event::SecureFailure(flags) => Err(Error::new(ErrorKind::Tls, WinHttpError::Tls(flags))),
        Event::Failed(code) => Err(Error::new(
            ErrorKind::Connect,
            WinHttpError::Request(Win32Error(code)),
        )),
        other => Err(Error::new(
            ErrorKind::Connect,
            WinHttpError::OutOfOrder {
                got: event_name(&other),
                expected,
            },
        )),
    }
}

/// The four things WinHTTP wants a request split into.
///
/// `WinHttpConnect` takes the host and port, `WinHttpOpenRequest` takes
/// the target and a secure flag — so the URI is taken apart here rather
/// than handed over whole, which is also what makes the default port a
/// decision this file can state.
fn split_uri(uri: &http::Uri) -> Result<(bool, String, u16, String), Error> {
    let refuse = |what: String| {
        Err(Error::new(
            ErrorKind::Connect,
            WinHttpError::Unsupported(what),
        ))
    };
    let secure = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        // A scheme WinHTTP has no flag for. Refused rather than guessed:
        // defaulting to `http` would send a request somewhere the caller
        // did not ask for.
        other => {
            return refuse(format!(
                "WinHTTP speaks `http` and `https`, and this request names `{}`",
                other.unwrap_or("no scheme")
            ));
        }
    };
    let Some(host) = uri.host() else {
        return refuse(format!("`{uri}` has no host for WinHttpConnect to name"));
    };
    // **`Uri::host` keeps the brackets ON an IPv6 literal**, and this
    // comment said the opposite for as long as it has existed — measured:
    // `"http://[::1]:8080/".parse::<Uri>().host()` is `Some("[::1]")`.
    // `WinHttpConnect` takes a host and not an authority, so the brackets
    // are RFC 3986 §3.2.2's URI syntax and must not reach it; with them,
    // every request to an IPv6 literal names a server that does not exist.
    //
    // Caught by this module's own unit test, which asserted the intended
    // behaviour and could not run anywhere but Windows — where, until this
    // week, the workspace did not compile at all.
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let target = uri
        .path_and_query()
        .map_or_else(|| "/".to_owned(), |p| p.as_str().to_owned());
    Ok((secure, host.to_owned(), port, target))
}

/// The caller's headers as one CRLF-delimited block.
///
/// **`Host` and `Content-Length` are left to WinHTTP**, which writes both
/// from the connect handle and from `WinHttpSendRequest`'s length
/// argument. Sending a second copy of either is the one way a header
/// block can make a message ambiguous rather than merely wrong.
///
/// A value that is not text is **refused, never skipped**. `http` allows
/// any visible byte in a header value and WinHTTP takes a wide string, so
/// there are values this boundary cannot carry — and dropping one would
/// send a request the caller did not write, which is the silent-omission
/// defect this workspace refuses everywhere.
fn header_block(headers: &http::HeaderMap) -> Result<String, Error> {
    let mut out = String::new();
    for (name, value) in headers {
        if name == http::header::HOST || name == http::header::CONTENT_LENGTH {
            continue;
        }
        let Ok(value) = value.to_str() else {
            return Err(Error::new(
                ErrorKind::Unsupported,
                WinHttpError::Unsupported(format!(
                    "`{name}` carries bytes that are not text, and WinHTTP's header API takes a \
                     wide string"
                )),
            ));
        };
        out.push_str(name.as_str());
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    Ok(out)
}

// The rewind bound moved to `hclient_core::MAX_REWIND_DEPTH`: this
// crate and `hclient-wasi` had each picked 16, and two other backends had
// no bound at all.

/// The bytes to put on the request, or `None` for no body at all.
///
/// **A body this backend cannot send is a typed error, never a silent
/// drop.** `Client` does not gate on
/// `Capabilities::streaming_request_body`, so a `Streaming` body reaching
/// here would otherwise go out as a request with no body: the request
/// would succeed and the payload would simply be gone.
///
/// Streaming is a real absence rather than a wall — `WinHttpWriteData`
/// exists, and the crate doc says what taking it would cost.
fn resolve_body(body: RequestBody) -> Result<Option<bytes::Bytes>, Error> {
    let refuse = |what: &str| {
        Err(Error::new(
            ErrorKind::Unsupported,
            WinHttpError::Unsupported(format!(
                "this backend sends a buffered body only, and this request carries {what}; \
                 `Capabilities::streaming_request_body` is `false` and says so"
            )),
        ))
    };
    // The unwrapping and its depth bound are `hclient_core`'s. This crate
    // had picked 16 and refused, and so had `hclient-wasi`, while
    // `hclient-native` and its HTTP/3 pump recursed without a bound —
    // `RequestBody::reduce` settles it once, and a body that streams is
    // still this backend's own refusal, because that is a fact about
    // WinHTTP rather than about the body.
    match body.reduce().map_err(|e| {
        Error::new(
            ErrorKind::Unsupported,
            WinHttpError::Unsupported(e.to_string()),
        )
    })? {
        hclient_core::Reduced::Empty => Ok(None),
        hclient_core::Reduced::Bytes(b) => Ok(Some(b)),
        hclient_core::Reduced::Streaming(_) => refuse("a streaming one"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(s: &str) -> http::Uri {
        s.parse().expect("a test URI")
    }

    #[test]
    fn a_default_port_comes_from_the_scheme() {
        let (secure, host, port, target) = split_uri(&uri("https://example.com/a?b=1")).unwrap();
        assert!(secure);
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(target, "/a?b=1");

        let (secure, _, port, _) = split_uri(&uri("http://example.com")).unwrap();
        assert!(!secure);
        assert_eq!(port, 80);
    }

    /// `WinHttpConnect` takes a host, not an authority, so the brackets
    /// an IPv6 literal wears in a URI must not reach it.
    #[test]
    fn an_ipv6_literal_loses_its_brackets() {
        let (_, host, port, _) = split_uri(&uri("http://[::1]:8080/")).unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 8080);
    }

    /// A URI with no path is `/` on the wire. WinHTTP would accept an
    /// empty target and send a malformed request line.
    #[test]
    fn an_absent_path_becomes_a_slash() {
        let (_, _, _, target) = split_uri(&uri("http://example.com")).unwrap();
        assert_eq!(target, "/");
    }

    /// A scheme WinHTTP has no flag for is refused rather than guessed:
    /// defaulting to `http` would send the request somewhere the caller
    /// did not ask for.
    #[test]
    fn a_scheme_winhttp_cannot_speak_is_refused() {
        let e = split_uri(&uri("ftp://example.com/x")).unwrap_err();
        assert!(
            format!("{e}").contains("ftp") || format!("{:?}", e).contains("ftp"),
            "the refusal must name the scheme it refused: {e:?}"
        );
    }

    /// Both are WinHTTP's to write — from the connect handle and from
    /// `WinHttpSendRequest`'s length — and a second copy of either is the
    /// one way a header block makes a message ambiguous.
    #[test]
    fn host_and_content_length_are_left_to_winhttp() {
        let mut h = http::HeaderMap::new();
        h.insert(http::header::HOST, "elsewhere.example".parse().unwrap());
        h.insert(http::header::CONTENT_LENGTH, "99".parse().unwrap());
        h.insert(http::header::ACCEPT, "text/plain".parse().unwrap());
        assert_eq!(header_block(&h).unwrap(), "accept: text/plain\r\n");
    }

    #[test]
    fn no_headers_is_an_empty_block() {
        assert_eq!(header_block(&http::HeaderMap::new()).unwrap(), "");
    }

    /// A value `http` allows and a wide string cannot carry is refused,
    /// never skipped — dropping it would send a request the caller did
    /// not write.
    #[test]
    fn a_header_value_that_is_not_text_is_refused() {
        let mut h = http::HeaderMap::new();
        h.insert(
            http::header::ACCEPT,
            http::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        let e = header_block(&h).unwrap_err();
        assert_eq!(*e.kind(), ErrorKind::Unsupported);
    }

    /// A body with nothing in it, which is still a `Streaming` one — the
    /// point is the variant, not the payload.
    #[derive(Debug)]
    struct NoBytes;

    impl http_body::Body for NoBytes {
        type Data = bytes::Bytes;
        type Error = Error;

        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, Error>>> {
            std::task::Poll::Ready(None)
        }
    }

    #[test]
    fn a_streaming_body_is_refused_rather_than_dropped() {
        let e = resolve_body(RequestBody::Streaming(Box::new(NoBytes))).unwrap_err();
        assert_eq!(*e.kind(), ErrorKind::Unsupported);
    }

    /// The `Rewindable` case that matters: a factory handing back a
    /// buffered body is unwrapped rather than refused.
    #[test]
    fn a_rewindable_body_over_bytes_is_unwrapped() {
        let b = bytes::Bytes::from_static(b"hi");
        let body = RequestBody::Rewindable(std::sync::Arc::new({
            let b = b.clone();
            move || RequestBody::Full(b.clone())
        }));
        assert_eq!(resolve_body(body).unwrap(), Some(b));
    }

    #[test]
    fn a_buffered_body_goes_through() {
        assert_eq!(resolve_body(RequestBody::Empty).unwrap(), None);
        let b = bytes::Bytes::from_static(b"hello");
        assert_eq!(resolve_body(RequestBody::Full(b.clone())).unwrap(), Some(b));
    }

    /// The capability set is the crate doc's claims, in the one place a
    /// caller can read them off a value.
    #[test]
    fn the_capabilities_say_what_the_crate_doc_says() {
        let c = capabilities();
        assert_eq!(c.redirects, RedirectSupport::Transparent);
        assert_eq!(c.cancel_on_drop, CancelSupport::Supported);
        assert_eq!(c.response_decompression, DecompressionSupport::None);
        assert_eq!(c.tls_config, TlsSupport::None);
        assert!(c.proxy, "the whole reason this backend exists");
        assert!(!c.owns_cookie_jar, "WINHTTP_DISABLE_COOKIES is set");
        assert!(!c.owns_cache, "WinHTTP has no response cache");
        assert!(!c.streaming_request_body, "and `resolve_body` says so too");
    }
}
