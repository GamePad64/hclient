//! A label this backend has not got is a **refusal**, never a connection
//! with the default identity.
//!
//! `hclient-native` resolves every label through `TlsIdentity::
//! config_id_for` and refuses before it opens a socket, so these arms are
//! unreachable through the transport. They are reachable through the
//! seam, which is public — and the alternative to refusing is presenting
//! one tenant's certificate to another tenant's server, which no comment
//! about unreachability is worth risking.

use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_rustls::Rustls;
use std::sync::Arc;

mod server;

fn empty_client_config() -> rustls::ClientConfig {
    let (_addr, ca) = server::spawn_tls_echo();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.into()).unwrap();
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

#[tokio::test]
async fn an_unregistered_label_is_refused_before_the_handshake() {
    let (addr, ca) = server::spawn_tls_echo();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = Rustls::from_config(Arc::new(cfg));

    use hclient_rt::TcpConnect;
    let tcp = hclient_rt_tokio::Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    let err = tls
        .connect(
            tcp,
            TlsRequest {
                identity: Some("a-name-nobody-registered"),
                server_name: "localhost",
                alpn: &[b"http/1.1"],
                ech: None,
                early_data: None,
            },
        )
        .await
        .expect_err("an unknown label must not produce a connection");

    let chain = format!("{err:#}") + &format!("{:?}", std::error::Error::source(&err));
    assert!(
        chain.contains("a-name-nobody-registered"),
        "the refusal must name the label the caller asked for: {chain}"
    );
}

/// The control. Without it the test above passes for a backend that
/// refuses every label, registered or not.
#[tokio::test]
async fn a_registered_label_still_connects() {
    let (addr, ca) = server::spawn_tls_echo();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = Rustls::from_config(Arc::new(cfg.clone())).with_identity("corp", Arc::new(cfg));

    use hclient_rt::TcpConnect;
    let tcp = hclient_rt_tokio::Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    tls.connect(
        tcp,
        TlsRequest {
            identity: Some("corp"),
            server_name: "localhost",
            alpn: &[b"http/1.1"],
            ech: None,
            early_data: None,
        },
    )
    .await
    .expect("a registered label must connect");
}

/// **The QUIC half, and the asymmetry is the point.** A backend that
/// refused on TCP and fell back on QUIC would present a certificate over
/// one stack and omit it over the other, which is worse than either
/// answer alone — and it is the shape this crate's own `quic_config_for`
/// nearly shipped twice.
#[allow(
    clippy::err_expect,
    reason = "the Ok side is `dyn quinn_proto::crypto::ClientConfig`, which is not `Debug`, so `expect_err` does not compile"
)]
#[cfg(feature = "quic")]
#[test]
fn the_quic_path_refuses_the_same_label() {
    use hclient_tls::quic::{QuicTlsConnect, QuicTlsRequest};

    let cfg = empty_client_config();
    let tls = Rustls::from_config(Arc::new(cfg.clone())).with_identity("corp", Arc::new(cfg));

    let err = tls
        .quic_client_config(QuicTlsRequest {
            alpn: &[b"h3"],
            ech: None,
            early_data: false,
            identity: Some("not-corp"),
        })
        // `err()` and not `expect_err`: the `Ok` side is a
        // `dyn quinn_proto::crypto::ClientConfig`, which is not `Debug`.
        .err()
        .expect("an unknown label must not produce a QUIC config");
    let chain = format!("{err:#}") + &format!("{:?}", std::error::Error::source(&err));
    assert!(chain.contains("not-corp"), "{chain}");

    // The control, on this path too.
    tls.quic_client_config(QuicTlsRequest {
        alpn: &[b"h3"],
        ech: None,
        early_data: false,
        identity: Some("corp"),
    })
    .expect("a registered label must build a QUIC config");
}
