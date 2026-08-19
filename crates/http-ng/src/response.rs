use bytes::{Bytes, BytesMut};
use http_body::Body as HttpBody;
use http_ng_core::{Error, ErrorKind};
use std::pin::Pin;

/// A response with its URL preserved. `into_parts` gives full fidelity;
/// `chunk`/`collect` are convenience on top of it.
#[derive(Debug)]
pub struct Response<B> {
    parts: http::response::Parts,
    body: B,
    url: http::Uri,
    /// Set once `chunk()` has returned `Some(Err(_))`, after which
    /// `chunk()` returns `None`, never touching `body` again.
    ///
    /// m6 of the branch's final review: without it, `chunk()` after an
    /// error polled the underlying body again, and a caller working with
    /// `Response::chunk` directly could spin in a loop over a body that
    /// returns an error on every poll. `SseStream` compensated for this
    /// with its own `done` flag and tested it thoroughly — but only for
    /// itself. Terminality here is exactly the same: the error is handed
    /// back once, then it's the end of the stream.
    sealed: bool,
}

/// A `4xx` or `5xx` the caller asked to be told about.
///
/// Carries the URL as well as the status, because by the time a caller is
/// looking at one they have usually stopped holding the response — and a
/// chain of redirects means the URL that failed is not the one they typed.
#[derive(Debug, Clone, thiserror::Error)]
#[error("the server answered {status} for {url}")]
pub struct UnexpectedStatus {
    pub status: http::StatusCode,
    pub url: http::Uri,
}

impl<B> Response<B> {
    pub(crate) fn new(resp: http::Response<B>, url: http::Uri) -> Self {
        let (parts, body) = resp.into_parts();
        Self {
            parts,
            body,
            url,
            sealed: false,
        }
    }
    /// `Ok(self)` for a `1xx`/`2xx`/`3xx`, and an
    /// [`ErrorKind::Status`] error for a `4xx` or `5xx`.
    ///
    /// # Why it takes `self` where nothing else here does
    ///
    /// The rest of this type is built around *not* consuming the response:
    /// `Collected` keeps the status, the headers and the URL after the
    /// body has been read, which is reqwest issue #1542 answered. This one
    /// consumes, and that is not an inconsistency — the whole point is
    /// that a caller who writes `?` after it is choosing to stop having a
    /// response. A `&self` form would leave the failed response in hand,
    /// which is what the caller just said they did not want.
    ///
    /// # What it is not
    ///
    /// Not a client setting, and deliberately so. reqwest, ureq and curl
    /// all put this at the call site rather than in a builder, and the
    /// reason is that `404` is a normal answer for about half the requests
    /// ever made — a client-wide *treat every 4xx as an error* would turn
    /// a HEAD probe or a conditional GET into a failure. The caller knows
    /// which of their requests has a status they can act on; a builder
    /// does not.
    ///
    /// A `3xx` is `Ok`, because reaching one means the redirect policy
    /// already decided to hand it back — see
    /// [`RedirectPolicy::None`](http_ng_proto::redirect::RedirectPolicy::
    /// None), where a `3xx` is stated to be the caller's answer rather than
    /// a failure to reach one. Treating it as an error here would overrule
    /// that from two layers up.
    ///
    /// [`ErrorKind::Status`]: http_ng_core::ErrorKind::Status
    pub fn error_for_status(self) -> Result<Self, Error> {
        if self.status().is_client_error() || self.status().is_server_error() {
            return Err(Error::new(
                ErrorKind::Status,
                UnexpectedStatus {
                    status: self.status(),
                    url: self.url().clone(),
                },
            ));
        }
        Ok(self)
    }

    pub fn status(&self) -> http::StatusCode {
        self.parts.status
    }
    pub fn headers(&self) -> &http::HeaderMap {
        &self.parts.headers
    }
    pub fn version(&self) -> http::Version {
        self.parts.version
    }
    /// **The URL this answer came from**, which is the last hop of a
    /// redirect chain rather than the one the caller asked for. The two
    /// differ exactly when a redirect was followed, and then the caller
    /// already has the first — they typed it.
    ///
    /// It used to be the requested URL, undocumented and untested, which
    /// meant `Response::url()` and `Collected::url()` answered *"where did
    /// you send this"* under a name that reads *"where did this come
    /// from"*. Found by writing [`Self::error_for_status`], whose error
    /// carries this value and is useless carrying the wrong one.
    pub fn url(&self) -> &http::Uri {
        &self.url
    }
    pub fn into_parts(self) -> (http::response::Parts, B) {
        (self.parts, self.body)
    }
}

