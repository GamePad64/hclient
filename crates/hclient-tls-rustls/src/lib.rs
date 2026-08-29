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
//!
//! # A client certificate, and two routes to one
//!
//! Both plain constructors here say `with_no_client_auth()`, so mTLS is
//! asked for explicitly. There are two ways to ask, and which one is
//! right turns on whether the certificate is a property of the
//! **backend** or of the **request**.
//!
//! [`Rustls::from_config`] takes a `rustls::ClientConfig` the caller
//! built with `.with_client_auth_cert(chain, key)`, and every connection
//! this backend makes presents it. That is the whole of mTLS for a
//! client with one identity, which is most of them.
//!
//! [`Rustls::with_identity`] registers a config under a **name**, and a
//! request carrying [`hclient_core::ClientIdentity`] selects it. That is
//! for a client that holds several — a tenant per certificate, a
//! smartcard beside a software key — where the choice cannot be made at
//! construction because it is not the same for every request.
//!
//! Two things hold for both routes and are worth knowing before assuming
//! otherwise. [`TlsIdentity::presents_client_certs`] asks the **config**
//! rather than remembering a constructor flag, so a `from_config` caller
//! is reported correctly by `Capabilities::client_certs` — and by the
//! HTTP/3 path, which clones this same config. And each config draws its
//! own [`TlsConfigId`], which is part of `hclient-native`'s pool key, so
//! two different certificates cannot share a connection: the isolation
//! is by construction rather than by a check, which matters because the
//! failure mode is presenting one tenant's certificate on another's
//! behalf.
#![forbid(unsafe_code)]

#[cfg(feature = "dangerous-insecure")]
mod insecure;
#[cfg(feature = "quic")]
mod quic;
mod stream;

pub use stream::TlsStream;

use hclient_core::{Error, ErrorKind};
use hclient_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use std::collections::HashMap;
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
    /// Named client identities, each its own config and its own
    /// [`TlsConfigId`].
    ///
    /// **The id is what isolates connections**: it is a component of
    /// `hclient-native`'s pool key, so two labels cannot share a
    /// connection by construction rather than by a check — which matters
    /// because the failure mode is presenting one tenant's certificate on
    /// another's behalf.
    ///
    /// An `Arc<HashMap>` rather than a `Mutex`: identities are registered
    /// at construction and never after, so there is nothing to lock. That
    /// is deliberate — a registry that could change under a live pool
    /// would let a label mean two things at two moments.
    identities: Arc<HashMap<Box<str>, Named>>,
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

/// One named identity: its config and the id that keys its connections.
#[derive(Debug, Clone)]
struct Named {
    config: Arc<rustls::ClientConfig>,
    id: TlsConfigId,
}

impl Rustls {
    /// Registers a client identity under a name of the caller's choosing.
    ///
    /// The `ClientConfig` is theirs to build — `with_client_auth_cert`
    /// for a certificate in hand, or a `ResolvesClientCert` of their own
    /// over a platform store. **This crate takes no position on where the
    /// certificate comes from**, because no representation of one is
    /// shared by Windows, macOS, PKCS#11 and Android; see
    /// `docs/mtls-design.md`.
    ///
    /// A fresh [`TlsConfigId`] is drawn per identity, which is what keeps
    /// two of them off one connection.
    #[must_use]
    pub fn with_identity(
        mut self,
        name: impl Into<Box<str>>,
        cfg: Arc<rustls::ClientConfig>,
    ) -> Self {
        let mut map = HashMap::clone(&self.identities);
        map.insert(
            name.into(),
            Named {
                config: cfg,
                id: TlsConfigId::new_unique(),
            },
        );
        self.identities = Arc::new(map);
        self
    }

    pub fn from_config(cfg: Arc<rustls::ClientConfig>) -> Self {
        Self {
            identities: Arc::new(HashMap::new()),
            base: cfg,
            by_alpn: Arc::new(Mutex::new(HashMap::new())),
            config_id: TlsConfigId::new_unique(),
            #[cfg(feature = "quic")]
            quic: Arc::new(OnceLock::new()),
        }
    }

