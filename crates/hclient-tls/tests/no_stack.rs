//! A QUIC TLS backend can be written naming **no QUIC stack**, and this
//! is where that is asserted rather than described.
//!
//! # Why a test rather than a paragraph
//!
//! For four verticals the seam answered
//! `Arc<dyn quinn_proto::crypto::ClientConfig>` inside a newtype, and the
//! argument for that shape was that the wrapper kept quinn out of the
//! *signature*. It did — and this crate linked quinn anyway, to hold the
//! value: measured at 38 crates against 21, with `chacha20`, `rand_core`
//! and `ring` among the difference, in the crate whose whole purpose is
//! that a backend chooses cryptography and this one does not.
//!
//! `QuicCryptoConfig` is declarative now — an ALPN list, two flags and an
//! identity **label** — and [`QuicTlsConnect::Session`] is opaque, so the
//! crate that drives a stack is the crate that names one. The graph half
//! of that is held by `just graph-tls-seam-carries-no-stack`; the *source*
//! half is here, because a graph guard cannot see a signature.
//!
//! **This was found from outside the workspace**, which is the instrument
//! this project records as different from a test written beside the code:
//! a scratch crate depending on `hclient-tls` and `hclient-core` alone
//! implements the seam and builds, with zero `quinn`, `rustls`, `ring` or
//! `chacha20` in its graph. What is kept here is the shape of that crate,
//! so the property cannot regress unnoticed.

use hclient_core::error::Error;
use hclient_tls::quic::{QuicCryptoConfig, QuicTlsConnect, QuicTlsRequest};
use hclient_tls::{TlsConfigId, TlsIdentity};

/// A backend whose session type is **its own**, mentioning no QUIC
/// implementation. The seam demands nothing of it.
#[derive(Debug, Default)]
struct OwnStack;

/// What this backend would hand its own QUIC stack. Deliberately not a
/// type any real stack defines: the point is that the seam accepts
/// whatever a backend answers.
#[derive(Debug, PartialEq, Eq)]
struct OwnSession {
    alpn: Vec<Vec<u8>>,
    early_data: bool,
    identity: Option<String>,
}

impl TlsIdentity for OwnStack {
    fn config_id(&self) -> TlsConfigId {
        TlsConfigId::new_unique()
    }
}

impl QuicTlsConnect for OwnStack {
    type Session = OwnSession;

    fn quic_client_config(&self, req: QuicTlsRequest<'_>) -> Result<QuicCryptoConfig, Error> {
        Ok(
            QuicCryptoConfig::new(req.alpn.iter().map(|a| a.to_vec()).collect())
                .early_data(req.early_data)
                .identity(req.identity.map(str::to_owned)),
        )
    }

    fn quic_session(&self, config: &QuicCryptoConfig) -> Result<Self::Session, Error> {
        Ok(OwnSession {
            alpn: config.alpn.clone(),
            early_data: config.early_data,
            identity: config.identity.clone(),
        })
    }
}

/// The declaration carries what was decided and nothing that was built
/// from it — which is what lets a backend with no stack produce one.
#[test]
fn the_config_is_data_a_backend_with_no_stack_can_produce() {
    let alpn: &[&[u8]] = &[b"h3", b"h3-29"];
    let cfg = OwnStack
        .quic_client_config(QuicTlsRequest::new(alpn).early_data(true))
        .expect("a declaration needs no crypto provider");

    assert_eq!(cfg.alpn, vec![b"h3".to_vec(), b"h3-29".to_vec()]);
    assert!(cfg.early_data);
    assert_eq!(cfg.identity, None);
    assert_eq!(cfg.ech, None);
}

/// **Only the label travels, never a key** — `docs/mtls-design.md` §3.1's
/// rule, and the reason it is a rule: a key in a smartcard cannot be
/// handed over as bytes, so a seam carrying key material would exclude
/// the deployments a label serves. What the label *means* is the
/// backend's own business, which is what "implementation-defined" is
/// asserting here: this one simply passes it through.
#[test]
fn an_identity_crosses_the_seam_as_a_label_and_nothing_else() {
    let alpn: &[&[u8]] = &[b"h3"];
    let cfg = OwnStack
        .quic_client_config(QuicTlsRequest::new(alpn).identity(Some("tenant-a")))
        .expect("a label is data");

    assert_eq!(cfg.identity.as_deref(), Some("tenant-a"));
}

/// The backend builds its own stack's value from the declaration, and
/// the seam never sees inside it.
#[test]
fn the_session_is_the_backends_own_type() {
    let alpn: &[&[u8]] = &[b"h3"];
    let cfg = OwnStack
        .quic_client_config(QuicTlsRequest::new(alpn).identity(Some("tenant-a")))
        .expect("declaration");
    let session = OwnStack.quic_session(&cfg).expect("own session");

    assert_eq!(
        session,
        OwnSession {
            alpn: vec![b"h3".to_vec()],
            early_data: false,
            identity: Some("tenant-a".to_owned()),
        }
    );
}

/// `QuicCryptoConfig` is `#[non_exhaustive]`, so a field added later is
/// not a breaking change for a backend — which is the input half of this
/// workspace's three-answer rule: a type the library *hands to* an
/// implementor, read and never built with a literal.
#[test]
fn the_config_cannot_be_built_with_a_literal_from_outside() {
    // A compile-time property, asserted by construction: every field is
    // set through the builder above, and `QuicCryptoConfig { .. }` from
    // out here is `E0639`. What is checked at run time is that the
    // builder reaches every field, which the tests above do.
    let cfg = QuicCryptoConfig::new(vec![b"h3".to_vec()])
        .early_data(true)
        .ech(Some(vec![1, 2, 3]))
        .identity(Some("label".to_owned()));
    assert!(cfg.early_data);
    assert_eq!(cfg.ech.as_deref(), Some(&[1u8, 2, 3][..]));
    assert_eq!(cfg.identity.as_deref(), Some("label"));
}
