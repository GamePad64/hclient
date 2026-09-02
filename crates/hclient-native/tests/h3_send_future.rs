//! `Native::execute`'s future is `Send` **with the QUIC arm installed**,
//! which the file next door does not check.
//!
//! `send_future.rs` builds `Native::new(..)` and asserts the property on
//! the default stack. That is the row that used to be the hard one, and
//! it is not this one: the h3 arm reaches `Native` through
//! `http3::arm`'s erasure — `Box<dyn BoxedStaged<'_>>` — which exists so
//! that `H3`'s bounds stay off `Native`'s `Transport` impl, and an erased
//! future is exactly where a `Send` goes missing. This workspace has
//! written that row down as `Send` since amendment C15 moved the bounds
//! onto the opt-in `Native::http3`, and nothing has asked the compiler.
//!
//! It is true. What was missing is the question, and this file is it —
//! the same shape as the claim about `hclient-dns-doh` that stayed in a
//! table for a week after the commit that made it false, and the reason
//! this project prefers a table that names a test to one that restates
//! what the test would have said.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

use hclient_core::unversioned::Transport;
use hclient_dns_system::SystemDns;
use hclient_native::{H3, Native};
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;

fn assert_send<T: Send>(_: T) {}

#[test]
fn the_exchange_future_crosses_a_thread_with_the_h3_arm_installed() {
    let tls = Rustls::with_webpki_roots();
    let quic = H3::new(Tokio, tls.clone(), SystemDns::new(Tokio)).expect("H3::new does no I/O");
    let transport = Native::new(Tokio, tls, SystemDns::new(Tokio))
        .http3(quic)
        .expect("the two stacks agree on every capability that has one true value");
    let req = http::Request::builder()
        .uri("http://192.0.2.1/")
        .body(hclient_core::RequestBody::Empty)
        .unwrap();
    // Never polled, like its neighbour: the property under test is the
    // future's type, and 192.0.2.1 (RFC 5737 TEST-NET-1) is unroutable.
    assert_send(transport.execute(req));
}
