//! `danger_accept_invalid_certs` against a server this machine has no way
//! to verify — with the control that says the test is measuring anything.
//!
//! The fixture's certificate is self-signed and its issuer is in no trust
//! store, so a verifying client must **refuse** it. That refusal is the
//! whole test: without it, a green "the insecure client connected" would
//! also be green for a build whose ordinary client never verified either.

#![cfg(feature = "dangerous-insecure")]

use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsIdentity, TlsRequest};
use hclient_tls_rustls::Rustls;
use std::sync::Arc;
use std::time::Duration;

mod server;

/// An ordinary verifying client whose trust store holds a real root — an
/// unrelated self-signed certificate. It must refuse the fixture's server.
fn verifying() -> Rustls {
    let unrelated =
        rcgen::generate_simple_self_signed(vec!["someone-else.invalid".into()]).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(unrelated.cert.der().to_vec().into()).unwrap();
    Rustls::from_config(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// A stall here is a regression, not a slow machine — the peer is on
/// loopback and answers or refuses within a round trip.
async fn bounded<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(OP_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| panic!("did not resolve within {OP_TIMEOUT:?}"))
}

async fn handshake(tls: &Rustls, addr: std::net::SocketAddr) -> Result<(), hclient_core::Error> {
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    bounded(tls.connect(
        tcp,
        TlsRequest {
            server_name: "localhost",
            alpn: &[b"http/1.1"],
            ech: None,
            early_data: None,
        },
    ))
    .await
    .map(|_| ())
}

#[tokio::test]
async fn an_unverifiable_server_is_refused_by_default_and_accepted_by_the_insecure_client() {
    let (addr, _ca_der) = server::spawn_tls_echo();

    // The control, and it is a verifying client with a populated trust
    // store rather than an empty one: these roots are real, they simply do
    // not include whoever issued the fixture's certificate. So the refusal
    // below is verification working, not a client with nothing to check
    // against.
    let refused = handshake(&verifying(), addr).await;
    assert!(
        refused.is_err(),
        "a verifying client must refuse a certificate no trust store issued — if this passes, \
         the assertion below proves nothing about verification being skipped"
    );

    // The feature.
    handshake(&Rustls::danger_accept_invalid_certs(), addr)
        .await
        .expect("danger_accept_invalid_certs must complete a handshake the default one refuses");
}

#[tokio::test]
async fn the_negotiated_alpn_still_comes_back() {
    // Skipping verification must not quietly cost the rest of the
    // handshake's results: `hclient-native` chooses HTTP/2 off this value,
    // and a `None` here would silently downgrade every insecure client to
    // HTTP/1.1.
    let (addr, _) = server::spawn_tls_echo();
    let tls = Rustls::danger_accept_invalid_certs();
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    let (_stream, info) = bounded(tls.connect(
        tcp,
        TlsRequest {
            server_name: "localhost",
            alpn: &[b"http/1.1"],
            ech: None,
            early_data: None,
        },
    ))
    .await
    .expect("handshake");
    assert_eq!(info.alpn.as_deref(), Some(b"http/1.1".as_slice()));
}

#[test]
fn an_insecure_config_cannot_share_a_pooled_connection_with_a_verifying_one() {
    // `TlsConfigId` is part of `hclient-native`'s pool key, so this is the
    // property that keeps a connection established without verification
    // from being handed to a client that asked for it. The dangerous
    // direction is the second one.
    let verifying = verifying();
    let insecure = Rustls::danger_accept_invalid_certs();
    assert_ne!(
        verifying.config_id(),
        insecure.config_id(),
        "a verifying and a non-verifying configuration must not key the same pool entry"
    );

    // And two insecure clients are not accidentally interchangeable
    // either — each constructor draws its own identity, which is what
    // keeps this from resting on a constant.
    assert_ne!(
        insecure.config_id(),
        Rustls::danger_accept_invalid_certs().config_id()
    );
}
