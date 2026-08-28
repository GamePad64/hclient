//! Cross-platform async HTTP client.
//!
//! ```text
//! cargo add hclient --features default-transport
//! ```
//!
//! **The flag is not optional and it is not a default** — see below for
//! why. Without it `Client::new()` refuses to compile, naming the feature
//! and the command, and this page's own first line is what a reader
//! copies, so the two lines are together deliberately. The version is not written here: one was, `0.1.0-alpha.1`,
//! and it was still on the rendered page for `0.1.0-alpha.2` — a version in
//! prose goes stale on the release after it is written, and this one had
//! already been taken out of both READMEs and left here.
//!
//! ```no_run
//! # async fn f() -> Result<(), hclient::Error> {
//! let client = hclient::Client::new()?;
//! let text = client
//!     .get("https://example.com")
//!     .send()
//!     .await?
//!     .collect()
//!     .await?
//!     .text()?;
//! # let _ = text;
//! # Ok(()) }
//! ```
//!
//! [`Client`] is `Send + Sync + Clone + 'static`, so it lives in
//! application state and is shared across threads. **Clone it rather than
//! wrapping it** — the clone is an `Arc` bump, and an `Arc<Client>` is a
//! second one around the first. A request **and** the body it produces
//! both cross a `tokio::spawn`, so nothing here needs a `LocalSet` on a
//! multi-threaded runtime:
//!
//! ```no_run
//! # async fn f(client: hclient::Client) {
//! let c = client.clone();
//! let handle = tokio::spawn(async move { c.get("https://example.com").send().await });
//! # let _ = handle;
//! # }
//! ```
//!
//! **`collect()` is a step reqwest does not have**, and it is where this
//! client asks rather than assumes: a response arrives as a stream, and
//! `collect` is the point at which a caller says *read all of it into
//! memory*. `Response::chunk` is the other answer, and
//! [`ClientBuilder::response_limit`] bounds the first one.
//!
//! # Why `default-transport` is not a default
//!
//! Cargo unifies features across a graph, so a default here is a
//! **floor**: a library that took this crate with defaults would put
//! tokio, rustls and the system resolver into every graph that also
//! contains a crate wanting none of them, and the party who wanted the
//! small build is not the party who decides. Measured on a scratch
//! workspace rather than argued. The cost is one flag read before
//! compiling, against a graph nobody can get out of afterwards.
//!
//! # The `Send` rule, as it actually stands
//!
//! This line read *"not a single declared `Send`/`Sync` bound"* until
//! [`Client`] stopped naming its transport, and it is worth correcting
//! precisely rather than deleting: the invariant that matters was never
//! "no bound anywhere", it was **no bound on a seam**, because a bound
//! declared where the type is abstract propagates to backends that cannot
//! satisfy it. That is what the `no-send-or-sync` guard in `scripts/`
//! exists for, and what `hclient-rt-embassy` would have paid.
//!
//! So: the seams declare none, and auto-traits still decide. The rule for
//! where a bound is allowed is **an opt-in call that takes a value from
//! the caller and puts it behind the facade's `Arc`** — and the types that
//! hold such a value, like [`erased::AnyStore`] or
//! [`predicate::RedirectPredicate`]. [`Client::builder`],
//! [`ClientBuilder::total_timeout`] and [`sse::SseBuilder::with_timer`]
//! are the shape.
//!
//! **The rule is written here and the list is not**, which is this
//! paragraph's second correction rather than its first: an enumeration
//! stood here for one commit and was already wrong — it missed
//! `ClientBuilder::new` and three public types. Every site carries a
//! `send-bound-exception` marker naming the amendment that admits it, so
//! `grep` answers *which* and *how many*, and cannot go stale the way a
//! sentence does.
//!
//! Still absolutely true, and the half that was doing the work: **not a
//! single `#[cfg]`-switched trait alias.** A `Send`-ness that depends on
//! the target is a thing a portable library cannot reason about, which is
//! why one was refused during the erasure even though it would have given
//! native callers spawnable response bodies back.
//!
//! [`Client`] is `Send + Sync`, and so are both halves of a request now —
//! the future and the response body. That took naming rather than
//! requiring: the seams a transport awaits carry associated futures, so a
//! consumer can name them while each implementor still answers for itself,
//! and `SendTransport` is a separate trait whose impl may carry bounds
//! `Transport` does not. This paragraph read *what a request produces is
//! not `Send`* for two verticals, on the argument that one body type
//! serves every backend and the browser's held a `dyn Stream` with no auto
//! trait — true then, and answered by an actor in `hclient-fetch` rather
//! than by a `#[cfg]`.
#![forbid(unsafe_code)]

