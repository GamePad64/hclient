//! What a caller can do with a `Client`, in the shape the question is
//! usually asked in: *can this be used the way `reqwest::Client` is?*
//!
//! # The answer this file pins
//!
//! `Client` is `Send + Sync + Clone + 'static`, so it lives in application
//! state, goes in an `Arc`, is cloned per task and shared across threads —
//! every structural thing `reqwest::Client` is used for. What a request
//! *produces* crosses a thread too: `tests/spawnable_body.rs` collects a
//! response body on another one.
//!
//! **What does not compile is `tokio::spawn(client.get(u).send())`**, and
//! that is deliberate rather than pending. `Client` is one erased type,
//! and its `BoxExchange` declares no auto trait; bounding it obliges the
//! blanket `BoxedTransport` impl to *prove* `Transport::execute`'s RPITIT
//! future `Send` for a generic parameter, which cannot be named on stable.
//! That whole change was built on nightly under return type notation and
//! reverted — CLAUDE.md carries the measurement, including what it costs
//! every generic consumer.
//!
//! Two things work today in its place, and both are checked here rather
//! than asserted: requests concurrently on a `LocalSet`, and — where a
//! `tokio::spawn` is really wanted — the transport held directly, whose
//! future *is* `Send` (`hclient-native`'s `tests/send_future.rs`).
#![cfg(all(not(target_family = "wasm"), feature = "default-transport"))]

use std::io::{Read, Write};

fn serve(n: usize) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming().take(n) {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    format!("http://{addr}/")
}

/// The structural half, which is the half that matters for holding one.
#[test]
fn a_client_is_send_sync_clone_and_static() {
    fn is<T: Send + Sync + Clone + 'static>() {}
    is::<hclient::Client>();

    // And spent that way: one client, another thread, through an `Arc`.
    let c = std::sync::Arc::new(hclient::Client::new().unwrap());
    let c2 = std::sync::Arc::clone(&c);
    std::thread::spawn(move || {
        let _ = c2.get("http://192.0.2.1/");
    })
    .join()
    .unwrap();
}

/// Concurrency without a `Send` future: three requests in flight at once,
/// on one thread. This is what a `!Send` request future does and does not
/// cost — it bars `tokio::spawn`, not concurrency.
#[test]
fn requests_run_concurrently_on_a_localset() {
    let u = serve(3);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let c = hclient::Client::new().unwrap();
        let mut hs = Vec::new();
        for _ in 0..3 {
            let c = c.clone();
            let u = u.clone();
            hs.push(tokio::task::spawn_local(
                async move { c.get(&u).send().await },
            ));
        }
        for h in hs {
            assert_eq!(h.await.unwrap().unwrap().status(), 200);
        }
    });
}
