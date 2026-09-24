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

#[cfg(feature = "quic")]
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
/// and those are two calls — so a backend that validated the name and
/// then built a session without it would refuse every *unknown* label and
/// silently present the **default** identity for every known one. That is
/// the silent substitution `docs/mtls-design.md` exists to remove, in the
/// one shape a refusal test cannot see: both halves answer `Ok`.
///
/// Found by mutation — dropping the label on the way into
/// `QuicCryptoConfig` passed the whole suite, including the refusal
/// above. **The first version of this test could not fail either**: it
/// asserted that the label travelled in the declaration and then that
/// `quic_session` answered `Ok` for both, which is also true of a
/// `quic_config_for` that resolves the label and then builds from `base`.
///
/// So the two configs now differ in something `quic_session` itself
/// decides on. QUIC protects its Initial packets with
/// `TLS13_AES_128_GCM_SHA256` (RFC 9001 §5.2), and
/// `quinn_proto::crypto::rustls::QuicClientConfig::try_from` refuses a
/// config whose crypto provider lacks it. The registered identity's
/// provider carries `ChaCha20` alone; the default is ordinary. A session
/// built from the label must therefore fail, and one built from the
/// default must not — the only outcome a substitution cannot produce.
#[cfg(feature = "quic")]
#[test]
fn a_registered_label_reaches_the_session_rather_than_the_default() {
    use hclient_tls::quic::{QuicTlsConnect, QuicTlsRequest};

    let base = empty_client_config();
    let mut chacha_only = rustls::crypto::ring::default_provider();
    chacha_only
        .cipher_suites
        .retain(|s| s.suite() == rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
    let named = rustls::ClientConfig::builder_with_provider(Arc::new(chacha_only))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();

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

    tls.quic_session(&without)
        .expect("the default identity is a TLS 1.3 config QUIC accepts");
    assert!(
        tls.quic_session(&with_label).is_err(),
        "the session for `corp` was built from a config with QUIC's Initial suite, so it \
         was not built from `corp`'s ChaCha20-only config: the default was substituted"
    );
}

/// **The TCP half of the same property**: a registered label is *served*,
/// not merely accepted.
///
/// `a_registered_label_still_connects` above registers a config identical
/// to the default, so it passes for a `config_for` that checks the name
/// and then connects with `base`. Here only the named config trusts the
/// server: with the label the handshake must succeed, and without it the
/// same server must be refused.
#[tokio::test]
async fn a_registered_label_is_the_config_the_handshake_uses() {
    let (addr, ca) = server::spawn_tls_echo();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.into()).unwrap();
    let trusting = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let trusting_nothing = rustls::ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    let tls =
        Rustls::from_config(Arc::new(trusting_nothing)).with_identity("corp", Arc::new(trusting));

    let dial = || async {
        hclient_rt_tokio::Tokio
            .connect(addr, &hclient_rt::TcpOpts::default())
            .await
            .unwrap()
    };

    tls.connect(
        dial().await,
        TlsRequest::new("localhost", &[b"http/1.1"]).identity(Some("corp")),
    )
    .await
    .expect("the named config trusts this server, so the labelled handshake must succeed");

    // The control: the default config does not trust it, so a substitution
    // is exactly what this refusal looks like.
    tls.connect(dial().await, TlsRequest::new("localhost", &[b"http/1.1"]))
        .await
        .expect_err("the default config trusts nothing");
}