    /// **Accepts any certificate from any server** — `curl -k`, `curl
    /// --insecure`, and behind this crate's `dangerous-insecure` feature.
    ///
    /// # What it is for, and what it is not for
    ///
    /// Reaching a host whose certificate this machine cannot verify and
    /// whose identity the caller is establishing some other way: a
    /// development server with a self-signed certificate, a device on a
    /// local network with a certificate for a name that is not in DNS, a
    /// staging environment behind an internal CA nobody installed. In
    /// every one of those the caller already knows what they are talking
    /// to.
    ///
    /// It is **not** a way to make a stubborn certificate error go away in
    /// production. A connection made this way is confidential and
    /// integrity-protected against somebody watching, and offers nothing
    /// at all against somebody *interposing*: an active attacker presents
    /// their own certificate and is believed, which is precisely the
    /// attack verification exists to stop.
    ///
    /// # What stays on
    ///
    /// Signature verification. The handshake still proves the peer holds
    /// the key for the certificate it sent; what is gone is the question
    /// of whose certificate that is — the chain, the expiry and the name.
    /// See `insecure::AcceptAnyServer` for why that split is the one to
    /// make.
    ///
    /// # It cannot share a connection with a verifying client
    ///
    /// This is an ordinary constructor, so it draws a fresh
    /// [`TlsConfigId`] like every other, and that identity is part of
    /// `hclient-native`'s pool key. A `Client` built this way therefore
    /// cannot be handed a pooled connection that was established under
    /// verification, or the reverse — which matters because the reverse is
    /// the dangerous direction and is the one a shared pool would
    /// otherwise allow.
    #[cfg(feature = "dangerous-insecure")]
    pub fn danger_accept_invalid_certs() -> Self {
        // The provider the config will use, so the signature checks that
        // remain are the ones this build actually ships.
        let provider = rustls::crypto::CryptoProvider::get_default().map_or_else(
            || std::sync::Arc::new(rustls::crypto::ring::default_provider()),
            std::sync::Arc::clone,
        );
        let verifier = std::sync::Arc::new(insecure::AcceptAnyServer::new(provider.clone()));
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the default provider supports the default protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Self::from_config(std::sync::Arc::new(cfg))
    }

