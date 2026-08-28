use crate::client::Client;
use std::error::Error as StdError;

use crate::response::Response;
use hclient_core::{Error, ErrorKind, RequestBody};
use hclient_proto::redirect::RedirectPolicy;

/// `Tm` is the CLIENT's clock, carried along so that `send`'s response
/// body can hold the operation's deadline (see [`crate::body::Deadline`]). It has the
/// same default as `Client`'s own parameter, so `RequestBuilder<'_, T>`
/// keeps naming what it always named.
#[derive(Debug)]
pub struct RequestBuilder<'a> {
    client: &'a Client,
    method: http::Method,
    /// Already resolved against the client's `base_url` (or just parsed,
    /// if there's no base). Resolution lives in `new`, not in `send`,
    /// because it has to be done on the original STRING: `http::Uri` can't
    /// represent a path-relative reference, and that's exactly the form
    /// the base exists for (see `config::effective_uri`).
    uri: Result<http::Uri, Error>,
    headers: http::HeaderMap,
    body: RequestBody,
    extensions: http::Extensions,
    /// The boundary of the multipart body currently set, if that is what
    /// the body is.
    ///
    /// **The `Content-Type` is written at `send()` rather than here**,
    /// which is the one place this builder defers a header it knows, and
    /// the reason is in [`Self::multipart`]. Keeping the boundary means
    /// the invariant is "this is `Some` exactly while the body is the
    /// multipart one" — so every method that replaces the body clears it.
    multipart: Option<crate::multipart::Boundary>,
    /// The credentials to answer a `401 Digest` with, if the caller gave
    /// any — see [`Self::digest_auth`].
    ///
    /// **Not in `extensions`, and that is the decision**: extensions reach
    /// `Transport::execute`, so a password there would be readable by any
    /// transport, including one this workspace did not write. It travels
    /// as an argument to `Client::execute` instead, which is also what
    /// keeps the cross-origin rule below in one place.
    #[cfg(feature = "digest-auth")]
    digest: Option<(String, String)>,
    /// The first build error. Surfaces in `send()`: a silently swallowed
    /// invalid header is exactly the silent no-op `ClientBuilder::build` was
    /// built against (see `check_supported` in config.rs). The shape this
    /// refuses is `if let (Ok(n), Ok(v)) = .. { .. }` with no `else`,
    /// which drops an invalid pair and reports nothing.
    error: Option<Error>,
}

impl<'a> RequestBuilder<'a> {
    pub(crate) fn new(client: &'a Client, method: http::Method, url: &str) -> Self {
        Self {
            client,
            method,
            uri: crate::config::effective_uri(client.config().base_url.as_ref(), url),
            headers: http::HeaderMap::new(),
            body: RequestBody::Empty,
            extensions: http::Extensions::new(),
            multipart: None,
            #[cfg(feature = "digest-auth")]
            digest: None,
            error: None,
        }
    }

