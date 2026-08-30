//! What can be asserted without a Windows machine.
//!
//! Nothing here opens a session, because `WinHttpOpen` needs the OS. What
//! it does pin is the shape of the seam — the properties a consumer reads
//! off the types rather than off a run — and those are exactly the things
//! a refactor could take away silently.
#![cfg(windows)]

use hclient_core::unversioned::{SendTransport, Transport};
use hclient_winhttp::{WinHttp, WinHttpBody};
use static_assertions::assert_impl_all;

/// The transport crosses threads, which is what `SendTransport` promises
/// on its behalf. A WinHTTP handle has no thread affinity and the
/// completion callback runs on a pool thread, so this is a property of
/// the API rather than of this crate — and losing it would mean
/// `hclient::Client` stopped accepting this backend.
#[test]
fn the_transport_and_its_body_cross_threads() {
    assert_impl_all!(WinHttp: Send);
    assert_impl_all!(WinHttp: Sync);
    assert_impl_all!(WinHttpBody: Send);
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

/// `Protocols` is a bitmask, composed and read from **outside** this
/// crate.
///
/// A shape test rather than a comment for the reason the literal version
/// of it had: the constants and the accessors are the whole public
/// surface, and a change to either would stop this line compiling where
/// nothing else would notice.
///
/// **What it replaced is worth knowing.** This was
/// `Protocols { http2: true, ..Default::default() }`, and the type's doc
/// argued against `#[non_exhaustive]` to keep exactly that expression
/// legal from here. A bitmask does not need the argument: a third
/// protocol is a new constant, which is additive without anybody writing
/// `..Default::default()` to be safe from it.
#[test]
fn the_protocol_set_is_composed_and_read_from_outside() {
    use hclient_winhttp::Protocols;

    assert!(Protocols::HTTP2.http2());
    assert!(!Protocols::HTTP2.http3());
    let both = Protocols::HTTP2 | Protocols::HTTP3;
    assert!(both.http2() && both.http3());
    assert_eq!(both, Protocols::all());
    // `Copy`, because `WinHttp::protocols` stores it and `mask_for` takes
    // it by value once per request.
    let two = both;
    assert_eq!(both, two);
    // The default is neither, which is WinHTTP's own `0x0` — HTTP/1.1 and
    // prior.
    assert_eq!(Protocols::default(), Protocols::NONE);
    assert!(!Protocols::default().http2());
}

/// The crate's one doctest, compiled where something actually compiles
/// it.
///
/// `just test-doc` runs `cargo test --doc --workspace` on a Linux host,
/// where this crate is `#![cfg(windows)]` and contributes **no
/// doctests at all** — so the `no_run` fence on
/// [`WinHttp::protocols`](hclient_winhttp::WinHttp::protocols) is a code
/// block nothing builds, which is the shape this workspace records three
/// times over as a check that cannot fail. This function is never
/// called and is built by `cargo check -p hclient-winhttp --target
/// x86_64-pc-windows-msvc --all-targets`, which `just check-targets`
/// runs on every push, so the example's chain is type-checked there
/// instead. Keep the two in step.
#[allow(dead_code)]
fn the_documented_construction_type_checks() -> Result<(), hclient_core::Error> {
    use hclient_winhttp::{Protocols, WinHttp};

    let _transport = WinHttp::new()?.protocols(Protocols::HTTP2 | Protocols::HTTP3);
    Ok(())
}
