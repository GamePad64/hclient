//! Доказательство центрального тезиса Task 8 на реальном TLS-хендшейке, а не
//! только на трейт-контракте: `TlsConnect` типизирован на
//! `hyper::rt::{Read, Write}`, а не на futures-io/tokio-io, поэтому ОДИН
//! адаптер (`Rustls::connect`) обслуживает и tokio, и smol без единой
//! рантайм-специфичной ветки в общем теле. Модель — pair-property-тест
//! `crates/http-ng-rt-pair-check` (Task 4, fix round 1): там то же самое
//! доказывается для голых способностей рантайма, здесь — для TLS-адаптера,
//! построенного поверх них.
//!
//! `handshake_and_echo` ниже — единственное тело: TCP-коннект через
//! переданный рантайм, TLS-хендшейк ТЕМ ЖЕ `Rustls`, обмен байтами через
//! `hyper::rt::{Read, Write}`. Ни `#[cfg]`, ни рантайм-специфичного бонда
//! внутри нет — оба инстанцирования ниже различаются только конкретным
//! типом рантайма и тем, чем каждый тест крутит свой executor
//! (`#[tokio::test]` vs. `futures_executor::block_on`), что неизбежно и
//! остаётся снаружи общего тела по построению.
use http_ng_rt::{TcpConnect, TcpOpts};
use http_ng_tls::{TlsConnect, TlsRequest};
use http_ng_tls_rustls::Rustls;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

mod server;

async fn handshake_and_echo<R: TcpConnect>(rt: R, addr: SocketAddr, ca_der: Vec<u8>) {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = Rustls::from_config(Arc::new(cfg));

    let tcp = rt.connect(addr, &TcpOpts::default()).await.unwrap();
    let (mut stream, info) = tls
        .connect(
            tcp,
            TlsRequest {
                server_name: "localhost",
                alpn: &[],
                ech: None,
            },
        )
        .await
        .expect("handshake");
    assert_eq!(info.protocol_version.as_deref(), Some("TLSv1.3"));

    let n =
        std::future::poll_fn(|cx| hyper::rt::Write::poll_write(Pin::new(&mut stream), cx, b"ping"))
            .await
            .unwrap();
    assert_eq!(n, 4);

    let mut store = [0u8; 16];
    let mut rb = hyper::rt::ReadBuf::new(&mut store);
    std::future::poll_fn(|cx| hyper::rt::Read::poll_read(Pin::new(&mut stream), cx, rb.unfilled()))
        .await
        .unwrap();
    assert_eq!(rb.filled(), b"ping");
}

#[tokio::test]
async fn same_adapter_completes_the_handshake_over_tokio() {
    use http_ng_rt_tokio::Tokio;
    let (addr, ca_der) = server::spawn_tls_echo();
    handshake_and_echo(Tokio, addr, ca_der).await;
}

#[test]
fn same_adapter_completes_the_handshake_over_smol() {
    use http_ng_rt_smol::Smol;
    let (addr, ca_der) = server::spawn_tls_echo();
    futures_executor::block_on(handshake_and_echo(Smol, addr, ca_der));
}
