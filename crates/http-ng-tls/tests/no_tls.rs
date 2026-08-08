//! `NoTls` must refuse, and must say so through `Capabilities` rather than
//! only at connect time.

use http_ng_core::{ErrorKind, TlsSupport};
use http_ng_tls::{NoTls, TlsConnect, TlsRequest};
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
