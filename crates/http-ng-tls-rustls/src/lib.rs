//! TLS backend on rustls.
//!
//! **rustls does not appear in `http-ng`'s public API** — otherwise 0.24's
//! release would become our own breaking release. 0.24 is expected to
//! bring: the `std` feature removed, providers split out into
//! `rustls-ring`/`rustls-aws-lc-rs`, edition 2024. One
//! rewritten crate is budgeted for.
//!
//! `forbid`, not `deny` (see `http-ng-rt`, Task 2 of vertical 2, fix round
//! 1): `deny(unsafe_code)` could be overridden with a local
//! `#[allow(unsafe_code)]` next to the `unsafe` block itself — the
//! compiler would stay silent; `forbid` cannot be overridden from inside
//! the crate at all (`E0453`).
#![forbid(unsafe_code)]

mod stream;

pub use stream::TlsStream;

use http_ng_core::{Error, ErrorKind};
use http_ng_tls::{TlsConnect, TlsInfo, TlsRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct Rustls {
    base: Arc<rustls::ClientConfig>,
    /// ALPN is set on connect, and `ClientConfig` stores it internally —
    /// so the config is cached per ALPN set. Without the cache, every
    /// request would rebuild the config from scratch, and that's the
    /// most expensive operation in rustls.
    by_alpn: Mutex<HashMap<Vec<Vec<u8>>, Arc<rustls::ClientConfig>>>,
}

impl Rustls {
    pub fn from_config(cfg: Arc<rustls::ClientConfig>) -> Self {
        Self {
            base: cfg,
            by_alpn: Mutex::new(HashMap::new()),
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

impl TlsConnect for Rustls {
    type Stream<S>
        = TlsStream<S>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    async fn connect<S>(&self, io: S, req: TlsRequest<'_>) -> Result<(TlsStream<S>, TlsInfo), Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        let name = rustls_pki_types::ServerName::try_from(req.server_name)
            .map_err(|e| Error::new(ErrorKind::Tls, e))?
            .to_owned();
        let conn = rustls::ClientConnection::new(self.config_for(req.alpn), name)
            .map_err(|e| Error::new(ErrorKind::Tls, e))?;
        let mut stream = TlsStream::new(io, conn);

        // Drive the handshake to completion before handing the stream back up.
        std::future::poll_fn(|cx| {
            let (io, conn) = stream.parts_mut();
            loop {
                std::task::ready!(stream::flush_outgoing(io, conn, cx))
                    .map_err(|e| Error::new(ErrorKind::Tls, e))?;
                if !conn.is_handshaking() {
                    return std::task::Poll::Ready(Ok::<(), Error>(()));
                }
                let more = std::task::ready!(stream::pump_incoming(io, conn, cx))
                    .map_err(|e| Error::new(ErrorKind::Tls, e))?;
                if !more {
                    return std::task::Poll::Ready(Err(Error::new(
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