#[cfg(feature = "cache")]
pub mod cache;
mod cached;
mod client;
mod config;
#[cfg(feature = "cookies")]
pub mod cookie;
mod deadline;
mod decompress;
#[cfg(feature = "digest-auth")]
// **Off the front page, not out of the crate.** A caller's entry point is
// `RequestBuilder::digest_auth`; nothing inside is theirs to name.
// `DigestError` in particular is unobtainable — the one call site in
// `Client::run` reads `best_challenge` with `if let Ok(..)` and drops the
// error — so publishing its vocabulary promised a distinction a caller
// could never observe. It stays `pub` because the only consumer outside
// `src` is `tests/digest_vectors.rs`, which runs RFC 7616 §3.9's printed
// answers against `answer` directly, and an integration test sees the
// public API only. Same shape as `hclient-fetch`'s test seams.
#[doc(hidden)]
pub mod digest;
pub mod erased;
mod limit;
mod predicate;
/// Mock transport and controllable timer, re-exported from `hclient-mock`.
///
/// The doubles live in their own crate because a `Transport` implementation
/// sits *below* this facade: reaching them through `hclient::mock` meant a
/// transport author depending upward on the whole client. This re-export
/// exists so that callers who already had the facade see no change.
#[cfg(feature = "test-util")]
pub use hclient_mock as mock;
pub mod multipart;
mod request;
mod response;
pub mod sse;
mod stages;

pub use client::{Client, ClientBuilder};

// The bound that carries `Client::new`'s refusal when there is no default
// transport to build one from. It is re-exported because a `pub fn`'s
// where-clause may not name a trait the outside world cannot reach — a
// `private_bounds` warning, and this workspace's builds carry none — and
// hidden because nobody implements it and nobody names it: it exists to be
// printed. The gate is `client.rs`'s, negated there and repeated here for
// the same reason both halves of the positive gate are repeated.
#[cfg(not(any(
    all(feature = "default-transport", not(target_family = "wasm")),
    all(
        feature = "default-transport",
        target_family = "wasm",
        target_os = "unknown"
    )
)))]
#[doc(hidden)]
pub use client::without_a_default_transport::DefaultTransportFeature;

// ---- the doors ----
//
// **The front page listed 73 items and about twelve of them are what a
// caller uses.** The rest is seam vocabulary, error payloads a caller
// reaches only through `Error::source`, and the response-body wrappers
// that are public solely because [`body::ClientBody`] spells them out.
// rustdoc renders one flat alphabetical list, so `AnyList` — a type nobody
// writes — sat above `Client`.
//
// Nothing here changes a type. What changes is the path a reader types,
// and modules render before items, so the front page is now a handful of
// names and these doors. Done before the first publish, because after it
// every one of these paths is a promise.

/// The failures, and the payloads [`crate::Error::source`] hands back.
///
/// [`crate::Error`] and [`crate::ErrorKind`] stay at the root: they are on
/// every signature in the crate, and a door in front of them would be one
/// every caller walks through immediately.
pub mod error {
    pub use crate::config::InvalidBaseUrl;
    pub use crate::deadline::TotalTimeoutElapsed;
    pub use crate::decompress::DecodeFailed;
    pub use crate::limit::ResponseTooLarge;
    pub use crate::predicate::RedirectRefused;
    pub use crate::request::{ColonInUsername, ContentTypeIsNotOursToKeep};
    #[cfg(feature = "charset")]
    pub use crate::response::CharsetError;
    pub use crate::response::UnexpectedStatus;
    pub use hclient_core::{Phase, UnsupportedCapability, VersionNotAvailable};
    pub use hclient_proto::uri::UriError;
}

