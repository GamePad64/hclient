//! Pluggable TLS **for QUIC** — a second seam beside [`TlsConnect`], not a
//! widening of it.
//!
//! # Why `TlsConnect` cannot carry this, and not by a small margin
//!
//! `TlsConnect::connect<S>(&self, io: S, req) -> (Self::Stream<S>,
//! TlsInfo)` is bytes in, bytes out, over an already-established stream.
//! QUIC does not have one: TLS handshake data travels in CRYPTO frames that
//! the *QUIC* layer frames, retransmits and encrypts, and what QUIC asks of
//! TLS is a key schedule per encryption level plus a QUIC-specific
//! transport-parameters extension that has no counterpart over TCP —
//! eleven methods on `quinn_proto::crypto::Session`, of which
//! `initial_keys`, `early_crypto` and `next_1rtt_keys` hand out `Keys` and
//! `read_handshake`/`write_handshake` move CRYPTO payloads.
//!
//! **The intersection of what `TlsConnect` offers and what that requires is
//! empty, and that is worse than a compile error rather than better.** An
//! adapter `impl<T: TlsConnect> quinn_proto::crypto::ClientConfig for
//! Quic<T>` type-checks — with an empty body, because there is no
//! expression in `TlsConnect`'s vocabulary whose value can become a `Keys`.
//! A seam that fails by compiling is the one shape this project treats as a
//! defect rather than an inconvenience, so the answer is a separate trait
//! that a backend either implements or does not.
//!
//! # Why this is its own crate rather than a feature of `hclient-tls`
//!
//! Cargo unifies features across a dependency graph. A `quic` feature on
//! `hclient-tls` would put `quinn-proto` into the graph of every build in
//! which *any* crate wanted HTTP/3 — including builds whose TLS is
//! [`NoTls`] and whose whole reason for existing is that they have no room
//! for a stack. A separate crate is paid for only by whoever depends on it.
//! This is the argument `json`, `gzip` and `brotli` are already behind
//! features for, applied one level up.
//!
//! # What it costs the two shipped TLS backends
//!
//! `hclient-tls-rustls` gains one implementation behind its own `quic`
//! feature. `hclient-tls-native-tls` gains **nothing at all, and implements
//! nothing** — and the reason is stronger than the ALPN one already
//! recorded for it. It is not that `async-native-tls` fails to expose
//! something; it is that SChannel's and Security.framework's QUIC support
//! is a different API surface which `native-tls` does not bind at any
//! level, so there is no partial implementation to write. Using it for
//! HTTP/3 is a compile error, which is the honest outcome and the same
//! shape [`NoTls`] already has for TLS itself.
//!
//! [`TlsConnect`]: crate::TlsConnect
//! [`NoTls`]: crate::NoTls
#![forbid(unsafe_code)]

use crate::TlsIdentity;
use hclient_core::error::Error;
use std::sync::Arc;

/// Parameters for one QUIC connection's TLS.
///
/// Deliberately **not** `TlsRequest`: two of that struct's four fields mean
/// something different here (see [`QuicTlsRequest::alpn`] and
/// [`QuicTlsRequest::early_data`]), and reusing a type whose fields have
/// shifted meaning is how a caller ends up setting one and getting the
/// other.
/// # Extensible, and the constructor is what makes that true
///
/// `#[non_exhaustive]`, so a field added later is not a breaking change
/// for a backend outside this workspace — and this crate's own rule says
/// when that attribute is right: a type the library **hands to** an
/// implementor, which reads it and never builds it. That is exactly this
/// one. The rule's opposite case — `TcpOpts`, built by a caller as
/// `Struct { one: .., ..Default::default() }` — is why the attribute is
/// refused there and taken here.
///
/// It costs the three call sites a builder rather than a literal, which
/// is the trade: `QuicTlsRequest::new(alpn).early_data(true)`. `alpn` is
/// the constructor's argument because it is the one field with no honest
/// default — RFC 9114 §3.2 makes it mandatory, and a `QuicTlsRequest`
/// offering nothing is a connection that cannot succeed.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct QuicTlsRequest<'a> {
    /// The ALPN protocols to offer.
    ///
    /// Over QUIC this is mandatory rather than optional: RFC 9114 §3.2 —
    /// *"During connection establishment, HTTP/3 support is indicated by
    /// selecting the ALPN token 'h3' in the TLS handshake"* — and a
    /// connection whose ALPN is not `h3` is an error, not a fallback to
    /// something older.
    ///
    /// **That is why there is no `reports_alpn` on this trait.**
    /// [`TlsConnect::reports_alpn`] exists because a backend can send an
    /// ALPN list and be unable to read the selection back, which over TCP
    /// leaves a client speaking HTTP/1 into an HTTP/2 connection. Here the
    /// same backend cannot implement [`QuicTlsConnect`] at all, so the
    /// question has no case to answer.
    ///
    /// [`TlsConnect::reports_alpn`]: crate::TlsConnect::reports_alpn
    pub alpn: &'a [&'a [u8]],
    /// RFC 9849 Encrypted Client Hello, from an HTTPS/SVCB record — the
    /// same `EchConfigList` [`TlsRequest::ech`] carries. It belongs on the
    /// request rather than on the connector for the same reason ALPN does:
    /// it comes from a DNS answer about one origin.
    ///
    /// [`TlsRequest::ech`]: crate::TlsRequest::ech
    pub ech: Option<&'a [u8]>,
    /// Whether to offer TLS 1.3 early data (0-RTT) on this connection.
    ///
    /// **A `bool`, where [`TlsRequest::early_data`] is an
    /// `Option<usize>`,** and the difference is a correction rather than a
    /// simplification: `max_early_data_size` is a *server* field in rustls,
    /// and a client's early-data budget comes from the ticket it
    /// remembered, not from a number it chooses. The `usize` had no
    /// client-side meaning to carry.
    ///
    /// `true` here asks the backend to offer early data. It does not say
    /// anything went into it — the acceptance verdict is not available at
    /// this layer or at this time, see [`QuicTlsConnect::offers_early_data`].
    ///
    /// [`TlsRequest::early_data`]: crate::TlsRequest::early_data
    pub early_data: bool,
    /// The client identity the caller named, mirroring
    /// [`TlsRequest::identity`](crate::TlsRequest::identity).
    ///
    /// **This field is here because omitting it would not be a compile
    /// error.** `QuicTlsRequest` is a separate type from `TlsRequest` by
    /// a deliberate decision, so a client-certificate seam that reached
    /// only the TCP path would present an identity over HTTP/1 and HTTP/2
    /// and silently omit it over HTTP/3 — one request, answered
    /// differently depending on which protocol the pool happened to
    /// offer, and nothing would fail to build.
    pub identity: Option<&'a str>,
}

