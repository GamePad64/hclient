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

/// **Nothing a request produces is `Send` after erasure — not the future,
/// not the response body — and the reason is the browser.**
///
/// `Client` names no transport type, so one `BoxBody` has to serve every
/// backend, and `hclient-fetch`'s body holds a `dyn Stream` with no auto
/// trait: putting `Send` on `BoxBody` does not weaken the browser backend,
/// it **excludes** it, `Client::builder(Fetch::new())` and all. Measured,
/// not reasoned — that is the state this file was written in for one
/// commit, and `cargo test -p hclient-fetch --target
/// wasm32-unknown-unknown --no-run` refused it.
///
/// So the trade is forced and it is worth naming both halves:
///
/// * **Lost.** `tokio::spawn` of a response body, which worked on
///   `hclient-native` and is an ordinary thing to want. `deadline.rs`
///   pins the negative.
/// * **Kept.** Every backend this workspace ships can be a `Client`.
///
/// The request future is a separate matter and costs less: putting `Send`
/// on `BoxExchange` would need the blanket `impl<T> BoxedTransport for T`
/// to prove `T::execute`'s RPITIT `Send` for an abstract `T`, which is
/// return type notation — `E0658` on stable. And `hclient-native`'s
/// `execute` future is *already* `!Send`, pinned by a paired doctest on
/// `Native`, so the only backend this takes it from is the mock.
///
/// **What stays `Send + Sync` is the `Client` itself**, which is the half
/// that has to: a client lives in shared application state.
#[test]
fn the_client_is_send_and_sync_and_what_a_request_produces_is_not() {
    fn is_send_sync<T: Send + Sync>() {}
    is_send_sync::<hclient::Client>();
    // The control for the line above: the same bound, asked of the type
    // this test says does *not* have it, is the `compile_fail` below.
    is_send_sync::<hclient::mock::MockTransport>();
}

/// The paired negatives, each with a control differing by one line.
///
/// Spawning a request does not compile:
///
/// ```compile_fail
/// # fn main() {
/// let c = hclient::Client::builder(hclient::mock::MockTransport::new())
///     .build()
///     .unwrap();
/// fn assert_send<T: Send>(_: T) {}
/// assert_send(c.execute(http::Request::new(hclient_core::RequestBody::Empty)));
/// # }
/// ```
///
/// Neither does asserting the response body is `Send`:
///
/// ```compile_fail
/// fn body_is_send<B: Send>() {}
/// body_is_send::<hclient::body::ClientBody>();
/// ```
///
/// And the control, identical but for the type asserted about, so that
/// both blocks above are known to fail for their own reason:
///
/// ```
/// fn body_is_send<B: Send>() {}
/// body_is_send::<hclient::mock::MockTransport>();
/// ```
#[allow(dead_code)]
struct NothingARequestProducesIsSend;

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

    let c = hclient::Client::builder(NotClone(hclient::mock::MockTransport::new()))
        .build()
        .expect("mock supports the default config");
    let _twin = c.clone();
}
