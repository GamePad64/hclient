//! The thing the boundary exists for: `hclient::Client` over a transport
//! that cannot cross a thread.
//!
//! `Client` boxes its transport `Send + Sync`, so before this the whole
//! facade — the cookie jar, redirects, the response cache, digest auth —
//! was out of reach on an embassy target. This is that, asserted rather
//! than described.

mod support;

use hclient_actor::{Limits, actor};
use support::Local;

#[test]
fn a_client_is_built_over_a_transport_that_cannot_cross_a_thread() {
    let (handle, driver) = actor(Local::new(b"{\"ok\":true}"), Limits::default());

    // The one line that could not be written before: `Client::builder`
    // demands `SendTransport`, which `Local` is not and the handle is.
    let client = hclient::Client::builder(handle)
        .build()
        .expect("caps agree");

    let mut pool = futures_executor::LocalPool::new();
    futures_util::task::LocalSpawnExt::spawn_local(&pool.spawner(), driver).expect("spawn");

    let text = pool
        .run_until(async {
            client
                .get("https://a/x")
                .send()
                .await?
                .collect()
                .await?
                .text()
        })
        .expect("the exchange completes through the facade");
    assert_eq!(text, "{\"ok\":true}");
}

/// And the `Client` it produces is `Send`, which is what the facade
/// promises everywhere else — the boundary must not make it a special
/// case.
#[test]
fn the_client_over_the_boundary_is_send_like_any_other() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<hclient::Client>();
    assert_sync::<hclient::Client>();
}
