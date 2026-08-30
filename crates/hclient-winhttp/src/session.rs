//! The transport: one WinHTTP session, one exchange per request.

use std::future::poll_fn;
use std::sync::Arc;

use hclient_core::unversioned::Transport;
use hclient_core::{
    CancelSupport, Capabilities, DecompressionSupport, Error, ErrorKind, RedirectSupport,
    RequestBody, RequireVersion, ReuseSupport, TlsSupport, check_version,
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
    protocols: Protocols,
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
            protocols: Protocols::default(),
            caps: capabilities(),
        })
    }

    /// The advanced HTTP versions this transport may negotiate.
    ///
    /// Off by default, and that is the same decision `hclient-native`
    /// makes about its own QUIC arm: **turning one on changes what every
    /// request puts on the wire** — the ALPN offer, and with HTTP/3 the
    /// transport protocol underneath it — so it is a caller's to make
    /// rather than a default to inherit.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), hclient_core::Error> {
    /// use hclient_winhttp::{Protocols, WinHttp};
    ///
    /// let transport = WinHttp::new()?.protocols(Protocols::HTTP2);
    ///
    /// // Or both, which is what a caller who wants whatever is fastest
    /// // asks for:
    /// let fastest = WinHttp::new()?.protocols(Protocols::HTTP2 | Protocols::HTTP3);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # What it costs and what it does not
    ///
    /// Nothing at construction: the mask is set per request, so a
    /// transport that enables nothing makes exactly the calls it made
    /// before this method existed. HTTP/1.1 is not switchable — WinHTTP
    /// documents the mask's `0x0` default as *"restricts the request to
    /// HTTP/1.1 and prior"* — so this widens what may be negotiated and
    /// never narrows it.
    ///
    /// # A version this Windows has never heard of is a refusal
    ///
    /// HTTP/3 reaches WinHTTP later than HTTP/2, and both are later than
    /// the option that carries them. An OS without one answers
    /// `ERROR_WINHTTP_INVALID_OPTION`, and this crate reports it rather
    /// than continuing over HTTP/1.1 — .NET's `WinHttpHandler` logs *"HTTP/2
    /// option not supported"* and carries on, which is the *silently
    /// ignored setting* this workspace closes wherever it finds one. A
    /// caller who wants the fallback asks for it by not asking for the
    /// protocol.
    #[must_use]
    pub fn protocols(mut self, protocols: Protocols) -> Self {
        self.protocols = protocols;
        self
    }

    /// Keeps an idle HTTP/2 or HTTP/3 connection alive, in the OS.
    ///
    /// After `every` of no activity WinHTTP sends HTTP/2 `PING` frames or
    /// QUIC keep-alives on the connection. It is the answer this backend
    /// can give and `hclient-native` cannot give for free: there,
    /// `h2_keep_alive` needs `multiplexed()` and a spawned driver to send
    /// the ping, and here the connection is WinHTTP's and so is the
    /// clock.
    ///
    /// # Two refusals rather than a rounded value
    ///
    /// WinHTTP documents a floor of five seconds on the HTTP/2 option —
    /// *"callers cannot set a timeout value less than 5000
    /// milliseconds"* — so a shorter interval is refused **naming the
    /// floor** rather than raised to it. Raising it would be this crate
    /// answering a question the caller asked, which is the reason
    /// `Standard::max_retry_after` stops a retry rather than shortening
    /// the wait one crate over. A duration that does not fit a `DWORD` of
    /// milliseconds is refused for the same reason.
    ///
    /// # It is inert without [`protocols`](Self::protocols)
    ///
    /// Both options are HTTP/2's and HTTP/3's, and a transport that
    /// negotiates neither has no connection for them to describe. Said
    /// here rather than refused, exactly as `Native::h2_keep_alive`
    /// without `multiplexed()` is: the two setters are independent, and a
    /// refusal would make their order part of the API.
    pub fn keep_alive(self, every: std::time::Duration) -> Result<Self, Error> {
        const FLOOR: std::time::Duration = std::time::Duration::from_secs(5);
        if every < FLOOR {
            return Err(Error::new(
                ErrorKind::Unsupported,
                WinHttpError::Unsupported(format!(
                    "WinHTTP's HTTP/2 keep-alive has a floor of {FLOOR:?} and this asks for {every:?}"
                )),
            ));
        }
        let millis = u32::try_from(every.as_millis()).map_err(|_| {
            Error::new(
                ErrorKind::Unsupported,
                WinHttpError::Unsupported(format!(
                    "WinHTTP takes a keep-alive in milliseconds as a DWORD, and {every:?} does not fit one"
                )),
            )
        })?;
        self.session
            .set_keep_alive(millis)
            .map_err(|e| setup("WinHttpSetOption(HTTP2_KEEPALIVE)", e))?;
        Ok(self)
    }
}

