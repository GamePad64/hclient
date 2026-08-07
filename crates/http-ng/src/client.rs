use crate::config::{
    Config, check_supported, check_timeouts_supported, effective_timeouts, effective_uri,
};
use crate::request::RequestBuilder;
use crate::stages::redirect::{HopParts, next_hop};
use http_ng_core::Timeouts;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, UnsupportedCapability};
use http_ng_proto::redirect::{RedirectAction, RedirectPolicy, decide};

#[derive(Debug)]
pub struct ClientBuilder<T> {
    transport: T,
    config: Config,
}

impl<T: Transport> ClientBuilder<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            config: Config::default(),
        }
    }
    pub fn redirect(mut self, policy: RedirectPolicy) -> Self {
        self.config.redirect = policy;
        self
    }
    /// Default timeouts for every request from this client.
    ///
    /// Overridden field by field by `RequestBuilder::timeouts`; the merge
    /// is done by `Client::execute` (`effective_timeouts`), and its result
    /// is what actually goes to the transport in `http::Extensions`. A
    /// phase the transport doesn't support is an error at `build()`, and
    /// with B1/M3 also at `execute()` for whatever the request itself set.
    pub fn timeouts(mut self, t: Timeouts) -> Self {
        self.config.timeouts = t;
        self
    }
    /// The base against which each request's URI is resolved.
    ///
    /// This library's answer to reqwest #988 and #213 (open since 2017 and
    /// 2020, 104 votes). Before fix round 3, the value stored here was
    /// never read from anywhere — the setter was a silent no-op.
    ///
    /// **The rule is RFC 3986 §5**, the exact same one that resolves a
    /// response's `Location:`: one client shouldn't understand `/x` two
    /// different ways depending on whether the server sent it or the
    /// caller did. Two consequences follow, worth reading before they
    /// surprise you:
    ///
    /// ```text
    /// base "https://api.test/v1/"   + "things"    -> https://api.test/v1/things
    /// base "https://api.test/v1/"   + "/things"   -> https://api.test/things      // a leading / REPLACES the base's path
    /// base "https://api.test/v1"    + "things"    -> https://api.test/things      // a base without / is not a directory
    /// base "https://api.test/v1/"   + "https://other.test/x" -> https://other.test/x
    /// ```
    ///
    /// In other words, a base with a path should almost always end in a
    /// slash, and a reference should NOT start with one. Neither of these
    /// lines is our own invention — they're the merge algorithm and RFC
    /// §5.2.2; the same rules drive `url::Url::join`, the browser's
    /// `new URL(ref, base)`, and `urllib.parse.urljoin`.
    ///
    /// The base itself must be absolute. A relative one (`/api/`) is a
    /// typed `InvalidBaseUrl` error from `send()`/`execute()`, not a
    /// silently ignored setting. There's deliberately no check at
    /// `build()`: that would require changing `build()`'s error type,
    /// which is wider than this round — noted in the report.
    ///
    /// A limitation worth knowing: `Client::execute`, which takes an
    /// already-built `http::Request`, sees an already-parsed `http::Uri`,
    /// and that type can't represent a path-relative reference at all
    /// (`"things"` is `InvalidUri`). Through this entry point the base can
    /// give the request a scheme and authority, but not a path.
    /// `RequestBuilder` (`client.get("things")`) resolves the original
    /// string before parsing and has no such limitation.
    pub fn base_url(mut self, uri: http::Uri) -> Self {
        self.config.base_url = Some(uri);
        self
    }
    /// Checks the configuration against the transport's capabilities. Not
    /// a single silent no-op: an unsupported setting is an error, here and
    /// now.
    pub fn build(self) -> Result<Client<T>, UnsupportedCapability> {
        check_supported(
            &self.config,
            self.transport.capabilities(),
            backend_name::<T>(),
        )?;
        Ok(Client {
            transport: self.transport,
            config: self.config,
        })
    }
}

fn backend_name<T>() -> &'static str {
    // The type name is informative enough for an error message and costs nothing.
    std::any::type_name::<T>()
}

