use crate::client::Client;
use crate::response::Response;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, ErrorKind, RequestBody};

#[derive(Debug)]
pub struct RequestBuilder<'a, T> {
    client: &'a Client<T>,
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

impl<'a, T: Transport> RequestBuilder<'a, T> {
    pub(crate) fn new(client: &'a Client<T>, method: http::Method, url: &str) -> Self {
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

    pub async fn send(self) -> Result<Response<T::Body>, Error>
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
