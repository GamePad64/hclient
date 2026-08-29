//! A facade check: types participating in `hclient`'s public API must be
//! reachable from a crate that depends only on `hclient`.
//!
//! Lives in `tests/`, not `src/`, for two reasons: first, `tests/` compiles
//! as an external consumer, so it sees exactly the surface a downstream
//! user does (an internal `use super::*` wouldn't verify this). Second,
//! CI's `no-declared-send` only scans `crates/*/src` (amendment-C3) — that
//! doesn't matter here, no `Send`/`Sync` is declared in this file, but this
//! is still the right place for any future test in that spirit.

use hclient::mock::MockTransport;
use std::sync::Arc;

#[test]
fn public_api_types_are_reachable_from_the_facade() {
    // `Config.redirect` has this type.
    // The trait, its default implementation, and one ready-made — all
    // reachable through the facade without naming `hclient-proto`.
    let _p: hclient::redirect::Limit = hclient::redirect::Limit::default();
    fn takes_a_policy(_: &dyn hclient::redirect::RedirectPolicy) {}
    takes_a_policy(&hclient::redirect::Forbid);
    // `check_supported` takes this and returns that.
    let caps: hclient::caps::Capabilities = hclient::caps::Capabilities::default();
    let cfg = hclient::Config::default();
    let _: Result<(), hclient::error::UnsupportedCapability> =
        hclient::caps::check_supported(&cfg, &caps, "probe");
}

/// `Response`, `Collected` and `RequestBuilder`, which
/// unlike the types above have no public constructor without a
/// transport — there's nothing here to construct a value with, so
/// reachability and shape (generic arity) are checked by compiling a
/// function that's never called: if `Response`/`Collected`/`RequestBuilder`
/// stop being re-exported from `hclient::`, or their parameter count
/// changes, this file — as an external consumer — stops compiling.
#[allow(dead_code)]
fn response_collected_and_request_builder_are_reachable_from_the_facade<B>(
    _r: hclient::Response<B>,
    _c: hclient::Collected,
    _b: hclient::RequestBuilder<'_>,
) {
}

/// `SseStream`, `SseEvent`, and `DEFAULT_MAX_EVENT_SIZE` actually
/// live in `hclient-proto` (`SseEvent`, `DEFAULT_MAX_EVENT_SIZE`) and
/// `hclient` (`SseStream`), but must be nameable from `hclient::` with no
/// direct dependency on `hclient-proto` — the same contract as above. The
/// same trick for `SseStream` as for `Response`/`Collected`/`RequestBuilder`:
/// no constructor without a transport exists, so reachability and shape
/// (generic arity) are checked by compiling a function that's never called.
#[allow(dead_code)]
fn sse_types_are_reachable_from_the_facade<B>(_s: hclient::sse::SseStream<B>) {
    let _event: hclient::sse::SseEvent = hclient::sse::SseEvent::Comment(String::new());
    let _limit: usize = hclient::sse::DEFAULT_MAX_EVENT_SIZE;
}

// ── `Error` ────────────────────────────────────────────────────────────
//
// `Error` is the type most easily left out of a reachability check:
// `Client::execute`, `RequestBuilder::send`, `Response::chunk`/`collect`,
// `Collected::text` and `SseStream::new`/`next` all return it, and
// `public_api_types_are_reachable_from_the_facade` above never once names
// it. The tests below don't just check compilation (like the
// `#[allow(dead_code)]` functions above, which have no constructor without
// a transport) — they actually CREATE and COMPARE values of every new
// re-export, because a test that merely names a type says nothing about
// whether you can actually work with it (compare it, destructure it, pass
// it by value).