/// What a transport says it can do, and the `build()` gate that reads it.
pub mod caps {
    pub use crate::config::check_supported;
    pub use hclient_core::{
        Capabilities, DecompressionSupport, EarlyDataSupport, RedirectSupport, TimeoutSupport,
        TlsSupport,
    };
}

/// The observability seam: implement [`hooks::Hooks`] and match on
/// [`hooks::Event`].
pub mod hooks {
    pub use hclient_core::unversioned::{
        CloseReason, Closed, ConnectTiming, Connected, ConnectionId, Event, Head, Hooks,
        Informational, NoHooks, Reused,
    };
}

/// The response body and the wrappers a client puts around a transport's.
///
/// A caller names none of these: they are here because
/// [`body::ClientBody`] is an alias over all four, and an alias cannot
/// name a private type.
pub mod body {
    pub use crate::cached::Cached;
    pub use crate::deadline::Deadline;
    pub use crate::decompress::Decompressed;
    pub use crate::limit::Limited;
    pub use hclient_core::{RetryKind, RewindFactory};

    /// The response body a [`crate::Client`] hands back: the transport's own body,
    /// with this client's three wrappers around it **in the order the client
    /// applies them**.
    ///
    /// The order is not a formatting choice. [`Cached`] is innermost, because
    /// it is the one wrapper that can *replace* the transport's body — a cache
    /// hit had no exchange and so has no `B` at all — and because what it
    /// records must be what the wire carried. [`Deadline`] goes around that,
    /// so it is polled once for every frame that arrives off the wire;
    /// [`Decompressed`] goes outside both, because reversing a content coding
    /// can consume many compressed frames before it produces a single byte for
    /// the caller. Written the other way round, one poll of the decoder could
    /// pull an unbounded amount of traffic without the clock ever being
    /// consulted — a slow server sending well-compressing padding would be
    /// bounded by nothing. See `Client::execute`, the `decompress` module's
    /// doc comment, and `cached`'s.
    ///
    /// The cache being **below** the decompressor is the load-bearing half of
    /// that order: a stored response is decoded on the way out by the same
    /// call that decodes a fresh one, and a `Vary: Accept-Encoding` entry is
    /// keyed on the coding actually asked for.
    ///
    /// All three wrappers are always present, whether or not any is doing
    /// anything: a type cannot appear and disappear with a runtime value. An
    /// unbounded, undecoded, uncached response pays two `Option` tests and one
    /// enum test per frame for that — and without the `cache` feature
    /// [`Cached`] is a newtype over `Option<B>` whose other two fields do not
    /// exist, so the third wrapper costs nothing a build did not ask for.
    /// The transport's body is erased into
    /// [`hclient_core::unversioned::erased::BoxBody`] at the seam, so this
    /// alias names no type parameters at all — which is what lets
    /// [`crate::Response`] and [`crate::Client`] name none either.
    pub type ClientBody = Limited<Decompressed<Deadline<Cached<BoxBody>>>>;

    /// The transport's own body, erased — the innermost layer of
    /// [`ClientBody`], re-exported so a caller naming the chain does not
    /// have to reach into `hclient-core` for one type.
    pub use hclient_core::unversioned::erased::BoxBody;
}

/// Following redirects: how many, and whether this one.
pub mod redirect {
    pub use crate::predicate::{ProposedRedirect, RedirectPredicate, RedirectVerdict};
    pub use hclient_proto::redirect::RedirectPolicy;
}

