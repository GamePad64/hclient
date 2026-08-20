//! Cross-platform async HTTP client.
//!
//! Crate invariant: not a single declared `Send`/`Sync` bound, not a single
//! `#[cfg]`-switched trait alias. Send-ness is inferred by auto-traits.
#![forbid(unsafe_code)]

/// The response cache, re-exported from `hclient-cache` — the `cache`
/// feature.
///
/// Re-exported for the reason `cookie` is, and with the same arrangement:
/// the cache is a leaf crate that knows nothing about this facade (nor
/// about `hclient-core`, nor about any transport), which is what makes
/// "caching behaves the same behind every backend" structural. A consumer
/// needs the names to call [`ClientBuilder::cache`], to read
/// [`Client::cache`], and to set [`Limits`](hclient_cache::Limits) or
/// supply their own [`CacheStore`](hclient_cache::CacheStore) — and should
/// not have to take a second dependency for them.
#[cfg(feature = "cache")]
pub use hclient_cache as cache;
mod cached;
mod client;
mod config;
/// The cookie jar, re-exported from `hclient-cookie` — the `cookies`
/// feature.
///
/// Re-exported for the same reason `mock` is, and with the same
/// arrangement: the jar is a leaf crate that knows nothing about this
/// facade (nor about `hclient-core`, nor about any transport), which is
/// what makes "cookies behave the same behind every backend" structural.
/// A consumer only needs the name to call
/// [`ClientBuilder::cookie_jar`] and to read [`Client::cookies`], and
/// should not have to take a second dependency for it.
#[cfg(feature = "cookies")]
pub use hclient_cookie as cookie;
mod deadline;
mod decompress;
#[cfg(feature = "digest-auth")]
pub mod digest;
mod erased;
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
mod sse;
mod stages;

pub use cached::Cached;
pub use client::{Client, ClientBuilder};
pub use config::{Config, InvalidBaseUrl, Timeouts, check_supported, effective_timeouts};
pub use deadline::{Deadline, NoClock, TotalTimeoutElapsed};
pub use decompress::{DecodeFailed, Decompressed};
#[cfg(feature = "cookies")]
pub use erased::AnyList;
#[cfg(feature = "cache")]
pub use erased::AnyStore;
pub use limit::{Limited, ResponseTooLarge};
pub use predicate::{ProposedRedirect, RedirectPredicate, RedirectRefused, RedirectVerdict};

/// The response body a [`Client`] hands back: the transport's own body,
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
pub type ClientBody<B, Tm> = Limited<Decompressed<Deadline<Cached<B>, Tm>>>;
// Task 17 fix round 1: this list must cover not just `Capabilities`/
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
// hands back a bare `&T` — fix round 2 removed that cost with a
// `Client::capabilities()` forwarder (`client.rs`) instead of re-exporting
// the trait: the most common question about `Capabilities` is answered,
// the quarantine stays a quarantine.
// `AllowEarlyData` and `EarlyDataSupport` are on this list for the reason
// every other capability type is, and one of their own: `AllowEarlyData` is
// a value the CALLER puts on a request — it is the only thing that can
// admit a request to 0-RTT, and `Client::run`'s `425` branch is what takes
// it back off a replay — so a facade that names `TimeoutSupport` while
// making callers reach past it into `hclient-core` for this one would be
// arbitrary.
//
// `UpgradeSupport` used to be on this list and is gone from the workspace
// (v0.3 W4 step 4): four variants, every backend answering `None`, and no
// caller decision turning on it. WebSocket is a trait a backend implements
// (`hclient_core::unversioned::WebSocketConnect`) rather than a capability
// anyone reads, so there is nothing to re-export in its place — see
// `docs/w4-upgrade-seam.md` §3. `tests/facade.rs`'s plumbing check moved
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
pub use hclient_core::{
    AllowEarlyData, Capabilities, DecompressionSupport, EarlyDataSupport, Error, ErrorKind, Phase,
    RedirectSupport, RequestBody, RequireVersion, RetryKind, RewindFactory, TimeoutSupport,
    TlsSupport, UnsupportedCapability, VersionNotAvailable,
};
// The observability seam (v0.4 W2), re-exported for the same reason
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
pub use hclient_core::unversioned::{
    CloseReason, Closed, ConnectTiming, Connected, ConnectionId, Event, Head, Hooks, Informational,
    NoHooks, Reused,
};
pub use hclient_proto::backoff::Backoff;
pub use hclient_proto::redirect::RedirectPolicy;
// Every URL this client is handed becomes an `http::Uri` through
// `hclient_proto::uri`, and every way that can fail — a base that is
// not a base, a host `http::Uri` will not hold, a non-ASCII host in a
// build without the `idn` feature — arrives at the caller as this type,
// as the `source()` of an `ErrorKind::Other`. It is re-exported for the
// same reason `InvalidBaseUrl` is: a caller has to be able to name it to
// tell "the URL you gave me is wrong" apart from every other `Other`.
pub use hclient_proto::sse::{DEFAULT_MAX_EVENT_SIZE, SseEvent};
pub use hclient_proto::uri::UriError;
pub use request::{ColonInUsername, ContentTypeIsNotOursToKeep, RequestBuilder};
#[cfg(feature = "charset")]
pub use response::CharsetError;
pub use response::{Collected, Response, UnexpectedStatus};
pub use sse::{ReconnectingSseBuilder, ReconnectingSseStream, SseBuilder, SseOptions, SseStream};