// A forked declaration, rather than one `pub struct Client<T = crate::
// DefaultTransport>` with a conditional default: Rust has no way to make a
// generic default itself conditional on a feature — a `#[cfg]` on a lone
// default parameter inside a single struct declaration isn't read by the
// compiler. Without the `default-transport` feature, `Client` must require
// `T` explicitly (an ordinary "missing generics" compile error on `Client`
// with no parameter — the same honest error as the missing
// `DefaultTransport`, see its doc comment in `lib.rs`), rather than
// resolving to a default from a branch that doesn't exist at all with the
// feature off. Both variants below carry the same set of fields;
// `impl<T: Transport> Client<T>` further down applies to both identically:
// the generic parameter's default only affects call sites where `Client`
// is written with no explicit `<...>` (e.g. `Client::new()`'s return type
// below), not the signatures of existing impl blocks.
#[cfg(feature = "default-transport")]
#[derive(Debug)]
pub struct Client<T = crate::DefaultTransport> {
    transport: T,
    config: Config,
}
#[cfg(not(feature = "default-transport"))]
#[derive(Debug)]
pub struct Client<T> {
    transport: T,
    config: Config,
}

impl<T: Transport> Client<T> {
    pub fn builder(transport: T) -> ClientBuilder<T> {
        ClientBuilder::new(transport)
    }
    pub fn transport(&self) -> &T {
        &self.transport
    }
    pub fn config(&self) -> &Config {
        &self.config
    }
    /// What this client's transport can do.
    ///
    /// This forwarder exists so that answering the most natural question
    /// about `Capabilities` doesn't require dragging `unversioned::
    /// Transport` into scope (Task 17 fix round 2) — the trait is
    /// deliberately in semver quarantine (see the doc comment on
    /// `http-ng-core/src/unversioned/mod.rs`) and isn't part of the
    /// `http-ng` facade. Without this forwarder, `client.transport().
    /// capabilities()` — a trait method — was the only path, and
    /// `client.transport()` returns `&T`, so calling `.capabilities()` on
    /// it would require `Transport` in a `use`.
    pub fn capabilities(&self) -> &Capabilities {
        self.transport.capabilities()
    }