impl<B> Response<B>
where
    B: HttpBody<Data = Bytes> + Unpin,
    // `B::Error: Send + Sync` — the second exception point in the "core
    // declares no Send/Sync" invariant, sibling of the bound on
    // `Client::execute` in `client.rs` (spec amendment-C1). Without it,
    // `Error::new(ErrorKind::Body, e)` below wouldn't compile: `Error`
    // stores its source as `Arc<dyn Error + Send + Sync>`, and type erasure
    // doesn't let an unbounded trait object's auto-traits through. The same
    // bound as `Client::execute`, only this time required on
    // `T::Body::Error` rather than `T::Error` — the body is read after the
    // transport has already returned it.
    B::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    /// The next data chunk. Trailer frames are skipped — for those, go
    /// through `into_parts` and poll the body directly.
    ///
    /// The error is terminal: after `Some(Err(_))` the body is sealed and
    /// all subsequent calls return `None` without polling it again (m6 of
    /// the branch's final review — see the `sealed` field).
    pub async fn chunk(&mut self) -> Option<Result<Bytes, Error>> {
        if self.sealed {
            return None;
        }
        loop {
            let frame = std::future::poll_fn(|cx| Pin::new(&mut self.body).poll_frame(cx)).await;
            match frame {
                Some(Ok(f)) => match f.into_data() {
                    Ok(d) => return Some(Ok(d)),
                    Err(_) => continue, // trailers
                },
                Some(Err(e)) => {
                    self.sealed = true;
                    return Some(Err(classify_body_error(e)));
                }
                None => {
                    self.sealed = true;
                    return None;
                }
            }
        }
    }

    pub async fn collect(mut self) -> Result<Collected, Error> {
        let mut acc = BytesMut::new();
        while let Some(c) = self.chunk().await {
            acc.extend_from_slice(&c?);
        }
        Ok(Collected {
            parts: self.parts,
            url: self.url,
            body: acc.freeze(),
        })
    }
}

/// Classifies a response body's read error — the response-half twin of
/// `Transport::to_error`'s default (`http-ng-core/src/unversioned/
/// transport.rs`), and for the same reason: if `e` is already our own
/// `Error`, its `kind()` was set at the point the backend actually
/// classified the failure (`ErrorKind::Cancelled` from a shutting-down
/// runtime, `ErrorKind::Tls` from a mid-stream handshake failure — whatever
/// it genuinely was), and re-wrapping it here would be exactly finding B2 of
/// vertical 1's final review, reproduced one seam later: `kind()` becomes
/// `Body` for everything, every `is_*` predicate lies, and `Display` prints
/// the category twice. Only a body whose error type carries no category of
/// its own — the common case for backends whose bodies are plain
/// `std::io::Error` or similar — falls back to `ErrorKind::Body`, which
/// remains the right default for a genuinely opaque body failure.
///
/// Found by vertical 2's final review (finding F2): `chunk()` used to wrap
/// unconditionally, and nothing in the test suite noticed, because
/// `NativeBody::poll_frame`'s own fallback already defaults to
/// `ErrorKind::Body` — the double-wrap was invisible by coincidence, not
/// because it was harmless. `Body`'s own `chunk_is_terminal_after_an_error_
/// and_does_not_poll_the_body_again` (in `tests/response.rs`) now pins the
/// non-coincidental case directly.
pub(crate) fn classify_body_error<E>(e: E) -> Error
where
    E: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    let boxed: Box<dyn std::any::Any> = Box::new(e);
    match boxed.downcast::<Error>() {
        Ok(already_ours) => *already_ours,
        Err(foreign) => Error::new(
            ErrorKind::Body,
            *foreign.downcast::<E>().unwrap_or_else(|_| {
                // Unreachable, and not an invariant spanning two distant
                // places: established three lines above in the same
                // expression — boxed exactly `E`, the first downcast
                // missed, so the second is guaranteed to hit.
                unreachable!("boxed exactly E three lines above")
            }),
        ),
    }
}

