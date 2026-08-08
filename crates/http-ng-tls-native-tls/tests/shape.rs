//! What can be checked without a TLS server: the seam is implemented, and
//! the one refusal this backend makes happens before it touches the
//! transport.

use http_ng_core::ErrorKind;
use http_ng_tls::{TlsConnect, TlsRequest};
use http_ng_tls_native_tls::NativeTls;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A transport that fails the test if anything reads or writes it. Any use
/// at all means the check under test happened too late.
#[derive(Debug)]
struct NeverTouched;

impl hyper::rt::Read for NeverTouched {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        panic!("the transport was read — the refusal came after the handshake started");
    }
}

impl hyper::rt::Write for NeverTouched {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        panic!("the transport was written — the refusal came after the handshake started");
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("the transport was flushed — the refusal came after the handshake started");
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("the transport was shut down — the refusal came after the handshake started");
    }
}

fn assert_tls_connect<T: TlsConnect>() {}

#[test]
fn native_tls_implements_the_seam() {
    assert_tls_connect::<NativeTls>();
}

/// ECH is refused, not silently skipped — and refused before a single byte
/// reaches the wire. Connecting without the encryption the caller asked for
/// would leak the SNI they were trying to hide, so "best effort" here is
/// worse than an error. `NeverTouched` panics on any IO, so a refusal that
/// arrived after the handshake began would fail this test rather than pass.
#[test]
fn ech_is_refused_before_the_transport_is_touched() {
    let tls = NativeTls::new();
    let req = TlsRequest {
        server_name: "example.com",
        alpn: &[b"h2"],
        ech: Some(&[1, 2, 3]),
    };
    let err = futures_executor::block_on(tls.connect(NeverTouched, req))
        .err()
        .expect("ECH must be refused, not ignored");
    assert_eq!(*err.kind(), ErrorKind::Tls, "{err}");
    assert!(
        err.to_string().to_lowercase().contains("ech"),
        "the error must name what was refused, so a caller can tell it from a handshake failure: {err}"
    );
}

/// The same request without ECH gets past the check and into the handshake
/// — proving the refusal above is conditional on `ech` and not on something
/// incidental. It panics inside `NeverTouched`, which is exactly the
/// evidence wanted: the code reached the transport.
#[test]
#[should_panic(expected = "the transport was")]
fn without_ech_the_handshake_actually_starts() {
    let tls = NativeTls::new();
    let req = TlsRequest {
        server_name: "example.com",
        alpn: &[b"h2"],
        ech: None,
    };
    let _ = futures_executor::block_on(tls.connect(NeverTouched, req));
}