    pub fn request(&self, method: http::Method, url: &str) -> RequestBuilder<'_, T> {
        RequestBuilder::new(self, method, url)
    }
    pub fn get(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::GET, url)
    }
    pub fn post(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::POST, url)
    }
    pub fn put(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::PUT, url)
    }
    pub fn delete(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::DELETE, url)
    }
    pub fn patch(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::PATCH, url)
    }
    pub fn head(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::HEAD, url)
    }

    /// Starts building a reconnecting SSE stream — see
    /// [`crate::SseBuilder`]/[`crate::ReconnectingSseStream`]. For a single,
    /// non-reconnecting stream over a response already in hand, use
    /// [`crate::SseStream::new`] directly instead.
    pub fn sse(&self, url: &str) -> crate::sse::SseBuilder<'_, T> {
        crate::sse::SseBuilder::new(self, url)
    }

    /// The stage order is fixed and correct by construction.
    /// In v0.1 there's one stage — redirect.
    ///
    /// `where T::Error: Send + Sync + 'static` — the second documented
    /// exception to the "core declares no Send/Sync" invariant (spec
    /// amendment-C1, sibling of the exception on `Error::source`). Without
    /// it, `Transport::to_error` below wouldn't be callable for an
    /// abstract `T`: its own where-clause requires the same bound, because
    /// its default body calls `Error::new`, and `Error` stores its source
    /// as `Arc<dyn Error + Send + Sync>`, and type erasure doesn't let an
    /// unbounded trait object's auto-traits through (verified by
    /// compilation — E0277 without this bound). The bound lives here, on
    /// the method itself, rather than on the `Transport` trait as a whole,
    /// exactly as documented in `http-ng-core`'s lib.rs: a transport with
    /// an honestly `!Send` error remains representable, it just can't be
    /// used with `Client`.
    pub async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<T::Body>, Error>
    where
        T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        let (parts, mut body) = req.into_parts();

        // The base URL is applied here, not only in `RequestBuilder`:
        // `execute` is a public entry point that takes an already-built
        // `http::Request`, and a setting that only worked on one of the
        // two paths would be half-fixed. Idempotent — `RequestBuilder::
        // send` resolves the same URI ahead of time (it needs the result
        // for `Response::url()`), and resolving an already-absolute URI
        // returns it unchanged (RFC 3986 §5.2.2).
        let uri = effective_uri(self.config.base_url.as_ref(), &parts.uri.to_string())?;

        let mut hp = HopParts {
            method: parts.method,
            uri,
            headers: parts.headers,
            version: parts.version,
            extensions: parts.extensions,
        };

        // B1/M3 of the branch's final review, two halves of one hole.
        // Before it, `effective_timeouts` was never called from anywhere
        // in production code — `ClientBuilder::timeouts()` was a silent
        // no-op, because the only channel to the transport is
        // `http::Extensions`, and the client's configuration never made
        // it in there; and symmetrically, `RequestBuilder::timeouts()`
        // wrote to `Extensions` with no check against `Capabilities` at
        // all, whereas the same setting at the client level produced an
        // `UnsupportedCapability` at `build()`.
        //
        // The merge and the check live here, not in `build()` and not in
        // `RequestBuilder`, because only here are BOTH operands known. The
        // result is stored in `extensions` before the loop: subsequent
        // hops clone them from the previous one
        // (`stages::redirect::next_hop`), so merging once is enough.
        let effective = effective_timeouts(&hp.extensions, &self.config.timeouts);
        check_timeouts_supported(
            &effective,
            self.transport.capabilities(),
            backend_name::<T>(),
        )
        .map_err(|e| Error::new(ErrorKind::Unsupported, e))?;
        hp.extensions.insert(effective);

        let mut hops: u8 = 0;

        loop {
            // The replay snapshot is taken BEFORE sending: after that, the
            // body is already consumed. For `Streaming` this returns
            // `None` — and that's known honestly ahead of time, not after
            // a failed retry.
            let replay = body.rewind();
            let sending = std::mem::replace(&mut body, RequestBody::Empty);

            let resp = self
                .transport
                .execute(hp.to_request(sending))
                .await
                // Not `Error::new(ErrorKind::Other, e)`: B2 of the
                // branch's final review — unconditional wrapping flattened
                // the category of ANY transport error into `Other`,
                // devaluing the whole `ErrorKind` taxonomy. The backend
                // decides, not this line: the default `Transport::to_error`
                // wraps exactly the same way, and a backend whose error is
                // already an `Error` hands it back as-is.
                .map_err(|e| self.transport.to_error(e))?;

            let location = resp
                .headers()
                .get(http::header::LOCATION)
                .map(|v| v.as_bytes());
            let action = decide(
                &self.config.redirect,
                hops,
                &hp.uri,
                &hp.method,
                resp.status(),
                location,
            );

            match action {
                RedirectAction::Stop => return Ok(resp),
                RedirectAction::TooManyRedirects => {
                    return Err(Error::new(
                        ErrorKind::Redirect,
                        TooMany(self.config.redirect.limit),
                    ));
                }
                RedirectAction::InvalidLocation => {
                    return Err(Error::new(ErrorKind::Redirect, BadLocation));
                }
                RedirectAction::Follow(f) => {
                    hops += 1;
                    let Some((next_hp, next_body)) = next_hop(&hp, replay, &f) else {
                        return Ok(resp);
                    };
                    hp = next_hp;
                    body = next_body;
                }
            }
        }
    }
}