/// The body read **together with** the status, headers, and URL.
///
/// reqwest's `Response::{text,json,bytes}` take `self` by value, which
/// leaves the status unreachable once the body's been read (issue #1542).
#[derive(Debug, Clone)]
pub struct Collected {
    parts: http::response::Parts,
    url: http::Uri,
    body: Bytes,
}

impl Collected {
    /// `Ok(self)` for a `1xx`/`2xx`/`3xx`, and an
    /// [`ErrorKind::Status`] error for a `4xx` or `5xx`.
    ///
    /// # Why it takes `self` where nothing else here does
    ///
    /// The rest of this type is built around *not* consuming the response:
    /// `Collected` keeps the status, the headers and the URL after the
    /// body has been read, which is reqwest issue #1542 answered. This one
    /// consumes, and that is not an inconsistency — the whole point is
    /// that a caller who writes `?` after it is choosing to stop having a
    /// response. A `&self` form would leave the failed response in hand,
    /// which is what the caller just said they did not want.
    ///
    /// # What it is not
    ///
    /// Not a client setting, and deliberately so. reqwest, ureq and curl
    /// all put this at the call site rather than in a builder, and the
    /// reason is that `404` is a normal answer for about half the requests
    /// ever made — a client-wide *treat every 4xx as an error* would turn
    /// a HEAD probe or a conditional GET into a failure. The caller knows
    /// which of their requests has a status they can act on; a builder
    /// does not.
    ///
    /// A `3xx` is `Ok`, because reaching one means the redirect policy
    /// already decided to hand it back — see
    /// [`RedirectPolicy::None`](http_ng_proto::redirect::RedirectPolicy::
    /// None), where a `3xx` is stated to be the caller's answer rather than
    /// a failure to reach one. Treating it as an error here would overrule
    /// that from two layers up.
    ///
    /// [`ErrorKind::Status`]: http_ng_core::ErrorKind::Status
    pub fn error_for_status(self) -> Result<Self, Error> {
        if self.status().is_client_error() || self.status().is_server_error() {
            return Err(Error::new(
                ErrorKind::Status,
                UnexpectedStatus {
                    status: self.status(),
                    url: self.url().clone(),
                },
            ));
        }
        Ok(self)
    }

    pub fn status(&self) -> http::StatusCode {
        self.parts.status
    }
    pub fn headers(&self) -> &http::HeaderMap {
        &self.parts.headers
    }
    pub fn url(&self) -> &http::Uri {
        &self.url
    }
    pub fn bytes(&self) -> &Bytes {
        &self.body
    }
    /// The body as UTF-8 text, or a [`ErrorKind::Decode`] error.
    ///
    /// **This method's meaning does not depend on any feature**, which is
    /// why the charset-aware path is [`Self::text_with_charset`] beside it
    /// rather than a smarter version of this one. Cargo unifies features
    /// across a graph, so a library calling `text()` would otherwise get a
    /// different answer depending on what an unrelated crate switched on —
    /// and the difference is silent, since `windows-1251` bytes would come
    /// back as plausible text instead of as an error.
    pub fn text(&self) -> Result<String, Error> {
        String::from_utf8(self.body.to_vec()).map_err(|e| Error::new(ErrorKind::Decode, e))
    }

