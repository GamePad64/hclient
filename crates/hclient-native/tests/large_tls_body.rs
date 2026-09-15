//! A large body over HTTPS, read through the whole stack.
//!
//! Reported from the field: `act pull` blocks on blobs over roughly a
//! megabyte with `tls: received plaintext buffer full`, reproducibly, and
//! the size is the trigger — the same pull with `Accept-Encoding:
//! identity` fails identically, so it is not a decoder's buffering.
//!
//! `rustls::read_tls` documents that refusal as **backpressure** rather
//! than failure: "errors of `ErrorKind::Other` are emitted to signal
//! backpressure … you should empty it through the `reader()`". So the
//! question this file asks is whether the whole stack — hyper's body
//! reader over `TlsStream` over a real socket — can fall far enough
//! behind a pushing server to meet it.
//!
//! `hclient-tls-rustls`'s own `adversarial_tls_stream.rs` asks the same
//! question of `TlsStream` alone and cannot reach it: reading 64 bytes at
//! a time from a server pushing a megabyte never fills the 64 KiB
//! plaintext buffer, because `poll_read` only pumps the transport when
//! `reader()` is already empty. This file is the same question one layer
//! up, where hyper rather than the test decides the rhythm.
#![cfg(not(target_family = "wasm"))]

use hclient_core::body::RequestBody;
use hclient_core::transport::Transport;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use http_body_util::BodyExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;

const N: usize = 4 * 1024 * 1024;

fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])
        .expect("a self-signed certificate");
    let key = PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    (cert.cert.der().clone(), key)
}

/// A server that answers every request with `N` bytes and writes them as
/// fast as the socket takes them.
fn spawn(n: usize) -> (SocketAddr, CertificateDer<'static>) {
    let (cert_der, key_der) = identity();
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    listener.set_nonblocking(true).expect("nonblocking");

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while tls.read_exact(&mut byte).await.is_ok() {
                        head.push(byte[0]);
                        if head.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let body: Vec<u8> =
                        (0..n).map(|i| u8::try_from(i % 251).unwrap_or(0)).collect();
                    let _ = tls
                        .write_all(
                            format!("HTTP/1.1 200 OK\r\nContent-Length: {n}\r\n\r\n").as_bytes(),
                        )
                        .await;
                    let _ = tls.write_all(&body).await;
                    let _ = tls.flush().await;
                    let _ = tls.shutdown().await;
                });
            }
        });
    });
    (addr, cert_der)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_multi_megabyte_body_over_tls_arrives_whole() {
    let (addr, cert) = spawn(N);
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).expect("the server's own certificate");
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let t = Native::new(
        Tokio,
        Rustls::from_config(Arc::new(cfg)),
        hclient_dns::IpLiteralOnly,
    );
    let req = http::Request::get(format!("https://127.0.0.1:{}/blob", addr.port()))
        .body(RequestBody::Empty)
        .expect("a well-formed request");

    let resp = tokio::time::timeout(std::time::Duration::from_secs(60), t.execute(req))
        .await
        .expect("the exchange must not hang")
        .expect("the exchange completes");
    let body = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        resp.into_body().collect(),
    )
    .await
    .expect("the body must not hang")
    .expect("the body arrives without a TLS error")
    .to_bytes();

    assert_eq!(body.len(), N, "the whole blob");
    assert!(
        body.iter()
            .enumerate()
            .all(|(i, b)| *b == u8::try_from(i % 251).unwrap_or(0)),
        "byte for byte"
    );
}
