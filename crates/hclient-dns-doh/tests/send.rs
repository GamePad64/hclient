//! A transport resolving through DoH crosses a thread.
//!
//! This was the last `!Send` in the workspace and it was invisible: the
//! property is lost at a `Box<dyn Stream>` inside this crate and shows up
//! as an error in whatever *consumer* asks for it, which is a different
//! crate and often a different repository. A test here is what turns that
//! into a line that fails where the cause is.
//!
//! The assertion is on the **outer transport's** future rather than on the
//! resolver, because that is what a caller holds: `hclient::Client` boxes
//! its transport `Send`, so a resolver that quietly gives the property up
//! takes `Client` with it.

#![cfg(not(target_family = "wasm"))]

use hclient_core::unversioned::Transport;
use hclient_dns::{IpLiteralOnly, Resolve};
use hclient_dns_doh::Doh;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;

/// The same transport the rest of this crate's tests use — a real one, per
/// the note in `Cargo.toml`: nothing here opens a socket, but building the
/// property on the type a caller actually has is the point.
fn transport() -> Native<Tokio, NoTls, IpLiteralOnly> {
    Native::new(Tokio, NoTls, IpLiteralOnly)
}

fn assert_send<T: Send>(_: &T) {}

fn endpoint() -> http::Uri {
    "https://1.1.1.1/dns-query".parse().unwrap()
}

fn request() -> http::Request<hclient_core::RequestBody> {
    http::Request::builder()
        .uri("https://example.test/x")
        .body(hclient_core::RequestBody::Empty)
        .unwrap()
}

/// A `Doh` over the mock — no network, no runtime, and the property is a
/// fact about types rather than about a run.
#[test]
fn a_transport_resolving_through_doh_has_a_send_exchange() {
    let doh = Doh::pinned(transport(), endpoint()).expect("an IP-literal endpoint");
    // The outer transport is the thing a caller holds, and `Doh` stands in
    // it where any other resolver would.
    let outer = Native::new(Tokio, NoTls, doh);
    let f = outer.execute(request());
    assert_send(&f);
}

/// With a fallback, which is the arm that awaits **another resolver's**
/// stream — so its `Send`-ness is a second, independent obligation and one
/// the bounds have to name.
#[test]
fn a_doh_with_a_fallback_resolver_is_send_too() {
    let doh = Doh::pinned(transport(), endpoint())
        .expect("an IP-literal endpoint")
        .with_fallback(IpLiteralOnly);
    assert_send(&doh.lookup_ipv4("example.test"));
    assert_send(&doh.lookup_ipv6("example.test"));
    assert_send(&Native::new(Tokio, NoTls, doh).execute(request()));
}
