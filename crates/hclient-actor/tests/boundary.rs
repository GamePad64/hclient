//! The claim: a transport that cannot cross a thread backs an
//! `hclient::Client` through this boundary, and the boundary is honest
//! about what it changed.

mod support;

use hclient_actor::{Handle, Limits, actor};
use hclient_core::unversioned::Transport;
use std::rc::Rc;
use support::Local;

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

fn req() -> http::Request<hclient_core::RequestBody> {
    http::Request::builder()
        .uri("https://a/x")
        .body(hclient_core::RequestBody::Empty)
        .expect("a literal request")
}

/// The whole point, and it is a fact about types rather than about a run:
/// the handle crosses a thread where the transport behind it cannot.
#[test]
fn the_handle_is_send_where_the_transport_is_not() {
    assert_send::<Handle>();
    assert_sync::<Handle>();

    // The control, without which the assertion above says nothing about
    // this boundary: the thing on the far side really is `!Send`. A real
    // negative, not `fn assert_not<T>()`, which accepts anything —
    // inherent methods win over trait ones, so `true` means the bound
    // holds.
    struct Probe<T>(std::marker::PhantomData<T>);
    trait Fallback {
        fn is() -> bool {
            false
        }
    }
    impl<T> Fallback for Probe<T> {}
    impl<T: Send> Probe<T> {
        fn is() -> bool {
            true
        }
    }
    assert!(
        !Probe::<Local>::is(),
        "the double must be !Send, or this proves nothing"
    );
    assert!(Probe::<Handle>::is());
}

/// Bytes go in and come back out, through a driver that is a plain future.
#[test]
fn a_request_crosses_and_the_response_comes_back() {
    let inner = Local::new(b"hello from the far side");
    let log = Rc::clone(&inner.log);
    let (handle, driver) = actor(inner, Limits::default());

    // One executor, one task: `LocalPool` is what an embassy `Spawner` is
    // here, and the driver is the single task a caller spawns.
    let mut pool = futures_executor::LocalPool::new();
    futures_util::task::LocalSpawnExt::spawn_local(&pool.spawner(), driver)
        .expect("spawn the driver");

    let out = pool.run_until(async {
        let resp = handle.execute(req()).await?;
        let (parts, body) = resp.into_parts();
        let bytes = http_body_util::BodyExt::collect(body).await?.to_bytes();
        Ok::<_, hclient_core::Error>((parts.status, bytes))
    });

    let (status, bytes) = out.expect("the exchange completes");
    assert_eq!(status, 200);
    assert_eq!(&bytes[..], b"hello from the far side");
    assert_eq!(log.requests.get(), 1);
}

/// A body past the run-time ceiling is a typed error, not a truncation and
/// not an allocation the device cannot afford.
#[test]
fn a_response_past_the_limit_is_refused_by_name() {
    let inner = Local::new(b"0123456789");
    let (handle, driver) = actor(inner, Limits::default().max_response(4));
    let mut pool = futures_executor::LocalPool::new();
    futures_util::task::LocalSpawnExt::spawn_local(&pool.spawner(), driver).expect("spawn");

    let err = pool
        .run_until(handle.execute(req()))
        .expect_err("ten bytes past a four-byte ceiling");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Body);
    let src = std::error::Error::source(&err).expect("a typed source");
    assert!(
        src.downcast_ref::<hclient_actor::ResponseTooLarge>()
            .is_some(),
        "the refusal must name itself: {src}"
    );
}

/// The ceiling is the caller's, not a constant — the same body passes when
/// the limit allows it, which is what makes the test above about the
/// bound rather than about the body.
#[test]
fn the_same_body_passes_under_a_larger_limit() {
    let inner = Local::new(b"0123456789");
    let (handle, driver) = actor(inner, Limits::default().max_response(10));
    let mut pool = futures_executor::LocalPool::new();
    futures_util::task::LocalSpawnExt::spawn_local(&pool.spawner(), driver).expect("spawn");
    assert!(pool.run_until(handle.execute(req())).is_ok());
}

/// **The boundary buffers, so it must not claim otherwise.** An
/// over-claimed `full_duplex` deadlocks a caller who believed it; the
/// inner transport's other capabilities are carried through unchanged.
#[test]
fn the_two_capabilities_the_boundary_changes_are_downgraded() {
    let mut caps = hclient_core::Capabilities::default();
    caps.streaming_request_body = true;
    caps.full_duplex = true;
    caps.response_trailers = true;

    struct Claiming(hclient_core::Capabilities);
    impl Transport for Claiming {
        type Body = http_body_util::Full<bytes::Bytes>;
        type Error = hclient_core::Error;
        async fn execute(
            &self,
            _req: http::Request<hclient_core::RequestBody>,
        ) -> Result<http::Response<Self::Body>, Self::Error> {
            unreachable!("this fixture is only asked for its capabilities")
        }
        fn to_error(&self, e: Self::Error) -> hclient_core::Error {
            e
        }
        fn capabilities(&self) -> &hclient_core::Capabilities {
            &self.0
        }
    }

    let (handle, _driver) = actor(Claiming(caps), Limits::default());
    let seen = handle.capabilities();
    assert!(
        !seen.streaming_request_body,
        "a collected body is not streamed"
    );
    assert!(!seen.full_duplex, "a collected body is not full duplex");
    assert!(
        seen.response_trailers,
        "everything the boundary does not change must survive it"
    );
}

/// A handle whose driver was never spawned fails by name rather than
/// hanging — which is the mistake this API most invites.
#[test]
fn a_handle_whose_driver_never_ran_says_so() {
    let (handle, driver) = actor(Local::new(b""), Limits::default());
    drop(driver);
    let err =
        futures_executor::block_on(handle.execute(req())).expect_err("no driver, no exchange");
    let src = std::error::Error::source(&err).expect("a typed source");
    assert!(
        src.downcast_ref::<hclient_actor::ActorGone>().is_some(),
        "the failure must name the cause: {src}"
    );
}

/// And the driver ends on its own when every handle is gone, so a caller
/// does not have to cancel the task by hand.
#[test]
fn the_driver_finishes_when_the_last_handle_is_dropped() {
    let (handle, driver) = actor(Local::new(b""), Limits::default());
    drop(handle);
    // Resolves rather than parking for ever.
    futures_executor::block_on(driver);
}