/// The advanced HTTP versions a request may negotiate, above HTTP/1.1.
///
/// WinHTTP's `WINHTTP_OPTION_ENABLE_HTTP_PROTOCOL` bitmask, as two
/// fields: `WINHTTP_PROTOCOL_FLAG_HTTP2` (`0x1`) and
/// `WINHTTP_PROTOCOL_FLAG_HTTP3` (`0x2`). Both off is the mask's
/// documented default and means HTTP/1.1 and prior, which is what this
/// transport does unless [`WinHttp::protocols`] says otherwise.
///
/// **A bitmask rather than a struct of `bool`s, because that is what it
/// is** — and because the two shapes differ in what a later version costs.
/// A third protocol is a new constant here, which no caller has to know
/// about; as a `pub` field it would be a new field in a struct callers
/// write as a literal, and only the ones who remembered
/// `..Default::default()` would survive it. `TcpOpts`' argument for
/// staying exhaustive was about exactly that expression, and a bitmask
/// does not need it.
///
/// `empty()` is the default, which is WinHTTP's own: the option's
/// documented `0x0` means HTTP/1.1 and prior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Protocols(ProtocolFlags);

bitflags::bitflags! {
    /// The bits themselves, kept private so that [`Protocols`] is the
    /// only spelling a caller meets.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    struct ProtocolFlags: u32 {
        const HTTP2 = windows_sys::Win32::Networking::WinHttp::WINHTTP_PROTOCOL_FLAG_HTTP2;
        const HTTP3 = windows_sys::Win32::Networking::WinHttp::WINHTTP_PROTOCOL_FLAG_HTTP3;
    }
}

impl Protocols {
    /// Neither — HTTP/1.1 and prior, WinHTTP's documented default.
    pub const NONE: Self = Self(ProtocolFlags::empty());
    /// Offer HTTP/2.
    pub const HTTP2: Self = Self(ProtocolFlags::HTTP2);
    /// Offer HTTP/3.
    ///
    /// QUIC, so UDP: a network that blocks it is a network where this
    /// costs a fallback rather than a failure — WinHTTP still offers
    /// HTTP/1.1, and HTTP/2 if that is set too — unless a
    /// [`RequireVersion`] demand has taken the fallback away, which is
    /// what that demand is for.
    pub const HTTP3: Self = Self(ProtocolFlags::HTTP3);

    /// Both, which is what a caller who wants whatever is fastest asks
    /// for.
    #[must_use]
    pub const fn all() -> Self {
        Self(ProtocolFlags::all())
    }

    /// Whether HTTP/2 is offered.
    #[must_use]
    pub const fn http2(self) -> bool {
        self.0.contains(ProtocolFlags::HTTP2)
    }

    /// Whether HTTP/3 is offered.
    #[must_use]
    pub const fn http3(self) -> bool {
        self.0.contains(ProtocolFlags::HTTP3)
    }
}

