//! Composition of client and request timeouts — the exact thing the doc
//! comments on `ClientBuilder::timeouts` and `RequestBuilder::timeouts`
//! promise, and which didn't exist in either direction before the whole
//! branch's final review (B1/M3): client-level ones were checked at
//! `build()` and **never reached** the transport, request-level ones
//! reached it but were **never checked** against `Capabilities`.
//!
//! All three properties are checked through `MockTransport`, not a unit
//! test on `effective_timeouts`: that function already has three unit
//! tests of its own in `config.rs`, and they stayed green the whole time
//! nothing called it. Red here can only come from an actually-exercised
//! `Client::execute` → `Transport::execute` path.

// `hclient::mock` lives behind the `test-util` feature (see `mock.rs`).
#![cfg(feature = "test-util")]

use hclient::caps::Capabilities;
use hclient::caps::TimeoutSupport;
use hclient::error::UnsupportedCapability;
use hclient::mock::MockTransport;
use hclient::{Client, ErrorKind, Timeouts};
use std::time::Duration;

fn secs(n: u64) -> Option<Duration> {
    Some(Duration::from_secs(n))
}

/// A transport that supports all three phases — otherwise `check_supported`
/// would reject the configuration before the test reaches the property
/// under test.
fn all_timeouts_supported() -> Capabilities {
    let mut caps = Capabilities::none();
    caps.timeouts = TimeoutSupport {
        resolve: false,
        connect: true,
        first_byte: true,
        between_bytes: true,
    };
    caps
}

/// B1. `ClientBuilder::timeouts()` was a silent no-op: `effective_timeouts`
/// existed, was public, and was covered by three unit tests — and was
/// called from nowhere in production code. The only channel to the
/// transport is `http::Extensions`, and the client's configuration never
/// made it in there.
#[test]
fn client_level_timeouts_reach_the_transport() {
    let m = MockTransport::new().with_capabilities(all_timeouts_supported());
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m)
        .timeouts(Timeouts {
            resolve: None,
            connect: secs(7),
            ..Default::default()
        })
        .build()
        .unwrap();
    futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    let seen = c.transport().requests();
    let t = seen[0]
        .extensions
        .get::<Timeouts>()
        .expect("client-level timeouts must reach the transport");
    assert_eq!(t.connect, secs(7));
}

/// B1, second half: "request-first, client-fallback" **field by field**,
/// not "all or nothing". The request sets only `first_byte`; the other two
/// phases must come from the client. A naive implementation ("extensions
/// already has a `Timeouts` — so don't look at the client") would leave
/// `None` here.
#[test]
fn request_timeouts_override_the_client_field_by_field() {
    let m = MockTransport::new().with_capabilities(all_timeouts_supported());
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m)
        .timeouts(Timeouts {
            resolve: None,
            connect: secs(1),
            first_byte: secs(2),
            between_bytes: secs(3),
        })
        .build()
        .unwrap();
    futures_executor::block_on(
        c.get("https://a/x")
            .timeouts(Timeouts {
                resolve: None,
                first_byte: secs(9),
                ..Default::default()
            })
            .send(),
    )
    .unwrap();

    let seen = c.transport().requests();
    let t = seen[0]
        .extensions
        .get::<Timeouts>()
        .expect("merged timeouts");
    assert_eq!(
        t.connect,
        secs(1),
        "not overridden by request — take the client's"
    );
    assert_eq!(t.first_byte, secs(9), "request overrides");
    assert_eq!(
        t.between_bytes,
        secs(3),
        "not overridden by request — take the client's"
    );
}

/// M3. `check_supported` runs once, at `build()`, over `config.timeouts`.
/// `RequestBuilder::timeouts()` used to write straight into `Extensions`,
/// bypassing `Capabilities` entirely — meaning an unsupported timeout at
/// the request level was accepted silently, in exactly the place where the
/// same timeout at the client level produced a typed error.
#[test]
fn unsupported_per_request_timeout_is_a_typed_error_not_a_silent_noop() {
    // `MockTransport::new()` — `Capabilities::none()`, all three phases `false`.
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let err = futures_executor::block_on(
        c.get("https://a/x")
            .timeouts(Timeouts {
                resolve: None,
                connect: secs(3),
                ..Default::default()
            })
            .send(),
    )
    .expect_err(
        "the transport can't do a connect timeout — that's an error, not a silently dropped value",
    );

    assert_eq!(*err.kind(), ErrorKind::Unsupported, "{err}");
    let src = std::error::Error::source(&err).expect("Error::new always sets a source");
    let unsupported = src
        .downcast_ref::<UnsupportedCapability>()
        .expect("the source must name the specific unsupported setting, not just be a string");
    assert_eq!(unsupported.what, "connect_timeout");

    // And the request must not have gone out: a rejected setting doesn't
    // turn into "sent as-is, just without the timeout".
    assert!(
        c.transport().requests().is_empty(),
        "a request with an unsupported setting must not reach the transport"
    );
}

/// The flip side of the unconditional insert: `Client::execute` puts the
/// merged `Timeouts` into `extensions` ALWAYS, including when not a single
/// timeout was set. Two consequences, and both must hold, or the
/// unconditional insert would be a regression:
///
/// 1. A transport with `Capabilities::none()` (all three phases
///    unsupported) doesn't reject this — the gate looks at the values, not
///    at presence.
/// 2. The extension is still stored anyway, with all its fields `None`.
///
/// The second point is why `Transport::execute`'s doc comment warns
/// backends not to read presence as intent: `extensions.get::<Timeouts>()
/// .is_some()` is true for EVERY request that comes through `Client`.
#[test]
fn an_all_none_timeouts_is_inserted_unconditionally_and_trips_no_capability_gate() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    futures_executor::block_on(c.get("https://a/x").send())
        .expect("no timeout was set — nothing to reject");

    let seen = c.transport().requests();
    let t = seen[0]
        .extensions
        .get::<Timeouts>()
        .expect("the merged Timeouts is inserted unconditionally");
    assert_eq!(
        *t,
        Timeouts::default(),
        "and it's empty: the extension's presence doesn't mean timeouts were requested"
    );
}

/// **`resolve_timeout` is refused by name**, which is the rule the field
/// landed under: the declaration and the enforcement in one change, and a
/// capability saying `false` meaning it.
///
/// It is checked separately from `connect_timeout` above rather than by
/// widening that test, because what is being asserted is that the refusal
/// names *this* setting — a caller who wrote `resolve` must not be told
/// `connect` was the problem.
#[test]
fn an_unsupported_resolve_timeout_is_refused_under_its_own_name() {
    let mut caps = hclient::caps::Capabilities::none();
    caps.timeouts = hclient::caps::TimeoutSupport {
        resolve: false,
        // The three a backend may well support while resolution is the
        // host's — which is `hclient-wasi`'s exact answer.
        connect: true,
        first_byte: true,
        between_bytes: true,
    };
    let err = Client::builder(MockTransport::new().with_capabilities(caps.clone()))
        .timeouts(Timeouts {
            resolve: secs(1),
            ..Default::default()
        })
        .build()
        .expect_err("the backend says it cannot bound resolution");
    assert_eq!(err.what, "resolve_timeout", "{err}");

    // The control: the same setting against a backend that can.
    caps.timeouts.resolve = true;
    assert!(
        Client::builder(MockTransport::new().with_capabilities(caps))
            .timeouts(Timeouts {
                resolve: secs(1),
                ..Default::default()
            })
            .build()
            .is_ok()
    );
}