// `not(target_family = "wasm")`, not just `feature = "default-transport"`
// — the same double gate as `DefaultTransport` itself (`lib.rs`): on wasm
// targets, where the `DefaultTransport` branch doesn't exist (see its doc
// comment), this `impl` for `Client<crate::DefaultTransport>` would refer
// to a nonexistent type. Separate gates would give the same behavior (an
// `impl` for a nonexistent type also fails to compile), but repeating the
// condition makes the reason visible on the spot, not only in `lib.rs`.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
impl Client<crate::DefaultTransport> {
    /// A client with the default transport.
    ///
    /// On native this requires a surrounding tokio runtime: `tokio::spawn`
    /// and `tokio::time::sleep` panic outside a runtime. reqwest behaves
    /// exactly the same way. The explicit path without this requirement is
    /// `Client::builder(Native::new(rt, tls, dns))` with a runtime of your
    /// choice (see `crates/http-ng/tests/two_runtimes.rs`, the same
    /// constructor for tokio and for smol).
    ///
    /// # Panics
    ///
    /// Panics in exactly one case: `Rustls::with_platform_verifier()`
    /// couldn't read the OS's system trust store (`rustls-platform-
    /// verifier`'s own view is that this is a runtime environment
    /// condition, not a client configuration error). A client setting the
    /// transport doesn't support (`UnsupportedCapability`, this function's
    /// error type) doesn't cause a panic — it comes back as an ordinary
    /// `Err`.
    ///
    /// The non-panicking alternative is [`Client::try_new`]: the same
    /// construction, but with both failure modes (trust store AND
    /// unsupported setting) as `Err`, for a caller that must handle any
    /// failure, not just the second one. `.expect(...)` here, rather than
    /// propagating through this function's `Result`, is the same decision
    /// as before: `UnsupportedCapability` (`what`, `backend`, both
    /// `&'static str`) is a typed answer to "the transport doesn't support
    /// this particular client setting", not to "the system trust store
    /// couldn't be read", and conflating them would mean lying about the
    /// category of failure. `try_new` below resolves this not by
    /// conflating them but with a wider error type — `http_ng_core::Error`,
    /// where both causes are already distinguishable via `ErrorKind`
    /// (`Tls` and `Unsupported` respectively), so the panic here stays an
    /// ergonomics choice for "the common case", not the only path.
    pub fn new() -> Result<Self, http_ng_core::UnsupportedCapability> {
        let transport = Self::default_native_transport().expect("platform verifier");
        Self::builder(transport).build()
    }

    /// The non-panicking version of [`Client::new`] — the exact same
    /// construction (`Native<Tokio, Rustls, SystemDns<Tokio>>` with the
    /// system trust store), but both failure points become `Err` instead
    /// of just one of the two: `Rustls::with_platform_verifier()` gives
    /// `ErrorKind::Tls` (`with_platform_verifier` itself already returns
    /// that; the `?` below simply doesn't silence it into a panic), an
    /// incompatible setting at `build()` gives `ErrorKind::Unsupported`,
    /// the same trick `Client::execute` already applies to
    /// `UnsupportedCapability` from `check_timeouts_supported`
    /// (`Error::new(ErrorKind::Unsupported, e)`, `config.rs`) — not a new
    /// trick invented for this function, but reuse of an existing one.
    ///
    /// For a process where a panic isn't an acceptable way to learn about
    /// an environment without a working system certificate store
    /// (embedded systems, long-lived processes where `catch_unwind` around
    /// the client constructor isn't an option) — this path, not
    /// [`Client::new`].
    pub fn try_new() -> Result<Self, http_ng_core::Error> {
        let transport = Self::default_native_transport()?;
        Self::builder(transport)
            .build()
            .map_err(|e| http_ng_core::Error::new(http_ng_core::ErrorKind::Unsupported, e))
    }

    /// The shared construction of the default transport for
    /// [`Client::new`] and [`Client::try_new`] — the one operation that
    /// can genuinely fail for both (`Rustls::with_platform_verifier()`),
    /// so it's factored out once rather than duplicated. `Result<_,
    /// http_ng_core::Error>`, not `UnsupportedCapability`:
    /// `with_platform_verifier()` already returns an `Error`
    /// (`ErrorKind::Tls`) itself, and `Native::new`/`SystemDns::new` can't
    /// fail at all (ordinary constructors, no IO) — wrapping their
    /// nonexistent failure in a `Result` would have nothing to justify it.
    fn default_native_transport() -> Result<crate::DefaultTransport, http_ng_core::Error> {
        let rt = http_ng_rt_tokio::Tokio;
        let tls = http_ng_tls_rustls::Rustls::with_platform_verifier()?;
        Ok(http_ng_native::Native::new(
            rt,
            tls,
            http_ng_dns_system::SystemDns::new(rt),
        ))
    }
}

#[derive(Debug)]
struct TooMany(u8);
impl std::fmt::Display for TooMany {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exceeded redirect limit of {}", self.0)
    }
}
impl std::error::Error for TooMany {}

#[derive(Debug)]
struct BadLocation;
impl std::fmt::Display for BadLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Location header is not a resolvable URI")
    }
}
impl std::error::Error for BadLocation {}
