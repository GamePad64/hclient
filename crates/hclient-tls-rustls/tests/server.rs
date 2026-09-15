//! A minimal TLS echo server on a self-signed certificate.
//! Lives in dev-dependencies and never reaches the public dependency graph.

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// # Panics
///
/// Panics if generating the self-signed certificate, building the server
/// config, or binding the listener fails — any of which means the test
/// fixture itself is broken, not the code under test.
pub fn spawn_tls_echo() -> (SocketAddr, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.signing_key.serialize_der();

    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der.clone().into()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )
        .unwrap();
    let mut cfg = cfg;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = tls.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if tls.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
    });

    (addr, cert_der)
}

/// A TLS server that **pushes** `n` bytes as fast as the socket takes
/// them, without waiting to be asked — a blob download rather than an
/// echo. `spawn_tls_echo`'s read-then-answer loop delivers in the
/// reader's own rhythm, so nothing can accumulate on the client side;
/// this one is what puts a slow reader behind.
///
/// # Panics
///
/// On any failure to build the certificate, the config, the listener or
/// the runtime — this is a fixture, and a fixture that cannot start has
/// nothing to say about the code under test.
#[allow(
    dead_code,
    reason = "this module is shared by several test binaries and each uses its own subset; `dead_code` is per-binary."
)]
pub fn spawn_tls_pusher(n: usize) -> (SocketAddr, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.signing_key.serialize_der();

    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der.clone().into()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )
        .unwrap();
    let mut cfg = cfg;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let body: Vec<u8> =
                        (0..n).map(|i| u8::try_from(i % 251).unwrap_or(0)).collect();
                    let _ = tls.write_all(&body).await;
                    let _ = tls.flush().await;
                });
            }
        });
    });
    (addr, cert_der)
}
