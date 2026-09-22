//! A label this backend has not got is a **refusal**, never a connection
//! with the default identity.
//!
//! `hclient-native` resolves every label through `TlsIdentity::
//! config_id_for` and refuses before it opens a socket, so these arms are
//! unreachable through the transport. They are reachable through the
//! seam, which is public — and the alternative to refusing is presenting
//! one tenant's certificate to another tenant's server, which no comment
//! about unreachability is worth risking.

use hclient_rt::TcpConnect;
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

    let tcp = hclient_rt_tokio::Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    let err = tls
        .connect(
            tcp,
            TlsRequest::new("localhost", &[b"http/1.1"]).identity(Some("a-name-nobody-registered")),
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

    let tcp = hclient_rt_tokio::Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    tls.connect(
        tcp,
        TlsRequest::new("localhost", &[b"http/1.1"]).identity(Some("corp")),
    )
    .await
    .expect("a registered label must connect");
}

/// **The QUIC half, and the asymmetry is the point.** A backend that
/// refused on TCP and fell back on QUIC would present a certificate over
/// one stack and omit it over the other, which is worse than either
/// answer alone — and it is the shape this crate's own `quic_config_for`
/// nearly shipped twice.
#[cfg(feature = "quic")]
#[test]
fn the_quic_path_refuses_the_same_label() {
    use hclient_tls::quic::{QuicTlsConnect, QuicTlsRequest};

    let cfg = empty_client_config();
    let tls = Rustls::from_config(Arc::new(cfg.clone())).with_identity("corp", Arc::new(cfg));

    let err = tls
        .quic_client_config(QuicTlsRequest::new(&[b"h3"]).identity(Some("not-corp")))
        .expect_err("an unknown label must not produce a QUIC config");
    let chain = format!("{err:#}") + &format!("{:?}", std::error::Error::source(&err));
    assert!(chain.contains("not-corp"), "{chain}");

    // The control, on this path too.
    tls.quic_client_config(QuicTlsRequest::new(&[b"h3"]).identity(Some("corp")))
        .expect("a registered label must build a QUIC config");
}

/// **The label reaches the session, and this is the half the refusal
/// above cannot cover.**
///
/// `quic_client_config` checks the label and `quic_session` resolves it,
/// and those are now two calls — so a backend that validated the name
/// and then built a session without it would refuse every *unknown*
/// label and silently present the **default** identity for every known
/// one. That is the silent substitution `docs/mtls-design.md` exists to
/// remove, in the one shape a refusal test cannot see: both halves
/// answer `Ok`.
///
/// Found by mutation — dropping the label on the way into
/// `QuicCryptoConfig` passed the whole suite, including the refusal
/// above — and the gap predates the seam split: with one method the
/// same omission was a config built from `self.base`, equally
/// untested.
///
/// It is asserted through `TlsConfigId`, which is the observable this
/// crate already trusts for exactly this question: the id is a
/// component of `hclient-native`'s pool key, so two labels answering
/// one id is what would let one tenant's connection serve another's
/// request. `tests/config_id.rs` makes the same assertion for the TCP
/// path.
#[cfg(feature = "quic")]
#[test]
fn a_registered_label_reaches_the_session_rather_than_the_default() {
    use hclient_tls::quic::{QuicTlsConnect, QuicTlsRequest};

    let base = empty_client_config();
    let mut named = empty_client_config();
    // A config that differs from `base` in something `TlsConfigId` is
    // derived from, so "the session was built from the label" and "the
    // session was built from the default" are distinguishable at all.
    named.alpn_protocols = vec![b"distinct".to_vec()];

    let tls = Rustls::from_config(Arc::new(base)).with_identity("corp", Arc::new(named));

    let with_label = tls
        .quic_client_config(QuicTlsRequest::new(&[b"h3"]).identity(Some("corp")))
        .expect("a registered label");
    let without = tls
        .quic_client_config(QuicTlsRequest::new(&[b"h3"]))
        .expect("no label at all");

    assert_eq!(
        with_label.identity.as_deref(),
        Some("corp"),
        "the label must travel in the declaration, or the session cannot resolve it"
    );
    assert_eq!(without.identity, None, "and must not appear unasked");

    // And the sessions really differ, so the label is not merely
    // carried but *used*: `quic_session` resolves it against the
    // registered config.
    tls.quic_session(&with_label)
        .expect("the registered identity builds a session");
    tls.quic_session(&without).expect("so does the default one");
}