/// Reaching origins through a proxy, behind the `proxy` feature.
///
/// A proxy is configured on the **transport**, not on the `Client` — it
/// answers *where does this connection go*, which is a question the thing
/// that opens connections owns. So the shape is
/// [`default_transport`], then one of these,
/// then [`Client::builder`](crate::Client::builder):
///
/// ```no_run
/// # #[cfg(all(feature = "proxy", not(target_family = "wasm")))]
/// # fn f() -> Result<(), Box<dyn std::error::Error>> {
/// use hclient::proxy::{HttpConnect, Proxy};
///
/// let transport = hclient::default_transport()?
///     .proxy(Proxy::new(HttpConnect::new(), "proxy.corp", 8080).bypass([".internal"]));
/// let client = hclient::Client::builder(transport).build()?;
/// # Ok(()) }
/// ```
///
/// With the `system-proxy` feature, the machine's own settings instead —
/// `HTTP_PROXY` and `HTTPS_PROXY` where the environment names them, the
/// registry on Windows, the dynamic store on macOS:
///
/// ```no_run
/// # #[cfg(all(feature = "system-proxy", not(target_family = "wasm")))]
/// # fn f() -> Result<(), Box<dyn std::error::Error>> {
/// let client = hclient::Client::builder(hclient::default_transport()?.system_proxy()?).build()?;
/// # Ok(()) }
/// ```
///
/// These are `hclient-native`'s types, re-exported. A caller who builds a
/// transport of their own — a different runtime, a different TLS backend
/// — reaches them there and needs nothing from here.
#[cfg(all(feature = "proxy", not(target_family = "wasm")))]
pub mod proxy {
    /// The machine's proxy settings as data, for a caller who wants to
    /// read them and decide rather than hand them straight over.
    #[cfg(feature = "system-proxy")]
    pub use hclient_native::proxy::system;
    pub use hclient_native::proxy::{
        Approach, ConnectError, Handshake, HttpConnect, NoProxy, Proxy, ProxyRefused, ProxyScheme,
        Socks4, Socks4HandshakeError, Socks4Refused, Socks5, Socks5HandshakeError, Socks5Refused,
        Step,
    };
}

/// The transport [`Client::new`] would have built, for a caller who wants
/// to configure it first.
///
/// `Client::new()` is `Client::builder(default_transport()?).build()`,
/// and this is the half in the middle — which is where a proxy, a socket
/// option or an `h1_opts` bound is set, because all of them belong to the
/// thing that opens connections rather than to the client above it.
///
/// ```no_run
/// # #[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
/// # fn f() -> Result<(), Box<dyn std::error::Error>> {
/// let client = hclient::Client::builder(hclient::default_transport()?).build()?;
/// # Ok(()) }
/// ```
///
/// # It does **not** read the machine's proxy settings, and `Client::new` does
///
/// The asymmetry is deliberate and it is about what comes next. This is
/// the seam for *configuring* the transport, and a configuration step
/// that silently installed a proxy would change what the following calls
/// do: `unix_socket` **refuses** when a proxy is configured, so
/// `default_transport()?.unix_socket(path)` would start failing on
/// machines that happen to have an `HTTP_PROXY` and not on others. An
/// environment-dependent failure in a builder chain is worse than an
/// explicit line.
///
/// So the convenience constructor is the good citizen and the seam does
/// exactly what it is told. A caller who wants both writes the line:
/// `hclient::default_transport()?.system_proxy()?`.
///
/// # The signature forks by target, exactly as `Client::new`'s does
///
/// On `wasm32-unknown-unknown` the default transport is the browser's
/// `fetch`, whose constructor cannot fail, so there is no `Result` and no
/// `?` — the same single difference `Client::new` has, for the same
/// reason and in the same place. See [`DefaultTransport`].
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
pub fn default_transport() -> Result<DefaultTransport, Error> {
    Client::default_native_transport()
}

