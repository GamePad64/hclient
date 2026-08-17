use crate::client::Client;

use crate::response::Response;
use http_ng_core::unversioned::{Timer, Transport};
use http_ng_core::{Error, ErrorKind, RequestBody};
use http_ng_proto::redirect::RedirectPolicy;

/// `Tm` is the CLIENT's clock, carried along so that `send`'s response
/// body can hold the operation's deadline (see [`crate::Deadline`]). It has the
/// same default as `Client`'s own parameter, so `RequestBuilder<'_, T>`
/// keeps naming what it always named.
#[derive(Debug)]
pub struct RequestBuilder<'a, T, Tm = crate::DefaultClock> {
    client: &'a Client<T, Tm>,
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
    /// The first build error. Surfaces in `send()`: a silently swallowed
    /// invalid header is exactly the silent no-op `ClientBuilder::build` was
    /// built against (see `check_supported` in config.rs). The brief's
    /// `header()` code dropped an invalid pair silently (`if let (Ok(n),
    /// Ok(v)) = .. { .. }`, no `else`) — a defect in the brief itself, not
    /// intended behavior: see the task report, Task 13 fix round 1.
    error: Option<Error>,
}

impl<'a, T: Transport, Tm: Timer + Clone> RequestBuilder<'a, T, Tm> {
    pub(crate) fn new(client: &'a Client<T, Tm>, method: http::Method, url: &str) -> Self {
        Self {
            client,
            method,
            uri: crate::config::effective_uri(client.config().base_url.as_ref(), url),
            headers: http::HeaderMap::new(),
            body: RequestBody::Empty,
            extensions: http::Extensions::new(),
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
    /// diagnostic at all — the same class of defect as the brief's
    /// `header()`, which Task 13 fixed and covered with a test.
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
    /// ENTIRE `http-ng` suite green (re-verified independently). Dead code
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
    /// The encoding is the WHATWG serialiser (`http_ng_proto::encode`),
    /// which is **not** RFC 3986 percent-encoding: a space is `+`, and
    /// only `*-._` survive as punctuation. That is the set a form parser
    /// on the other end will undo.
    pub fn query<K: AsRef<str>, V: AsRef<str>>(
        mut self,
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        let added = http_ng_proto::encode::form_urlencoded(pairs);
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
        let encoded = http_ng_proto::encode::form_urlencoded(pairs);
        if !self.headers.contains_key(http::header::CONTENT_TYPE) {
            self.headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/x-www-form-urlencoded"),
            );
        }
        self.body = RequestBody::Full(bytes::Bytes::from(encoded));
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
            }
            Err(e) => self.fail(e),
        }
        self
    }

    /// `Authorization: Basic`, RFC 7617.
    ///
    /// The value is marked sensitive, so a `Debug` of the request does not
    /// print the password — the same thing `http-ng-native`'s proxy
    /// credential does, and the reason both go through one encoder in
    /// `http-ng-proto`.
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
        let raw = http_ng_proto::encode::base64(format!("{user}:{password}").as_bytes());
        self.authorization(&format!("Basic {raw}"))
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
        e: impl std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
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
    pub fn timeouts(mut self, t: http_ng_core::Timeouts) -> Self {
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
    /// `http-ng` was a fresh `Client` per request — the very cost
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

    /// Sends the request.
    ///
    /// The body comes back wrapped in [`crate::Deadline`], which carries the
    /// client's whole-operation bound past the response head — inert, and
    /// costing one `Option` test per frame, for a client that never set
    /// one.
    pub async fn send(self) -> Result<Response<crate::ClientBody<T::Body, Tm>>, Error>
    where
        // Sibling of the bound on `Client::execute` in `client.rs` (spec
        // amendment-C1): `send` calls `Client::execute`, which requires
        // `T::Error: Send + Sync + 'static` — a generic function must repeat
        // its callee's bound, the trait itself doesn't carry it.
        T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        if let Some(e) = self.error {
            return Err(e);
        }
        let uri = self.uri?;
        let mut req = http::Request::new(self.body);
        *req.method_mut() = self.method;
        *req.uri_mut() = uri.clone();
        *req.headers_mut() = self.headers;
        *req.extensions_mut() = self.extensions;
        let resp = self.client.execute(req).await?;
        Ok(Response::new(resp, uri))
    }
}

/// RFC 7617 §2 makes `:` the separator between a username and a password,
/// so a username containing one is not representable — and encoding it
/// anyway would make `("a:b", "")` and `("a", "b")` the same bytes.
#[derive(Debug, thiserror::Error)]
#[error("a Basic-auth username may not contain a colon")]
pub struct ColonInUsername;
