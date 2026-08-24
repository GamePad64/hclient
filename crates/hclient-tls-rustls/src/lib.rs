//! TLS backend on rustls.
//!
//! **rustls does not appear in `hclient`'s public API** — otherwise 0.24's
//! release would become our own breaking release. 0.24 is expected to
//! bring: the `std` feature removed, providers split out into
//! `rustls-ring`/`rustls-aws-lc-rs`, edition 2024. One
//! rewritten crate is budgeted for.
//!
//! `forbid`, not `deny`: `deny(unsafe_code)` could be overridden with a
//! local
//! `#[allow(unsafe_code)]` next to the `unsafe` block itself — the
//! compiler would stay silent; `forbid` cannot be overridden from inside
//! the crate at all (`E0453`).
#![forbid(unsafe_code)]

#[cfg(feature = "quic")]
mod quic;
mod stream;

pub use stream::TlsStream;

use hclient_core::{Error, ErrorKind};
use hclient_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use std::collections::HashMap;
use std::future::poll_fn;
#[cfg(feature = "quic")]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::task::Poll;

/// The per-ALPN config cache, named because the type outgrew the line it
/// sat on when it moved behind an `Arc` — and `clippy::type_complexity`
/// said so before a reader had to.
///
/// The key is the ALPN list exactly as it goes on the wire: a `Vec` of
/// protocol names, each a `Vec<u8>`, ordered, because rustls stores the
/// list inside the `ClientConfig` and order is preference.
type AlpnCache = Arc<Mutex<HashMap<Vec<Vec<u8>>, Arc<rustls::ClientConfig>>>>;

/// **`Clone` shares everything, and that is a correctness property rather
/// than an optimisation.** `config_id` below is copied by value, so two
/// clones claim the same TLS identity — and a pool treats connections
/// under one identity as interchangeable. A clone that forked `by_alpn`
/// would merely waste the most expensive operation rustls has; one that
/// forked the QUIC ticket store would let a resumption ticket earned by
/// one value be invisible to the other while both still claim to be the
/// same configuration. So both live behind an `Arc`.
///
/// What a caller gets from this is one read of the OS trust store where
/// two stacks need the same connector — `Selecting` owns a `Native` and an
/// `H3` and its `T` is one type, so without `Clone` the only way to build
/// the pair is to construct the backend twice.
#[derive(Debug, Clone)]
pub struct Rustls {
    base: Arc<rustls::ClientConfig>,
    /// ALPN is set on connect, and `ClientConfig` stores it internally —
    /// so the config is cached per ALPN set. Without the cache, every
    /// request would rebuild the config from scratch, and that's the
    /// most expensive operation in rustls.
    by_alpn: AlpnCache,
    /// See [`TlsConfigId`]. Drawn once here, in the constructor, and never
    /// recomputed: everything that decides whether a server is acceptable
    /// lives in `base`, and `base` is immutable for this value's lifetime.
    ///
    /// The per-ALPN clones in `by_alpn` deliberately do NOT get identities
    /// of their own. They differ from `base` in `alpn_protocols` alone,
    /// which is not a trust decision — and the protocol spoken on a
    /// connection is a separate component of the pool key anyway, so the
    /// distinction is already carried, once, where it belongs.
    config_id: TlsConfigId,
    /// The QUIC path's config cache and ticket store, built on first use.
    /// See `crate::quic`'s module doc for why they are separate from
    /// `by_alpn` and from `base`'s own resumption store.
    #[cfg(feature = "quic")]
    quic: Arc<OnceLock<quic::QuicState>>,
}

impl Rustls {
    pub fn from_config(cfg: Arc<rustls::ClientConfig>) -> Self {
        Self {
            base: cfg,
            by_alpn: Arc::new(Mutex::new(HashMap::new())),
            config_id: TlsConfigId::new_unique(),
            #[cfg(feature = "quic")]
            quic: Arc::new(OnceLock::new()),
        }
    }

