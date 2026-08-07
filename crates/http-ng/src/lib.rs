//! Cross-platform async HTTP client.
//!
//! Crate invariant: not a single declared `Send`/`Sync` bound, not a single
//! `#[cfg]`-switched trait alias. Send-ness is inferred by auto-traits.
#![forbid(unsafe_code)]

mod client;
mod config;
#[cfg(feature = "test-util")]
pub mod mock;
mod request;
mod response;
mod sse;
mod stages;

pub use client::{Client, ClientBuilder};
pub use config::{Config, InvalidBaseUrl, Timeouts, check_supported, effective_timeouts};
// Task 17 fix round 1: this list must cover not just `Capabilities`/
// `RequestBody`/`UnsupportedCapability`, but EVERY `http-ng-core` type
// reachable from the signature, a field, or a variant of something already
// re-exported here — otherwise a consumer depending only on `http-ng` can't
// name a type they already have in hand. `Error` is the most common case:
// returned by `Client::execute`, `RequestBuilder::send`, `Response::chunk`/
// `collect`, `Collected::text`/`json`, `SseStream::new`/`next`, and
// `mock::MockTransport::push_response_frames_then_error` even takes it as a
// parameter. `ErrorKind` is re-exported for `Error::kind()`, `Phase` for the
// `ErrorKind::Timeout(Phase)` variant, both needed so the result of
// `.kind()` can be compared to anything at all. `RetryKind` is the
// invariant of `RequestBody::retry_kind()` and the field
// `mock::RecordedRequest::retry_kind`. `RedirectSupport`/`TlsSupport`/
// `TimeoutSupport`/`UpgradeSupport` are `Capabilities` fields that need
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
// `http-ng-core/src/unversioned/mod.rs`), not part of the facade for a
// consumer who just builds requests and reads responses. That decision had
// a cost — `client.transport().capabilities()` would need `Transport` in
// scope, since `capabilities()` is a trait method there and `.transport()`
// hands back a bare `&T` — fix round 2 removed that cost with a
// `Client::capabilities()` forwarder (`client.rs`) instead of re-exporting
// the trait: the most common question about `Capabilities` is answered,
// the quarantine stays a quarantine.
pub use http_ng_core::{
    Capabilities, Error, ErrorKind, Phase, RedirectSupport, RequestBody, RetryKind, RewindFactory,
    TimeoutSupport, TlsSupport, UnsupportedCapability, UpgradeSupport,
};
pub use http_ng_proto::backoff::Backoff;
pub use http_ng_proto::redirect::RedirectPolicy;
pub use http_ng_proto::sse::{DEFAULT_MAX_EVENT_SIZE, SseEvent};
pub use request::RequestBuilder;
pub use response::{Collected, Response};
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
///   parameter, or `http_ng::DefaultTransport`, is an ordinary compile
///   error ("cannot find type", "missing generics"), not a silent fallback
///   to something weaker. The same decision Task 9 of vertical 2 already
///   verified empirically for trusted TLS anchors: a build without a
///   verifier fails to compile rather than silently trusting everything.
/// - With the `default-transport` feature on any NON-wasm target
///   (linux/macos/windows — the only branch below,
///   `not(target_family = "wasm")`): `Native<Tokio, Rustls,
///   SystemDns<Tokio>>` — `http-ng-rt-tokio` as the runtime, `http-ng-tls-
///   rustls` with `rustls-platform-verifier` (the OS's system trust store,
///   not `webpki-roots`: `Client::new()` is a client that "just works", like
///   a browser user's or `curl`'s, not a client with an explicitly chosen
///   set of root certificates), `http-ng-dns-system` for `getaddrinfo` via
///   the `Blocking` capability. Pulls in `tokio` unconditionally (see the
///   README, "What's in the dependency graph": `hyper` depends on `tokio`
///   even on the HTTP/1 path, not just this branch).
/// - With the `default-transport` feature on `wasm32-unknown-unknown`
///   (browser) OR `wasm32-wasip2`/`wasm32-wasip1` (`target_os = "wasi"`):
///   there's no branch below for either target — the type doesn't exist,
///   the same honest compile error as without the feature at all. The
///   browser transport is vertical 3, not yet started. The WASI transport
///   (`http_ng_wasi::WasiHttp`) already exists (vertical 1) and can be used
///   directly via `Client::builder(http_ng_wasi::WasiHttp::new())` — but
///   NOT through this mechanism: `http-ng` deliberately doesn't depend on
///   `http-ng-wasi` (`http-ng-wasi/Cargo.toml` itself records this as an
///   invariant — it has `http-ng` in `dev-dependencies` for its own
///   example, no reverse dependency exists), and adding one here would mean
///   adding a path that no CI job in this repository builds: the
///   `wasip2` job runs `http-ng-wasi` directly, not `http-ng` under
///   `default-transport` on wasm. An optional branch never checked by a
///   build is exactly the "implicitly swapping an error message for
///   imagined availability" that this type is specifically obligated not
///   to do. The decision is left as a finding instead of silently
///   following the brief: the vertical's black-box acceptance test
///   (`crates/http-ng/tests/two_runtimes.rs`) doesn't require this branch —
///   both of its tests build `Native` explicitly, the same way
///   `Client::builder` does.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
pub type DefaultTransport = http_ng_native::Native<
    http_ng_rt_tokio::Tokio,
    http_ng_tls_rustls::Rustls,
    http_ng_dns_system::SystemDns<http_ng_rt_tokio::Tokio>,
>;
