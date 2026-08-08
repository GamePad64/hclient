//! `TlsConnect::config_id` for the rustls backend.
//!
//! # Why this test exists, given that no pool can reach it today
//!
//! `http_ng_native`'s pool keys on this value, but within any one pool it is
//! a constant: a pool belongs to one `Native`, and a `Native` owns one
//! `TlsConnect`. So the component cannot be exercised end to end, and the
//! obvious "simplification" six months from now is to drop it, or to
//! implement it with a `TypeId` — which compiles, reads plausibly, and is
//! wrong in the one direction that matters: two `Rustls` values built from
//! different root stores are the same type, so every client in the process
//! would claim to share a trust configuration with every other.
//!
//! That is what these two tests fail on. They are not about rustls; they are
//! about the property `TlsConfigId` exists to carry, checked at the level
//! where it is still observable.
use http_ng_tls::TlsConnect;
use http_ng_tls_rustls::Rustls;
use std::sync::Arc;

fn config_with(roots: rustls::RootCertStore) -> Arc<rustls::ClientConfig> {
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// A certificate authority nobody else has, so that two stores built from
/// two of these genuinely differ in what they will accept.
fn a_root() -> rustls_pki_types::CertificateDer<'static> {
    // The same call every other test in this crate uses to make a
    // certificate; what matters here is only that two calls produce two
    // different ones, not that either is a realistic CA.
    let cert = rcgen::generate_simple_self_signed(vec!["test-ca.invalid".into()])
        .expect("self-signed certificate");
    rustls_pki_types::CertificateDer::from(cert.cert.der().to_vec())
}

fn store_with(root: rustls_pki_types::CertificateDer<'static>) -> rustls::RootCertStore {
    let mut store = rustls::RootCertStore::empty();
    store.add(root).expect("a self-signed CA is a valid root");
    store
}

/// The property a `TypeId` implementation would break: same type, different
/// trust decisions, and therefore different identities.
#[test]
fn two_configurations_with_different_roots_have_different_identities() {
    let a = Rustls::from_config(config_with(store_with(a_root())));
    let b = Rustls::from_config(config_with(store_with(a_root())));

    assert_ne!(
        a.config_id(),
        b.config_id(),
        "two connectors that trust different certificate authorities must not \
         be interchangeable, and `TypeId` cannot tell them apart"
    );
}

/// The other half, and it is not free: an identity drawn freshly on every
/// call would satisfy the test above and pool absolutely nothing, silently.
/// `TlsConfigId::new_unique`'s own doc names that mistake; this is what
/// catches it.
#[test]
fn one_connector_reports_the_same_identity_every_time() {
    let tls = Rustls::from_config(config_with(store_with(a_root())));
    let first = tls.config_id();
    for _ in 0..3 {
        assert_eq!(
            tls.config_id(),
            first,
            "a connector's identity is fixed for its lifetime — drawn once in \
             the constructor, not per call"
        );
    }
}
