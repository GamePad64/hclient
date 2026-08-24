//! The central claim, on a real TLS handshake rather than at the
//! trait-contract level: `TlsConnect` is typed on `hyper::rt::{Read,
//! Write}`, not on futures-io/tokio-io, so ONE adapter (`Rustls::connect`)
//! serves both tokio and smol with no runtime-specific branch anywhere in
//! the shared body. The model is the pair-property test in
//! `crates/hclient-rt-pair-check`, which proves the same thing for the
//! bare runtime capabilities; this is the TLS adapter on top of them.
//!
//! `handshake_and_echo` below is the one shared body: TCP connect through
//! the passed-in runtime, TLS handshake through that SAME `Rustls`, byte
//! exchange through `hyper::rt::{Read, Write}`. There is no `#[cfg]`, no
//! runtime-specific bound inside it — the two instantiations below differ
//! only in the concrete runtime type and in how each test drives its own
//! executor (`#[tokio::test]` vs. `futures_executor::block_on`), which is
//! unavoidable and stays outside the shared body by construction.
use hclient_rt::{TcpConnect, TcpOpts};
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_rustls::Rustls;
use std::future::poll_fn;
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
                early_data: None,
            },
        )
        .await
        .expect("handshake");
    assert_eq!(info.protocol_version.as_deref(), Some("TLSv1.3"));

    let n = poll_fn(|cx| hyper::rt::Write::poll_write(Pin::new(&mut stream), cx, b"ping"))
        .await
        .unwrap();
    assert_eq!(n, 4);

    let mut store = [0u8; 16];
    let mut rb = hyper::rt::ReadBuf::new(&mut store);
    poll_fn(|cx| hyper::rt::Read::poll_read(Pin::new(&mut stream), cx, rb.unfilled()))
        .await
        .unwrap();
    assert_eq!(rb.filled(), b"ping");
}

#[tokio::test]
async fn same_adapter_completes_the_handshake_over_tokio() {
    use hclient_rt_tokio::Tokio;
    let (addr, ca_der) = server::spawn_tls_echo();
    handshake_and_echo(Tokio, addr, ca_der).await;
}

#[test]
fn same_adapter_completes_the_handshake_over_smol() {
    use hclient_rt_smol::Smol;
    let (addr, ca_der) = server::spawn_tls_echo();
    futures_executor::block_on(handshake_and_echo(Smol, addr, ca_der));
}
