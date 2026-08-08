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

/// Cloning shares the transport rather than copying it, which is what makes
/// a `Client` safe to hand to several tasks. The check is not "does it
/// clone" — `#[derive(Clone)]` would have satisfied that while duplicating
/// the transport, and a duplicated transport means a second connection
/// pool, a second TLS configuration and a queue of mock responses that the
/// other half cannot see.
#[test]
fn cloning_shares_one_transport_rather_than_copying_it() {
    let c = http_ng::Client::builder(http_ng::mock::MockTransport::new())
        .build()
        .expect("mock supports the default config");
    c.transport()
        .push_response(http::Response::builder().status(200).body("one").unwrap());
    c.transport()
        .push_response(http::Response::builder().status(201).body("two").unwrap());

    let twin = c.clone();
    let a = futures_executor::block_on(c.get("https://a/").send()).unwrap();
    let b = futures_executor::block_on(twin.get("https://a/").send()).unwrap();

    assert_eq!((a.status().as_u16(), b.status().as_u16()), (200, 201));
    assert_eq!(
        c.transport().requests().len(),
        2,
        "both halves must land on the same transport — a copy would have seen one request each"
    );
}

/// A `Client` does not require `T: Clone` to be cloneable. The `Arc` is what
/// clones, and a transport that cannot be copied at all must still be
/// shareable.
#[test]
fn clone_does_not_require_the_transport_to_be_clone() {
    #[derive(Debug)]
    struct NotClone(http_ng::mock::MockTransport);
    impl http_ng_core::unversioned::Transport for NotClone {
        type Body = <http_ng::mock::MockTransport as http_ng_core::unversioned::Transport>::Body;
        type Error = <http_ng::mock::MockTransport as http_ng_core::unversioned::Transport>::Error;
        fn execute(
            &self,
            req: http::Request<http_ng_core::RequestBody>,
        ) -> impl std::future::Future<Output = Result<http::Response<Self::Body>, Self::Error>>
        {
            self.0.execute(req)
        }
        fn capabilities(&self) -> &http_ng_core::Capabilities {
            self.0.capabilities()
        }
    }

    let c = http_ng::Client::builder(NotClone(http_ng::mock::MockTransport::new()))
        .build()
        .expect("mock supports the default config");
    let _twin = c.clone();
}
