//! What the server asked for, when it asked for a client certificate.
//!
//! rustls hands the `CertificateRequest`'s contents to a
//! `ResolvesClientCert` and to nothing else, so the only way to see them
//! is to be the resolver. These tests check the three answers that shape
//! is worth having: the server did not ask, it asked and nothing was
//! sent, it asked and something was.
//!
//! The middle one is the load-bearing case, because it is the one a
//! caller cannot get any other way: a `403` over a connection where a
//! certificate *would* have been accepted and none was sent is a
//! different fact from a `403` over a connection where none was wanted,
//! and only `answered` separates them.

use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsInfo, TlsRequest};
use hclient_tls_rustls::Rustls;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

mod server;

const OP_TIMEOUT: Duration = Duration::from_secs(10);

async fn bounded<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(OP_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| panic!("did not resolve within {OP_TIMEOUT:?}"))
}

/// A certificate authority, and one certificate it issued.
struct Ca {
    cert_der: Vec<u8>,
    issued_chain: Vec<rustls_pki_types::CertificateDer<'static>>,
    issued_key: rustls_pki_types::PrivateKeyDer<'static>,
}

fn ca() -> Ca {
    let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "hclient client CA");
    let key = rcgen::KeyPair::generate().unwrap();
    let issuer = rcgen::CertifiedIssuer::self_signed(params, key).unwrap();

    let mut leaf_params = rcgen::CertificateParams::new(vec!["client".to_string()]).unwrap();
    leaf_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "hclient client");
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

    Ca {
        cert_der: issuer.der().to_vec(),
        issued_chain: vec![leaf.der().to_vec().into()],
        issued_key: rustls_pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
    }
}

/// A TLS server that **asks** for a client certificate and accepts a
/// connection without one.
///
/// `allow_unauthenticated`, deliberately: the case this file exists for
/// is the handshake that completes with an empty certificate, which is
/// what makes the observe-then-choose sequence possible at all. A
/// verifier that required one would fail the handshake and there would be
/// no `TlsInfo` to read.
fn spawn_asking(client_ca: &[u8]) -> (SocketAddr, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.signing_key.serialize_der();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(client_ca.to_vec().into()).unwrap();
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .allow_unauthenticated()
        .build()
        .unwrap();

    let mut cfg = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![cert_der.clone().into()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )
        .unwrap();
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
                    // One read is enough to drive the handshake to
                    // completion on this side; the client only needs the
                    // handshake, never a byte of application data.
                    use tokio::io::AsyncReadExt;
                    let mut buf = [0u8; 64];
                    let _ = tls.read(&mut buf).await;
                });
            }
        });
    });

    (addr, cert_der)
}

async fn handshake(addr: SocketAddr, tls: Rustls) -> TlsInfo {
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    let (_stream, info) = bounded(tls.connect(
        tcp,
        TlsRequest {
            identity: None,
            server_name: "localhost",
            alpn: &[b"http/1.1"],
            ech: None,
            early_data: None,
        },
    ))
    .await
    .expect("handshake");
    info
}

fn trusting(server_ca: &[u8]) -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(server_ca.to_vec().into()).unwrap();
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

#[tokio::test]
async fn a_server_that_asks_is_reported_with_the_authorities_it_named() {
    let ca = ca();
    let (addr, server_ca) = spawn_asking(&ca.cert_der);

    let tls = Rustls::from_config(Arc::new(trusting(&server_ca)));
    let info = handshake(addr, tls).await;

    let hclient_tls::ClientCertAsk::Asked(asked) = info.client_cert else {
        panic!(
            "the server sent a CertificateRequest and it must be reported: {:?}",
            info.client_cert
        )
    };
    assert_eq!(
        asked.authority_names.len(),
        1,
        "the verifier was built with exactly one root, so exactly one \
         distinguished name goes on the wire"
    );
    // The name is the CA's own subject, not something this crate minted:
    // a DER subject sequence appears verbatim inside the certificate that
    // carries it, so containment is the check that the bytes came off the
    // wire rather than out of a constructor.
    let dn = &asked.authority_names[0];
    assert!(
        ca.cert_der.windows(dn.len()).any(|w| w == dn.as_slice()),
        "the reported authority name must be a subsequence of the CA \
         certificate it was taken from"
    );
    assert!(
        !asked.sigschemes.is_empty(),
        "a CertificateRequest always carries signature_algorithms"
    );
    assert!(
        !asked.answered,
        "this client holds no certificate, so it sent none — which is the \
         fact a caller cannot get any other way"
    );
}

#[tokio::test]
async fn a_server_that_does_not_ask_reports_nothing() {
    // The control, and the reason the field is an `Option`: without this
    // a `Some` carrying an empty list would be indistinguishable from a
    // server that never asked.
    let (addr, server_ca) = server::spawn_tls_echo();
    let tls = Rustls::from_config(Arc::new(trusting(&server_ca)));
    let info = handshake(addr, tls).await;
    assert_eq!(
        info.client_cert,
        hclient_tls::ClientCertAsk::NotAsked,
        "this backend watches every handshake through its own resolver, so a \
         server that did not ask is an answer and not a silence"
    );
}

#[tokio::test]
async fn a_certificate_that_was_sent_is_reported_as_answered() {
    let ca = ca();
    let (addr, server_ca) = spawn_asking(&ca.cert_der);

    let mut roots = rustls::RootCertStore::empty();
    roots.add(server_ca.clone().into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(ca.issued_chain, ca.issued_key)
        .unwrap();

    let info = handshake(addr, Rustls::from_config(Arc::new(cfg))).await;

    let hclient_tls::ClientCertAsk::Asked(asked) = info.client_cert else {
        panic!("the server asked: {:?}", info.client_cert)
    };
    assert!(
        asked.answered,
        "a certificate was configured and matched, so one was presented"
    );
    // The wrap must not change what is sent — the pair with the test
    // above is what says `Recording` observes rather than intervenes.
    assert_eq!(asked.authority_names.len(), 1);
}