impl std::ops::BitOr for Protocols {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl Protocols {
    /// The bitmask WinHTTP wants.
    fn bits(self) -> u32 {
        self.0.bits()
    }
}

/// What `WINHTTP_OPTION_HTTP_PROTOCOL_USED` said, as a version.
///
/// `0` is *neither*, which is HTTP/1.1 or prior — so the answer is the
/// status line's, which the head parser already read. HTTP/3 is tested
/// first because the two flags are independent bits and a value carrying
/// both would otherwise be reported as the weaker of the two.
fn version_used(flags: u32, from_status_line: http::Version) -> http::Version {
    use windows_sys::Win32::Networking::WinHttp as w;
    if flags & w::WINHTTP_PROTOCOL_FLAG_HTTP3 != 0 {
        http::Version::HTTP_3
    } else if flags & w::WINHTTP_PROTOCOL_FLAG_HTTP2 != 0 {
        http::Version::HTTP_2
    } else {
        from_status_line
    }
}

/// The mask for one request, and whether WinHTTP must refuse to fall off
/// it.
///
/// A **pure function of what this transport enables and what this request
/// demands**, taking the first as a parameter rather than reading it off
/// `self` — which is `hc --backend`'s lesson written down as a signature:
/// the refusing arms are unreachable in the configuration that enables
/// everything, so a decision that can only be exercised through a live
/// request is a decision no test can reach.
///
/// # A demand narrows the mask rather than being checked against it
///
/// [`RequireVersion`] is an exact match, so `RequireVersion(HTTP_2)`
/// leaves `0x1` and takes HTTP/3 *off* even where the transport offers
/// it. The `bool` is `WINHTTP_OPTION_HTTP_PROTOCOL_REQUIRED`, which turns
/// the narrowed mask from an offer into a condition: without it WinHTTP
/// would fall back to HTTP/1.1 and the demand could only be *noticed*
/// after the head, which is [`check_version`]'s own definition of a check
/// placed too late.
///
/// `HTTP_11` is answered by the mask alone — `0x0` is documented as
/// *"restricts the request to HTTP/1.1 and prior"*, so there is nothing
/// for `REQUIRED` to add. Every other version, and an `HTTP_2` or
/// `HTTP_3` demand this transport does not offer, is refused here with
/// the type a connection that negotiated the wrong protocol raises,
/// before a socket exists — which is `hclient-native`'s `route` doing the
/// same thing for the same reason.
fn mask_for(enabled: Protocols, extensions: &http::Extensions) -> Result<(u32, bool), Error> {
    use windows_sys::Win32::Networking::WinHttp as w;
    let Some(&RequireVersion(demanded)) = extensions.get::<RequireVersion>() else {
        return Ok((enabled.bits(), false));
    };
    match demanded {
        http::Version::HTTP_2 if enabled.http2() => Ok((w::WINHTTP_PROTOCOL_FLAG_HTTP2, true)),
        http::Version::HTTP_3 if enabled.http3() => Ok((w::WINHTTP_PROTOCOL_FLAG_HTTP3, true)),
        http::Version::HTTP_11 => Ok((0, false)),
        _ => Err(check_version(extensions, http::Version::HTTP_11)
            .expect_err("a demand for HTTP/1.1 is answered by the arm above")),
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
    // `Response::version()` is read from `WINHTTP_OPTION_HTTP_PROTOCOL_
    // USED`, falling back to the status line — which is the one place
    // this backend cannot take the obvious route. **An HTTP/2 or HTTP/3
    // response has no status line**, and WinHTTP synthesises `HTTP/1.1`
    // into the raw header block, so a client reading
    // `WINHTTP_QUERY_VERSION` reports every h2 and h3 response as
    // HTTP/1.1 — a capability that lies rather than one that
    // under-reports. The option is queried on every response rather than
    // only where `protocols` enabled something, so the answer does not
    // rest on the mask's documented `0x0` default still being the
    // default on some future Windows.
    c.version_reported = true;
    // `true` is *honours a demand*, not *chooses a version* — the
    // distinction `hclient-h3` is the standing example of, reporting
    // `true` while speaking one protocol. This backend honours
    // `RequireVersion` by narrowing the mask before the request is sent
    // and asking WinHTTP to refuse a fallback off it (`mask_for`), and
    // refuses a version it cannot offer with the same `VersionNotAvailable`
    // a mismatch raises.
    //
    // **It was `false` before there were protocols to select between**,
    // and that was an under-claim rather than a lie: `Client` refused
    // every demand, including `RequireVersion(HTTP_11)`, which this
    // backend has always satisfied trivially. `caps.rs` names exactly
    // that as the failure mode to avoid.
    c.version_select = true;
    c
}

impl WinHttp {
    /// The session every handle here is derived from, for
    /// `websocket.rs`'s handshake.
    pub(crate) fn session(&self) -> &Session {
        &self.session
    }
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
        // Set before the headers and the send, because it decides what
        // goes on the wire rather than describing it. A demand that this
        // transport cannot offer has already been refused by `mask_for`,
        // with no handle open and nothing sent.
        let (mask, required) = mask_for(self.protocols, &parts.extensions)?;
        request
            .set_protocols(mask)
            .map_err(|e| setup("WinHttpSetOption(ENABLE_HTTP_PROTOCOL)", e))?;
        if required {
            request
                .require_protocols()
                .map_err(|e| setup("WinHttpSetOption(HTTP_PROTOCOL_REQUIRED)", e))?;
        }
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

        // What WinHTTP *used*, which the status line cannot say for h2
        // or h3 — see `capabilities`. A failure here is an older Windows
        // that does not have the option, and an OS with no option for
        // advanced protocols used none, so `0` is the honest reading of
        // a refusal rather than an error to report.
        let version = version_used(request.protocol_used().unwrap_or(0), head.version);
        // Checked rather than trusted, and it is the second half of one
        // guarantee rather than a second guarantee: `mask_for` asked
        // WinHTTP to refuse a fallback off the narrowed mask, and this
        // confirms it did. It is the one obligation in this crate
        // resting on an undocumented buffer type — see
        // `Request::require_protocols` — so it is the one worth
        // confirming. A violation caught here is late, the request
        // having been sent; handing back a response over a version the
        // caller ruled out would be later still.
        //
        // **Only where a fallback was forbidden**, which keeps this from
        // quietly becoming a second rule. A `RequireVersion(HTTP_11)`
        // demand is answered by the mask alone, and rechecking it here
        // would compare against the *status line* — so a server
        // answering `HTTP/1.0` would fail a demand `hclient-native`
        // satisfies, on a difference that has nothing to do with the
        // protocols this option selects between.
        if required {
            check_version(&parts.extensions, version)?;
        }

        let mut resp = http::Response::new(WinHttpBody::new(
            request,
            connect,
            Arc::clone(&self.session),
            ex,
        ));
        *resp.status_mut() = head.status;
        *resp.version_mut() = version;
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
pub(crate) fn setup(call: &'static str, source: Win32Error) -> Error {
    Error::new(ErrorKind::Connect, WinHttpError::Call { call, source })
}

/// Wait for the next completion and insist it is the expected one.
pub(crate) async fn expect(ex: &Arc<Exchange>, expected: &'static str) -> Result<(), Error> {
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
pub(crate) fn split_uri(uri: &http::Uri) -> Result<(bool, String, u16, String), Error> {
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
pub(crate) fn header_block(headers: &http::HeaderMap) -> Result<String, Error> {
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
    use windows_sys::Win32::Networking::WinHttp as w;

    /// `mask_for` takes what is enabled as a parameter, so every arm is
    /// reachable at any feature setting and on any Windows — which is
    /// the whole reason it is not a method. These run only on Windows,
    /// because the crate is `#![cfg(windows)]`; a Linux host builds them
    /// through `--target x86_64-pc-windows-msvc` and runs none of it.
    fn demanding(v: http::Version) -> http::Extensions {
        let mut e = http::Extensions::new();
        e.insert(RequireVersion(v));
        e
    }

    const BOTH: Protocols = Protocols::all();

    #[test]
    fn with_no_demand_a_request_offers_what_the_transport_enables() {
        let none = http::Extensions::new();
        assert_eq!(mask_for(Protocols::default(), &none).unwrap(), (0, false));
        assert_eq!(
            mask_for(BOTH, &none).unwrap(),
            (
                w::WINHTTP_PROTOCOL_FLAG_HTTP2 | w::WINHTTP_PROTOCOL_FLAG_HTTP3,
                false
            )
        );
    }

    /// The demand is an **exact match**, so it takes the other protocol
    /// off a transport that offers both rather than being checked against
    /// the offer. A `mask_for` that passed `enabled.bits()` through and
    /// only refused what it could not serve would satisfy
    /// `RequireVersion(HTTP_2)` with an HTTP/3 connection.
    #[test]
    fn a_demand_narrows_the_mask_to_itself_and_forbids_the_fallback() {
        assert_eq!(
            mask_for(BOTH, &demanding(http::Version::HTTP_2)).unwrap(),
            (w::WINHTTP_PROTOCOL_FLAG_HTTP2, true)
        );
        assert_eq!(
            mask_for(BOTH, &demanding(http::Version::HTTP_3)).unwrap(),
            (w::WINHTTP_PROTOCOL_FLAG_HTTP3, true)
        );
    }

    /// `0x0` is documented as *"restricts the request to HTTP/1.1 and
    /// prior"*, so the mask is the whole answer and there is nothing for
    /// `HTTP_PROTOCOL_REQUIRED` to add. The transport offering HTTP/2 is
    /// the half that matters: without the narrowing, a request that said
    /// it could not use HTTP/2 would be offered HTTP/2.
    #[test]
    fn a_demand_for_http_1_1_takes_the_advanced_versions_off() {
        assert_eq!(
            mask_for(BOTH, &demanding(http::Version::HTTP_11)).unwrap(),
            (0, false)
        );
    }

    /// A demand this transport cannot offer is refused before a handle
    /// exists, with `VersionNotAvailable` rather than a second spelling
    /// of it — which is what `version_select: true` promises.
    #[test]
    fn a_version_this_transport_cannot_offer_is_refused_before_anything_is_sent() {
        for (enabled, demand) in [
            (Protocols::default(), http::Version::HTTP_2),
            (Protocols::default(), http::Version::HTTP_3),
            (Protocols::HTTP2, http::Version::HTTP_3),
            (BOTH, http::Version::HTTP_10),
        ] {
            let e = mask_for(enabled, &demanding(demand)).expect_err("not offered");
            assert_eq!(*e.kind(), ErrorKind::Unsupported, "{demand:?}");
        }
    }

    /// `WINHTTP_OPTION_HTTP_PROTOCOL_USED` is the only thing that can say
    /// which of the three answered, and `0` — an older Windows without
    /// the option included — means the status line was right after all.
    #[test]
    fn the_version_comes_from_the_protocol_used_and_falls_back_to_the_status_line() {
        let line = http::Version::HTTP_11;
        assert_eq!(version_used(0, line), http::Version::HTTP_11);
        assert_eq!(
            version_used(0, http::Version::HTTP_10),
            http::Version::HTTP_10
        );
        assert_eq!(
            version_used(w::WINHTTP_PROTOCOL_FLAG_HTTP2, line),
            http::Version::HTTP_2
        );
        assert_eq!(
            version_used(w::WINHTTP_PROTOCOL_FLAG_HTTP3, line),
            http::Version::HTTP_3
        );
        // Both bits at once is not a shape WinHTTP is documented to
        // report, and reporting the *weaker* of two would be the wrong
        // direction if it ever did.
        assert_eq!(
            version_used(
                w::WINHTTP_PROTOCOL_FLAG_HTTP2 | w::WINHTTP_PROTOCOL_FLAG_HTTP3,
                line
            ),
            http::Version::HTTP_3
        );
    }

    /// The pair `caps.rs` makes a biconditional one crate over: this
    /// backend reads the version off the OS and answers demands, so both
    /// are `true` and neither may be moved without the other being
    /// re-argued.
    #[test]
    fn the_version_capabilities_are_both_claimed() {
        let c = capabilities();
        assert!(c.version_reported);
        assert!(c.version_select);
    }

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
