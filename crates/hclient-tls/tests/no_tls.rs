//! `NoTls` must refuse, and must say so through `Capabilities` rather than
//! only at connect time.

use hclient_core::{ErrorKind, TlsSupport};
use hclient_tls::{NoTls, TlsConnect, TlsRequest};
use std::pin::Pin;
use std::task::{Context, Poll};

#[derive(Debug)]
struct NeverTouched;

impl hyper::rt::Read for NeverTouched {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        panic!("NoTls must not touch the transport");
    }
}
impl hyper::rt::Write for NeverTouched {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        panic!("NoTls must not touch the transport");
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("NoTls must not touch the transport");
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("NoTls must not touch the transport");
    }
}

/// The capability is what a caller reads BEFORE making a request. Getting
/// this wrong is the failure mode the whole `Capabilities` registry exists
/// to prevent, and it is why `TlsConnect::tls_support` exists at all.
#[test]
fn no_tls_advertises_none_not_full() {
    assert_eq!(NoTls.tls_support(), TlsSupport::None);
}

#[test]
fn no_tls_refuses_and_names_the_host_without_touching_the_transport() {
    let req = TlsRequest {
        server_name: "example.com",
        alpn: &[b"http/1.1"],
        ech: None,
        early_data: None,
    };
    let err = futures_executor::block_on(NoTls.connect(NeverTouched, req))
        .expect_err("NoTls must refuse every connection");
    assert_eq!(*err.kind(), ErrorKind::Tls, "{err}");
    assert!(
        err.to_string().contains("example.com"),
        "the error must name the host, so a caller can tell it from a handshake failure: {err}"
    );
}

/// `reports_alpn` defaults to `false`, and an implementation that says
/// nothing must get that default.
///
/// This is the direction the default has to fail in: `NoTls` never
/// negotiates anything, so `false` is simply true of it — but the value
/// is also what a transport reads before deciding whether to *offer* a
/// protocol whose selection it would then have to read back. An
/// implementation that forgot to override it understates itself and works
/// slowly; one that got a `true` by default would have a caller speaking
/// the wrong protocol on every request. `NoTls` is the one implementation
/// in this crate, so it is where the default is pinned.
#[test]
fn reports_alpn_defaults_to_the_conservative_answer() {
    assert!(!NoTls.reports_alpn());
}