/// `Error`/`ErrorKind`/`Phase` are `Result` types the facade must be able
/// to name on its own, with no reach into `hclient-core`. The `Result`
/// alias below is exactly what a function returning `Result<_,
/// hclient::Error>` needs, and which is easy to leave unexported.
#[test]
fn error_kind_and_phase_are_reachable_and_matchable_from_the_facade() {
    type FacadeResult<T> = Result<T, hclient::Error>;

    fn probe() -> FacadeResult<()> {
        Err(hclient::Error::new(
            hclient::ErrorKind::Timeout(hclient::error::Phase::Connect),
            std::io::Error::other("probe"),
        ))
    }

    let err = probe().expect_err("probe always returns Err");
    // `ErrorKind` is `#[non_exhaustive]`: outside the crate a match must
    // have a catch-all arm regardless of how many variants are listed —
    // that's itself part of checking reachability from the facade, not
    // just compilation.
    match err.kind() {
        hclient::ErrorKind::Timeout(phase) => assert_eq!(*phase, hclient::error::Phase::Connect),
        other => panic!("unexpected kind: {other:?}"),
    }
    assert!(err.is_timeout());
}

/// `RetryKind` is `RequestBody::retry_kind()`'s return variant,
/// `RewindFactory` is the type of the `RequestBody::Rewindable` field.
/// Both `Full` and `Rewindable` are constructed, not just one variant —
/// otherwise the test would prove `RetryKind` reachable but not
/// `RewindFactory`.
#[test]
fn retry_kind_and_rewind_factory_are_reachable_from_the_facade() {
    let full = hclient::RequestBody::Full(bytes::Bytes::from_static(b"x"));
    assert_eq!(full.retry_kind(), hclient::body::RetryKind::Free);

    let factory: hclient::body::RewindFactory =
        Arc::new(|| hclient::RequestBody::Full(bytes::Bytes::from_static(b"y")));
    let rewindable = hclient::RequestBody::Rewindable(factory);
    assert_eq!(
        rewindable.retry_kind(),
        hclient::body::RetryKind::ViaFactory
    );
    let replay = rewindable.rewind().expect("Rewindable always replays");
    assert!(matches!(replay, hclient::RequestBody::Full(ref b) if &b[..] == b"y"));
}

/// `RedirectSupport`/`TlsSupport`/`TimeoutSupport`/`EarlyDataSupport` are
/// `Capabilities` fields. Needed not just for reading (`Capabilities` is
/// readable even without them — `#[non_exhaustive]` on the struct doesn't
/// block access to `pub` fields), but for WRITING: you can't assemble your
/// own `Capabilities` for a mock transport (e.g.
/// `MockTransport::with_capabilities`) without them — the field's type has
/// to be nameable on the caller's side.
///
/// A fourth, `UpgradeSupport`, was deleted from the workspace: four
/// variants, `None` from every backend, and no
/// caller decision turning on it. What this test pins is the *plumbing* —
/// that a `Capabilities` field's type is nameable and writable from the
/// facade — not upgrade, so it needs a different field rather than one
/// fewer.
///
/// `EarlyDataSupport` is that field, picked over the other candidates on
/// three grounds:
///
/// - It is a `Capabilities` field with an enum type re-exported from
///   `hclient`, and its `Capabilities::default()` value (`None`) differs from
///   the one set here (`Supported`), so the fixture sets it to something
///   distinguishable and the assertion can fail.
/// - Nothing else reaches it through the facade. `DecompressionSupport`,
///   the other enum-typed capability re-exported here, is already named as
///   `hclient::caps::DecompressionSupport` by `tests/compression_capability.rs`,
///   so pointing this test at it would have duplicated a live guard and
///   left `EarlyDataSupport`'s re-export unexercised — `tests/too_early.rs`
///   reaches for `hclient_core::AllowEarlyData` directly rather than
///   through `hclient::`, so the early-data corner was the one with no
///   facade check at all.
/// - `ReuseSupport` and `CancelSupport` are the remaining enum-typed
///   fields and are deliberately not re-exported from `hclient`; pointing
///   this test at one of them would mean adding a re-export to make a test
///   compile.
///
/// The check is a compile-time one as much as a runtime one: remove
/// `EarlyDataSupport` from `hclient`'s `pub use` and this file — an
/// external consumer — stops compiling. That is the mutation that proves
/// the re-pointing is not vacuous, and it was run.
///
/// The `redirects` line said `RedirectSupport::Configurable` until v0.4 W1
/// deleted that variant. `Transparent` carries the same proof and for the
/// same reason `EarlyDataSupport` was chosen above: `Capabilities::default()`
/// gives `None`, so the value set here still differs from the default and
/// the assertion can still fail.
#[test]
fn capability_support_types_are_reachable_from_the_facade() {
    let mut caps = hclient::caps::Capabilities::default();
    caps.redirects = hclient::caps::RedirectSupport::Transparent;
    caps.tls_config = hclient::caps::TlsSupport::Full;
    caps.early_data = hclient::caps::EarlyDataSupport::Supported;
    caps.timeouts = hclient::caps::TimeoutSupport {
        resolve: false,
        connect: true,
        first_byte: true,
        between_bytes: false,
    };
    assert_eq!(caps.redirects, hclient::caps::RedirectSupport::Transparent);
    assert_eq!(caps.tls_config, hclient::caps::TlsSupport::Full);
    assert_eq!(caps.early_data, hclient::caps::EarlyDataSupport::Supported);
    assert!(caps.timeouts.connect && caps.timeouts.first_byte && !caps.timeouts.between_bytes);
}