/// The default transport, chosen by **the target, not the user**.
///
/// The default is an opinion, not a restriction: `Client` with no parameter
/// means `Client<DefaultTransport>`, and `Client<Whatever>` works just the
/// same. No mutually exclusive cargo features arise, because it's the
/// target that chooses, not a set of enabled features — `default-transport`
/// only turns the ability to write `Client` with no parameter on or off as
/// a whole; it doesn't choose BETWEEN variants: on each concrete target
/// exactly one branch below compiles (or none, see further down).
///
/// # What resolves under which feature — measured, not assumed
///
/// - Without the `default-transport` feature (the crate's default,
///   `default = []`): the type doesn't exist at all. `Client` with no
///   parameter, or `hclient::DefaultTransport`, is an ordinary compile
///   error ("cannot find type", "missing generics"), not a silent fallback
///   to something weaker. The same decision Task 9 of vertical 2 already
///   verified empirically for trusted TLS anchors: a build without a
///   verifier fails to compile rather than silently trusting everything.
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
///   (browser): `hclient_fetch::Fetch` — the second branch below, added in
///   vertical 3 once that transport existed and was tested in a real
///   browser. `Client::new()` on this target returns `Self`, not a
///   `Result`: `Fetch::new()` has no fallible step (see `Client::new`'s own
///   doc comment in `client.rs`).
/// - With the `default-transport` feature on `wasm32-wasip2`/
///   `wasm32-wasip1` (`target_os = "wasi"`): there's still no branch below
///   — the type doesn't exist, the same honest compile error as without
///   the feature at all. The WASI transport (`hclient_wasi::WasiHttp`)
///   already exists (vertical 1) and can be used directly via
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
///   to do. The decision is left as a finding instead of silently
///   following the brief: the vertical's black-box acceptance test
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
/// }` is the shape a consumer actually writes, and `docs/v02-design.md`
/// §W5 rejects tower layers for compression on exactly this ground.
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
/// - `wasm32-wasip2` with the feature: no branch, the same deliberate
///   compile error as [`DefaultTransport`] there.
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

/// The clockless branch of [`DefaultClock`]: without the
/// `default-transport` feature there is no target-chosen clock to point
/// at, so the default clock is the one that measures nothing. See the
/// first branch's doc comment, and [`NoClock`] for why that is not a
/// silent no-op.
#[cfg(not(feature = "default-transport"))]
pub type DefaultClock = NoClock;