    #[cfg(feature = "webpki-roots")]
    pub fn with_webpki_roots() -> Self {
        let roots: rustls::RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
        Self::from_config(Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        ))
    }

    /// This task's brief suggested `rustls_platform_verifier::tls_config()`
    /// — no such free function exists in `rustls-platform-verifier` 0.7
    /// (checked against the crate's source: `src/lib.rs` exports only two
    /// extension traits, `BuilderVerifierExt` on
    /// `ConfigBuilder<ClientConfig, WantsVerifier>` and
    /// `ConfigVerifierExt` on `ClientConfig` itself). The correct call is
    /// the extension method `ClientConfig::with_platform_verifier()` from
    /// `ConfigVerifierExt`.
    #[cfg(feature = "platform-verifier")]
    pub fn with_platform_verifier() -> Result<Self, Error> {
        use rustls_platform_verifier::ConfigVerifierExt;
        let cfg = rustls::ClientConfig::with_platform_verifier()
            .map_err(|e| Error::new(ErrorKind::Tls, e))?;
        Ok(Self::from_config(Arc::new(cfg)))
    }

    fn config_for(&self, alpn: &[&[u8]]) -> Arc<rustls::ClientConfig> {
        if alpn.is_empty() {
            return self.base.clone();
        }
        let key: Vec<Vec<u8>> = alpn.iter().map(|a| a.to_vec()).collect();
        let mut cache = self.by_alpn.lock().expect("alpn cache poisoned");
        cache
            .entry(key.clone())
            .or_insert_with(|| {
                let mut cfg = (*self.base).clone();
                cfg.alpn_protocols = key;
                Arc::new(cfg)
            })
            .clone()
    }
}

/// The refusal [`Rustls::connect`] makes when a caller asks for ECH — the
/// same answer `hclient-tls-native-tls` gives, and the same one this
/// crate's own QUIC path gives (`crate::quic`), so that no backend in this
/// workspace reads [`TlsRequest::ech`] and drops it.
///
/// # Why refuse, when rustls *has* ECH
///
/// It does: `rustls::client::EchConfig` and `EchMode` are public API, not
/// an unstable one. What is missing is not the protocol but a place to put
/// it, and the three obstacles below were measured against rustls 0.23.43
/// rather than assumed. Each names the check that would refute it.
///
/// 1. **This backend's crypto provider has no HPKE at all.**
///    `EchConfig::new(list, hpke_suites)` takes `&[&'static dyn Hpke]`, and
///    the only implementation of that trait in rustls is
///    `src/crypto/aws_lc_rs/hpke.rs` (`ALL_SUPPORTED_SUITES`);
///    `src/crypto/ring/` contains no `hpke.rs`. This crate builds rustls
///    with `features = ["ring"]`, and `quinn-proto` beside it with
///    `rustls-ring` — the two must name the same provider, or the QUIC
///    config would be built from one the TLS config does not use. So there
///    is no suite to hand `EchConfig::new`, and it cannot succeed.
///    Honouring ECH begins with adding `aws-lc-rs`: a C toolchain in the
///    build, on every target this crate claims.
/// 2. **ECH is not a field on a built `ClientConfig`, and ALPN is.**
///    `ClientConfig::alpn_protocols` is `pub`, which is the whole reason
///    [`Rustls::config_for`] can clone `base` and cache per ALPN set.
///    `ClientConfig::ech_mode` is `pub(super)`
///    (`src/client/client_conn.rs:289`); the only way in is
///    `ConfigBuilder<ClientConfig, WantsVersions>::with_ech`
///    (`src/client/builder.rs:27`), i.e. **before** the verifier is chosen.
///    [`Rustls::from_config`] is handed an already-built
///    `Arc<ClientConfig>` whose `verifier`, `provider` and `versions` are
///    `pub(super)` too, so it cannot be taken apart and rebuilt with an ECH
///    config the way it is cloned with a new ALPN list. The caller's trust
///    decisions would have to be re-declared, not carried over.
/// 3. **`with_ech` pins the connection to TLS 1.3 alone**
///    (`with_protocol_versions(&[&TLS13])`, same file) — a second
///    per-connection property a clone of `base` cannot express, and a third
///    dimension on the config cache next to ALPN.
///
/// Past construction there is protocol work too: a server that rejects ECH
/// answers with retry configs
/// (`PeerIncompatible::ServerRejectedEncryptedClientHello`,
/// `src/client/ech.rs:831`) and the handshake is expected to be retried
/// with them. So the field is not wiring even once a config can be built.
///
/// # Why refusing is not the same as ignoring
///
/// ECH exists to keep the server name off the wire. A connection made
/// without it still succeeds, still validates, and still sends that name in
/// the clear — so the one thing the caller asked for is the one thing
/// silently missing, and nothing in the response says so.
/// `crates/hclient-tls-rustls/tests/ech.rs` measures both halves from the
/// peer's side of a loopback socket: with `ech: Some(_)` not one byte
/// arrives, and with `ech: None` the ClientHello that arrives carries the
/// name in plaintext — the leak this refusal prevents, exhibited by the
/// same observer that asserts its absence.
fn ech_refused() -> Error {
    Error::new(
        ErrorKind::Tls,
        std::io::Error::other(
            "hclient-tls-rustls does not apply an ECH config on the TCP path; \
             the connection would have sent the server name in the clear",
        ),
    )
}