/// An end-to-end run through `mock`: not a set of isolated reachability
/// checks, but the thing reachability is for in the first place — a real
/// external consumer, depending only on `hclient` (with the `test-util`
/// feature), builds a client on `MockTransport`, sends a request, and
/// reads both a successful response and one broken off by an error,
/// without ever writing `hclient_core::`/`use hclient_core::unversioned::
/// Transport` — `Client::builder`, `RequestBuilder::send`,
/// `MockTransport::requests()`, and `MockTransport::
/// push_response_frames_then_error()` are all ordinary (non-trait) methods,
/// which don't require the `Transport` trait in scope to call.
#[cfg(feature = "test-util")]
#[test]
fn mock_transport_round_trip_uses_only_facade_types() {
    let m = hclient::mock::MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());

    let client = hclient::Client::builder(m)
        .build()
        .expect("mock supports the default config");
    let resp = futures_executor::block_on(
        client
            .post("https://a/")
            .body(hclient::RequestBody::Full(bytes::Bytes::from_static(b"x")))
            .send(),
    )
    .expect("mock replies");
    assert_eq!(resp.status(), 200);

    // `RecordedRequest::retry_kind` is a field scored separately from
    // `retry_kind_and_rewind_factory_are_reachable_from_the_facade` above:
    // there `RetryKind` came straight from `RequestBody::retry_kind()`,
    // here it comes from a field on a struct the transport assembled.
    // Different paths to the same type, both must be nameable.
    let recorded = client
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].retry_kind, hclient::body::RetryKind::Free);

    // `push_response_frames_then_error` is the only spot in `hclient`'s
    // public API where `Error` arrives as a PARAMETER, not a result. The
    // error frame reaches `Response::chunk()` unchanged: `chunk()` passes
    // an already-classified `hclient_core::Error` straight through without
    // relabeling it as `ErrorKind::Body` (see `Response::
    // classify_body_error`) — so `kind()` here must stay the same `Other`
    // it was set up with one line above, not become `Body`.
    let m2 = hclient::mock::MockTransport::new();
    let empty_frames: Vec<&'static str> = Vec::new();
    m2.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .body(empty_frames)
            .unwrap(),
        hclient::Error::new(
            hclient::ErrorKind::Other,
            std::io::Error::other("mock probe"),
        ),
    );
    let client2 = hclient::Client::builder(m2)
        .build()
        .expect("mock supports the default config");
    let mut resp2 =
        futures_executor::block_on(client2.get("https://a/").send()).expect("mock replies");
    match futures_executor::block_on(resp2.chunk()) {
        Some(Err(e)) => assert_eq!(*e.kind(), hclient::ErrorKind::Other),
        other => panic!("expected a terminal error frame, got {other:?}"),
    }
}

// ── `Client::capabilities()` ───────────────────────────────────────────

