//! Assertions about the shape of the public API, kept outside `src`: CI's
//! `no-declared-send` check only scans `crates/*/src`.

#![cfg(feature = "test-util")]

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn mock_transport_is_send_and_sync_so_client_futures_can_be_spawned() {
    // If the mock stopped being Sync, `&MockTransport` would stop being
    // Send, and `Client::execute`'s future would end up !Send — meaning
    // the test double itself would take away our ability to check the
    // design's central property.
    assert_send_sync::<http_ng::mock::MockTransport>();
}

#[test]
fn client_future_is_send_when_the_transport_is() {
    fn assert_send<T: Send>(_: T) {}
    let c = http_ng::Client::builder(http_ng::mock::MockTransport::new())
        .build()
        .expect("mock supports the default config");
    // This exact property has broken twice over the life of the project —
    // through type erasure in `Error` and in `RequestBody`. It's the
    // reason the core declares not a single Send bound.
    assert_send(c.execute(http::Request::new(http_ng_core::RequestBody::Empty)));
}
