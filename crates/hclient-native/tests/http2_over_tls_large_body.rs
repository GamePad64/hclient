//! HTTP/2 **over TLS**, with a body far larger than rustls' plaintext
//! buffer — the one configuration this crate has no fixture for, and the
//! one a reported field failure runs in.
//!
//! `act` builds with `hclient/http2`, `hclient-native/http2` and
//! `hclient-tls-rustls`, and its blob pulls block over roughly a megabyte
//! with `tls: received plaintext buffer full`. That string comes from
//! `hclient-tls-rustls`'s `pump_incoming`, which wraps every `read_tls`
//! error — and rustls documents exactly that one as **backpressure**
//! rather than failure: "errors of `ErrorKind::Other` are emitted to
//! signal backpressure … you should empty it through the `reader()`".
//!
//! **Why this file rather than the two that already exist.** Neither
//! reaches the shape:
//!
//! * `http2.rs` runs a real `h2::server` on a **plain** socket, so no
//!   rustls buffer is in the path at all.
//! * `tls_facts.rs` and friends run real TLS over **HTTP/1.1**, where the
//!   body's own reader drives the socket: `poll_read` pumps the transport
//!   only when rustls' plaintext buffer is already empty, so nothing can
//!   accumulate. Measured: 4 MiB over HTTP/1.1 arrives whole, and
//!   `read_tls` does not signal backpressure once.
//!
//! On HTTP/2 the connection **driver** reads the socket, independently of
//! whether anything is consuming the body — which is the difference this
//! file exists to exercise.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]

use bytes::Bytes;
use hclient_core::body::RequestBody;
use hclient_core::transport::Transport;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use http_body_util::BodyExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;

/// 128 chunks of 64 KiB: 8 MiB, well past both rustls' 64 KiB plaintext
/// buffer and h2's default 64 KiB flow-control window.
const CHUNK: usize = 64 * 1024;
const CHUNKS: usize = 128;
const TOTAL: usize = CHUNK * CHUNKS;

fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])
        .expect("a self-signed certificate");
    let key = PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    (cert.cert.der().clone(), key)
}

/// An `h2::server` behind a rustls acceptor that announces `h2` in ALPN,
/// pushing `TOTAL` bytes as fast as the peer's windows allow.
fn spawn() -> (SocketAddr, CertificateDer<'static>) {
    let (cert_der, key_der) = identity();
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    cfg.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    listener.set_nonblocking(true).expect("nonblocking");

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
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
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let Ok(mut conn) = h2::server::handshake(tls).await else {
                        return;
                    };
                    // `accept` is what drives this connection's IO, so the
                    // handler is spawned rather than awaited inline — h2's
                    // own server example's shape, and `http2.rs`'s.
                    while let Some(Ok((_req, mut respond))) = conn.accept().await {
                        tokio::spawn(async move {
                            let response = http::Response::builder()
                                .status(200)
                                .body(())
                                .expect("a well-formed response");
                            let Ok(mut send) = respond.send_response(response, false) else {
                                return;
                            };
                            for _ in 0..CHUNKS {
                                send.reserve_capacity(CHUNK);
                                if send
                                    .send_data(Bytes::from(vec![b'y'; CHUNK]), false)
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            let _ = send.send_data(Bytes::new(), true);
                        });
                    }
                });
            }
        });
    });
    (addr, cert_der)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_eight_megabyte_body_over_h2_and_tls_arrives_whole() {
    let (addr, cert) = spawn();
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

    let resp = tokio::time::timeout(std::time::Duration::from_secs(90), t.execute(req))
        .await
        .expect("the exchange must not hang")
        .expect("the exchange completes");
    assert_eq!(
        resp.version(),
        http::Version::HTTP_2,
        "ALPN must have settled on h2, or this file tests HTTP/1.1 twice"
    );

    let body = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        resp.into_body().collect(),
    )
    .await
    .expect("the body must not hang")
    .expect("the body arrives without a TLS error")
    .to_bytes();

    assert_eq!(body.len(), TOTAL, "the whole blob");
    assert!(body.iter().all(|b| *b == b'y'), "byte for byte");
}