impl<'a> QuicTlsRequest<'a> {
    /// A request offering `alpn` and nothing else.
    ///
    /// The other three fields default to the understating answer this
    /// workspace applies everywhere: no ECH config, no early data, no
    /// named identity. Each is the safe direction — early data in
    /// particular is replayable, so a request must never end up offering
    /// it because nobody said otherwise.
    #[must_use]
    pub const fn new(alpn: &'a [&'a [u8]]) -> Self {
        Self {
            alpn,
            ech: None,
            early_data: false,
            identity: None,
        }
    }

    /// The ECH config list from an HTTPS record, if one is to be applied.
    #[must_use]
    pub const fn ech(mut self, ech: Option<&'a [u8]>) -> Self {
        self.ech = ech;
        self
    }

    /// Whether this connection may offer early data.
    #[must_use]
    pub const fn early_data(mut self, yes: bool) -> Self {
        self.early_data = yes;
        self
    }

    /// The client identity the caller named, by label.
    #[must_use]
    pub const fn identity(mut self, identity: Option<&'a str>) -> Self {
        self.identity = identity;
        self
    }
}

/// The crypto configuration for one QUIC connection, as an opaque value.
///
/// **A newtype so that `quinn-proto` is not in this seam's signature**,
/// which is the same argument the byte-stream seam settled when it stopped
/// naming `hyper::rt`: a public bound naming a foreign type puts that
/// crate's major version in the manifest of every implementor. `quinn` is
/// at `0.11`, a series where every minor release may break — so a seam
/// spelling `Arc<dyn quinn_proto::crypto::ClientConfig>` makes a
/// `quinn-proto` bump a breaking change for every backend, in this
/// workspace and outside it.
///
/// **This does not make the value portable and does not pretend to.** What
/// is inside is quinn's, both ends know it, and a second QUIC
/// implementation would need its own variant rather than reusing this one.
/// What the newtype buys is narrower and is the whole of it: the *name*
/// crosses the seam instead of the type, so the version is named in two
/// manifests — this crate's and the transport's — rather than in every
/// implementor's signature.
///
/// The value is opaque to everyone but the two lines that make it and
/// consume it: `hclient-tls-rustls` builds one from a
/// `rustls::ClientConfig`, and `hclient-native`'s QUIC arm hands it
/// straight to `quinn::ClientConfig::new`. Nothing between those two ever
/// looks inside, which is what makes a newtype sufficient where an
/// associated type would have been ceremony — the objection the paragraph
/// this replaces raised, and correctly.
#[derive(Clone)]
pub struct QuicCryptoConfig(Arc<dyn quinn_proto::crypto::ClientConfig>);

