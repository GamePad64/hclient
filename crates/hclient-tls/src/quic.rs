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
//! # Why this is neither its own crate nor a feature any more
//!
//! It was both, in that order, and each reason expired rather than being
//! wrong. As `hclient-tls-quic` it was a crate because Cargo unifies
//! features: a `quic` feature on `hclient-tls` would have put
//! `quinn-proto` into the graph of every build in which *any* crate
//! wanted HTTP/3, including builds whose TLS is [`NoTls`] and whose
//! reason for existing is that they have no room for a stack. Folded in
//! at `169dbdd`, it became that feature, on the measurement that the
//! identical cost had already been accepted one crate over.
//!
//! **Both arguments were about the same thing, and it is gone.** They
//! turned on this module *carrying* an `Arc<dyn
//! quinn_proto::crypto::ClientConfig>`, so whichever crate held the seam
//! linked quinn — 38 crates against 21, with `chacha20`, `rand_core` and
//! `ring` among the difference. [`QuicCryptoConfig`] is this crate's own
//! declarative type now and [`QuicTlsConnect::Session`] is opaque, so
//! there is no dependency to gate and no cost to move: the two seams are
//! **peers**, each describing what a backend must answer, and a `NoTls`
//! build carries both descriptions and neither implementation.
//!
//! What a feature would buy at this point is a flag a consumer has to
//! remember for nothing, which is the distinction with one reachable
//! side this workspace deletes rather than keeps.
//!
//! # What it costs the two shipped TLS backends
//!
//! `hclient-tls-rustls` gains one implementation behind its own `quic`
//! feature — **which stays, and is the one place a feature still earns
//! its keep**: that crate really does link `quinn-proto`, to turn a
//! [`QuicCryptoConfig`] into the session its stack wants, and a build
//! that speaks no HTTP/3 should not. `hclient-tls-native-tls` gains **nothing at all, and implements
//! nothing** — and the reason is stronger than the ALPN one already
//! recorded for it. It is not that `async-native-tls` fails to expose
//! something; it is that `SChannel`'s and Security.framework's QUIC support
//! is a different API surface which `native-tls` does not bind at any
//! level, so there is no partial implementation to write. Using it for
//! HTTP/3 is a compile error, which is the honest outcome and the same
//! shape [`NoTls`] already has for TLS itself.
//!
//! [`TlsConnect`]: crate::TlsConnect
//! [`NoTls`]: crate::NoTls

use crate::TlsIdentity;
use hclient_core::error::Error;

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

/// What a QUIC handshake is to be configured with — **this crate's own
/// type, carrying no other crate's**.
///
/// # It is declarative, and that is the decision
///
/// This was `Arc<dyn quinn_proto::crypto::ClientConfig>` in a newtype,
/// then briefly a `rustls::ClientConfig` in one, and both are wrong in
/// the same way: a seam whose purpose is that a backend does not promise
/// somebody else's major version cannot *carry* a value typed by
/// somebody else. A newtype hides the name from a rendered page and not
/// from the dependency graph — measured, the wrapper cost this crate
/// **38 crates** with `quinn-proto` and **29** with `rustls`, against
/// **21** with neither, and `chacha20`, `rand` and `ring` were among
/// them. A crate that exists to have no cryptography in it had
/// cryptography in it.
///
/// So this carries **what was decided**, not a thing that was built
/// from it: the ALPN list, whether early data is offered, the ECH
/// config list a DNS answer supplied, and the client identity's
/// **label**. Every field is data this crate already understands,
/// because every one of them arrived in a [`QuicTlsRequest`].
///
/// # Why a private key is not here, and cannot be
///
/// A label, never a key — which is the rule `docs/mtls-design.md` §3.1
/// states for the TCP path and which this path now states the same way.
/// What a label *means* is **implementation-defined**: it is a name the
/// caller invented, registered with whichever backend they built, and
/// resolved by that backend alone. A key in a smartcard cannot be handed
/// over as bytes at all, so a seam carrying key material would exclude
/// exactly the deployments a label serves — and a seam carrying a
/// *store query* would have to pick a platform, since
/// `CERT_FIND_SUBJECT_STR` means nothing to PKCS#11.
///
/// Trust roots, verifiers and certificate resolvers are absent for a
/// simpler reason: they are the backend's own, settled when it was
/// constructed — `Rustls::with_platform_verifier`, `with_webpki_roots`,
/// `with_identity` — and never per connection. Nothing that is not a
/// per-connection decision belongs in a per-connection value.
///
/// # What a transport does with it
///
/// Turns it into whatever its QUIC stack wants. On `hclient-native` that
/// is two expressions over `quinn`; a `quiche` transport would write two
/// different ones, and **neither is expressible in terms of the other** —
/// which is the whole reason this stopped being a wrapper. The
/// conversion is the transport's because the QUIC stack is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct QuicCryptoConfig {
    /// The ALPN protocols to offer, as [`QuicTlsRequest::alpn`] gave
    /// them.
    ///
    /// Owned rather than borrowed because this outlives the request: a
    /// transport holds it while it dials, and the backend that built it
    /// has returned.
    pub alpn: Vec<Vec<u8>>,
    /// Whether to offer TLS 1.3 early data — [`QuicTlsRequest::early_data`]
    /// as the backend resolved it.
    ///
    /// **Not the request's field copied.** A backend that cannot offer
    /// early data answers `false` here however it was asked, which is
    /// what makes [`QuicTlsConnect::offers_early_data`] a claim about
    /// this value rather than beside it.
    pub early_data: bool,
    /// RFC 9849 Encrypted Client Hello, from an HTTPS/SVCB record.
    ///
    /// `None` where the request carried none **or** where the backend
    /// refuses to apply one: `hclient-tls-rustls` errors rather than
    /// dropping it silently, so a `None` here beside a `Some` on the
    /// request cannot happen — the request is refused instead.
    pub ech: Option<Vec<u8>>,
    /// The client identity's label, and **only** the label.
    ///
    /// Implementation-defined: what it names is between the caller and
    /// the backend they registered it with. See this type's own
    /// documentation for why no key can be here.
    pub identity: Option<String>,
}

