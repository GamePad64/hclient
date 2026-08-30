//! The per-request settings a caller can only make through the builder.
//!
//! **Three of these values shipped with no route from `RequestBuilder`**,
//! and the shape is one this workspace has already recorded once:
//! `require_version` was unreachable from the builder for two verticals
//! while `tests/require_version.rs` exercised the gate by building its
//! requests with `extensions_mut().insert(..)` — testing around the
//! builder is what let the builder have no route to the gate.
//!
//! So these tests go through `RequestBuilder` **only**, and read the
//! result at the transport boundary. A test here that reached for
//! `extensions_mut` would be reintroducing the defect it exists to catch.

#![cfg(feature = "test-util")]

use hclient::mock::MockTransport;

fn client() -> hclient::Client {
    hclient::Client::builder(MockTransport::new())
        .build()
        .expect("mock supports the default config")
}

fn ok() -> http::Response<&'static str> {
    http::Response::builder().status(200).body("ok").unwrap()
}

/// `client_identity` is the one that had no caller at all: it reached
/// `Transport` through the extensions and nothing above could put it
/// there.
#[test]
fn a_client_identity_set_on_the_builder_reaches_the_transport() {
    let c = client();
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response(ok());

    let _ = futures_executor::block_on(c.get("https://a/").client_identity("corp").send())
        .expect("response");

    let m = c.transport_as::<MockTransport>().expect("the mock");
    let sent = m.requests();
    let id = sent[0]
        .extensions
        .get::<hclient_core::ClientIdentity>()
        .expect("the label reaches the transport");
    assert_eq!(id.name(), "corp");
}

/// `allow_early_data` was reachable only by hand, and `tests/too_early.rs`
/// is where the hand-built requests are.
#[test]
fn allow_early_data_set_on_the_builder_reaches_the_transport() {
    let c = client();
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response(ok());

    let _ = futures_executor::block_on(c.get("https://a/").allow_early_data().send())
        .expect("response");

    let m = c.transport_as::<MockTransport>().expect("the mock");
    assert!(
        m.requests()[0]
            .extensions
            .get::<hclient_core::AllowEarlyData>()
            .is_some(),
        "the mark reaches the transport"
    );
}

/// The general channel, for a type this crate has never heard of — which
/// is the case a tracing decorator's context is, and the one the two
/// named setters structurally cannot serve.
#[test]
fn an_arbitrary_extension_reaches_the_transport() {
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SomebodyElses(u32);

    let c = client();
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response(ok());

    let _ = futures_executor::block_on(c.get("https://a/").extension(SomebodyElses(7)).send())
        .expect("response");

    let m = c.transport_as::<MockTransport>().expect("the mock");
    assert_eq!(
        m.requests()[0].extensions.get::<SomebodyElses>(),
        Some(&SomebodyElses(7)),
        "a value this crate cannot name still travels"
    );
}

/// **The control, and it is the half that makes the three above mean
/// something.** Without it they would pass for a builder that inserted
/// every value it had ever been given into every request.
#[test]
fn a_request_that_asked_for_none_of_them_carries_none_of_them() {
    let c = client();
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response(ok());

    let _ = futures_executor::block_on(c.get("https://a/").send()).expect("response");

    let m = c.transport_as::<MockTransport>().expect("the mock");
    let e = &m.requests()[0].extensions;
    assert!(e.get::<hclient_core::ClientIdentity>().is_none());
    assert!(e.get::<hclient_core::AllowEarlyData>().is_none());
}

/// A setting survives the redirect it was set before, because
/// `next_hop` clones the extensions — the same arrangement `Timeouts`
/// relies on. `AllowEarlyData` is the one value that does **not**, and
/// only across an origin; `tests/too_early.rs` pins that half.
#[test]
fn a_setting_travels_to_the_next_hop_of_the_same_origin() {
    let c = hclient::Client::builder(MockTransport::new())
        .redirect(hclient::redirect::Limit::new(10))
        .build()
        .expect("mock supports the default config");
    let m = c.transport_as::<MockTransport>().expect("the mock");
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a/two")
            .body("")
            .unwrap(),
    );
    m.push_response(ok());

    let _ = futures_executor::block_on(c.get("https://a/one").client_identity("corp").send())
        .expect("response");

    let m = c.transport_as::<MockTransport>().expect("the mock");
    let sent = m.requests();
    assert_eq!(sent.len(), 2, "two hops");
    assert_eq!(
        sent[1]
            .extensions
            .get::<hclient_core::ClientIdentity>()
            .map(|i| i.name().to_owned()),
        Some("corp".to_owned()),
        "the identity is a property of the operation, not of one hop"
    );
}
