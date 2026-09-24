//! What this backend answers to every question the two seams ask of it —
//! each defaulted member it overrides, each one it deliberately inherits,
//! and the wrapper it puts round every config.
//!
//! The mutation sweep that preceded this file found `presents_client_certs`,
//! `config_id_for` and `offers_early_data` each replaceable by a constant
//! with the suite green: the overrides were tested through `hclient-native`
//! and nowhere in the crate that makes the claim. The same sweep found the
//! recording resolver's two forwarding methods unpinned, which matters
//! more than it looks, because that wrapper is put round **every** config —
//! a forwarder that answered a constant would change what a caller's own
//! `rustls::ClientConfig` puts on the wire.

use hclient_core::caps::TlsSupport;
use hclient_rt::TcpConnect;
use hclient_tls::{TlsConnect, TlsIdentity, TlsRequest};
use hclient_tls_rustls::Rustls;
use std::sync::Arc;

fn no_roots() -> rustls::RootCertStore {
    rustls::RootCertStore::empty()
}

fn without_a_certificate() -> rustls::ClientConfig {
    rustls::ClientConfig::builder()
        .with_root_certificates(no_roots())
        .with_no_client_auth()
}

fn with_a_certificate() -> rustls::ClientConfig {
    let cert = rcgen::generate_simple_self_signed(vec!["client.invalid".into()]).unwrap();
    rustls::ClientConfig::builder()
        .with_root_certificates(no_roots())
        .with_client_auth_cert(
            vec![cert.cert.der().clone()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into()),
        )
        .unwrap()
}

/// Asked of the configs rather than remembered, and asked of **both** the
/// default config and every named one — so each of the three shapes is its
/// own row: nothing anywhere, a certificate on the default, a certificate
/// only on a named identity. The third is the row that separates `||` from
/// `&&`, and it is also the one `hclient-native`'s `Capabilities` would get
/// wrong if only the default were asked.
#[test]
fn presents_client_certs_is_true_exactly_when_some_config_has_one() {
    assert!(!Rustls::from_config(Arc::new(without_a_certificate())).presents_client_certs());
    assert!(Rustls::from_config(Arc::new(with_a_certificate())).presents_client_certs());
    assert!(
        Rustls::from_config(Arc::new(without_a_certificate()))
            .with_identity("tenant", Arc::new(with_a_certificate()))
            .presents_client_certs(),
        "a certificate held only by a named identity is still one this backend presents"
    );
    assert!(
        !Rustls::from_config(Arc::new(without_a_certificate()))
            .with_identity("tenant", Arc::new(without_a_certificate()))
            .presents_client_certs(),
        "and a named identity with no certificate adds none"
    );
}

/// `Some` for a registered label, `None` otherwise — and the `Some` is the
/// label's **own** id, not the connector's, because that difference is
/// what keeps two tenants off one pooled connection.
#[test]
fn config_id_for_answers_a_registered_label_with_its_own_identity() {
    let tls = Rustls::from_config(Arc::new(without_a_certificate()))
        .with_identity("a", Arc::new(without_a_certificate()))
        .with_identity("b", Arc::new(without_a_certificate()));

    let a = tls.config_id_for("a").expect("`a` is registered");
    let b = tls.config_id_for("b").expect("`b` is registered");
    assert_ne!(a, b, "two labels, two identities");
    assert_ne!(a, tls.config_id(), "a label is not the default identity");
    assert_eq!(
        tls.config_id_for("c"),
        None,
        "an unregistered label is refused"
    );
}

/// The two `TlsConnect` defaults this backend **inherits**, taken on
/// purpose: `Full`, because TLS here is configured by this client rather
/// than by a platform; and `applies_ech() == false`, because `connect`
/// refuses a non-`None` `ech` (see `tests/ech.rs`). An override of either
/// would be a claim this file should have to be edited to allow.
#[test]
fn the_inherited_tcp_defaults_are_the_true_ones() {
    let tls = Rustls::from_config(Arc::new(without_a_certificate()));
    assert_eq!(tls.tls_support(), TlsSupport::Full);
    assert!(!tls.applies_ech());
}

/// `true`: the QUIC path sets `enable_early_data` when asked, so the
/// backend can offer it. What the transport reads this for is whether to
/// mark a request for early data at all.
#[cfg(feature = "quic")]
#[test]
fn the_quic_path_says_it_can_offer_early_data() {
    use hclient_tls::quic::QuicTlsConnect;
    assert!(Rustls::from_config(Arc::new(without_a_certificate())).offers_early_data());
}

/// A client-certificate resolver that holds nothing and speaks only raw
/// public keys (RFC 7250), which rustls announces in the `ClientHello` as a
/// `client_certificate_type` extension.
#[derive(Debug)]
struct RawKeysOnly;

impl rustls::client::ResolvesClientCert for RawKeysOnly {
    fn resolve(
        &self,
        _: &[&[u8]],
        _: &[rustls::SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        None
    }
    fn only_raw_public_keys(&self) -> bool {
        true
    }
    fn has_certs(&self) -> bool {
        false
    }
}

/// What a peer sees of `client_certificate_type`, read by rustls' own
/// server-side `Acceptor` from the bytes the backend sent.
async fn announced_client_cert_types(
    cfg: rustls::ClientConfig,
) -> Option<Vec<rustls::server::CertificateType>> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut acceptor = rustls::server::Acceptor::default();
        loop {
            acceptor.read_tls(&mut sock).unwrap();
            if let Some(accepted) = acceptor.accept().map_err(|(e, _)| e).unwrap() {
                // The socket goes back with the answer so that it outlives
                // the join: a peer that closed it would end the handshake
                // below with an error before the answer was collected.
                let types = accepted
                    .client_hello()
                    .client_cert_types()
                    .map(<[_]>::to_vec);
                return (types, sock);
            }
        }
    });

    let tls = Rustls::from_config(Arc::new(cfg));
    let tcp = hclient_rt_tokio::Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    // The peer never answers; only the first flight matters, so the
    // handshake is abandoned once the peer has read it.
    let handshake = tls.connect(tcp, TlsRequest::new("localhost", &[]));
    let seen = tokio::task::spawn_blocking(move || peer.join().unwrap());
    tokio::select! {
        biased;
        seen = seen => seen.unwrap().0,
        _ = handshake => panic!("the handshake cannot finish against a peer that never answers"),
    }
}

/// **The recording wrapper forwards `only_raw_public_keys`.** Every config
/// this backend hands out is wrapped, so a wrapper answering the trait's
/// default instead would silently drop a caller's RFC 7250 configuration
/// from the `ClientHello`. The control is an ordinary config, for which
/// nothing is announced — without it the assertion would pass for an
/// observer that reads the extension into every hello.
#[tokio::test]
async fn the_recording_wrapper_keeps_a_raw_public_key_resolver_raw() {
    let mut raw = without_a_certificate();
    raw.client_auth_cert_resolver = Arc::new(RawKeysOnly);
    assert_eq!(
        announced_client_cert_types(raw).await,
        Some(vec![rustls::server::CertificateType::RawPublicKey]),
    );
    assert_eq!(
        announced_client_cert_types(without_a_certificate()).await,
        None
    );
}
