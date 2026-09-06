//! The session-resumption seam, on both paths.
//!
//! `ClientSessionStore` is rustls' trait and its `ClientSessionMemoryCache`
//! is the default this crate keeps. What a caller installs must be what
//! is actually consulted, because a setter that reaches nothing compiles
//! exactly as well as one that works.
//!
//! **The two halves are asserted where each is reachable**, which is the
//! honest split rather than a tidy one: the TCP store is consulted by
//! `rustls::ClientConnection`, reached through a private `config_for`, so
//! that assertion is a unit test in `lib.rs`; the QUIC store is fetched
//! by `quic_config_for`, so `quic.rs` pins that the setter reaches the
//! field it reads. What is left here is the property neither of those
//! covers — that changing where sessions live changes the configuration
//! identity, which is a component of the connection pool's key.
//!
//! **Nothing persists anything.** The seam exists so that the decision to
//! can be a caller's; neither Chromium nor Firefox makes it, and neither
//! does this crate.
#![cfg(not(target_family = "wasm"))]

use hclient_tls::TlsIdentity;
use hclient_tls_rustls::Rustls;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A store that counts what rustls asks it, and holds nothing.
///
/// The counter lives outside the connector, so a connector that quietly
/// used a store of its own would leave it at zero.
#[derive(Debug, Default)]
struct Counting {
    asked: Arc<AtomicUsize>,
}

impl rustls::client::ClientSessionStore for Counting {
    fn set_kx_hint(&self, _: rustls::pki_types::ServerName<'static>, _: rustls::NamedGroup) {}
    fn kx_hint(&self, _: &rustls::pki_types::ServerName<'_>) -> Option<rustls::NamedGroup> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        None
    }
    fn set_tls12_session(
        &self,
        _: rustls::pki_types::ServerName<'static>,
        _: rustls::client::Tls12ClientSessionValue,
    ) {
    }
    fn tls12_session(
        &self,
        _: &rustls::pki_types::ServerName<'_>,
    ) -> Option<rustls::client::Tls12ClientSessionValue> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        None
    }
    fn remove_tls12_session(&self, _: &rustls::pki_types::ServerName<'_>) {}
    fn insert_tls13_ticket(
        &self,
        _: rustls::pki_types::ServerName<'static>,
        _: rustls::client::Tls13ClientSessionValue,
    ) {
    }
    fn take_tls13_ticket(
        &self,
        _: &rustls::pki_types::ServerName<'_>,
    ) -> Option<rustls::client::Tls13ClientSessionValue> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        None
    }
}

fn connector() -> Rustls {
    Rustls::with_platform_verifier().expect("a platform verifier")
}

/// Changing where sessions are kept changes *which client may resume
/// whose*, and that is a component of the pool key — so the identity has
/// to move with it, or two connectors differing only in their store would
/// share pooled connections.
#[test]
fn installing_a_store_redraws_the_configuration_identity() {
    let base = connector();
    let before = base.config_id();

    let with = connector().with_session_store(Arc::new(Counting::default()));
    assert_ne!(
        before,
        with.config_id(),
        "a connector with a different session store is a different configuration"
    );
}