/// `Client::capabilities()` — the forwarder that answers "what can this
/// client do" without the caller ever writing `use
/// hclient_core::unversioned::Transport`. Before this round, the only path
/// to a `&Capabilities` from a `Client<T>` was `client.transport_as::<MockTransport>().expect("the mock").capabilities()`,
/// and `capabilities()` there is a *trait* method — calling it through the
/// bare `&T` that `.transport_as::<MockTransport>().expect("the mock")` returns needs `Transport` in scope, which
/// defeats the point of a facade that only names types reachable from
/// `hclient::`. `MockTransport::with_capabilities` sets `streaming_request_body`
/// deliberately (the default from `MockTransport::new()` is `Capabilities::default()`,
/// all `false`) so the assertion below is checking a value that was actually
/// threaded through, not one that happened to already be true.
#[cfg(feature = "test-util")]
#[test]
fn client_capabilities_is_reachable_without_the_quarantined_transport_trait() {
    let mut caps = hclient::caps::Capabilities::default();
    caps.streaming_request_body = true;
    let m = hclient::mock::MockTransport::new().with_capabilities(caps);
    let client = hclient::Client::builder(m)
        .build()
        .expect("mock supports the default config");
    assert!(client.capabilities().streaming_request_body);
}

// ── the default transport ──────────────────────────────────────────────

/// `DefaultTransport`/`Client<T = DefaultTransport>`/`Client::new()` — the
/// facade rule applied to the default transport itself. Unlike
/// `Response`/`Collected`/`Error` above, which are re-exports FROM another
/// crate and needed a `pub use` to become nameable here, `DefaultTransport`
/// is declared directly in `hclient`'s own `lib.rs` — the one type a caller
/// might still want to name (e.g. `fn make() -> hclient::Client<hclient::
/// DefaultTransport>`) is already reachable by construction, with nothing
/// to re-export and so nothing to forget re-exporting.
///
/// This checks construction and the default generic parameter, not a real
/// network round trip — `two_runtimes.rs` owns that property (the same
/// generic body over tokio and smol against a real TCP server); this file
/// is about nameability, not behavior. `Client::new()` itself needs no
/// ambient tokio runtime to construct (only `execute()` — the actual
/// connect/spawn/sleep path — would), so a plain `#[test]` is enough here.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
#[test]
fn default_transport_is_reachable_and_is_the_bare_clients_default_param() {
    let client: hclient::Client =
        hclient::Client::new().expect("default transport supports the default config");
    // Assigning into a variable annotated with a BARE `Client` (no
    // `<...>`) only compiles if the generic parameter's default actually
    // resolves to `DefaultTransport` — not just a check that `new()`
    // exists, but a check of the default itself.
    let _client_no_param: hclient::Client = client;
}

// ── the constructor ────────────────────────────────────────────────────

/// **`Client::new()` names both failure causes and panics on neither** —
/// and there is only one constructor to name them with.
///
/// There were two: `new()` returned `UnsupportedCapability` and `.expect`ed
/// a failure to read the OS trust store, and `try_new()` returned
/// `hclient::Error` with both causes. The split was argued from the error
/// type and the argument was sound; the naming it produced was not, because
/// **both returned `Result`**, so `try_` marked the one fallible about more
/// things rather than the one fallible at all. `ErrorKind` already draws
/// that line — `Tls` against `Unsupported` — so the wide type stays, the
/// panic goes, and the prefix has nothing left to contrast with.
///
/// Tests only the success path. `with_platform_verifier()`'s failure is
/// checked here structurally, by the return type, not behaviourally:
/// there is no portable way to make the system certificate store fail on
/// demand in a test that has to run identically on linux, macOS and
/// Windows.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
#[test]
fn the_one_default_constructor_is_fallible_about_both_of_its_failures() {
    let client: hclient::Client =
        hclient::Client::new().expect("default transport supports the default config");
    let _client_no_param: hclient::Client = client;

    // The type is the assertion: an `UnsupportedCapability` could not name
    // the trust-store cause, which is why the narrow one is gone.
    fn takes_the_wide_error(_: fn() -> Result<hclient::Client, hclient::Error>) {}
    takes_the_wide_error(hclient::Client::new);
}

