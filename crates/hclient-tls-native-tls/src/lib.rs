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
//! **`TlsInfo::alpn` is reported, and for two verticals this paragraph
//! said it could not be.** It read that the negotiated answer was
//! unreadable, so ALPN-driven protocol selection did not work over this
//! backend — and it named the cause correctly as the wrapper's rather than
//! the platform's, without that ever being acted on.
//! `native_tls::TlsStream::negotiated_alpn` is public;
//! `async_native_tls::TlsStream` simply did not re-export it. This crate
//! owns its stream now (`stream.rs`), so `reports_alpn` is `true` and h2
//! can be negotiated over the platform stack.
//!
//! **What else it cannot report, and why `TlsInfo`'s fields are all
//! `Option`.** No protocol version and no cipher suite — `native-tls`
//! exposes neither — and only the LEAF certificate rather than the chain,
//! returned as a one-element `Vec` rather than `None`, because there IS a
//! certificate and the chain is what is missing. `None` throughout means
//! "this backend cannot tell you", never "there was none".
//! `hclient-tls`'s own doc comment anticipated exactly this backend when it
//! made every field optional.
//! # One `unsafe`, and where it is
//!
//! `deny` rather than `forbid`, which is the same change
//! `hclient-fetch` made and for a related reason: `stream.rs` carries this
//! workspace's second `unsafe` (amendment C17), because bridging
//! `native-tls`'s synchronous `Read`/`Write` to a poll-based world means
//! giving the synchronous side a way to reach the current task's waker.
//! That file's own doc says why the sans-io shape
//! `hclient-tls-rustls` uses is not available here, and what the port
//! bought besides — this backend reports ALPN now.
#![deny(unsafe_code)]

mod hyper_io;
mod stream;

use hclient_core::{Error, ErrorKind};
use hclient_rt::FuturesIo;
use hclient_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use hyper_io::HyperIo;
use std::fmt::Debug;

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
impl Debug for NativeTls {
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
    /// what it cannot say back — the protocol version and the cipher
    /// suite, the ALPN having since been recovered — and this
    /// is the other direction: `identity()` is the whole point of reaching
    /// for the platform's stack, since a smartcard or an OS-held key is
    /// exactly what a caller cannot hand to rustls as bytes.
    fn presents_client_certs(&self) -> bool {
        self.identity.is_some()
    }
}

impl TlsConnect for NativeTls {
    type Stream<S>
        = FuturesIo<crate::stream::TlsStream<HyperIo<S>>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    /// A named type, so `Send` follows from `S` rather than being chosen —
    /// see `stream.rs`'s module doc for why that took owning the stream,
    /// and what it cost.
    type Handshake<'a, S>
        = Handshaking<S>
    where
        Self: 'a,
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'a;

    /// Everything that can fail without touching the socket happens
    /// **here** — the ECH refusal, the ALPN strings, building the
    /// connector — so the future has one job. `hclient-tls-rustls`'s
    /// `connect` is arranged the same way and for the same reason.
    fn connect<'a, S>(&'a self, io: S, req: TlsRequest<'a>) -> Self::Handshake<'a, S>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'a,
    {
        if req.ech.is_some() {
            // Refused, not ignored. ECH (RFC 9849) requires the TLS stack to
            // encrypt the ClientHello against a config from an HTTPS/SVCB
            // record; no platform stack behind `native-tls` exposes that
            // knob. Connecting anyway would leak the SNI the caller asked to
            // protect — the one case where best-effort is worse than an
            // error.
            return Handshaking::new(crate::stream::Handshaking::failed(Error::new(
                ErrorKind::Tls,
                std::io::Error::other(
                    "native-tls cannot perform ECH: no platform stack exposes ClientHello encryption",
                ),
            )));
        }

        let mut builder = native_tls::TlsConnector::builder();
        if let Some(id) = self.identity.clone() {
            builder.identity(id);
        }
        for root in self.roots.iter().cloned() {
            builder.add_root_certificate(root);
        }
        let protocols: Vec<&str> = match req
            .alpn
            .iter()
            .map(|p| std::str::from_utf8(p).map_err(|e| Error::new(ErrorKind::Tls, e)))
            .collect::<Result<_, _>>()
        {
            Ok(v) => v,
            Err(e) => return Handshaking::new(crate::stream::Handshaking::failed(e)),
        };
        if !protocols.is_empty() {
            builder.request_alpns(&protocols);
        }
        let connector = match builder.build() {
            Ok(c) => c,
            Err(e) => {
                return Handshaking::new(crate::stream::Handshaking::failed(Error::new(
                    ErrorKind::Tls,
                    e,
                )));
            }
        };

        Handshaking::new(crate::stream::Handshaking::start(
            connector,
            req.server_name.to_owned(),
            HyperIo::new(io),
        ))
    }

    /// **`true` since this crate owns its stream.**
    ///
    /// It was `false` for two verticals, and the module doc called the
    /// limitation concrete: `async_native_tls::TlsStream` did not
    /// re-export `negotiated_alpn` and gave no way to the stream
    /// underneath. `native_tls::TlsStream::negotiated_alpn` is public, and
    /// this crate holds one of those now — so the limitation was the
    /// wrapper's rather than the platform's, and protocol selection driven
    /// by ALPN works over this backend.
    fn reports_alpn(&self) -> bool {
        true
    }
}

/// [`NativeTls`]'s handshake, with the `TlsInfo` this backend can report
/// assembled from the finished stream.
///
/// A newtype over `stream::Handshaking` rather than that type directly,
/// because the seam's output is `(Self::Stream<S>, TlsInfo)` and the
/// `TlsInfo` is this file's business, not the stream module's.
#[derive(Debug)]
pub struct Handshaking<S>(crate::stream::Handshaking<HyperIo<S>>);

impl<S> Handshaking<S> {
    fn new(inner: crate::stream::Handshaking<HyperIo<S>>) -> Self {
        Self(inner)
    }
}

impl<S> std::future::Future for Handshaking<S>
where
    S: hyper::rt::Read + hyper::rt::Write + Unpin,
{
    type Output = Result<(FuturesIo<crate::stream::TlsStream<HyperIo<S>>>, TlsInfo), Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let stream = std::task::ready!(std::pin::Pin::new(&mut self.0).poll(cx))?;
        let info = TlsInfo {
            // Readable at last: `negotiated_alpn` is
            // `native_tls::TlsStream`'s, and this crate owns one now. The
            // paragraph this replaces called the absence a limitation of
            // the wrapper rather than of the platform, and it was right.
            alpn: stream.negotiated_alpn(),
            // The leaf only, and as a one-element `Vec` rather than
            // `None`: there IS a certificate, the chain is what is
            // missing. Telling those apart is why this field is a `Vec`
            // inside an `Option`.
            peer_certificates: stream.peer_certificate_der().map(|der| vec![der]),
            // `native-tls` reports neither. `None` means "this backend
            // cannot tell you", which a caller must not read as "TLS 1.2".
            protocol_version: None,
            cipher_suite: None,
            // Nothing offered, so nothing to report.
            early_data_accepted: None,
        };
        std::task::Poll::Ready(Ok((FuturesIo::new(stream), info)))
    }
}
