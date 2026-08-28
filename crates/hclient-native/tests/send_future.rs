//! `Native::execute`'s future is `Send` on the default stack — and that is
//! a property of the *concrete* transport, inferred, not a bound declared
//! anywhere.
//!
//! # Why this file exists
//!
//! A caller who wants `tokio::spawn(client_request)` needs the future to
//! cross a thread. Until `connect.rs`'s `Answers` held its resolver stream
//! as `Pin<Box<dyn Stream<..>>>`, it could not: one `dyn` with no declared
//! auto traits erased the `Send` of every concrete resolver behind it, and
//! the error named exactly that box. Holding the stream as `Pin<Box<S>>`
//! instead — the same allocation, the same absence of `unsafe`, the type
//! merely no longer thrown away — lets the compiler see through to the
//! concrete stream, and the property falls out with **no bound, no
//! `send-bound-exception` marker and no new dependency**.
//!
//! Checked in the failing direction before it was believed: with the `dyn`
//! back, this file fails with `dyn Stream<Item = Result<ResolvedAddr,
//! Error>> cannot be sent between threads safely`, naming `Answers` as the
//! type that contains it.
//!
//! # What it deliberately does not claim
//!
//! **Only a resolver whose streams are `Send`.**
//! The seam is untouched, so a `Resolve` that hands back a `!Send` stream
//! still works and still yields a `!Send` future — the answer is per
//! instantiation, which is what inference means and what a declaration on
//! the seam would have taken away.
//!
//! **HTTP/2 included, and it was not at first.** `http2::On1xx` was
//! `&'a dyn Fn(StatusCode, &HeaderMap)`, held across an await, so the one
//! borrowed trait object made this false for every build with that feature
//! on — including one whose hook is an ordinary `Send` type. It is a type
//! parameter now. `just test-no-default` runs this crate's suite under
//! `--features http2`, which is what checks it: the workspace run is
//! `--all-features`, where `http3` switches this file off.
//!
//! **With `http3` too, and this file was gated out of that configuration
//! for two weeks after it stopped being true.** The gate read
//! `not(feature = "http3")`, because the QUIC arm erased through
//! `Box<dyn BoxedStaged<'_>>` and `Staging<'a>` and declaring `Send` there
//! needed `StagedConnect::connect`'s RPITIT future named. It carries
//! associated futures now, so the arm declares `Send` and the workspace's
//! own `--all-features` run — which is where this file was *not* being
//! compiled — is where it runs.
//!
//! **Through `hclient::Client` as well**, since `BoxExchange` declares
//! `Send` (amendment C16). That is a different property with a different
//! proof — `hclient/tests/client_shape.rs` — and it is why this file's
//! subject is worth keeping separate: here the answer is *inferred* from
//! one concrete stack, there it is *declared* and every backend has to
//! satisfy it.
#![cfg(not(target_family = "wasm"))]

use hclient_core::unversioned::Transport;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;

fn assert_send<T: Send>(_: T) {}

#[test]
fn the_exchange_future_crosses_a_thread_on_the_default_stack() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let req = http::Request::builder()
        .uri("http://192.0.2.1/")
        .body(hclient_core::RequestBody::Empty)
        .unwrap();
    // Never polled: the property under test is the future's type, not what
    // it does, and 192.0.2.1 (RFC 5737 TEST-NET-1) is unroutable anyway.
    assert_send(t.execute(req));
}

/// The type-level claim above, spent the way a caller spends it: the
/// exchange runs on a **multi-threaded** runtime through `tokio::spawn`,
/// which takes `F: Send + 'static`, so this file would not compile if the
/// property were only nearly true.
///
/// It is a separate test from the one above and not a replacement for it.
/// This one needs a socket, a server and a runtime, so a failure here has
/// several possible causes; the one above has exactly one, and names it.
/// The pair is the assertion: `assert_send` says the type is right, this
/// says the type is the one a `spawn` actually demands — `'static` among
/// them, which `assert_send` does not ask for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawned_exchange_answers() {
    use std::io::{Read, Write};

    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });

    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let req = http::Request::builder()
        .uri(format!("http://{addr}/"))
        .body(hclient_core::RequestBody::Empty)
        .unwrap();

    // The transport is moved into the task, so the future is `'static` as
    // well as `Send` — the two halves `spawn` asks for.
    let joined = tokio::spawn(async move { t.execute(req).await })
        .await
        .expect("the task must not panic")
        .expect("the exchange must succeed");
    assert_eq!(joined.status(), 200);
}