    /// The body as text, decoded by the charset the server **declared**.
    ///
    /// Behind the `charset` feature, off by default: `encoding_rs` is a
    /// megabyte and more of conversion tables, and a build that only ever
    /// meets UTF-8 has no use for them.
    ///
    /// Four answers, and each is a decision:
    ///
    /// - **No `charset` parameter, or no `Content-Type` at all** — UTF-8,
    ///   i.e. exactly [`Self::text`]. RFC 7231 removed the ISO-8859-1
    ///   default that RFC 2616 had, so there is no other honest guess, and
    ///   this crate does not sniff: content sniffing is a browser's job,
    ///   done against a security model this type does not have.
    /// - **A label the WHATWG Encoding Standard names** — decoded with it.
    /// - **A label it does not name** — [`CharsetError::UnknownLabel`],
    ///   naming the label. Falling back to UTF-8 would turn a server
    ///   saying something we did not understand into mojibake with no
    ///   sign that anything went wrong.
    /// - **Bytes that are malformed in that charset** —
    ///   [`CharsetError::Malformed`], **not** U+FFFD replacement
    ///   characters. `text()` refuses invalid UTF-8 rather than patching
    ///   it, and a method that refused one and patched the other would be
    ///   two policies under one name. A caller who wants the lossy answer
    ///   has [`Self::bytes`] and their choice of decoder.
    ///
    /// One inherited behaviour worth knowing: `encoding_rs`'s `decode`
    /// does the Encoding Standard's BOM sniffing, so a body opening with a
    /// UTF-8 or UTF-16 byte order mark is decoded as that and the declared
    /// label is overridden. That is the rule every browser follows, and
    /// the BOM is not part of the returned text.
    #[cfg(feature = "charset")]
    pub fn text_with_charset(&self) -> Result<String, Error> {
        let Some(label) = self
            .parts
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(charset_param)
        else {
            return self.text();
        };
        let Some(encoding) = encoding_rs::Encoding::for_label(label.as_bytes()) else {
            return Err(Error::new(
                ErrorKind::Decode,
                CharsetError::UnknownLabel {
                    label: label.to_owned(),
                },
            ));
        };
        let (text, used, malformed) = encoding.decode(&self.body);
        if malformed {
            return Err(Error::new(
                ErrorKind::Decode,
                CharsetError::Malformed {
                    charset: used.name(),
                },
            ));
        }
        Ok(text.into_owned())
    }
    /// Deserializes the body as JSON. Part of the interface declared for
    /// this task (`Collected::json<T>()`), but missing from step 3 of the
    /// brief — see the task report.
    ///
    /// Behind the `json` feature, off by default: `serde`/`serde_json`
    /// aren't needed by a consumer who only streams the body or reads it
    /// as bytes — see the comment on the feature in Cargo.toml about the
    /// cost on wasm.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, Error> {
        serde_json::from_slice(&self.body).map_err(|e| Error::new(ErrorKind::Decode, e))
    }
}

/// What [`Collected::text_with_charset`] could not do.
#[cfg(feature = "charset")]
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CharsetError {
    /// The `charset` parameter named something the WHATWG Encoding
    /// Standard has no encoding for.
    #[error("the response declared `charset={label}`, which names no encoding")]
    UnknownLabel { label: String },
    /// The bytes are not a valid sequence in the charset that was used —
    /// which is the declared one unless a byte order mark overrode it.
    #[error("the response body is not valid {charset}")]
    Malformed { charset: &'static str },
}

/// The `charset` parameter of a `Content-Type` value, if it has one.
///
/// Not a `mime` crate for one parameter of one header — `url` was removed
/// from this graph at the cost of writing RFC 3986 §5.2 by hand, and
/// base64 is twenty lines in `http-ng-proto` for the same reason. The
/// rules are RFC 9110 §5.6.6: parameters are `;`-separated after the media
/// type, names are case-insensitive, and a value is a token or a
/// quoted-string.
///
/// **A `\`-escape inside a quoted value is deliberately not unescaped.**
/// No encoding label contains a backslash or a quote, so a value that
/// needed unescaping names no encoding either way and comes back as
/// [`CharsetError::UnknownLabel`] — which is the right answer, and reached
/// without a second string to allocate.
#[cfg(feature = "charset")]
fn charset_param(value: &str) -> Option<&str> {
    // Past the media type. A media type is two tokens and a slash, so its
    // terminating `;` cannot be inside a quoted string.
    let mut rest = value.split_once(';')?.1;
    loop {
        let (param, tail) = split_at_unquoted_semicolon(rest);
        if let Some((name, val)) = param.split_once('=')
            && name.trim().eq_ignore_ascii_case("charset")
        {
            let val = val.trim();
            return Some(
                val.strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(val),
            );
        }
        rest = tail?;
    }
}

/// Splits at the first `;` that is not inside a quoted-string, answering
/// `None` for the tail when there is no further parameter.
#[cfg(feature = "charset")]
fn split_at_unquoted_semicolon(s: &str) -> (&str, Option<&str>) {
    let (mut quoted, mut escaped) = (false, false);
    for (i, c) in s.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            b'\\' if quoted => escaped = true,
            b'"' => quoted = !quoted,
            // The index is an ASCII byte's, so it is a char boundary.
            b';' if !quoted => return (&s[..i], Some(&s[i + 1..])),
            _ => {}
        }
    }
    (s, None)
}
