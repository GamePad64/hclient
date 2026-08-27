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
//! **Only the TCP stack, and only a resolver whose streams are `Send`.**
//! The seam is untouched, so a `Resolve` that hands back a `!Send` stream
//! still works and still yields a `!Send` future — the answer is per
//! instantiation, which is what inference means and what a declaration on
//! the seam would have taken away.
//!
//! **Not with `http3`.** That arm erases through `Box<dyn BoxedStaged<'_>>`
//! and `Staging<'a>`, neither of which declares `Send`, and declaring it
//! there obliges the *generic* blanket impl in `http3::arm` to prove
//! `StagedConnect::connect`'s RPITIT future `Send` — which cannot be
//! named, so cannot be bounded. That is the same wall one seam down, and
//! closing it means converting `StagedConnect`, and behind it `TcpConnect`
//! and `TlsConnect`, to a form whose futures can be named. Mechanical, and
//! deliberately not done here.
//!
//! **Not through `hclient::Client`.** The facade boxes the transport with
//! no declared auto traits, so erasure takes the property away again.
#![cfg(all(not(target_family = "wasm"), not(feature = "http3")))]

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