/// Normalizes the protocol version, which rustls names after its enum
/// variant (`TLSv1_3`, underscored), to the registry form
/// `TlsInfo::protocol_version` documents (`"TLSv1.3"`, dotted) — the same
/// one OpenSSL's `SSL_get_version()` uses.
///
/// An explicit `match` over the four TLS-family variants, not
/// `format!("{v:?}").replace('_', ".")`: `rustls::ProtocolVersion` is
/// `#[non_exhaustive]` and carries variants outside the TLS family
/// (`SSLv2`, `SSLv3`, `DTLSv1_0/2/3`) plus an `Unknown(u16)` variant for
/// unrecognized values. `rustls::ClientConnection` in this build
/// (features `std`, `ring`, `tls12`, without `unstable_apis`) never
/// negotiates anything outside TLS 1.0–1.3 — neither SSL nor DTLS is
/// implemented in rustls at all — so the `_ => None` arm here is not
/// observable in practice, not a guess: it's a guard against a future
/// enum extension, not the current case. `None` follows the same
/// principle `TlsInfo::protocol_version`'s doc already establishes: a
/// value with nothing to back it up as one of the four canonical strings
/// honestly stays `None`, rather than becoming an approximate or
/// outright wrong string.
fn normalize_protocol_version(v: rustls::ProtocolVersion) -> Option<String> {
    use rustls::ProtocolVersion::*;
    match v {
        TLSv1_0 => Some("TLSv1.0".to_string()),
        TLSv1_1 => Some("TLSv1.1".to_string()),
        TLSv1_2 => Some("TLSv1.2".to_string()),
        TLSv1_3 => Some("TLSv1.3".to_string()),
        _ => None,
    }
}

/// Normalizes the cipher suite name, which rustls names after its enum
/// variant, to the IANA registry name `TlsInfo::cipher_suite` documents.
///
/// Only TLS 1.3 suites carry a `13` version infix in their variant name
/// (`TLS13_AES_128_GCM_SHA256` and four more constants in
/// `rustls::CipherSuite`) — IANA does not use this infix in the suite
/// name (`TLS_AES_128_GCM_SHA256`), and the implementation must strip it.
/// For TLS 1.2-and-older suites, rustls already names the variant exactly
/// as IANA does (`TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256`) — nothing to
/// strip, the name passes through unchanged.
///
/// `CipherSuite::as_str()`, not `format!("{suite:?}")`: this enum
/// (generated by the `enum_builder!` macro, `rustls/src/msgs/macros.rs`)
/// has a public `as_str(&self) -> Option<&'static str>`, contrary to what
/// this task's brief claimed. That doesn't make the normalization
/// unnecessary — for recognized variants, `as_str()` returns literally
/// the same name as `Debug` (`"TLS13_AES_128_GCM_SHA256"`), with the same
/// infix that still needs stripping — but, unlike `Debug`, it honestly
/// returns `None` for `CipherSuite::Unknown(_)`, rather than a formatted
/// string like `"CipherSuite(0x9999)"` that would then need to be
/// separately recognized and discarded. A suite the crypto provider
/// couldn't name is the same case as
/// `normalize_protocol_version`'s "nothing to back up the canonical
/// form": an honest `None`, not an invented string.
fn normalize_cipher_suite(suite: rustls::CipherSuite) -> Option<String> {
    let raw = suite.as_str()?;
    Some(match raw.strip_prefix("TLS13_") {
        Some(rest) => format!("TLS_{rest}"),
        None => raw.to_string(),
    })
}

impl TlsIdentity for Rustls {
    fn config_id(&self) -> TlsConfigId {
        self.config_id
    }

    /// Asked of the config rather than remembered from a constructor, so
    /// a `from_config` caller who built their own
    /// `with_client_auth_cert(..)` is answered for correctly — which is
    /// the only way to get a client certificate onto this backend, since
    /// the two convenience constructors here both say
    /// `with_no_client_auth()`.
    ///
    /// The QUIC path is covered by the same line: `quic_config_for` clones
    /// this same `base`, so the cert resolver it carries is this one.
    fn presents_client_certs(&self) -> bool {
        self.base.client_auth_cert_resolver.has_certs()
    }
}

impl TlsConnect for Rustls {
    type Stream<S>
        = TlsStream<S>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    /// rustls reports the selection: `connect` below fills `TlsInfo::alpn`
    /// from `ClientConnection::alpn_protocol()`, so `None` from this
    /// backend really does mean "nothing was negotiated". Overriding the
    /// `false` default is what lets a transport offer `h2` at all — see
    /// `TlsConnect::reports_alpn` for why the default is the weak answer.
    fn reports_alpn(&self) -> bool {
        true
    }