/// The browser branch of [`default_transport`] — infallible, because
/// `Fetch::new()` is.
#[cfg(all(
    feature = "default-transport",
    target_family = "wasm",
    target_os = "unknown"
))]
pub fn default_transport() -> DefaultTransport {
    hclient_fetch::Fetch::new()
}

pub use config::{Config, Timeouts, effective_timeouts};
pub use deadline::NoClock;

// This list must cover not just `Capabilities`/
// `RequestBody`/`UnsupportedCapability`, but EVERY `hclient-core` type
// reachable from the signature, a field, or a variant of something already
// re-exported here — otherwise a consumer depending only on `hclient` can't
// name a type they already have in hand. `Error` is the most common case:
// returned by `Client::execute`, `RequestBuilder::send`, `Response::chunk`/
// `collect`, `Collected::text`/`json`, `SseStream::new`/`next`, and
// `mock::MockTransport::push_response_frames_then_error` even takes it as a
// parameter. `ErrorKind` is re-exported for `Error::kind()`, `Phase` for the
// `ErrorKind::Timeout(Phase)` variant, both needed so the result of
// `.kind()` can be compared to anything at all. `RetryKind` is the
// invariant of `RequestBody::retry_kind()` and the field
// `mock::RecordedRequest::retry_kind`. `RedirectSupport`/`TlsSupport`/
// `TimeoutSupport`/`EarlyDataSupport` are `Capabilities` fields that need
// naming to hand-assemble your own `Capabilities` for
// `MockTransport::with_capabilities` (available even without
// `unversioned::Transport` — `with_capabilities` is an ordinary tooling
// method). `RewindFactory` is the type of the `RequestBody::Rewindable`
// variant; not strictly blocking (it's an alias, `Arc<dyn Fn() ->
// RequestBody + Send + Sync>` is expressible without the alias's name too),
// re-exported for nameability and symmetry with the other variants.
//
// `unversioned::{Transport, Timer}` are deliberately NOT re-exported: that's
// a quarantine contract for backend/runtime authors (see the doc comment on
// `hclient-core/src/unversioned/mod.rs`), not part of the facade for a
// consumer who just builds requests and reads responses. That decision had
// a cost — `client.transport().capabilities()` would need `Transport` in
// scope, since `capabilities()` is a trait method there and `.transport()`
// hands back a bare `&T` — so a `Client::capabilities()` forwarder
// (`client.rs`) pays it instead of re-exporting the trait: the most common
// question about `Capabilities` is answered, the quarantine stays a
// quarantine.
// `AllowEarlyData` and `EarlyDataSupport` are on this list for the reason
// every other capability type is, and one of their own: `AllowEarlyData` is
// a value the CALLER puts on a request — it is the only thing that can
// admit a request to 0-RTT, and `Client::run`'s `425` branch is what takes
// it back off a replay — so a facade that names `TimeoutSupport` while
// making callers reach past it into `hclient-core` for this one would be
// arbitrary.
//
// `UpgradeSupport` used to be on this list and is gone from the workspace:
// four variants, every backend answering `None`, and no caller decision
// turning on it. WebSocket is a trait a backend implements
// (`hclient_core::unversioned::WebSocketConnect`) rather than a capability
// anyone reads, so there is nothing to re-export in its place.
// `tests/facade.rs`'s plumbing check moved
// to `EarlyDataSupport`, and that file says why it and not another.
//
// `RequireVersion` and `VersionNotAvailable` arrive on the same argument as
// `AllowEarlyData`, one step further: `RequireVersion` is a value the
// CALLER puts on a request, and `VersionNotAvailable` is the `source()` a
// caller downcasts to in order to tell "this connection cannot do what I
// asked" from every other `ErrorKind::Unsupported`. A facade that named the
// mark and hid its refusal would leave the error unmatchable without a
// direct dependency on `hclient-core`. `check_version` is NOT re-exported:
// it is the seam's own comparison, for transports, and a consumer has
// nothing to call it on.
pub use hclient_core::{AllowEarlyData, Error, ErrorKind, RequestBody, RequireVersion};
// The observability seam, re-exported for the same reason
// `AllowEarlyData` is: what a caller writes is an `impl Hooks for MyType`
// and a `match` over `Event`, and both live in the core. A facade that
// let a caller *set* a hook (through the transport) but not *name* the
// trait it must implement would force a direct dependency on
// `hclient-core` for the one thing this feature exists to make easy.
//
// `Hooks` and `NoHooks` come from `unversioned`, which is the semver
// quarantine — one backend implements this and the vocabulary has not
// been tried against a second (see that module's own doc). Re-exporting
// them here does not promise otherwise: the quarantine is a statement
// about the trait, not about where its name is written.
// Every URL this client is handed becomes an `http::Uri` through
// `hclient_proto::uri`, and every way that can fail — a base that is
// not a base, a host `http::Uri` will not hold, a non-ASCII host in a
// build without the `idn` feature — arrives at the caller as this type,
// as the `source()` of an `ErrorKind::Other`. It is re-exported for the
// same reason `InvalidBaseUrl` is: a caller has to be able to name it to
// tell "the URL you gave me is wrong" apart from every other `Other`.
pub use request::RequestBuilder;
pub use response::{Collected, Response};

