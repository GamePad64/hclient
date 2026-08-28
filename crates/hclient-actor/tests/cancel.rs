//! Dropping the future `execute` returned must stop the exchange on the
//! far side of the channel.
//!
//! `Transport::execute` promises it, and a spawned driver does not honour
//! it by accident: nothing about a spawn says the work should end when its
//! caller walked away. **The pair is the assertion** — a driver that
//! ignored the reply channel closing passes the first test and fails the
//! second.

mod support;

use hclient_actor::{Limits, actor};
use hclient_core::unversioned::Transport;
use std::rc::Rc;
use support::Local;

fn req() -> http::Request<hclient_core::RequestBody> {
    http::Request::builder()
        .uri("https://a/x")
        .body(hclient_core::RequestBody::Empty)
        .expect("a literal request")
}

#[test]
fn a_completed_exchange_leaves_nothing_dropped_midway() {
    let inner = Local::new(b"done");
    let log = Rc::clone(&inner.log);
    let (handle, driver) = actor(inner, Limits::default());
    let mut pool = futures_executor::LocalPool::new();
    futures_util::task::LocalSpawnExt::spawn_local(&pool.spawner(), driver).expect("spawn");

    pool.run_until(handle.execute(req())).expect("completes");
    assert_eq!(log.requests.get(), 1);
    assert!(
        !log.dropped_midway.get(),
        "the exchange finished, so nothing was cancelled"
    );
}

#[test]
fn dropping_the_caller_stops_an_exchange_that_would_never_finish() {
    // A transport that never answers — the shape of a request to a server
    // that has gone away, which is exactly when a caller gives up.
    let inner = Local::hanging();
    let log = Rc::clone(&inner.log);
    let (handle, driver) = actor(inner, Limits::default());
    let mut pool = futures_executor::LocalPool::new();
    futures_util::task::LocalSpawnExt::spawn_local(&pool.spawner(), driver).expect("spawn");

    // Start it and let the driver reach the inner transport, so this is a
    // cancellation of work in flight rather than of work never begun.
    let mut fut = Box::pin(handle.execute(req()));
    let waker = futures_util::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    assert!(
        std::future::Future::poll(fut.as_mut(), &mut cx).is_pending(),
        "a hanging transport cannot answer"
    );
    pool.run_until_stalled();
    assert_eq!(log.requests.get(), 1, "the driver reached the transport");
    assert!(
        !log.dropped_midway.get(),
        "and is still inside it — nothing has cancelled anything yet"
    );

    drop(fut);
    pool.run_until_stalled();
    assert!(
        log.dropped_midway.get(),
        "dropping the caller's future must drop the exchange on the far side — otherwise \
         a request runs to completion behind a caller who walked away, which \
         `Transport::execute`'s contract forbids"
    );
}
