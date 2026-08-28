//! Certificate verification, switched off on purpose — `curl -k`, and it
//! is behind a feature so that a build which must not contain it does not.

use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use std::sync::Arc;

/// Accepts any certificate chain from any server, for any name.
///
/// # What it does not switch off
///
/// **Signature verification stays on.** `verify_tls12_signature` and
/// `verify_tls13_signature` below are rustls' own, delegated to the
/// provider's algorithms — so the handshake still proves the peer holds
/// the private key for the certificate it presented. What is gone is the
/// question *whose certificate is that* — the chain to a trust anchor, the
/// expiry, and the name.
///
/// That split is the same one `curl -k` makes, and it is worth stating
/// because it decides what the mode is good for: a connection made this
/// way is still confidential and still integrity-protected against a
/// passive observer, and offers **nothing at all** against an active one,
/// who simply presents their own certificate and is believed.
#[derive(Debug)]
pub(crate) struct AcceptAnyServer(Arc<CryptoProvider>);

impl AcceptAnyServer {
    pub(crate) fn new(provider: Arc<CryptoProvider>) -> Self {
        Self(provider)
    }
}

impl ServerCertVerifier for AcceptAnyServer {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