/// The two doors, and they are **`#[doc(hidden)]` rather than `pub`**.
///
/// Together they are the only place in this crate's surface where a
/// `quinn_proto` type is nameable, and that is what the hiding is for: a
/// rendered page carrying `pub fn new(config: Arc<dyn
/// quinn_proto::crypto::ClientConfig>)` puts quinn's `0.11` — a series
/// where every minor release may break — into the public plane of a seam
/// whose whole point is that a backend does not promise it.
///
/// **This is not the `bon` hole one crate over**, which is the objection
/// to reach for and is worth answering rather than waving at.
/// `hclient-core`'s `req.rs` records a generated builder whose *public
/// setters named hidden types*, so a caller met `SetConnect<S>` in a
/// signature and in a compiler error and could not write it. Nothing here
/// appears in any public signature: `quic_client_config` answers
/// `QuicCryptoConfig`, and a caller who never opens one never meets
/// quinn at all.
///
/// **What it costs is real and is named rather than glossed.** A QUIC TLS
/// backend written outside this workspace cannot construct one without
/// reaching a hidden item, so it is not a supported extension point
/// today. Two things make that the right trade rather than an oversight.
/// `quinn_proto::crypto::ClientConfig` has exactly one implementation in
/// practice — `quinn_proto::crypto::rustls::QuicClientConfig` — so the
/// backend this excludes is a second rustls binding rather than a second
/// QUIC stack. And the alternative measured before choosing: taking a
/// `rustls::ClientConfig` instead would make the door portable and put
/// **rustls and ring into `hclient-tls`'s own graph**, which is 33 crates
/// with `quic` on and zero of them rustls today — trading a narrow leak
/// for a heavier one, in the crate that exists to have neither.
///
/// The day a second implementation exists, the door becomes `pub` and
/// takes whatever the two have in common. Until then it names the one
/// thing it can.
impl QuicCryptoConfig {
    /// Wrap a backend's configuration.
    #[doc(hidden)]
    #[must_use]
    pub fn from_quinn(config: Arc<dyn quinn_proto::crypto::ClientConfig>) -> Self {
        Self(config)
    }

    /// The configuration back, for the transport that drives quinn.
    #[doc(hidden)]
    #[must_use]
    pub fn into_quinn(self) -> Arc<dyn quinn_proto::crypto::ClientConfig> {
        self.0
    }
}

/// Hand-written: `quinn_proto::crypto::ClientConfig` is not `Debug`, and
/// what a reader wants here is that the value exists rather than what is
/// in it.
impl std::fmt::Debug for QuicCryptoConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicCryptoConfig").finish_non_exhaustive()
    }
}

/// A TLS backend that can drive a QUIC handshake.
///
/// One method that produces anything, like [`TlsConnect`], and for the same
/// reason: no caller in this workspace wants half of it.
///
/// [`TlsConnect`]: crate::TlsConnect
pub trait QuicTlsConnect: TlsIdentity {
    /// Build the crypto configuration for one QUIC connection.
    ///
    /// # Why a newtype rather than quinn's trait object or an associated type
    ///
    /// This returned `Arc<dyn quinn_proto::crypto::ClientConfig>` until the
    /// byte-stream seam stopped naming `hyper::rt`, and the argument
    /// recorded for it was that decision's: an abstraction is worth having
    /// only if it carries something, and an opaque `type ClientConfig`
    /// would carry nothing, since the consumer must bound it back to
    /// `Into<Arc<dyn ..>>` before it can do anything — this module's
    /// empty-body adapter one level up, dressed as generality.
    ///
    /// **That half is still right and is why there is no associated type
    /// here.** What it did not weigh is *whose major version the seam
    /// promises*. `quinn` is at `0.11`, where every minor may break, so
    /// naming its type in this signature made a `quinn-proto` bump a
    /// breaking change for every implementor rather than for the two lines
    /// that actually touch the value. [`QuicCryptoConfig`] is neither of
    /// the two shapes that argument compared: it carries the same value
    /// unchanged and costs one `::new` and one `::into_inner`.
    ///
    /// # Errors
    ///
    /// A backend refuses rather than substitutes: an
    /// [`identity`](QuicTlsRequest::identity) naming a label this backend
    /// has not registered is an error naming the label, never a config
    /// built with the default identity instead.
    fn quic_client_config(&self, req: QuicTlsRequest<'_>) -> Result<QuicCryptoConfig, Error>;

    /// Whether [`QuicTlsRequest::early_data`] is honoured when set.
    ///
    /// **Defaulted to `false`, and there is no `true` default anywhere on
    /// this path.** The rule is [`TlsConnect::reports_alpn`]'s and the
    /// reason is a step stronger. Over-claiming a capability normally costs
    /// a caller a buffered copy or a lost optimisation; over-claiming this
    /// one costs *replay exposure*, because early data is data an attacker
    /// can capture and send again. A backend that forgets this method
    /// understates itself and every request waits for the full handshake,
    /// which is slower and safe.
    ///
    /// Note what it does **not** answer: whether a particular request's
    /// early data was accepted. In QUIC that verdict arrives *after* the
    /// response — measured at 8.63 ms against a response at 8.58 ms
    /// — so it is a future, not a property of
    /// a connector, and it is deliberately not on this trait.
    ///
    /// [`TlsConnect::reports_alpn`]: crate::TlsConnect::reports_alpn
    fn offers_early_data(&self) -> bool {
        false
    }
}
