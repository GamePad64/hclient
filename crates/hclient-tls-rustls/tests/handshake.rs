//! This test brings up a real TLS server on rustls and checks that our
//! adapter drives the handshake to completion and pushes bytes both ways.

use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_rustls::Rustls;
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

mod server; // see Step 3: a minimal TLS echo server on a self-signed cert

/// Every call in this file is time-bounded — a regression that hangs
/// during the handshake or while pumping bytes would otherwise stall CI
/// with no diagnostic message at all, rather
/// than failing with an explicit `FAILED`. `Rustls::connect` deliberately
/// carries no timeout of its own (`TlsRequest` carries no deadline — see
/// `close_notify_and_handshake_bounds.rs`), so the bound belongs here, at
/// the test level, not inside the implementation.
const OP_TIMEOUT: Duration = Duration::from_secs(10);

async fn bounded<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(OP_TIMEOUT, fut).await.unwrap_or_else(|_| {
        panic!(
            "operation did not resolve within {OP_TIMEOUT:?} - treating a stall as a regression \
             (FAILED), not letting it hang the job with no diagnosis"
        )
    })
}

#[tokio::test]
async fn completes_handshake_and_echoes() {
    let (addr, ca_der) = server::spawn_tls_echo();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let tls = Rustls::from_config(Arc::new(cfg));
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    let (mut stream, info) =
        bounded(tls.connect(tcp, TlsRequest::new("localhost", &[b"http/1.1"])))
            .await
            .expect("handshake");

    assert_eq!(
        info.alpn.as_deref(),
        Some(b"http/1.1".as_slice()),
        "the negotiated ALPN must be visible"
    );

    // Push bytes through the seam this crate is written against.
    let sent = b"ping";
    let n = bounded(poll_fn(|cx| {
        futures_io::AsyncWrite::poll_write(Pin::new(&mut stream), cx, sent)
    }))
    .await
    .unwrap();
    assert_eq!(n, 4);

    let mut store = [0u8; 16];
    let n = bounded(poll_fn(|cx| {
        futures_io::AsyncRead::poll_read(Pin::new(&mut stream), cx, &mut store)
    }))
    .await
    .unwrap();
    assert_eq!(&store[..n], b"ping");
}

#[tokio::test]
async fn rejects_an_untrusted_certificate() {
    let (addr, _ca) = server::spawn_tls_echo();
    // A trust store holding one real root that did not issue the fixture's
    // certificate. Not `with_webpki_roots()`: that needs a feature, and
    // this test is about refusal rather than about which roots are bundled.
    let tls = Rustls::from_config(Arc::new(trusting_someone_else()));
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    let err = bounded(tls.connect(tcp, TlsRequest::new("localhost", &[])))
        .await
        .expect_err("must fail");
    assert!(
        matches!(err.kind(), hclient_core::error::ErrorKind::Tls),
        "{err}"
    );
}

/// The one-line answer that decides whether a transport may offer `h2` at
/// all — see `TlsConnect::reports_alpn`, whose default is `false` because
/// a backend that over-claims it leaves a client speaking HTTP/1 into a
/// connection the server switched to HTTP/2.
///
/// This backend may say `true`, and `completes_handshake_and_echoes`
/// above is why: it offers `http/1.1` to a real server and reads the
/// selection back out of `TlsInfo::alpn`. The assertion here is what ties
/// that demonstrated ability to the value `hclient-native` acts on;
/// without it the override is a claim with nothing behind it.
#[test]
fn this_backend_declares_that_it_reports_alpn() {
    assert!(Rustls::from_config(Arc::new(trusting_someone_else())).reports_alpn());
}

/// A client config whose only root is an unrelated self-signed certificate,
/// so it verifies properly and trusts nothing this file's server presents.
fn trusting_someone_else() -> rustls::ClientConfig {
    let unrelated =
        rcgen::generate_simple_self_signed(vec!["someone-else.invalid".into()]).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(unrelated.cert.der().clone()).unwrap();
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}