    /// The first build error wins and survives further calls — it isn't
    /// overwritten by a second invalid pair, and isn't lost if a valid
    /// `header()` call follows it.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if self.error.is_some() {
            return self;
        }
        match (
            name.parse::<http::HeaderName>(),
            value.parse::<http::HeaderValue>(),
        ) {
            (Ok(n), Ok(v)) => {
                self.headers.insert(n, v);
            }
            (Err(e), _) => self.error = Some(Error::new(ErrorKind::Other, e)),
            (_, Err(e)) => self.error = Some(Error::new(ErrorKind::Other, e)),
        }
        self
    }

    /// Adds to the headers already set, rather than replacing them.
    ///
    /// `self.headers = headers` (as it was before m4 of the branch's final
    /// review) threw away everything `header()` had managed to set, with no
    /// diagnostic at all — the same class of defect as a silently
    /// dropped `header()`, and covered by a test for the same reason.
    /// `HeaderMap::extend` overrides a same-named value rather than
    /// accumulating a duplicate (verified against `http`'s contract: the
    /// first value for a key from the extending map goes through `insert`,
    /// later ones through `append`), so "adds to" doesn't turn into two
    /// `accept`s on the wire.
    ///
    /// **The error slot is NOT consulted here, unlike in `header()`, and
    /// that's not a forgotten symmetry.** In `header()` the guard
    /// `if self.error.is_some() { return self; }` carries weight: that
    /// method can set an error itself, and without the guard a second
    /// invalid pair would overwrite the first — verified by mutation, it
    /// kills `header_first_error_wins_name_over_later_value_error`. In
    /// `headers()` there's nothing to set an error from: the `HeaderMap` is
    /// already valid by construction. The same guard here wouldn't change
    /// anything observable — `send()` returns the stored error before it
    /// ever looks at headers — and it really was here for one round, along
    /// with a test that could never fail: removing the guard left the
    /// ENTIRE `hclient` suite green (re-verified independently). Dead code
    /// and a test that can't go red are worse than nothing, so both were
    /// removed.
    ///
    /// Trigger to bring the guard back: the moment `headers()` learns to
    /// reject something — say, the `Capabilities::forbidden_request_headers`
    /// filter planned for v0.2 — it becomes observable again, and it must
    /// come back TOGETHER with a test that goes red without it.
    pub fn headers(mut self, headers: http::HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn body(mut self, body: RequestBody) -> Self {
        self.body = body;
        self.multipart = None;
        self
    }

    /// A `multipart/form-data` body — RFC 7578.
    ///
    /// ```no_run
    /// # use hclient::multipart::{Form, Part};
    /// # async fn f(c: &hclient::Client) -> Result<(), hclient::Error> {
    /// c.post("https://example.com/upload")
    ///     .multipart(
    ///         Form::new()
    ///             .part(Part::text("title", "a holiday photo"))
    ///             .part(Part::bytes("file", &b"\x89PNG\r\n"[..])
    ///                 .file_name("beach.png")
    ///                 .mime("image/png")),
    ///     )
    ///     .send()
    ///     .await?;
    /// # Ok(()) }
    /// ```
    ///
    /// **Whether this body can be retried is decided by the parts**, not
    /// by an argument: a form whose parts are all bytes comes out
    /// [`RetryKind::ViaFactory`] and carries a `Content-Length`, and one
    /// with a stream in it comes out [`RetryKind::Impossible`]. The
    /// module documentation has the table and the reason there is no
    /// flag.
    ///
    /// **`Content-Type` is not set here, and that is the one place this
    /// builder does not follow [`Self::form`]'s rule.** For `form` and
    /// `json` the header is a *label* — the body is the same bytes
    /// whatever it says — so a caller who set one meant it and it is left
    /// alone. Multipart's header is not a label: it carries the boundary,
    /// which is part of the encoding and did not exist when the caller
    /// wrote their header. Deferring to a caller-set value would put a
    /// `Content-Type` on the wire that cannot describe the body, and the
    /// server would see one unparseable part.
    ///
    /// So the header is written at [`Self::send`], from the boundary, and
    /// a `Content-Type` set by the caller — before this call or after it,
    /// through [`Self::header`] or [`Self::headers`] — is a typed build
    /// error rather than a silent override in either direction. A caller
    /// who really wants a different multipart subtype has
    /// [`crate::multipart::Form::encode`] and [`Self::body`].
    ///
    /// [`RetryKind::ViaFactory`]: hclient_core::RetryKind::ViaFactory
    /// [`RetryKind::Impossible`]: hclient_core::RetryKind::Impossible
    pub fn multipart(mut self, form: crate::multipart::Form) -> Self {
        let boundary = match crate::multipart::Boundary::random() {
            Ok(b) => b,
            Err(e) => {
                self.fail(e);
                return self;
            }
        };
        match form.encode(&boundary) {
            Ok(body) => {
                self.body = body;
                self.multipart = Some(boundary);
            }
            Err(e) => self.fail(e),
        }
        self
    }

    /// Append `application/x-www-form-urlencoded` pairs to the URI's query.
    ///
    /// **Appended, not replaced**, and each call appends again — so a
    /// query already in the URL survives, and two calls are one query. A
    /// setter that replaced would silently drop what a caller put in their
    /// own URL string, which is the failure that cannot be seen from the
    /// call site.
    ///
    /// The encoding is the WHATWG serialiser (`hclient_proto::encode`),
    /// which is **not** RFC 3986 percent-encoding: a space is `+`, and
    /// only `*-._` survive as punctuation. That is the set a form parser
    /// on the other end will undo.
    pub fn query<K: AsRef<str>, V: AsRef<str>>(
        mut self,
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        let added = hclient_proto::encode::form_urlencoded(pairs);
        if added.is_empty() {
            return self;
        }
        let Ok(uri) = &self.uri else { return self };
        let mut parts = uri.clone().into_parts();
        let path = parts
            .path_and_query
            .as_ref()
            .map_or("/", |p| p.path())
            .to_owned();
        let existing = parts.path_and_query.as_ref().and_then(|p| p.query());
        let joined = match existing {
            Some(q) if !q.is_empty() => format!("{path}?{q}&{added}"),
            _ => format!("{path}?{added}"),
        };
        match joined.parse::<http::uri::PathAndQuery>() {
            Ok(pq) => {
                parts.path_and_query = Some(pq);
                match http::Uri::from_parts(parts) {
                    Ok(u) => self.uri = Ok(u),
                    Err(e) => self.fail(e),
                }
            }
            Err(e) => self.fail(e),
        }
        self
    }

    /// An `application/x-www-form-urlencoded` body, with the header.
    ///
    /// Sets `Content-Type` only if the caller has not — the same rule
    /// `Host:` follows one layer down, and for the same reason: a caller
    /// who set it meant it.
    pub fn form<K: AsRef<str>, V: AsRef<str>>(
        mut self,
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        let encoded = hclient_proto::encode::form_urlencoded(pairs);
        if !self.headers.contains_key(http::header::CONTENT_TYPE) {
            self.headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/x-www-form-urlencoded"),
            );
        }
        self.body = RequestBody::Full(bytes::Bytes::from(encoded));
        self.multipart = None;
        self
    }

    /// A JSON body, with the header.
    ///
    /// Behind the `json` feature, off by default, for the reason
    /// [`crate::Collected::json`] is: a caller who streams bytes should
    /// not link a serialiser, and on wasm that cost is paid in download
    /// size.
    ///
    /// **Serialised here rather than at send time**, so a value that
    /// cannot be serialised is the builder's first error — surfacing out
    /// of `send()` like every other one — instead of a failure discovered
    /// after a connection has been opened.
    ///
    /// Sets `Content-Type` only if the caller has not, the same rule
    /// [`Self::form`] follows.
    #[cfg(feature = "json")]
    pub fn json<V: serde::Serialize + ?Sized>(mut self, value: &V) -> Self {
        match serde_json::to_vec(value) {
            Ok(bytes) => {
                if !self.headers.contains_key(http::header::CONTENT_TYPE) {
                    self.headers.insert(
                        http::header::CONTENT_TYPE,
                        http::HeaderValue::from_static("application/json"),
                    );
                }
                self.body = RequestBody::Full(bytes::Bytes::from(bytes));
                self.multipart = None;
            }
            Err(e) => self.fail(e),
        }
        self
    }

    /// `Authorization: Basic`, RFC 7617.
    ///
    /// The value is marked sensitive, so a `Debug` of the request does not
    /// print the password — the same thing `hclient-native`'s proxy
    /// credential does, and the reason both go through one encoder in
    /// `hclient-proto`.
    ///
    /// A username containing `:` is refused rather than encoded: RFC 7617
    /// §2 makes the colon the separator, so `a:b` and `a` with password
    /// `b` would produce identical bytes and one of the two callers would
    /// be silently wrong.
    pub fn basic_auth(mut self, user: &str, password: &str) -> Self {
        if user.contains(':') {
            self.fail(ColonInUsername);
            return self;
        }
        let raw = hclient_proto::encode::base64(format!("{user}:{password}").as_bytes());
        self.authorization(&format!("Basic {raw}"))
    }

    /// Answer a `401` digest challenge on this request — RFC 7616.
    ///
    /// Unlike [`basic_auth`](Self::basic_auth) beside it, **nothing is
    /// sent until the server asks**: a digest response is computed from a
    /// nonce the server chooses, so the first request goes out
    /// unauthenticated and the client answers the challenge that comes
    /// back. That costs one round trip per request and is written down as
    /// a deliberate absence in [`crate::digest`], with what removing it
    /// would need.
    ///
    /// **The credentials do not cross an origin.** A redirect that changes
    /// host, scheme or port drops them, by the same rule and in the same
    /// place that already strips `Authorization` — so a `401` from the
    /// second origin is the caller's answer rather than an invitation to
    /// send a password somewhere they never named.
    ///
    /// Behind the `digest-auth` feature, off by default: the hashes cost
    /// nine crates, measured, which a caller who never meets a digest
    /// challenge should not link.
    #[cfg(feature = "digest-auth")]
    #[must_use]
    pub fn digest_auth(mut self, user: &str, password: &str) -> Self {
        self.digest = Some((user.to_owned(), password.to_owned()));
        self
    }

    /// `Authorization: Bearer`, RFC 6750 §2.1.
    pub fn bearer_auth(self, token: &str) -> Self {
        self.authorization(&format!("Bearer {token}"))
    }

    fn authorization(mut self, value: &str) -> Self {
        match http::HeaderValue::from_str(value) {
            Ok(mut v) => {
                v.set_sensitive(true);
                self.headers.insert(http::header::AUTHORIZATION, v);
            }
            Err(e) => self.fail(e),
        }
        self
    }

    /// Record a build error, first one wins — see [`Self::header`].
    fn fail(
        &mut self,
        // `Error::new`'s own bound, and it is this crate's for the same
        // reason: the source is stored behind `dyn Error`, which drops
        // auto traits.
        e: impl StdError + Send + Sync + 'static, // send-bound-exception: amendment-C1
    ) {
        if self.error.is_none() {
            self.error = Some(Error::new(ErrorKind::Other, e));
        }
    }

    /// Timeouts for this request only. Stored in `Extensions`, where the
    /// transport reads them from; unset fields fall back to the client's
    /// configuration — the merge is done by `Client::execute` via
    /// `config::effective_timeouts`, which also checks the **merged**
    /// result against the transport's `Capabilities`, so a phase the
    /// backend can't do becomes `ErrorKind::Unsupported` out of `send()`,
    /// not a value silently dropped.
    ///
    /// reqwest can't do this at all (issue #2641), which forces `act-cli`
    /// to build a separate `reqwest::Client` for every component call —
    /// with its own connection pool.
    pub fn timeouts(mut self, t: hclient_core::Timeouts) -> Self {
        self.extensions.insert(t);
        self
    }

    /// The redirect policy for this request only, overriding
    /// [`ClientBuilder::redirect`] wholesale — not field by field like
    /// `timeouts` above: `RedirectPolicy` is a plain value, not an `Option`,
    /// not an `Option`, so there is nothing to fall through (see
    /// `config::effective_redirect`, which does the merge).
    ///
    /// The reason this exists is a real shape, not a symmetry: `act`'s
    /// `http-client` component computes its limit per call, as
    /// `if args.follow_redirects { 10 } else { 0 }`, from a per-request
    /// argument. Before this method the only way to express that through
    /// `hclient` was a fresh `Client` per request — the very cost
    /// `RequestBuilder::timeouts` already exists to avoid (reqwest #2641).
    ///
    /// **The consumer's other branch is [`RedirectPolicy::None`], not
    /// `Limited(0)`.** `Limited(0)` means "follow up to zero hops", so the
    /// first 301/302/303/307/308 carrying a `Location` becomes
    /// `ErrorKind::Redirect`. `None` returns that response to the caller
    /// untouched — which is what `wasi-fetch`, the library that component
    /// migrates from, did for `redirect_limit(0)`
    /// (`wasi-fetch/src/request.rs`: `if redirect_limit > 0 &&
    /// status.is_redirection()`), and the component forwards its status
    /// and `Location` upward. `reqwest` keeps the two intents apart the same
    /// way, as `Policy::none()` and `Policy::limited(0)` — and so does this
    /// type, since the acceptance that ported that component found a
    /// `limit: u8` field could express only the second.
    ///
    /// A policy of any kind is what a backend which follows redirects
    /// internally can never honour: against `RedirectSupport::Internal`
    /// (the browser
    /// `fetch` transport, whose `redirect: "follow"` default isn't
    /// overridable through this crate) it comes back as
    /// `ErrorKind::Unsupported` from `send()` rather than being silently
    /// dropped. `Client::execute` checks the MERGED policy, so a
    /// per-request one is checked on the same footing as a client-level
    /// one.
    ///
    /// Stored in `Extensions`, the same channel `timeouts` uses — but read
    /// by `Client::execute` itself, not by the transport: no transport
    /// reads a `RedirectPolicy`.
    ///
    /// [`ClientBuilder::redirect`]: crate::ClientBuilder::redirect
    pub fn redirect(mut self, policy: RedirectPolicy) -> Self {
        self.extensions.insert(policy);
        self
    }

    /// Demand a protocol version, and fail rather than fall back.
    ///
    /// This is the route this crate's own documentation names as the
    /// honest one — `Capabilities` report the **floor**, the value that
    /// holds on the worst protocol the transport might negotiate, so
    /// asking a capability whether HTTP/2 will be used gives the wrong
    /// answer by design. The pair that answers it is a demand before the
    /// head and [`crate::Response::version`] after it. Until now the first
    /// half was unreachable through this builder: `RequireVersion` lives
    /// in `Extensions`, and `timeouts` and `redirect` were the only two
    /// keys with a setter.
    ///
    /// A transport that cannot select a version refuses the demand rather
    /// than ignoring it — `Capabilities::version_select` is the gate, and
    /// a `false` there makes this an `UnsupportedCapability` instead of a
    /// setting that quietly does nothing.
    ///
    /// Stored in `Extensions`, the same channel `timeouts` uses, and read
    /// by the transport rather than by `Client`.
    pub fn require_version(mut self, version: http::Version) -> Self {
        self.extensions
            .insert(hclient_core::RequireVersion(version));
        self
    }

    /// Sends the request.
    ///
    /// The body comes back wrapped in [`crate::body::Deadline`], which carries the
    /// client's whole-operation bound past the response head — inert, and
    /// costing one `Option` test per frame, for a client that never set
    /// one.
    /// The `where` clause this used to carry is gone with the type
    /// parameters: the transport's error is converted into
    /// [`hclient_core::Error`] at the erased seam, so nothing here has to
    /// repeat a bound about it.
    pub async fn send(self) -> Result<Response<crate::body::ClientBody>, Error> {
        if let Some(e) = self.error {
            return Err(e);
        }
        let uri = self.uri?;
        let mut headers = self.headers;
        // The one header this builder writes last rather than at the call
        // that asked for it — see `multipart`'s doc comment. Asked here
        // rather than there so the answer does not depend on the order
        // the caller wrote the two calls in: a `Content-Type` set after
        // `multipart(..)` is the same refusal as one set before it.
        if let Some(b) = self.multipart {
            if headers.contains_key(http::header::CONTENT_TYPE) {
                return Err(Error::new(ErrorKind::Other, ContentTypeIsNotOursToKeep));
            }
            headers.insert(http::header::CONTENT_TYPE, b.content_type());
        }
        let mut req = http::Request::new(self.body);
        *req.method_mut() = self.method;
        *req.uri_mut() = uri.clone();
        *req.headers_mut() = headers;
        *req.extensions_mut() = self.extensions;
        let (resp, final_uri) = self
            .client
            .execute_with(
                req,
                #[cfg(feature = "digest-auth")]
                self.digest,
            )
            .await?;
        // **The URL the answer came from, not the one that was asked
        // for.** They differ exactly when a redirect was followed, and
        // then it is the last hop that is worth reporting — the caller
        // already has the first, having typed it. `uri` is still what the
        // builder resolved against `base_url`, so a chain that never
        // redirected reports the same value it always did.
        Ok(Response::new(resp, final_uri))
    }
}

/// A `multipart/form-data` body was set and so was a `Content-Type`.
///
/// The two cannot both stand: the header carries the boundary, so the
/// caller's value would describe a body that is not there. Refusing names
/// both, where overriding would lose the caller's header silently and
/// deferring would send bytes no receiver can parse.
#[derive(Debug, thiserror::Error)]
#[error(
    "a multipart body sets its own Content-Type, because the header carries the boundary — \
     remove the Content-Type, or build the body with multipart::Form::encode and set both"
)]
pub struct ContentTypeIsNotOursToKeep;

/// RFC 7617 §2 makes `:` the separator between a username and a password,
/// so a username containing one is not representable — and encoding it
/// anyway would make `("a:b", "")` and `("a", "b")` the same bytes.
#[derive(Debug, thiserror::Error)]
#[error("a Basic-auth username may not contain a colon")]
pub struct ColonInUsername;