    /// A fixed set of roots compiled into the binary, from `webpki-roots`.
    ///
    /// **Behind the `webpki-roots` feature**, and there is a stand-in
    /// constructor of the same name in a build without it, so that calling
    /// this says which feature is missing rather than which name is.
    ///
    /// That stand-in — and the `WebpkiRootsFeature` trait carrying its
    /// message — exists only when the feature is **off**, so it is named
    /// here rather than linked: this page is rendered with the feature on,
    /// where there is no such item to link to.
    #[cfg(feature = "webpki-roots")]
    pub fn with_webpki_roots() -> Self {
        let roots: rustls::RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
        Self::from_config(Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        ))
    }

    /// The platform's own trust store, through `rustls-platform-verifier`.
    ///
    /// There is no `rustls_platform_verifier::tls_config()` free function
    /// to reach for, which is the obvious guess and is worth writing down
    /// once: 0.7 exports two extension traits and no such function —
    /// `BuilderVerifierExt` on `ConfigBuilder<ClientConfig,
    /// WantsVerifier>` and `ConfigVerifierExt` on `ClientConfig` itself.
    /// The call is the extension method
    /// `ClientConfig::with_platform_verifier()`.
    #[cfg(feature = "platform-verifier")]
    pub fn with_platform_verifier() -> Result<Self, Error> {
        use rustls_platform_verifier::ConfigVerifierExt;
        let cfg = rustls::ClientConfig::with_platform_verifier()
            .map_err(|e| Error::new(ErrorKind::Tls, e))?;
        Ok(Self::from_config(Arc::new(cfg)))
    }

    /// The config registered under a name.
    pub(crate) fn config_for_identity(&self, name: &str) -> Option<Arc<rustls::ClientConfig>> {
        self.identities.get(name).map(|n| n.config.clone())
    }

    fn config_for(&self, alpn: &[&[u8]], identity: Option<&str>) -> Arc<rustls::ClientConfig> {
        // **A named identity is not cached per ALPN**, and that is a
        // decision rather than an omission: the ALPN cache exists because
        // cloning a `ClientConfig` is rustls' most expensive operation on
        // a path taken for every connection, where a named identity is
        // taken by a caller who asked for one. Caching the cross product
        // would multiply the entries by the identities to save a clone on
        // a rarer path.
        //
        // A name this backend has not got cannot arrive here: the
        // transport resolved it through `config_id_for` before opening a
        // socket, and refused there.
        if let Some(name) = identity {
            let Some(named) = self.config_for_identity(name) else {
                return self.base.clone();
            };
            if alpn.is_empty() {
                return named;
            }
            let mut cfg = (*named).clone();
            cfg.alpn_protocols = alpn.iter().map(|a| a.to_vec()).collect();
            return Arc::new(cfg);
        }
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
/// was first assumed. That doesn't make the normalization
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
            || self
                .identities
                .values()
                .any(|n| n.config.client_auth_cert_resolver.has_certs())
    }

    fn config_id_for(&self, name: &str) -> Option<TlsConfigId> {
        self.identities.get(name).map(|n| n.id)
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

    /// A named type rather than an `async fn`'s opaque one, so that
    /// `Handshaking<S>: Send` follows from `S: Send` instead of being
    /// fixed here — the trait's own doc says why that is the only true
    /// answer of the three available.
    type Handshake<'a, S>
        = Handshaking<S>
    where
        Self: 'a,
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'a;

    /// Everything that can fail without awaiting happens **here**, not in
    /// the future: the ECH refusal, the server name and the
    /// `ClientConnection`. That is not a rearrangement for its own sake —
    /// it is what leaves the future with one job, the poll loop, which is
    /// what makes writing it as a type rather than an `async fn` a
    /// dozen lines instead of a state machine.
    fn connect<'a, S>(&'a self, io: S, req: TlsRequest<'a>) -> Self::Handshake<'a, S>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'a,
    {
        if req.ech.is_some() {
            return Handshaking::failed(ech_refused());
        }
        let name = match rustls_pki_types::ServerName::try_from(req.server_name) {
            Ok(n) => n.to_owned(),
            Err(e) => return Handshaking::failed(Error::new(ErrorKind::Tls, e)),
        };
        let conn =
            match rustls::ClientConnection::new(self.config_for(req.alpn, req.identity), name) {
                Ok(c) => c,
                Err(e) => return Handshaking::failed(Error::new(ErrorKind::Tls, e)),
            };
        Handshaking::driving(TlsStream::new(io, conn))
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

/// [`Rustls`]'s handshake, as a type.
///
/// # Why this is not an `async fn`
///
/// `TlsConnect::Handshake` is an associated type so that a consumer can
/// **name** it — `hclient-native`, so that it can prove its own future
/// `Send`, so that `hclient::Client`'s can be. An `async fn` body has no
/// name, so the alternative would be a box, and a box has to decide
/// `Send` once for every `S`. Here the honest answer varies with `S`: a
/// handshake over a socket that can cross a thread can, and one over
/// `hclient-rt-embassy`'s cannot. A concrete type **derives** that
/// instead of choosing it — `Handshaking<S>` is `Send` exactly when `S`
/// is, by ordinary auto-trait inference and with nothing declared.
///
/// # What it holds
///
/// One of two states, because everything fallible that does not await
/// already happened in [`Rustls::connect`]: either an error waiting to be
/// returned on the first poll, or the stream whose handshake is being
/// driven. There is no third state and no `Done`: `poll` is not called
/// again after it returns `Ready`, which is the `Future` contract, and a
/// state to enforce it would be a state nothing can reach.
#[derive(Debug)]
pub struct Handshaking<S> {
    state: Handshaking2<S>,
}

/// The two states, and the size difference between them is deliberate.
///
/// `Driving` is ~1056 bytes against `Failed`'s 24, because it holds a
/// `rustls::ClientConnection`. Clippy asks for the large one to be boxed
/// and that would be a regression rather than a fix: the `async fn` this
/// replaced held the identical `TlsStream<S>` in its own opaque state, so
/// the size is unchanged and boxing would add one allocation per
/// handshake that the previous code did not make. What changed is only
/// that the type has a name, which is why the lint can see it at all.
#[allow(
    clippy::large_enum_variant,
    reason = "measured: 1056 vs 24 bytes, and the same size the async fn's opaque state already was — boxing would add an allocation rather than remove one"
)]
#[derive(Debug)]
enum Handshaking2<S> {
    Failed(Option<Error>),
    Driving(TlsStream<S>),
}

impl<S> Handshaking<S> {
    fn failed(e: Error) -> Self {
        Self {
            state: Handshaking2::Failed(Some(e)),
        }
    }