/// The default transport, chosen by **the target, not the user**.
///
/// The default is an opinion, not a restriction: [`crate::Client::new`]
/// builds one of these, and `Client::builder(whatever)` builds a `Client`
/// over any other backend — the type is the same either way, because
/// `Client` names no transport. No mutually exclusive cargo features
/// arise, because it's the target that chooses, not a set of enabled
/// features — `default-transport` only turns `Client::new()` and this type
/// on or off as a whole; it doesn't choose BETWEEN variants: on each
/// concrete target exactly one branch below compiles (or none, see further
/// down).
///
/// # What resolves under which feature — measured, not assumed
///
/// - Without the `default-transport` feature (which is not in `default`,
///   deliberately — see the crate doc): the type doesn't exist at all.
///   Naming `hclient::DefaultTransport` is "cannot find type", and
///   `Client::new()` is a refusal that names the feature and the command
///   to add it — `client.rs`'s `without_a_default_transport`, which exists
///   because rustc's own *"found an item that was configured out"* note is
///   emitted for path resolution and not for associated-item lookup, so
///   `default_transport()` announces its gate and an inherent `fn` in a
///   `#[cfg]`-ed-out `impl` block did not. Either way a compile error and
///   not a silent fallback to something weaker: the same decision the TLS
///   backends make about trust anchors, where a build without a verifier
///   fails to compile rather than silently trusting everything.
/// - With the `default-transport` feature on any NON-wasm target
///   (linux/macos/windows — the only branch below,
///   `not(target_family = "wasm")`): `Native<Tokio, Rustls,
///   SystemDns<Tokio>>` — `hclient-rt-tokio` as the runtime, `hclient-tls-
///   rustls` with `rustls-platform-verifier` (the OS's system trust store,
///   not `webpki-roots`: `Client::new()` is a client that "just works", like
///   a browser user's or `curl`'s, not a client with an explicitly chosen
///   set of root certificates), `hclient-dns-system` for `getaddrinfo` via
///   the `Blocking` capability. Pulls in `tokio` unconditionally (see the
///   README, "What's in the dependency graph": `hyper` depends on `tokio`
///   even on the HTTP/1 path, not just this branch).
/// - With the `default-transport` feature on `wasm32-unknown-unknown`
///   (browser): `hclient_fetch::Fetch` — the second branch below.
///   `Client::new()` on this target returns `Self`, not a
///   `Result`: `Fetch::new()` has no fallible step (see `Client::new`'s own
///   doc comment in `client.rs`).
/// - With the `default-transport` feature on `wasm32-wasip2`/
///   `wasm32-wasip1` (`target_os = "wasi"`): there's still no branch below
///   — the type doesn't exist, and `Client::new()` takes the same refusal
///   as without the feature at all, whose third note is written for
///   exactly this case: enabling the feature does not help here. The WASI
///   transport (`hclient_wasi::WasiHttp`)
///   already exists and can be used directly via
///   `Client::builder(hclient_wasi::WasiHttp::new())` — but NOT through
///   this mechanism: `hclient` deliberately doesn't depend on
///   `hclient-wasi` (`hclient-wasi/Cargo.toml` itself records this as an
///   invariant — it has `hclient` in `dev-dependencies` for its own
///   example, no reverse dependency exists), and adding one here would mean
///   adding a path that no CI job in this repository builds: the
///   `wasip2` job runs `hclient-wasi` directly, not `hclient` under
///   `default-transport` on wasm. An optional branch never checked by a
///   build is exactly the "implicitly swapping an error message for
///   imagined availability" that this type is specifically obligated not
///   to do. The decision is left as a finding rather than taken
///   silently: the black-box acceptance test
///   (`crates/hclient/tests/two_runtimes.rs`) doesn't require this branch —
///   both of its tests build `Native` explicitly, the same way
///   `Client::builder` does. Note that the browser branch below does NOT
///   set this precedent aside: it is checked by a build, on every push
///   (`cargo check -p hclient --features test-util --target
///   wasm32-unknown-unknown`, in CI's `msrv` job) and executed by real
///   browser tests on two engines (`crates/hclient/tests/wasm_default.rs`,
///   run by the `browser` job under `wasm-pack`).
///
///   That sentence was written one commit before it became true: when this
///   type was added, the `msrv` job ran that command for `hclient-fetch`
///   only, and no CI job executed a browser test at all. Both arrived in the
///   two commits that followed. It is stated here because the asymmetry
///   above rests on it — "CI builds one branch and not the other" is only an
///   argument while it is a fact, and it is worth knowing that it briefly
///   was not one.
///
///   Note the command carries `--features test-util`, not `--all-features`:
///   `--all-features` on this target pulls native-only dev-dependencies that
///   do not build there. The narrower flag checks this branch, which is what
///   the argument needs, and nothing wider.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
pub type DefaultTransport = hclient_native::Native<
    hclient_rt_tokio::Tokio,
    hclient_tls_rustls::Rustls,
    hclient_dns_system::SystemDns<hclient_rt_tokio::Tokio>,
    hclient_core::unversioned::NoHooks,
    // **It names `HttpConnect` even where no proxy is configured**, and
    // that is what keeps it one type. `Client::new` reads the machine's
    // settings, so on a proxied machine the transport it builds holds
    // HTTP proxies and on every other machine it holds an empty list of
    // them. Naming `NoProxy` here would have made `Client::new`'s
    // transport a *different type* from `default_transport()`'s, and
    // `transport_as::<DefaultTransport>()` — the documented way past the
    // facade — would work on one machine and not on the next.
    hclient_native::HttpConnect,
