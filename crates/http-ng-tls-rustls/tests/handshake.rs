//! Тест поднимает настоящий TLS-сервер на rustls и проверяет, что наш адаптер
//! доводит хендшейк до конца и прокачивает байты в обе стороны.

use http_ng_rt::TcpConnect;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::{TlsConnect, TlsRequest};
use http_ng_tls_rustls::Rustls;
use std::time::Duration;

mod server; // см. Step 3: минимальный TLS-эхо-сервер на самоподписанном серте

/// Fix round 1 (review, Verdict B #4): ни один вызов в этом файле раньше не
/// был ограничен по времени — регресс, зависающий на хендшейке или на
/// прокачке байт, останавливал бы CI без единого диагностического
/// сообщения, а не падал явным `FAILED`. `Rustls::connect` сознательно не
/// несёт собственного таймаута (`TlsRequest` не несёт дедлайна — см.
/// `close_notify_and_handshake_bounds.rs`), так что ограничение — здесь, на
/// уровне теста, а не внутри реализации.
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

    let tls = Rustls::from_config(std::sync::Arc::new(cfg));
    let tcp = Tokio
        .connect(addr, &http_ng_rt::TcpOpts::default())
        .await
        .unwrap();
    let (mut stream, info) = bounded(tls.connect(
        tcp,
        TlsRequest {
            server_name: "localhost",
            alpn: &[b"http/1.1"],
            ech: None,
        },
    ))
    .await
    .expect("handshake");

    assert_eq!(
        info.alpn.as_deref(),
        Some(b"http/1.1".as_slice()),
        "согласованный ALPN должен быть виден"
    );

    // Прокачка байтов через hyper::rt-интерфейс.
    let sent = b"ping";
    let n = bounded(std::future::poll_fn(|cx| {
        hyper::rt::Write::poll_write(std::pin::Pin::new(&mut stream), cx, sent)
    }))
    .await
    .unwrap();
    assert_eq!(n, 4);

    let mut store = [0u8; 16];
    let mut rb = hyper::rt::ReadBuf::new(&mut store);
    bounded(std::future::poll_fn(|cx| {
        hyper::rt::Read::poll_read(std::pin::Pin::new(&mut stream), cx, rb.unfilled())
    }))
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
    let err = bounded(tls.connect(
        tcp,
        TlsRequest {
            server_name: "localhost",
            alpn: &[],
            ech: None,
        },
    ))
    .await
    .expect_err("must fail");
    assert!(matches!(err.kind(), http_ng_core::ErrorKind::Tls), "{err}");
}