    fn driving(s: TlsStream<S>) -> Self {
        Self {
            state: Handshaking2::Driving(s),
        }
    }
}

impl<S> Future for Handshaking<S>
where
    S: hyper::rt::Read + hyper::rt::Write + Unpin,
{
    type Output = Result<(TlsStream<S>, TlsInfo), Error>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        // `TlsStream<S>` is `Unpin` when `S` is, and `S: Unpin` is on this
        // impl — so a plain `get_mut` rather than a projection, and no
        // `unsafe` (this crate forbids it).
        let me = self.get_mut();
        let stream = match &mut me.state {
            Handshaking2::Failed(e) => {
                return Poll::Ready(Err(e
                    .take()
                    .expect("a Future is not polled after it returns Ready")));
            }
            Handshaking2::Driving(s) => s,
        };

        // Drive the handshake to completion before handing the stream up.
        loop {
            let (io, conn) = stream.parts_mut();
            std::task::ready!(stream::flush_outgoing(io, conn, cx))
                .map_err(|e| Error::new(ErrorKind::Tls, e))?;
            if !conn.is_handshaking() {
                break;
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

        let c = stream.conn();
        // `early_data_accepted` is left unset: this backend never offers
        // early data, so there is nothing to report as accepted or
        // refused — see the field's own doc on why the two are different
        // answers.
        let info = TlsInfo::new()
            .alpn(c.alpn_protocol().map(|a| a.to_vec()))
            .peer_certificates(
                c.peer_certificates()
                    .map(|cs| cs.iter().map(|d| d.as_ref().to_vec()).collect()),
            )
            .protocol_version(c.protocol_version().and_then(normalize_protocol_version))
            .cipher_suite(
                c.negotiated_cipher_suite()
                    .and_then(|s| normalize_cipher_suite(s.suite())),
            );
        let Handshaking2::Driving(stream) =
            std::mem::replace(&mut me.state, Handshaking2::Failed(None))
        else {
            unreachable!("the match above already established this arm")
        };
        Poll::Ready(Ok((stream, info)))
    }
}

/// The stand-in for [`Rustls::with_webpki_roots`] in a build without the
/// `webpki-roots` feature.
///
/// ACT, the first consumer to port onto this workspace, reported that
/// calling the constructor without the feature produced an error whose
/// *suggestion* pointed at `Rustls::from_config` — a correct name, and a
/// far bigger detour than "turn the feature on". A missing method has no
/// way to say why it is missing, so rustc offers the nearest name it can
/// see, and the nearest name here is the general-purpose escape hatch.
///
/// This is [`crate::Rustls::with_webpki_roots`] existing anyway, with an
/// unsatisfiable bound carrying the message. Same shape as `hclient`'s
/// `Client::new()` under a build with no `default-transport`, and for the
/// same reason: **a name that is absent can only be guessed at, where a
/// name that is present can explain itself.**
///
/// **The lifetime parameter is load-bearing and must not be tidied away.**
/// A `where Self: Trait` predicate that mentions no generic parameter is
/// checked where the method is *defined*, so the plain form makes this
/// crate itself fail to compile rather than the caller. Naming a lifetime
/// the caller never writes defers the check to the call site, which is the
/// only place the message is any use.
#[cfg(not(feature = "webpki-roots"))]
#[diagnostic::on_unimplemented(
    message = "`Rustls::with_webpki_roots()` needs the `webpki-roots` feature of `hclient-tls-rustls`",
    label = "this build of `hclient-tls-rustls` compiled in no root certificates",
    note = "add it: `hclient-tls-rustls = {{ version = \"..\", features = [\"webpki-roots\"] }}`",
    note = "or use the platform's own trust store, which needs no feature and is what `Client::new()` uses: `Rustls::with_platform_verifier()`",
    note = "`Rustls::from_config(..)` also works and is a much larger step — it asks you to build the whole `rustls::ClientConfig`"
)]
pub trait WebpkiRootsFeature<'g> {}

#[cfg(not(feature = "webpki-roots"))]
impl Rustls {
    /// Not available in this build — see [`WebpkiRootsFeature`].
    pub fn with_webpki_roots<'g>() -> Self
    where
        Self: WebpkiRootsFeature<'g>,
    {
        unreachable!(
            "`Rustls::with_webpki_roots` has an unsatisfiable bound; no call to it compiles"
        )
    }
}