>;

/// The default transport on `wasm32-unknown-unknown`: the browser `fetch`
/// API, via `hclient-fetch`.
///
/// The forked declaration (this item and the one above are the same name
/// under mutually exclusive `#[cfg]`s) is what "chosen by the target, not
/// the user" means in practice — see the other branch's doc comment for
/// the full per-target table, including why `wasm32-wasip2` deliberately
/// still has no branch at all despite `hclient_wasi::WasiHttp` existing.
///
/// `all(target_family = "wasm", target_os = "unknown")`, not a bare
/// `target_family = "wasm"`: WASI targets are `wasm` too, and the whole
/// point of the paragraph above is that they must keep resolving to
/// nothing.
///
/// What this transport can and cannot do is not hidden behind the
/// convenience: `Fetch` reports `RedirectSupport::Internal` and no timeout
/// support at all, so a client configured with a `RedirectPolicy` or any
/// phase timeout is an `UnsupportedCapability` at `build()` rather than a
/// setting that silently does nothing. `Client::new()` itself configures
/// neither, which is exactly why it can be infallible.
#[cfg(all(
    feature = "default-transport",
    target_family = "wasm",
    target_os = "unknown"
))]
pub type DefaultTransport = hclient_fetch::Fetch;

/// The clock `Client` measures a total timeout with when the caller does
/// not supply one — chosen by **the target**, exactly like
/// [`DefaultTransport`], and for the same reason: a clock that only exists
/// on one target would put a `#[cfg]` in the facade crate, which is what
/// `crates/hclient-rt-pair-check` exists to prevent and what
/// `SseBuilder::with_timer`'s doc comment already argues against at
/// length.
///
/// This alias is the second type parameter's default, so
/// `Client::new()?.total_timeout(d)` needs no clock argument **and stays
/// `Client`** — the type does not grow parameters because a timeout was
/// switched on. That property is not cosmetic: `struct App { http: Client
/// }` is the shape a consumer actually writes, and it is the same ground
/// on which tower layers were rejected for compression.
/// `crates/hclient/tests/deadline_client_type.rs` pins it.
///
/// - Non-wasm, with `default-transport`: [`hclient_rt_tokio::Tokio`] — the
///   very clock already inside `DefaultTransport`
///   (`Native<Tokio, Rustls, SystemDns<Tokio>>`), not a second one. Its
///   `sleep` panics outside a tokio runtime, the same condition
///   [`Client::new`] already documents.
/// - `wasm32-unknown-unknown`, with `default-transport`:
///   `hclient_fetch::BrowserClock`, a `setTimeout` clock. `Fetch` has no
///   clock inside it, so unlike the native branch this one is a genuinely
///   separate choice — the browser's transport and the browser's clock are
///   two independent facts.
/// - Without the feature: [`NoClock`], which cannot measure anything. Every
///   setter that could put a bound on a client with this clock is
///   `#[cfg]`-ed away with the feature, so nothing silently fails to be
///   measured — see [`NoClock`]'s own doc comment for the complete list.
/// - `wasm32-wasip2` with the feature: [`NoClock`], the same as without it.
///   **This used to have no branch at all**, on the reasoning that it was
///   the same deliberate compile error as [`DefaultTransport`] there — and
///   the two are not the same. `DefaultTransport` is named only by someone
///   asking for it, so its absence is a refusal aimed at that line.
///   `DefaultClock` is the default type parameter of `ClientBuilder`,
///   `RequestBuilder` and both forks of [`Client`], so its absence does not
///   refuse anything: it stops the crate compiling at all. And **Cargo
///   unifies features across a graph**, so the trigger was never the wasip2
///   user's own choice — any unrelated crate turning `default-transport` on
///   broke their build. `DefaultTransport` is still absent here, which is
///   where the refusal belongs and where it still reads as one.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
pub type DefaultClock = hclient_rt_tokio::Tokio;

/// The browser branch of [`DefaultClock`] — see the other branch's doc
/// comment for the full per-target table.
#[cfg(all(
    feature = "default-transport",
    target_family = "wasm",
    target_os = "unknown"
))]
pub type DefaultClock = hclient_fetch::BrowserClock;

/// The clockless branch of [`DefaultClock`], and the one that must catch
/// everything the other two do not: without the `default-transport`
/// feature there is no target-chosen clock to point at, and on
/// `wasm32-wasip2` there is none *with* it either, because that target has
/// no [`DefaultTransport`] to take one from. Either way the default clock
/// is the one that measures nothing. See the first branch's doc comment,
/// and [`NoClock`] for why that is not a silent no-op.
///
/// The condition is the negation of the two branches above rather than a
/// third guess at the target list, so the three are exhaustive and
/// non-overlapping by construction: exactly one arm matches every
/// (target, feature) pair.
#[cfg(any(
    not(feature = "default-transport"),
    all(target_family = "wasm", not(target_os = "unknown")),
))]
pub type DefaultClock = NoClock;
