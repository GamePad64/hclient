//! Assertions about the shape of the public API, kept outside `src`: CI's
//! `no-declared-send` check only scans `crates/*/src`.

#![cfg(feature = "test-util")]

use hclient::mock::MockTransport;

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn mock_transport_is_send_and_sync_so_client_futures_can_be_spawned() {
    // If the mock stopped being Sync, `&MockTransport` would stop being
    // Send, and `Client::execute`'s future would end up !Send — meaning
    // the test double itself would take away our ability to check the
    // design's central property.
    assert_send_sync::<hclient::mock::MockTransport>();
}

/// **Everything a request produces is `Send`, and this file asserted the
/// opposite with three checks that never ran.**
///
/// The claim was true when it was written. `Client` had just given up its
/// type parameter, one `BoxBody` had to serve every backend, and
/// `hclient-fetch`'s body held a `dyn Stream` with no auto trait — so
/// declaring `Send` there would have **excluded** the browser rather than
/// weakened it. Amendments C14 and C16 removed the cause: `hclient-fetch`
/// hands `Bytes` across a channel instead of a JS handle, `BoxBody` and
/// `BoxExchange` carry `Send`, and `SendTransport` is where a backend
/// proves it at a concrete type. `AGENTS.md` records the measurement.
///
/// **The instrument is the finding.** The three assertions were
/// ```` ```compile_fail ```` fences in this file's doc comments, and
/// **rustdoc collects doctests from library targets only** — measured:
/// `cargo test --doc -p hclient --all-features` runs 13 doctests and every
/// one is from `src/`, none from `tests/`. So a `compile_fail` in an
/// integration test is not a lenient check or a slow one. It is not a
/// check: nothing ever compiled it, in either direction, and it went on
/// asserting a fact that had stopped being true for months.
///
/// That is why the assertions below are ordinary `#[test]` functions.
/// A type-level claim needs no runtime, but it does need to be **compiled**,
/// and in a `tests/` file the only thing that compiles is code.
///
/// The rule this earns: **a doc fence in `tests/` is dead text.** Put the
/// fence in `src/`, where `just test-doc` will run it, or write a test.
#[test]
fn what_a_request_produces_is_send() {
    fn assert_send<T: Send>(_: T) {}
    fn assert_send_ty<T: Send>() {}

    let c = hclient::Client::builder(MockTransport::new())
        .build()
        .expect("mock supports the default config");

    // The request future. This is the one `AGENTS.md` records as lost to
    // erasure and regained by C16, so it is the one worth naming.
    assert_send(c.execute(http::Request::new(hclient_core::RequestBody::Empty)));

    // The response body, through the alias a caller actually meets.
    assert_send_ty::<hclient::body::ClientBody>();

    // And what a caller holds between them.
    assert_send_ty::<hclient::Response>();
}

/// **The control for the test above is in `src/`, and it has to be.**
///
/// `assert_send<T: Send>` is a live bound — a type that stopped being
/// `Send` fails to compile there — but the *helper* could be weakened to
/// `assert_send<T>` and the test would pass vacuously. Catching that needs
/// a compile that must **fail**, and this file has just established that
/// `tests/` cannot hold one: rustdoc never reads it.
///
/// So the negative lives on `body::ClientBody`'s own doc comment, as a
/// ```` ```compile_fail ```` fence that `just test-doc` runs, and the two
/// halves are cross-referenced rather than co-located. That is the cost of
/// the rule above, paid rather than hidden.

/// Cloning shares the transport rather than copying it, which is what makes
/// a `Client` safe to hand to several tasks. The check is not "does it
/// clone" — `#[derive(Clone)]` would have satisfied that while duplicating
/// the transport, and a duplicated transport means a second connection
/// pool, a second TLS configuration and a queue of mock responses that the
/// other half cannot see.
#[test]
fn cloning_shares_one_transport_rather_than_copying_it() {
    let c = hclient::Client::builder(hclient::mock::MockTransport::new())
        .build()
        .expect("mock supports the default config");
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response(http::Response::builder().status(200).body("one").unwrap());
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response(http::Response::builder().status(201).body("two").unwrap());

    let twin = c.clone();
    let a = futures_executor::block_on(c.get("https://a/").send()).unwrap();
    let b = futures_executor::block_on(twin.get("https://a/").send()).unwrap();

    assert_eq!((a.status().as_u16(), b.status().as_u16()), (200, 201));
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
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
    struct NotClone(hclient::mock::MockTransport);
    impl hclient_core::unversioned::Transport for NotClone {
        type Body = <hclient::mock::MockTransport as hclient_core::unversioned::Transport>::Body;
        type Error = <hclient::mock::MockTransport as hclient_core::unversioned::Transport>::Error;
        fn execute(
            &self,
            req: http::Request<hclient_core::RequestBody>,
        ) -> impl std::future::Future<Output = Result<http::Response<Self::Body>, Self::Error>>
        {
            self.0.execute(req)
        }
        fn capabilities(&self) -> &hclient_core::Capabilities {
            self.0.capabilities()
        }
    }
    // One line, and it is the whole of what a backend now owes the erased
    // `Client`: hand the same exchange over in a form whose `Send` has a
    // name. At a concrete type — which is what a backend always is — that
    // is inference, not proof.
    impl hclient_core::unversioned::SendTransport for NotClone {
        fn execute_send(
            &self,
            req: http::Request<hclient_core::RequestBody>,
        ) -> hclient_core::unversioned::BoxSendExchange<'_, Self::Body, Self::Error> {
            Box::pin(<Self as hclient_core::unversioned::Transport>::execute(
                self, req,
            ))
        }
    }

    let c = hclient::Client::builder(NotClone(hclient::mock::MockTransport::new()))
        .build()
        .expect("mock supports the default config");
    let _twin = c.clone();
}
