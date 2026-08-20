//! TLS through the platform's own stack — SChannel on Windows,
//! Security.framework on Apple targets, OpenSSL elsewhere — behind the same
//! [`TlsConnect`] seam `hclient-tls-rustls` implements.
//!
//! Why both exist: rustls is the default because it is memory-safe and
//! reproducible across platforms, but an organisation whose trust decisions
//! live in the OS store — enterprise roots pushed by policy, smartcard
//! client certs, a FIPS-validated provider — needs the platform stack, and
//! that is a deployment fact no library can argue with.
//!
//! **The limitation to know before choosing this backend: `TlsInfo::alpn`
//! is always `None` here.** ALPN is still SENT — the server sees the offer
//! — but the negotiated answer is unreadable, so a caller cannot learn from
//! this backend whether h2 was agreed. ALPN-driven protocol selection does
//! not work over it; use `hclient-tls-rustls` where the negotiated protocol
//! matters. This is the wrapper's doing rather than the platform's: the
//! reason is at [`NativeTls::connect`], on the field itself.
//!
//! **What else it cannot report, and why `TlsInfo`'s fields are all
//! `Option`.** No protocol version and no cipher suite — `native-tls`
//! exposes neither — and only the LEAF certificate rather than the chain,
//! returned as a one-element `Vec` rather than `None`, because there IS a
//! certificate and the chain is what is missing. `None` throughout means
//! "this backend cannot tell you", never "there was none".
//! `hclient-tls`'s own doc comment anticipated exactly this backend when it
//! made every field optional.
#![forbid(unsafe_code)]

mod hyper_io;

use hclient_core::{Error, ErrorKind};
use hclient_rt::FuturesIo;
use hclient_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use hyper_io::HyperIo;

/// The platform TLS backend.
///
/// Holds only what can be re-applied per connection. `async-native-tls`'s
/// `TlsConnector` is a consuming builder — every setter takes `self` by
/// value — so there is no built connector to store and share. That turns
/// out to suit the seam rather than fight it: `TlsRequest::alpn` is
/// per-connection by design (version pinning and h2-prior-knowledge need
/// different lists against the same origin), so a connector has to be built
/// per `connect` regardless.
#[derive(Clone)]
pub struct NativeTls {
    identity: Option<native_tls::Identity>,
    roots: Vec<native_tls::Certificate>,
    /// See [`TlsConfigId`]. Redrawn by every builder method below that
    /// changes a trust decision, which is what keeps it honest under
    /// `Clone`: cloning copies a configuration, so the copy is genuinely
    /// interchangeable with the original and shares its identity — but the
    /// moment the copy is given a different client certificate or an extra
    /// root, it stops being interchangeable and stops sharing.
    config_id: TlsConfigId,
}

/// Hand-written, not derived: `TlsConfigId` has no `Default` and must not
/// get one. A default identity would be a constant, and a constant would
/// make every separately built `NativeTls` in the process claim to be the
/// same trust configuration — exactly the confusion the type exists to
/// prevent.
impl Default for NativeTls {
    fn default() -> Self {
        Self {
            identity: None,
            roots: Vec::new(),
            config_id: TlsConfigId::new_unique(),
        }
    }
}

/// Hand-written because neither `native_tls::Identity` nor
/// `native_tls::Certificate` implements `Debug`, and both are secrets or
/// near-secrets: printing their contents would be worse than useless.
impl std::fmt::Debug for NativeTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeTls")
            .field("identity", &self.identity.as_ref().map(|_| "<set>"))
            .field("extra_roots", &self.roots.len())
            .finish()
    }
}

impl NativeTls {
    /// The platform defaults: the OS trust store, the OS's own protocol and
    /// cipher policy.
    pub fn new() -> Self {
        Self::default()
    }

    /// A client certificate, for mutual TLS.
    pub fn identity(mut self, identity: native_tls::Identity) -> Self {
        self.identity = Some(identity);
        self.config_id = TlsConfigId::new_unique();
        self
    }