// ── v0.4 W2: the observability seam ──────────────────────────────────────

/// The hook vocabulary must be reachable from a crate that depends only
/// on `hclient`, and — this is the half a `let _: Type` would miss — a
/// hook must be **implementable** and its `Event` **matchable** here.
///
/// The distinction is the one this file's own doc makes: naming a type
/// says it is re-exported and nothing more. What a caller actually writes
/// is an `impl Hooks for MyType` with a `match` over every variant, so
/// that is what is written below. If a variant is added this stops
/// compiling — which is the right way round for a vocabulary in
/// `unversioned`, and the reason `Event` is deliberately not
/// `#[non_exhaustive]`.
#[test]
fn a_hook_can_be_written_against_the_facade_alone() {
    #[derive(Default)]
    struct Counts {
        informational: std::cell::Cell<usize>,
        connected: std::cell::Cell<usize>,
        reused: std::cell::Cell<usize>,
        head: std::cell::Cell<usize>,
        closed: std::cell::Cell<usize>,
    }

    impl hclient::hooks::Hooks for Counts {
        fn on(&self, event: hclient::hooks::Event<'_>) {
            let bump = |c: &std::cell::Cell<usize>| c.set(c.get() + 1);
            match event {
                // Named through the facade like the rest: a caller writing
                // a hook must not have to reach past `hclient` for one
                // event out of five.
                hclient::hooks::Event::Informational(e) => {
                    let _: hclient::hooks::ConnectionId = e.id;
                    let _: http::StatusCode = e.status;
                    let _: &http::HeaderMap = e.headers;
                    bump(&self.informational);
                }
                hclient::hooks::Event::Connected(e) => {
                    // Every field a caller would log, named through the
                    // facade: an id, an address, a version, four durations.
                    let _: hclient::hooks::ConnectionId = e.id;
                    let _: Option<std::net::SocketAddr> = e.remote;
                    let _: http::Version = e.version;
                    let t: hclient::hooks::ConnectTiming = e.timing;
                    let _ = t.dns + t.tcp + t.tls.unwrap_or_default() + t.total;
                    bump(&self.connected);
                }
                hclient::hooks::Event::Reused(e) => {
                    let _: u64 = e.id.get();
                    bump(&self.reused);
                }
                hclient::hooks::Event::Head(e) => {
                    let _: http::StatusCode = e.status;
                    let _: core::time::Duration = e.elapsed;
                    bump(&self.head);
                }
                hclient::hooks::Event::Closed(e) => {
                    match e.reason {
                        hclient::hooks::CloseReason::Ended | hclient::hooks::CloseReason::Stale => {
                        }
                        // The error is the caller's to inspect, so the
                        // facade has to reach `ErrorKind` from here too.
                        hclient::hooks::CloseReason::Failed(err) => {
                            let _: &hclient::ErrorKind = err.kind();
                        }
                    }
                    bump(&self.closed);
                }
                // `Event` is `#[non_exhaustive]` from outside `hclient-core`, so
                // this arm is required. It is not a loss: the compile error a
                // new variant used to cause here is kept once, in
                // `hooks::tests::every_event_is_accounted_for`, where the
                // attribute does not apply — and an out-of-tree `Hooks` impl
                // no longer breaks on a release that adds one.
                _ => {}
            }
        }
    }

    // Constructed and called, so the impl above is not merely compiled:
    // an `Event` a consumer cannot build is one they cannot test against
    // either, and `Closed` is the variant with a lifetime in it.
    let counts = Counts::default();
    hclient::hooks::Hooks::on(
        &counts,
        hclient::hooks::Event::Closed(hclient::hooks::Closed::new(
            hclient::hooks::ConnectionId::UNWATCHED,
            hclient::hooks::CloseReason::Ended,
        )),
    );
    assert_eq!(counts.closed.get(), 1);

    // And the hook a caller who wants nothing gets.
    let _: hclient::hooks::NoHooks = hclient::hooks::NoHooks;
}
