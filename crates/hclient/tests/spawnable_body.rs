//! A response body handed back by the erased `Client` crosses a thread.
//!
//! # What this pins, and what it deliberately does not
//!
//! `Client` stopped carrying its transport as a type parameter, and
//! erasure drops auto traits — so `ClientBody` was `!Send` and
//! `tokio::spawn` of a response body, which worked while the transport was
//! generic, stopped working. `erased::{BoxBody, BoxSleep, BoxInstant}`
//! declare `Send` now (amendment C14) and this is what that buys.
//!
//! **Not the request future.** `BoxExchange` is still unbounded on
//! purpose: `Send` there propagates down seven seam methods and excludes
//! `hclient-rt-embassy`, whose `connect` future holds a `RefCell` because
//! its executor is single-threaded. So `tokio::spawn(client.get(u).send())`
//! still does not compile, and a caller who needs that reaches past the
//! facade — `hclient-native`'s own `tests/send_future.rs` is where that
//! property lives. What crosses a thread here is what a request
//! *produced*, not the act of making it.
//!
//! The bound is only payable because every backend satisfies it, which was
//! not true until the browser's body stopped holding a `js_sys::JsFuture`;
//! `hclient-fetch`'s `body::pump` is that change, and its cost is written
//! there.
// `Client::new()` is `default-transport`'s, so the whole file is: without
// it there is no transport for a body to come out of, and `just
// test-no-default` builds exactly that configuration.
#![cfg(all(not(target_family = "wasm"), feature = "default-transport"))]

use http_body_util::BodyExt;
use std::io::{Read, Write};

/// The type-level half. It is a separate assertion from the run below
/// because `spawn` demands `'static` as well, and this one isolates the
/// auto trait: if it fails, the cause is the erasure and nothing else.
#[test]
fn the_erased_body_type_is_send() {
    fn is_send<T: Send>() {}
    is_send::<hclient::body::ClientBody>();
}

/// And spent the way a caller spends it — the response is collected on
/// another thread while the client stays here.
///
/// The runtime is built by hand rather than through `#[tokio::test]`:
/// this crate's `tokio` dev-dependency carries `rt-multi-thread` and not
/// `macros`, and a feature added for one test is a feature every other
/// build of this crate then resolves.
#[test]
fn a_response_body_is_collected_on_another_thread() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(run());
}

async fn run() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });

    let client = hclient::Client::new().unwrap();
    let resp = client.get(format!("http://{addr}/")).send().await.unwrap();
    let (_parts, body) = resp.into_parts();

    let collected = tokio::spawn(async move { body.collect().await })
        .await
        .expect("the task must not panic")
        .expect("the body must read cleanly");
    assert_eq!(&collected.to_bytes()[..], b"ok");
}
