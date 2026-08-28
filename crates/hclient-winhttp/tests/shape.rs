//! What can be asserted without a Windows machine.
//!
//! Nothing here opens a session, because `WinHttpOpen` needs the OS. What
//! it does pin is the shape of the seam — the properties a consumer reads
//! off the types rather than off a run — and those are exactly the things
//! a refactor could take away silently.
#![cfg(windows)]

use hclient_core::unversioned::{SendTransport, Transport};
use hclient_winhttp::{WinHttp, WinHttpBody};

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

/// The transport crosses threads, which is what `SendTransport` promises
/// on its behalf. A WinHTTP handle has no thread affinity and the
/// completion callback runs on a pool thread, so this is a property of
/// the API rather than of this crate — and losing it would mean
/// `hclient::Client` stopped accepting this backend.
#[test]
fn the_transport_and_its_body_cross_threads() {
    assert_send::<WinHttp>();
    assert_sync::<WinHttp>();
    assert_send::<WinHttpBody>();
}

/// `Client::builder` requires `SendTransport`, so this is the line
/// between *there is a transport* and *there is a client*.
#[test]
fn it_is_a_send_transport() {
    fn takes<T: SendTransport>() {}
    takes::<WinHttp>();
}

/// The body is an `http_body::Body` over `Bytes`, which is what every
/// wrapper in `hclient` is written against.
#[test]
fn the_body_is_an_http_body() {
    fn takes<B: http_body::Body<Data = bytes::Bytes>>() {}
    takes::<WinHttpBody>();
}

/// `Transport::Body` really is the body above, rather than something
/// that merely happens to be returned.
#[test]
fn the_associated_body_is_the_one_this_crate_exports() {
    fn same<T: Transport<Body = WinHttpBody>>() {}
    same::<WinHttp>();
}
