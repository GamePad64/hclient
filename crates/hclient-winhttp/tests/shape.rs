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

/// `Protocols` is written with a struct literal and a functional update
/// from **outside** this crate, which is what its lack of
/// `#[non_exhaustive]` is for — `TcpOpts`' argument one crate over, and
/// the reason a shape test rather than a comment: the attribute would
/// make this line stop compiling and nothing else would notice.
#[test]
fn the_protocol_set_is_written_as_a_literal_from_outside() {
    use hclient_winhttp::Protocols;
    let one = Protocols {
        http2: true,
        ..Default::default()
    };
    assert_eq!(
        one,
        Protocols {
            http2: true,
            http3: false
        }
    );
    // `Copy`, because `WinHttp::protocols` stores it and `mask_for` takes
    // it by value once per request.
    let two = one;
    assert_eq!(one, two);
    assert_eq!(
        Protocols::default(),
        Protocols {
            http2: false,
            http3: false
        }
    );
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

    let _transport = WinHttp::new()?.protocols(Protocols {
        http2: true,
        ..Default::default()
    });
    Ok(())
}