    async fn connect<S>(&self, io: S, req: TlsRequest<'_>) -> Result<(TlsStream<S>, TlsInfo), Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        if req.ech.is_some() {
            return Err(ech_refused());
        }
        let name = rustls_pki_types::ServerName::try_from(req.server_name)
            .map_err(|e| Error::new(ErrorKind::Tls, e))?
            .to_owned();
        let conn = rustls::ClientConnection::new(self.config_for(req.alpn), name)
            .map_err(|e| Error::new(ErrorKind::Tls, e))?;
        let mut stream = TlsStream::new(io, conn);

        // Drive the handshake to completion before handing the stream back up.
        poll_fn(|cx| {
            let (io, conn) = stream.parts_mut();
            loop {
                std::task::ready!(stream::flush_outgoing(io, conn, cx))
                    .map_err(|e| Error::new(ErrorKind::Tls, e))?;
                if !conn.is_handshaking() {
                    return Poll::Ready(Ok::<(), Error>(()));
                }
                let more = std::task::ready!(stream::pump_incoming(io, conn, cx))
                    .map_err(|e| Error::new(ErrorKind::Tls, e))?;
                if !more {
                    return Poll::Ready(Err(Error::new(
                        ErrorKind::Tls,
                        std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
                    )));
                }
            }
        })
        .await?;

        let c = stream.conn();
        let info = TlsInfo {
            alpn: c.alpn_protocol().map(|a| a.to_vec()),
            peer_certificates: c
                .peer_certificates()
                .map(|cs| cs.iter().map(|d| d.as_ref().to_vec()).collect()),
            protocol_version: c.protocol_version().and_then(normalize_protocol_version),
            cipher_suite: c
                .negotiated_cipher_suite()
                .and_then(|s| normalize_cipher_suite(s.suite())),
            // Reserved, not implemented — this backend never offers early
            // data, so there is nothing to report as accepted or refused.
            // `None`, not `Some(false)`: see `TlsInfo::early_data_accepted`
            // on why the two are different answers.
            early_data_accepted: None,
        };
        Ok((stream, info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Manually mutation-tested (see the task report): temporarily
    // reverting `normalize_protocol_version`/`normalize_cipher_suite` to
    // `format!("{v:?}")`/`format!("{:?}", s.suite())` turns exactly these
    // two tests red — `TLSv1_3`/`TLS13_AES_128_GCM_SHA256` don't match the
    // expected `TLSv1.3`/`TLS_AES_128_GCM_SHA256`.

    #[test]
    fn protocol_version_is_dotted_not_underscored() {
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::TLSv1_3).as_deref(),
            Some("TLSv1.3"),
            "rustls's Debug prints TLSv1_3 (underscore) — the registry form is dotted"
        );
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::TLSv1_2).as_deref(),
            Some("TLSv1.2")
        );
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::TLSv1_1).as_deref(),
            Some("TLSv1.1")
        );
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::TLSv1_0).as_deref(),
            Some("TLSv1.0")
        );
    }

    #[test]
    fn protocol_version_outside_tls_family_is_none_not_a_guess() {
        // Neither SSL, nor DTLS, nor an unrecognized ordinal — the rustls
        // client never negotiates any of these; the arm guards the
        // `#[non_exhaustive]` enum against a future extension, not a case
        // observed today.
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::SSLv3),
            None
        );
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::Unknown(0xABCD)),
            None
        );
    }

    #[test]
    fn cipher_suite_strips_the_tls13_version_infix() {
        assert_eq!(
            normalize_cipher_suite(rustls::CipherSuite::TLS13_AES_128_GCM_SHA256).as_deref(),
            Some("TLS_AES_128_GCM_SHA256"),
            "rustls's Debug prints TLS13_AES_128_GCM_SHA256 — the IANA registry name has no version infix"
        );
        assert_eq!(
            normalize_cipher_suite(rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256).as_deref(),
            Some("TLS_CHACHA20_POLY1305_SHA256")
        );
    }

    #[test]
    fn cipher_suite_tls12_name_already_matches_iana_unchanged() {
        assert_eq!(
            normalize_cipher_suite(rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256)
                .as_deref(),
            Some("TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256")
        );
    }

    #[test]
    fn cipher_suite_unrecognised_by_the_provider_is_none_not_debug_passthrough() {
        // `CipherSuite::Unknown(_)` is a variant with no registry name at
        // all; `Debug` would print `CipherSuite(0x9999)`, which is
        // neither a valid IANA name nor an honest `None`.
        assert_eq!(
            normalize_cipher_suite(rustls::CipherSuite::Unknown(0x9999)),
            None
        );
    }
}
