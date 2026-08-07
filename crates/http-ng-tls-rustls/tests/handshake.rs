//! Тест поднимает настоящий TLS-сервер на rustls и проверяет, что наш адаптер
//! доводит хендшейк до конца и прокачивает байты в обе стороны.

use http_ng_rt::TcpConnect;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::{TlsConnect, TlsRequest};
use http_ng_tls_rustls::Rustls;

mod server; // см. Step 3: минимальный TLS-эхо-сервер на самоподписанном серте

#[tokio::test]
async fn completes_handshake_and_echoes() {
    let (addr, ca_der) = server::spawn_tls_echo();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let tls = Rustls::from_config(std::sync::Arc::new(cfg));
    let tcp = Tokio
        .connect(addr, &http_ng_rt::TcpOpts::default())
        .await
        .unwrap();
    let (mut stream, info) = tls
        .connect(
            tcp,
            TlsRequest {
                server_name: "localhost",
                alpn: &[b"http/1.1"],
                ech: None,
            },
        )
        .await
        .expect("handshake");

    assert_eq!(
        info.alpn.as_deref(),
        Some(b"http/1.1".as_slice()),
        "согласованный ALPN должен быть виден"
    );

    // Прокачка байтов через hyper::rt-интерфейс.
    let sent = b"ping";
    let n = std::future::poll_fn(|cx| {
        hyper::rt::Write::poll_write(std::pin::Pin::new(&mut stream), cx, sent)
    })
    .await
    .unwrap();
    assert_eq!(n, 4);

    let mut store = [0u8; 16];
    let mut rb = hyper::rt::ReadBuf::new(&mut store);
    std::future::poll_fn(|cx| {
        hyper::rt::Read::poll_read(std::pin::Pin::new(&mut stream), cx, rb.unfilled())
    })
    .await
    .unwrap();
    assert_eq!(rb.filled(), b"ping");
}

#[tokio::test]
async fn rejects_an_untrusted_certificate() {
    let (addr, _ca) = server::spawn_tls_echo();
    let tls = Rustls::with_webpki_roots(); // публичные корни — наш серт им неизвестен
    let tcp = Tokio
        .connect(addr, &http_ng_rt::TcpOpts::default())
        .await
        .unwrap();
    let err = tls
        .connect(
            tcp,
            TlsRequest {
                server_name: "localhost",
                alpn: &[],
                ech: None,
            },
        )
        .await
        .expect_err("must fail");
    assert!(matches!(err.kind(), http_ng_core::ErrorKind::Tls), "{err}");
}