    /// An extra trust root, *in addition to* the platform store — not
    /// instead of it. `native-tls` offers no way to replace the store, and
    /// this method does not pretend otherwise.
    pub fn add_root_certificate(mut self, cert: native_tls::Certificate) -> Self {
        self.roots.push(cert);
        self.config_id = TlsConfigId::new_unique();
        self
    }
}

impl TlsIdentity for NativeTls {
    fn config_id(&self) -> TlsConfigId {
        self.config_id
    }

    /// The one thing this backend reports *more* of than
    /// `hclient-tls-rustls` does by default. Its module doc is a list of
    /// what it cannot say back — the negotiated ALPN above all — and this
    /// is the other direction: `identity()` is the whole point of reaching
    /// for the platform's stack, since a smartcard or an OS-held key is
    /// exactly what a caller cannot hand to rustls as bytes.
    fn presents_client_certs(&self) -> bool {
        self.identity.is_some()
    }
}

impl TlsConnect for NativeTls {
    type Stream<S>
        = FuturesIo<async_native_tls::TlsStream<HyperIo<S>>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    async fn connect<S>(
        &self,
        io: S,
        req: TlsRequest<'_>,
    ) -> Result<(Self::Stream<S>, TlsInfo), Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        if req.ech.is_some() {
            // Refused, not ignored. ECH (RFC 9849) requires the TLS stack to
            // encrypt the ClientHello against a config from an HTTPS/SVCB
            // record; no platform stack behind `native-tls` exposes that
            // knob. Connecting anyway would leak the SNI the caller asked to
            // protect — the one case where best-effort is worse than an
            // error.
            return Err(Error::new(
                ErrorKind::Tls,
                std::io::Error::other(
                    "native-tls cannot perform ECH: no platform stack exposes ClientHello encryption",
                ),
            ));
        }

        let mut connector = async_native_tls::TlsConnector::new();
        if let Some(id) = self.identity.clone() {
            connector = connector.identity(id);
        }
        for root in self.roots.iter().cloned() {
            connector = connector.add_root_certificate(root);
        }
        if !req.alpn.is_empty() {
            let protocols: Vec<&str> = req
                .alpn
                .iter()
                .map(|p| std::str::from_utf8(p).map_err(|e| Error::new(ErrorKind::Tls, e)))
                .collect::<Result<_, _>>()?;
            connector = connector.request_alpns(&protocols);
        }

        let stream = connector
            .connect(req.server_name, HyperIo::new(io))
            .await
            .map_err(|e| Error::new(ErrorKind::Tls, e))?;

        let info = TlsInfo {
            // `None`, always — and this is a limitation of the wrapper, not
            // of the platform. `native_tls::TlsStream` does expose
            // `negotiated_alpn()`, but `async_native_tls::TlsStream` does
            // not re-export it and gives no access to the inner stream
            // (`get_ref` returns the transport underneath the TLS, not the
            // TLS stream itself). Checked against async-native-tls 0.6.0's
            // `tls_stream.rs`, whose entire inherent surface is `get_ref`,
            // `get_mut`, `buffered_read_size`, `peer_certificate` and
            // `tls_server_end_point`.
            //
            // The consequence is concrete and must not be papered over: a
            // caller cannot learn from this backend whether h2 was
            // negotiated, so ALPN-driven protocol selection does not work
            // over it. `alpn` is still SENT — `request_alpns` above — so the
            // server sees the offer; only the answer is unreadable here.
            // Use `hclient-tls-rustls` where the negotiated protocol
            // matters.
            alpn: None,
            // The leaf only, and as a one-element `Vec` rather than `None`:
            // there IS a certificate, the chain is what is missing. Telling
            // those apart is why this field is a `Vec` inside an `Option`.
            peer_certificates: stream
                .peer_certificate()
                .ok()
                .flatten()
                .and_then(|c| c.to_der().ok())
                .map(|der| vec![der]),
            // `native-tls` reports neither. `None` means "this backend
            // cannot tell you", which a caller must not read as "TLS 1.2".
            protocol_version: None,
            cipher_suite: None,
            // Nothing offered, so nothing to report — and this backend
            // could not report it in any case, for the same reason it
            // cannot report `alpn` above.
            early_data_accepted: None,
        };

        Ok((FuturesIo::new(stream), info))
    }
}