impl QuicCryptoConfig {
    /// A configuration offering `alpn` and nothing else.
    ///
    /// `const` and taking the one field with no honest default, which is
    /// [`QuicTlsRequest::new`]'s shape: RFC 9114 §3.2 makes ALPN
    /// mandatory over QUIC, where early data, ECH and an identity are
    /// each absent until something asks for them.
    #[must_use]
    pub const fn new(alpn: Vec<Vec<u8>>) -> Self {
        Self {
            alpn,
            early_data: false,
            ech: None,
            identity: None,
        }
    }

    /// Offer early data on this connection.
    #[must_use]
    pub const fn early_data(mut self, offer: bool) -> Self {
        self.early_data = offer;
        self
    }

    /// Apply this ECH config list.
    #[must_use]
    pub fn ech(mut self, ech: Option<Vec<u8>>) -> Self {
        self.ech = ech;
        self
    }

    /// Present the identity this label names.
    #[must_use]
    pub fn identity(mut self, label: Option<String>) -> Self {
        self.identity = label;
        self
    }
}

/// A TLS backend that can drive a QUIC handshake.
///
/// One method that produces anything, like [`TlsConnect`], and for the same
/// reason: no caller in this workspace wants half of it.
///
/// [`TlsConnect`]: crate::TlsConnect
pub trait QuicTlsConnect: TlsIdentity {
    /// The QUIC stack's session configuration, whatever that stack is.
    ///
    /// Unbounded here **on purpose**: naming a bound would name a QUIC
    /// stack, and this crate's reason for existing is that it names
    /// none. The consumer bounds it — see
    /// [`quic_session`](Self::quic_session).
    type Session;

    /// Build the crypto configuration for one QUIC connection.
    ///
    /// # Why a declaration rather than a built thing
    ///
    /// This returned `Arc<dyn quinn_proto::crypto::ClientConfig>`, then
    /// the same value inside a newtype of ours — the second fixed *whose
    /// major version the signature promised* and still made this crate
    /// link quinn to hold the value. [`QuicCryptoConfig`] is what was
    /// **decided** — ALPN, early data, ECH, the identity's label — and
    /// building the stack's own object from it is
    /// [`quic_session`](Self::quic_session)'s job. The split is what lets
    /// every check here run with no QUIC stack in the graph: a refusal is
    /// answered at this method, before anything a stack defines exists.
    ///
    /// # Errors
    ///
    /// A backend refuses rather than substitutes: an
    /// [`identity`](QuicTlsRequest::identity) naming a label this backend
    /// has not registered is an error naming the label, never a config
    /// built with the default identity instead.
    fn quic_client_config(&self, req: QuicTlsRequest<'_>) -> Result<QuicCryptoConfig, Error>;

    /// The QUIC stack's own session configuration, built from what
    /// [`quic_client_config`](Self::quic_client_config) decided.
    ///
    /// # Why this is a second method and an associated type
    ///
    /// A QUIC handshake needs more than a declaration: quinn wants an
    /// `Arc<dyn quinn_proto::crypto::ClientConfig>`, quiche wants a
    /// `quiche::Config`, and **neither is expressible in terms of the
    /// other**. Only the backend can build one, because only it holds
    /// the trust roots, the verifier and the certificate resolver that
    /// go into it — those are settled when the backend is constructed
    /// and never per connection.
    ///
    /// So the value is the backend's and its *type* is the backend's
    /// too. This crate names neither, which is the whole point: it had
    /// `quinn-proto` in its graph for four verticals because the seam
    /// carried quinn's trait object inside a newtype — 38 crates against
    /// 21, with `chacha20`, `rand` and `ring` among the difference, in
    /// the crate that exists to have no cryptography in it.
    ///
    /// **The objection this answers was recorded here and was half
    /// right.** It said an opaque associated type carries nothing,
    /// because a consumer must bound it back before it can do anything —
    /// true, and the bound is exactly where it belongs:
    /// `hclient-native`'s QUIC arm writes
    /// `T: QuicTlsConnect<Session = Arc<dyn quinn_proto::crypto::ClientConfig>>`,
    /// naming the stack **it** drives. A `quiche` transport writes a
    /// different bound and needs nothing of this crate changed. What the
    /// objection missed is that the alternative was not a simpler seam
    /// but a dependency: carrying the value means linking whoever
    /// defines its type.
    ///
    /// # Errors
    ///
    /// Whatever the backend's own construction can fail at — for
    /// rustls, a provider with no initial cipher suite. A backend
    /// refuses rather than substituting, as
    /// [`quic_client_config`](Self::quic_client_config) does.
    fn quic_session(&self, config: &QuicCryptoConfig) -> Result<Self::Session, Error>;

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
