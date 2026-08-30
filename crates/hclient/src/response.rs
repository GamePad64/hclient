#[cfg(feature = "charset")]
pub(crate) use crate::error::CharsetError;
pub(crate) use crate::error::UnexpectedStatus;
use bytes::{Bytes, BytesMut};
use hclient_core::{Error, ErrorKind};
use hclient_proto::link::Links;
use http_body::Body as HttpBody;
use std::error::Error as StdError;
use std::fmt::Debug;
use std::future::poll_fn;
use std::pin::Pin;

// Hand-written, so that it carries **no `B: Debug` bound**. `#[derive]`
// would add one, and after erasure the body in a real client is
// `dyn http_body::Body`, which is not `Debug` — so the derive would take
// `.unwrap()` on a response away from every caller. What a `{:?}` wants
// here is the head anyway: a body is a stream, and its contents were never
// printable without consuming them.
impl<B> Debug for Response<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.parts.status)
            .field("headers", &self.parts.headers)
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// A response with its URL preserved. `into_parts` gives full fidelity;
/// `chunk`/`collect` are convenience on top of it.
///
/// **`B` defaults to [`crate::body::ClientBody`]**, so a caller who got
/// this from a [`Client`](crate::Client) writes `hclient::Response` and
/// nothing else — which is the point of `Client` naming no parameters, one
/// type along. The parameter itself stays, and is not decoration:
/// [`crate::sse::SseStream::new`] takes a `Response<B>` over any body, so a
/// caller can build one on a response their own transport produced.
///
/// # Testing: there is no public constructor, and that is the signpost
///
/// A consumer cannot build one of these, which reads as a wall. It points
/// the other way: script the response on
/// [`mock::MockTransport`](crate::mock::MockTransport) instead and let a
/// real [`Client`](crate::Client) produce it. The test then exercises the
/// redirect, cookie, decompression and retry code a live request goes
/// through, where a hand-built `Response` would exercise a path
/// production never takes.
///
/// The first consumer to port onto this crate worked around the absence
/// before finding the mock, and reported that the missing thing was a
/// pointer rather than a constructor. This is it.
pub struct Response<B = crate::body::ClientBody> {
    parts: http::response::Parts,
    body: B,
    url: http::Uri,
    /// Set once `chunk()` has returned `Some(Err(_))`, after which
    /// `chunk()` returns `None`, never touching `body` again.
    ///
    /// Without it, `chunk()` after an error polls the underlying body
    /// again, and a caller working with
    /// `Response::chunk` directly could spin in a loop over a body that
    /// returns an error on every poll. `SseStream` compensated for this
    /// with its own `done` flag and tested it thoroughly — but only for
    /// itself. Terminality here is exactly the same: the error is handed
    /// back once, then it's the end of the stream.
    sealed: bool,
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
    /// [`Forbid`](hclient_proto::redirect::Forbid),
    /// where a `3xx` is stated to be the caller's answer rather than
    /// a failure to reach one. Treating it as an error here would overrule
    /// that from two layers up.
    ///
    /// [`ErrorKind::Status`]: hclient_core::ErrorKind::Status
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
    /// The requested URL would answer *"where did you send this"* under a
    /// name that reads *"where did this come from"*, and the two differ
    /// exactly when a redirect was followed. Found by writing
    /// [`Self::error_for_status`], whose error
    /// carries this value and is useless carrying the wrong one.
    pub fn url(&self) -> &http::Uri {
        &self.url
    }
    /// Every [`Link:`](hclient_proto::link) header, in the order the server
    /// wrote them, with **each target resolved against [`Self::url`]**.
    ///
    /// ```no_run
    /// # async fn f(c: &hclient::Client) -> Result<(), hclient::Error> {
    /// let page = c.get("https://api.example.com/items").send().await?;
    /// if let Some(next) = page.links().get("next") {
    ///     let _more = c.get(next.target()).send().await?;
    /// }
    /// # Ok(()) }
    /// ```
    ///
    /// # Resolved, and the base is the URL that answered
    ///
    /// The header may write a relative reference, and the caller's next act is
    /// to request the thing — so handing back `/items?page=2` would be handing
    /// back a value that fails at request time, a layer away from anything
    /// that could explain it. The base is [`Self::url`] rather than the URL
    /// the caller typed, which is RFC 3986 §5.1.3 and matters exactly when a
    /// redirect was followed: a `Link` on the answer is relative to where the
    /// answer came from.
    ///
    /// [`hclient_proto::link::Links::parse_value`] is the same parse without
    /// the resolution, for a caller who wants the header's own text; a target
    /// that cannot be resolved at all is left as written rather than dropped,
    /// which is argued at
    /// [`resolved_against`](hclient_proto::link::Links::resolved_against).
    ///
    /// # Why it is on both this type and [`Collected`]
    ///
    /// [`Self::error_for_status`]'s reason, one method up: the two are used at
    /// different moments. A caller streaming the body has a `Response` and
    /// wants the next page's URL before deciding to read this one; a caller
    /// who wrote `.collect().await?.json()?` has a `Collected` and meets
    /// pagination only after the body is in hand. Making them go back for it
    /// would mean keeping a response they had finished with.
    pub fn links(&self) -> Links {
        Links::from_headers(self.headers()).resolved_against(self.url())
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
    B::Error: StdError + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    /// The next data chunk. Trailer frames are skipped — for those, go
    /// through `into_parts` and poll the body directly.
    ///
    /// The error is terminal: after `Some(Err(_))` the body is sealed and
    /// all subsequent calls return `None` without polling it again — see
    /// the `sealed` field.
    pub async fn chunk(&mut self) -> Option<Result<Bytes, Error>> {
        if self.sealed {
            return None;
        }
        loop {
            let frame = poll_fn(|cx| Pin::new(&mut self.body).poll_frame(cx)).await;
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

    /// The body as a stream of lines, without their terminators —
    /// NDJSON, or a log being tailed.
    ///
    /// ```no_run
    /// # async fn f(c: &hclient::Client) -> Result<(), hclient::Error> {
    /// let mut lines = c.get("https://example.com/log").send().await?.lines();
    /// while let Some(line) = lines.next().await {
    ///     println!("{}", String::from_utf8_lossy(&line?));
    /// }
    /// # Ok(()) }
    /// ```
    ///
    /// A single line is bounded by [`lines::DEFAULT_MAX_LINE`]; a caller
    /// who knows their records' size uses [`lines::LineStream::new`]
    /// instead. [`lines::LineStream`] argues why that bound is not
    /// [`ClientBuilder::response_limit`](crate::ClientBuilder::response_limit)'s
    /// job, and [`hclient_proto::lines`] argues the terminator set.
    ///
    /// # There is no `Collected::lines`, and that is not symmetry lost
    ///
    /// [`Self::links`] is on both types because a header is readable at
    /// both moments. A [`Collected`] has no stream left to adapt — the
    /// bytes are already in memory, so `text()?.lines()` is `std`'s own
    /// one-liner and it needs nothing from this crate. Writing a second
    /// one here would be a second line-terminator policy in one crate,
    /// differing from `str::lines` on a lone CR, for a case that has no
    /// frames in it.
    ///
    /// [`lines::DEFAULT_MAX_LINE`]: crate::lines::DEFAULT_MAX_LINE
    /// [`lines::LineStream`]: crate::lines::LineStream
    /// [`lines::LineStream::new`]: crate::lines::LineStream::new
    pub fn lines(self) -> crate::lines::LineStream<B> {
        crate::lines::LineStream::new(self, crate::lines::DEFAULT_MAX_LINE)
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
/// `Transport::to_error`'s default (`hclient-core/src/unversioned/
/// transport.rs`), and for the same reason: if `e` is already our own
/// `Error`, its `kind()` was set at the point the backend actually
/// classified the failure (`ErrorKind::Cancelled` from a shutting-down
/// runtime, `ErrorKind::Tls` from a mid-stream handshake failure — whatever
/// it genuinely was), and re-wrapping it here would repeat the defect
/// `Transport::to_error` exists against, one seam later: `kind()` becomes
/// `Body` for everything, every `is_*` predicate lies, and `Display` prints
/// the category twice. Only a body whose error type carries no category of
/// its own — the common case for backends whose bodies are plain
/// `std::io::Error` or similar — falls back to `ErrorKind::Body`, which
/// remains the right default for a genuinely opaque body failure.
///
/// Wrapping unconditionally is invisible to the test suite, because
/// `NativeBody::poll_frame`'s own fallback already defaults to
/// `ErrorKind::Body` — the double-wrap was invisible by coincidence, not
/// because it was harmless. `Body`'s own `chunk_is_terminal_after_an_error_
/// and_does_not_poll_the_body_again` (in `tests/response.rs`) now pins the
/// non-coincidental case directly.
pub(crate) fn classify_body_error<E>(e: E) -> Error
where
    E: StdError + Send + Sync + 'static, // send-bound-exception: amendment-C1
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
    /// [`Forbid`](hclient_proto::redirect::Forbid),
    /// where a `3xx` is stated to be the caller's answer rather than
    /// a failure to reach one. Treating it as an error here would overrule
    /// that from two layers up.
    ///
    /// [`ErrorKind::Status`]: hclient_core::ErrorKind::Status
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
    /// Every [`Link:`](hclient_proto::link) header, in the order the server
    /// wrote them, with **each target resolved against [`Self::url`]**.
    ///
    /// ```no_run
    /// # async fn f(c: &hclient::Client) -> Result<(), hclient::Error> {
    /// let page = c.get("https://api.example.com/items").send().await?;
    /// if let Some(next) = page.links().get("next") {
    ///     let _more = c.get(next.target()).send().await?;
    /// }
    /// # Ok(()) }
    /// ```
    ///
    /// # Resolved, and the base is the URL that answered
    ///
    /// The header may write a relative reference, and the caller's next act is
    /// to request the thing — so handing back `/items?page=2` would be handing
    /// back a value that fails at request time, a layer away from anything
    /// that could explain it. The base is [`Self::url`] rather than the URL
    /// the caller typed, which is RFC 3986 §5.1.3 and matters exactly when a
    /// redirect was followed: a `Link` on the answer is relative to where the
    /// answer came from.
    ///
    /// [`hclient_proto::link::Links::parse_value`] is the same parse without
    /// the resolution, for a caller who wants the header's own text; a target
    /// that cannot be resolved at all is left as written rather than dropped,
    /// which is argued at
    /// [`resolved_against`](hclient_proto::link::Links::resolved_against).
    ///
    /// # Why it is on both this type and [`Response`]
    ///
    /// [`Self::error_for_status`]'s reason, one method up: the two are used at
    /// different moments. A caller streaming the body has a `Response` and
    /// wants the next page's URL before deciding to read this one; a caller
    /// who wrote `.collect().await?.json()?` has a `Collected` and meets
    /// pagination only after the body is in hand. Making them go back for it
    /// would mean keeping a response they had finished with.
    pub fn links(&self) -> Links {
        Links::from_headers(self.headers()).resolved_against(self.url())
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
    /// Deserializes the body as JSON.
    ///
    /// ```no_run
    /// # #[derive(serde::Deserialize)] struct User { id: u64 }
    /// # async fn f(c: &hclient::Client) -> Result<(), hclient::Error> {
    /// let user: User = c.get("https://example.com/u/1")
    ///     .send()
    ///     .await?
    ///     .collect()
    ///     .await?
    ///     .json()?;
    /// # let _ = user.id;
    /// # Ok(()) }
    /// ```
    ///
    /// Takes `&self`, so the same [`Collected`] can also be read as
    /// [`text`](Self::text) or [`bytes`](Self::bytes) — which is what a
    /// caller wants when a body fails to parse and the raw text is the
    /// diagnosis.
    ///
    /// A body that is not the JSON asked for is an
    /// [`ErrorKind::Decode`], the same category
    /// [`text`](Self::text) uses for bytes that are not UTF-8 — the
    /// server answered, and what came back is the problem. A `4xx` is
    /// **not** that: it is an ordinary answer whose body is usually JSON
    /// of a different shape, so put [`error_for_status`](Self::error_for_status)
    /// before this call rather than reading the failure as a parse error.
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

/// The `charset` parameter of a `Content-Type` value, if it has one.
///
/// Not a `mime` crate for one parameter of one header — `url` was removed
/// from this graph at the cost of writing RFC 3986 §5.2 by hand, and
/// base64 is twenty lines in `hclient-proto` for the same reason. The
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
    use hclient_proto::field::{ows, quoted_string_raw, token};
    use winnow::combinator::{alt, preceded, separated};
    use winnow::token::take_till;
    use winnow::{ModalResult, Parser};

    /// One `parameter = token BWS "=" BWS ( token / quoted-string )`.
    ///
    /// The three productions under it are `hclient_proto::field`'s rather
    /// than three more copies here — this module was one of the four this
    /// crate had. `quoted_string_raw` is the borrowing form, and the
    /// reason is the label: no encoding `encoding_rs` knows contains a
    /// quote, so a `quoted-pair` inside one cannot name a charset either
    /// way. It is still *consumed* correctly, so a `\"` cannot end the
    /// parameter early and let the rest be read as further parameters.
    fn parameter<'a>(i: &mut &'a str) -> ModalResult<(&'a str, &'a str)> {
        let name = preceded(ows, token).parse_next(i)?;
        (ows, '=', ows).parse_next(i)?;
        let v = alt((quoted_string_raw, token)).parse_next(i)?;
        ows.parse_next(i)?;
        Ok((name, v))
    }

    let mut input = value;
    // Past the media type. A media type is two tokens and a slash, so its
    // terminating `;` cannot be inside a quoted string — which is what
    // makes this first cut safe where the ones after it are not.
    let params: Vec<(&str, &str)> =
        preceded((take_till(0.., ';'), ';'), separated(0.., parameter, ';'))
            .parse_next(&mut input)
            .ok()?;

    params
        .into_iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("charset"))
        .map(|(_, v)| v)
}
