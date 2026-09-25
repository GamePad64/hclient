//! TLS over a byte stream: [`TlsConnect`], what it is handed and what it
//! hands back, and [`NoTls`], the backend for a build with no stack.
//!
//! The other seam is [`crate::quic`], and the identity both require is
//! [`TlsIdentity`] at the crate root.

use crate::{ClientCertAsk, TlsConfigId, TlsIdentity};
use hclient_core::caps::TlsSupport;
use hclient_core::error::{Error, ErrorKind};
use std::future::Future;

// Maintainer notes (not rendered):
//
// # Built with [`new`](Self::new), read by field
//
// `#[non_exhaustive]` by this workspace's three-answer rule: the transport
// builds one and a backend only reads it, so a field added later must not
// be a breaking change for every `TlsConnect` written against this. It
// used to carry **reserved slots** for exactly that reason — `ech` and
// `early_data` went in before anything filled them — and the attribute is
// what replaces the practice: the next field arrives when it is designed,
// in the shape the design gives it, rather than in a shape guessed ahead.
// [`QuicTlsRequest`](crate::quic::QuicTlsRequest) has had this form from
// the start.
//
// # 0-RTT over TCP is not a field yet, deliberately
//
// `early_data: Option<usize>` sat here, documented as reserved and read by
// no backend, and it left before this type was frozen: a stable field is a
// promise about a shape, and nobody had designed this one. Its answer,
// `TlsInfo::early_data_accepted: Option<bool>`, left with it.
//
// Two things the pair had established and are worth keeping. The verdict
// needs **three** states — accepted, rejected and to be resent, *this
// backend cannot tell* — because a caller reading "cannot tell" as
// "rejected" resends needlessly and one reading it as "accepted" drops a
// request. And a field on a handshake result is the right shape only for
// TLS over TCP, where the verdict is known when the handshake completes:
// in QUIC it resolves *after* the response (measured, 8.63 ms against
// 8.58 ms), which is why `hclient-native`'s HTTP/3 arm holds a future.
// Three things whoever implements it needs, written down here so
// they are not rediscovered:
//
// 1. **0-RTT is replayable, and that makes it a client policy
//    question before it is a crypto one.** An attacker can replay
//    early data; which requests may go into it is therefore a
//    decision about the request, not about the connection. The
//    vocabulary for that decision already exists —
//    `hclient_core::body::RequestBody::retry_kind()`, and the reasoning
//    around it that v0.2 W2's retry is built on. Start there.
// 2. **The floor rule applies here with unusual force.** Over-claiming
//    a capability normally costs a buffered copy or a lost
//    optimisation; over-claiming this one costs exposure to replay.
//    So whatever `Capabilities` end up saying about it must be the
//    value that holds on the worst case, exactly as
//    `full_duplex` is (see `hclient-native`'s `Native::new`).
// 3. **`native-tls` will not be able to do it**, for the same reason
//    it cannot report ALPN — so the answer must come from the backend
//    ([`TlsConnect::reports_alpn`] is the shape), with the
//    conservative value as the default.
//
// One thing that is already half in place, and is not obvious:
// rustls keeps session resumption in `ClientConfig`
// (`ClientSessionStore`), and `hclient_tls_rustls::Rustls::
// from_config` stores exactly one `Arc<ClientConfig>` — so the
// session cache is already scoped to one `Rustls` value, which is
// the same thing [`TlsConfigId`] identifies and which v0.2 W2 already
// put in the connection pool's key. **Half, not ready**: rustls keys
// its ticket store by `ServerName` alone, while a TLS 1.3 ticket also
// carries transport parameters, and `enable_early_data` sits on the
// config rather than on a per-connection request. The part that
// assembled itself is "which client may resume whose sessions"; the
// rest has not been designed.

/// Parameters for a single TLS connection.
///
/// ALPN lives on the **connect call**, not on the config: version pinning
/// and h2-prior-knowledge each require different ALPN sets for different
/// connections to the same origin (for example, one attempt forces
/// `h2`-only, the next falls back to `http/1.1`). An implementation for
/// which recomputing this on every connect is expensive is free to cache
/// its TLS config per concrete ALPN set internally — that's its own
/// business, not this trait's.
///
/// This type is `#[non_exhaustive]`: the transport builds one and a
/// backend only reads it, so a field added later must not be a breaking
/// change for every `TlsConnect` written against this.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct TlsRequest<'a> {
    // Maintainer notes (not rendered):
    //
    // This was not written down when the field was added, and the gap
    // cost three live defects: `hclient-native`'s connector, and both of
    // `hclient-h3`'s two uses of the same string. Their tests are
    // `hclient-native`'s `tests/tls_server_name.rs` and `hclient-h3`'s
    // `tests/quic_server_name.rs`, each asserting a completed handshake
    // against a certificate with an IP SAN.
    /// The name to present in SNI and to verify the certificate against —
    /// a DNS name or an IP address, **never a URI authority**.
    ///
    /// # Whose job the normalisation is: the caller's
    ///
    /// `http::Uri::host()` returns an IPv6 literal **with its brackets**
    /// (`[2001:db8::1]`), because they belong to the authority's grammar
    /// rather than to the host's — RFC 3986 §3.2.2. A transport that
    /// passes that string through gets `invalid dns name` from every
    /// backend here and from every backend that could exist:
    /// `rustls_pki_types::ServerName::try_from` tries a DNS name, then an
    /// IP address, and a bracket is neither. So the caller strips, with
    /// [`hclient_core::url::bare_host`], before filling this field.
    ///
    /// **It is the caller's and not the backend's, and the reason is that
    /// a backend cannot know.** This field is a name, not a URI: a caller
    /// may have built it from a `Host` header, from a configuration file,
    /// or from a pinned identity that has nothing to do with the address
    /// dialled. A backend that stripped defensively would be guessing
    /// which of those it had, and would be the second place in the graph
    /// doing this normalisation — the first being the resolver, which has
    /// to strip too (`hclient_dns::IpLiteralOnly::literal`,
    /// `hclient_dns_doh`'s `ip_literal`). Two places normalising is how
    /// they come to disagree.
    ///
    /// The rule generalises past this field: **`Uri::host()`'s answer is
    /// URI syntax until someone takes the brackets off**, and the
    /// authority-shaped consumers — the `Host` header, HTTP/2's
    /// `:authority` — need them left on. Only the step *out* of URI-land
    /// strips.
    pub server_name: &'a str,
    /// The ALPN protocols to offer, most preferred first — see the type's
    /// own documentation for why this is per connection.
    pub alpn: &'a [&'a [u8]],
    /// RFC 9849 Encrypted Client Hello. The `EchConfigList` comes from an
    /// HTTPS/SVCB record (`hclient_dns::SvcbEndpoint::ech_config_list`).
    ///
    /// **Reserved is not the same as ignorable, and this field is the one
    /// where the difference is a security property.** No backend in this
    /// workspace implements ECH; all three refuse a non-`None` value with a
    /// typed error before a byte reaches the wire —
    /// `hclient-tls-native-tls`, `hclient-tls-rustls` on the TCP path, and
    /// the same crate's QUIC path. A backend that connected anyway would
    /// send in the clear the very name the caller asked to encrypt, and
    /// would report success while doing it: the caller cannot detect the
    /// difference from the response, which is what makes best-effort worse
    /// here than an error. A new `TlsConnect` implementation that does not
    /// honour this field owes the same refusal.
    pub ech: Option<&'a [u8]>,
    /// The client identity the caller named, or `None` for this
    /// backend's default.
    ///
    /// A **label the caller invented**, never a certificate and never a
    /// store query: what it resolves to is the backend's business, and
    /// that is the only thing that can be the same on Windows, macOS,
    /// PKCS#11 and Android at once. See `docs/mtls-design.md` §3.1.
    ///
    /// A backend that does not know the name must refuse rather than
    /// connect with its default — silently substituting an identity is
    /// how one tenant's certificate reaches another's server.
    pub identity: Option<&'a str>,
}

impl<'a> TlsRequest<'a> {
    /// A request for `server_name`, offering `alpn`, with no ECH and the
    /// backend's default identity.
    ///
    /// The two fields with no honest default, which is
    /// [`QuicTlsRequest::new`](crate::quic::QuicTlsRequest::new)'s shape:
    /// a handshake has to present *some* name, and an ALPN list — even an
    /// empty one — is a decision the caller made.
    #[must_use]
    pub const fn new(server_name: &'a str, alpn: &'a [&'a [u8]]) -> Self {
        Self {
            server_name,
            alpn,
            ech: None,
            identity: None,
        }
    }

    /// Apply this ECH config list — see [`ech`](Self::ech) for what a
    /// backend that cannot owes.
    #[must_use]
    pub const fn ech(mut self, ech: Option<&'a [u8]>) -> Self {
        self.ech = ech;
        self
    }

    /// Present the identity this label names, or the backend's default for
    /// `None`.
    #[must_use]
    pub const fn identity(mut self, identity: Option<&'a str>) -> Self {
        self.identity = identity;
        self
    }
}

/// The outcome of a TLS handshake, as visible to the caller.
///
/// **Every field is `Option`**: native-tls (the only backend available
/// without picking a specific crypto library) hands back only the leaf
/// certificate, ALPN, and tls-server-end-point — not the full chain, not
/// the protocol version, not the cipher suite. `hclient-tls-native-tls`
/// reports exactly that set — the negotiated ALPN and the leaf — and `None`
/// for the version and the suite. The trait must allow for a
/// backend with that reduced a set. Symmetrically: a backend that can't
/// report a field must leave it `None`, not substitute a plausible-looking
/// value — a capability that lies about its own state is worse than a
/// capability that's simply absent (the same principle that split
/// `RedirectSupport::None`/`Transparent` in `hclient-core` and
/// `supports`/the empty stream in `hclient-dns`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TlsInfo {
    /// The negotiated ALPN protocol for this connection — a single item,
    /// the result of negotiation, not the whole proposed list from
    /// `TlsRequest::alpn`.
    pub alpn: Option<Vec<u8>>,
    /// Whether the server asked for a client certificate, and what for.
    ///
    /// **Three states rather than an `Option`**, and the type carries the
    /// argument: a backend that cannot observe the `CertificateRequest`
    /// must be distinguishable from a server that did not send one.
    /// `hclient-tls-rustls` sees it by wrapping the config's
    /// `ResolvesClientCert`; `native-tls` exposes no such hook and leaves
    /// the default [`ClientCertAsk::Unobserved`], which is the
    /// understating value and [`TlsConnect::reports_alpn`]'s rule.
    pub client_cert: ClientCertAsk,
    /// The peer's certificate chain, DER, in leaf → root order. Backends
    /// like native-tls hand back only the leaf — in that case a
    /// single-element `Vec`, not `None`: there is a certificate, the chain
    /// is just incomplete.
    pub peer_certificates: Option<Vec<Vec<u8>>>,
    /// The TLS protocol version negotiated on this connection.
    ///
    /// A `String`, not an enum defined by this crate: version enums differ
    /// across backends (rustls, native-tls over OpenSSL/SChannel/
    /// `SecureTransport`) in exactly which variants they carry, and defining
    /// a unifying enum here would mean either lagging behind a new backend
    /// or carrying variants a given backend will never produce. So that
    /// two backends don't name the same version differently, the value
    /// must be a registry-style string, the same one used by both
    /// `openssl`'s `SSL_get_version()` and rustls: `"TLSv1.3"`,
    /// `"TLSv1.2"`, `"TLSv1.1"`, `"TLSv1.0"` — not the `Debug` formatting
    /// of the backend's internal enum (rustls's `Debug` for
    /// `ProtocolVersion::TLSv1_3`, for example, prints `TLSv1_3`, with an
    /// underscore instead of a dot — the implementation must normalize
    /// this to the canonical form, not pass `Debug`'s output through as
    /// is).
    pub protocol_version: Option<String>,
    /// The cipher suite negotiated on this connection.
    ///
    /// The same argument as `protocol_version`, for the same reason — a
    /// `String`, not an enum. The value must be a name from the IANA TLS
    /// Cipher Suites registry, e.g. `"TLS_AES_128_GCM_SHA256"` — the same
    /// name rustls uses (`CipherSuite::TLS13_AES_128_GCM_SHA256` must be
    /// normalized to the registry name with its version prefix stripped,
    /// not passed through `Debug` as is), whereas OpenSSL by default names
    /// the same cipher suite with an alias like
    /// `"ECDHE-RSA-AES128-GCM-SHA256"` — an implementation on top of
    /// OpenSSL must translate the alias to the registry name, or two
    /// backends will report the same cipher as two different strings, and
    /// a caller comparing them will get it wrong.
    pub cipher_suite: Option<String>,
}

impl TlsInfo {
    /// Nothing reported, which is the honest starting point: every field
    /// is what a backend *may* be able to say, and `hclient-tls-native-tls`
    /// cannot say two of them at all.
    ///
    /// Chained setters rather than a literal, because this type is
    /// `#[non_exhaustive]` and built by TLS backends outside this
    /// workspace — the attribute protects the reader from a new field, and
    /// the setters keep the writer able to produce the value at all.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The protocol the peer selected through ALPN.
    #[must_use]
    pub fn alpn(mut self, alpn: Option<Vec<u8>>) -> Self {
        self.alpn = alpn;
        self
    }

    /// The peer's certificate chain, DER-encoded, leaf first.
    #[must_use]
    pub fn peer_certificates(mut self, certs: Option<Vec<Vec<u8>>>) -> Self {
        self.peer_certificates = certs;
        self
    }

    /// The negotiated protocol version, where the backend exposes one.
    #[must_use]
    pub fn protocol_version(mut self, version: Option<String>) -> Self {
        self.protocol_version = version;
        self
    }

    /// The negotiated cipher suite, where the backend exposes one.
    #[must_use]
    pub fn cipher_suite(mut self, suite: Option<String>) -> Self {
        self.cipher_suite = suite;
        self
    }

    /// Whether the server asked for a client certificate, and what for.
    #[must_use]
    pub fn client_cert(mut self, asked: ClientCertAsk) -> Self {
        self.client_cert = asked;
        self
    }
}

// Maintainer notes (not rendered):
//
// One method, `connect`, not separate "handshake" and "wrap" steps:
// there's nothing to gain from splitting them — no caller anywhere in
// this vertical wants a bare handshake without a wrapped stream, or the
// reverse.

/// A pluggable TLS handshake over an arbitrary transport.
pub trait TlsConnect: TlsIdentity {
    // Maintainer notes (not rendered):
    //
    // runtime stream's `poll_close` to be. Checked by writing exactly
    // that backend over `futures-rustls`, from a crate outside this
    // workspace, and running it under `hclient::Client`.

    /// The wrapped stream after the handshake.
    ///
    /// **A backend over somebody else's TLS library needs a newtype here**,
    /// because that library's stream implements `futures-io` and cannot
    /// implement [`hclient_rt::Shutdown`], a trait it has never heard of.
    /// Forwarding `poll_shutdown` to the library's own `poll_close` is
    /// right when that `poll_close` sends `close_notify` and then closes
    /// the transport beneath with *its* `poll_close` — which is the
    /// half-close [`hclient_rt::Shutdown`]'s documentation asks every
    /// runtime stream's `poll_close` to be.
    ///
    /// The `S` bound appears in both places (on the
    /// type itself and in its where clause) — an implementation can't
    /// promise a wrapper for only some possible `S`; every `S` capable of
    /// `connect` must get back a working `Stream<S>` too.
    type Stream<S>: futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin
    where
        S: futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin;

    /// The future [`connect`](Self::connect) hands back.
    ///
    /// **An associated type, not an RPITIT**, for the reason
    /// `hclient_rt::TcpConnect::Connecting` gives at length: a consumer
    /// that must prove its own future `Send` has to be able to *name*
    /// this one, and `impl Future` has no name.
    ///
    /// **And a named type rather than a boxed one, which is what makes
    /// this seam different from `TcpConnect`'s.** The handshake's future
    /// is `Send` exactly when `S` is, so a box would have to pick one
    /// answer for every `S` — and the two available answers are both
    /// wrong. `+ Send` on the box excludes an IO that cannot cross a
    /// thread, and `hclient-rt-embassy`'s can not, which would take that
    /// runtime out of `Native` altogether since `Native` requires a
    /// `TlsConnect`. Leaving it off makes every TLS handshake `!Send` for
    /// everybody. A concrete type derives the answer from `S` instead of
    /// choosing it, which is the only one of the three that is true.
    ///
    /// The cost lands on the implementor: an `async fn` body has no name,
    /// so a backend that awaits anything writes its handshake as a type
    /// with a `poll` rather than as an `async fn`. Both shipped ones do,
    /// and their synchronous preparation moved into `connect` where it
    /// belongs anyway.
    type Handshake<'a, S>: Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>>
    where
        Self: 'a,
        S: futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin + 'a;

    /// Perform a TLS client handshake over `io` and hand back the encrypted
    /// stream with what was negotiated.
    ///
    /// Performs a TLS handshake over an already established `io` (a
    /// runtime's own `hclient_rt::TcpConnect::Stream`, handed over as it
    /// is — `connect` itself knows nothing about the transport) and returns
    /// the encrypted stream along with whatever negotiated parameters the
    /// implementation can honestly report.
    ///
    /// `io` is an already connected byte stream — usually a runtime's
    /// [`hclient_rt::TcpConnect::Stream`], handed over as it is; this method
    /// knows nothing about how it was opened. Work that needs no I/O
    /// (building the session, validating `req`) may happen here, before the
    /// future is returned; everything that touches `io` happens when it is
    /// polled.
    ///
    /// What `req` asks for must be honoured or refused, never approximated:
    ///
    /// - [`server_name`](TlsRequest::server_name) is sent in SNI and the
    ///   certificate is verified against it.
    /// - [`alpn`](TlsRequest::alpn) is offered as given; the protocol the
    ///   server chose goes into [`TlsInfo`], if this backend can read it
    ///   back (see [`reports_alpn`](Self::reports_alpn)).
    /// - A non-`None` [`ech`](TlsRequest::ech) the backend cannot apply is
    ///   an error, because connecting anyway sends the name in the clear.
    /// - A non-`None` [`identity`](TlsRequest::identity) must present that
    ///   identity or fail — never fall back to the default one.
    ///
    /// The returned [`TlsInfo`] reports only what the backend really
    /// observed; a field it cannot read stays `None`.
    ///
    /// # Errors
    ///
    /// The future resolves to an [`Error`] — conventionally of kind
    /// [`ErrorKind::Tls`] — when the handshake fails (certificate
    /// verification, protocol alert, I/O on `io`) or when `req` asks for
    /// something this backend cannot do.
    fn connect<'a, S>(&'a self, io: S, req: TlsRequest<'a>) -> Self::Handshake<'a, S>
    where
        S: futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin + 'a;

    /// What a transport built on this implementation should advertise in
    /// [`Capabilities::tls_config`](hclient_core::caps::Capabilities::tls_config).
    ///
    /// Defaulted to [`TlsSupport::Full`], the one default on these seams
    /// that is not the understating value — because here it is the truth
    /// by construction. A `TlsConnect` is built by whoever builds the
    /// transport, so its roots, its identity and its ALPN list are that
    /// caller's configuration, which is what `Full` says.
    ///
    /// The method exists for the backend that performs no TLS at all:
    /// [`NoTls`] answers [`TlsSupport::None`], and a transport that asks
    /// instead of assuming cannot advertise TLS it will refuse to perform.
    /// No `TlsConnect` answers [`TlsSupport::Platform`]; that one belongs
    /// to backends whose platform does TLS beneath them, which is a
    /// transport's fact and not a TLS seam's.
    ///
    /// The same shape as `hclient_dns::Resolve::supports`, and for
    /// the same reason: a capability has to come from the component that
    /// knows, not from whoever assembles it.
    fn tls_support(&self) -> TlsSupport {
        TlsSupport::Full
    }

    /// Whether [`TlsInfo::alpn`] is filled in when a protocol was actually
    /// negotiated — that is, whether `None` from this backend means "the
    /// peer selected nothing" rather than "I cannot tell you".
    ///
    /// **Defaulted to `false`, which is the opposite of
    /// [`tls_support`](Self::tls_support)'s default, and deliberately so.**
    /// A default must never be stronger than the truth, and the two
    /// methods differ in what being wrong costs. An implementation that
    /// forgets `tls_support` claims it performs TLS, which it does. An
    /// implementation that forgot *this* one, under a `true` default,
    /// would claim it can report ALPN — and a caller acting on that claim
    /// offers `h2`, is told `None`, concludes HTTP/1.1, and speaks HTTP/1
    /// down a connection on which the server selected HTTP/2. That is not
    /// a lost optimisation, it is a protocol error on every request. Under
    /// `false` the same forgetful backend merely understates itself: no
    /// `h2` is offered, everything works, slower.
    ///
    /// This is not hypothetical. `hclient-tls-native-tls` **sends** the
    /// ALPN list it is given (`native_tls`'s `request_alpns`) and cannot
    /// read the selection back, because `async-native-tls` does not expose
    /// it — see that crate's module doc. It is exactly the backend the
    /// `false` default describes, and it does not override this method.
    ///
    /// The same shape as [`tls_support`](Self::tls_support) and
    /// `hclient_dns::Resolve::supports`, for the same reason: a
    /// capability has to come from the component that knows, not from
    /// whoever assembles it.
    fn reports_alpn(&self) -> bool {
        false
    }

    /// Whether a non-`None` [`TlsRequest::ech`] would actually be applied
    /// — that is, whether this backend encrypts the `ClientHello` with the
    /// config it is handed, rather than refusing the request.
    ///
    /// **`false` today for every backend in this workspace**, and that is
    /// the truth rather than a placeholder: all three refuse ECH by name
    /// (see [`TlsRequest::ech`]). The method exists because a *caller* now
    /// has an ECH config to offer — `hclient-native`'s connector reads
    /// `SvcbEndpoint::ech_config_list` out of an HTTPS record — and
    /// without an answer here it would have to choose between two wrong
    /// things: filling the field, which turns the refusal into "every
    /// origin that publishes an ECH config is unreachable", or dropping
    /// the config on the floor, which is the silent no-op the refusal was
    /// added to end.
    ///
    /// So the third option is this: the discovery layer fills the field
    /// only for a backend that says it will use it, and records in its own
    /// documentation what a `false` here costs — the server name goes out
    /// in the clear to an origin that asked for it not to. That is a fact
    /// about privacy the caller can read *before* making a request
    /// (`TlsConnect` is in hand at construction), not one it would have to
    /// infer from a failure.
    ///
    /// **The default is `false` for the same reason
    /// [`reports_alpn`](Self::reports_alpn)'s is**, and the asymmetry with
    /// [`tls_support`](Self::tls_support) is the same one: a default must
    /// never be stronger than the truth. A backend that forgot this method
    /// under a `true` default would be handed an ECH config, would refuse
    /// (or, worse, would ignore it), and the caller would have been told
    /// its name was protected. Under `false` the same backend merely gets
    /// no ECH config, which is what it can handle.
    ///
    /// **This is not `reports_alpn` with a different name**, and the two
    /// must not be collapsed: `reports_alpn` is about reading a value back
    /// out of a completed handshake, this is about putting one in. A
    /// backend could do either without the other.
    fn applies_ech(&self) -> bool {
        false
    }
}

/// A [`TlsConnect`] that performs no TLS, for a client built without it.
///
/// For constrained targets that have `std` but no room for a TLS stack:
/// plain HTTP works, and `https://` fails at connect with a typed error
/// instead of failing to link. `Native<R, NoTls, D>` drops rustls,
/// native-tls and their transitive trees from the build entirely.
///
/// It advertises [`TlsSupport::None`], so a caller who reads
/// `Capabilities::tls_config` before making a request learns the truth
/// rather than discovering it at connect time.
///
/// `Stream<S>` is an uninhabited type. That is not a trick: it is the type
/// system carrying the same fact the error does — this implementation
/// cannot produce a TLS stream, and no code path can pretend otherwise,
/// because there is no value to pretend with.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTls;

/// The stream [`NoTls`] never returns. Uninhabited, so every method is
/// unreachable by construction rather than by a panic.
#[derive(Debug)]
pub enum NoStream {}

impl futures_io::AsyncRead for NoStream {
    fn poll_read(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
        _: &mut [u8],
    ) -> core::task::Poll<std::io::Result<usize>> {
        match *self {}
    }
}

impl futures_io::AsyncWrite for NoStream {
    fn poll_write(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
        _: &[u8],
    ) -> core::task::Poll<std::io::Result<usize>> {
        match *self {}
    }
    fn poll_flush(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        match *self {}
    }
    fn poll_close(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        match *self {}
    }
}

impl hclient_rt::Shutdown for NoStream {
    fn poll_shutdown(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        match *self {}
    }
}

impl TlsIdentity for NoTls {
    /// See [`TlsConfigId::no_tls`]: every `NoTls` is interchangeable with
    /// every other, because none of them makes any trust decision at all.
    fn config_id(&self) -> TlsConfigId {
        TlsConfigId::no_tls()
    }
}

impl TlsConnect for NoTls {
    type Stream<S>
        = NoStream
    where
        S: futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin;

    /// [`std::future::Ready`], because this refuses without awaiting
    /// anything — the one backend for which no `poll` had to be written.
    /// It ignores `S` entirely, which is why it stays a `TlsConnect` for
    /// a runtime whose IO cannot cross a thread.
    type Handshake<'a, S>
        = std::future::Ready<Result<(NoStream, TlsInfo), Error>>
    where
        Self: 'a,
        S: futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin + 'a;

    fn connect<'a, S>(&'a self, _io: S, req: TlsRequest<'a>) -> Self::Handshake<'a, S>
    where
        S: futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin + 'a,
    {
        std::future::ready(Err(Error::new(
            ErrorKind::Tls,
            std::io::Error::other(format!(
                "this client was built without TLS support (NoTls); cannot secure a connection to {}",
                req.server_name
            )),
        )))
    }

    fn tls_support(&self) -> TlsSupport {
        TlsSupport::None
    }
}

#[cfg(test)]
mod tests {

    /// **The understating default, asserted rather than described.** A
    /// backend that fills nothing must report *I could not see*, never
    /// *the server did not ask*: the second is an answer, and a backend
    /// that never watched has none. This is `reports_alpn`'s rule with a
    /// test, and it is the property every backend that forgets the field
    /// silently relies on.
    #[test]
    fn a_backend_that_says_nothing_reports_unobserved() {
        assert_eq!(TlsInfo::new().client_cert, ClientCertAsk::Unobserved);
        assert_eq!(TlsInfo::default().client_cert, ClientCertAsk::Unobserved);
        assert_eq!(ClientCertAsk::default(), ClientCertAsk::Unobserved);
    }

    /// `asked()` is the caller's shortcut and must not paper over the
    /// distinction the type exists for: both non-`Asked` states answer
    /// `None`, and it is the *caller* choosing to treat them alike.
    #[test]
    fn the_shortcut_flattens_only_at_the_call_site() {
        assert!(ClientCertAsk::Unobserved.asked().is_none());
        assert!(ClientCertAsk::NotAsked.asked().is_none());
        assert!(
            ClientCertAsk::Asked(ClientCertRequest::new())
                .asked()
                .is_some()
        );
    }
    use super::*;
    use crate::ClientCertRequest;
    use futures_io::{AsyncRead as Read, AsyncWrite as Write};
    use hclient_rt::Shutdown;
    use std::collections::VecDeque;
    use std::future::poll_fn;
    use std::io;
    use std::pin::{Pin, pin};
    use std::task::{Context, Poll, Waker};

    /// Polls a single `Future`/`poll_fn` synchronously and demands
    /// immediate readiness. Every future in this module's tests is built
    /// on `Loopback`, which never returns `Pending` — a real executor
    /// would add nothing here; `Waker::noop()` (stable since 1.85, well under this
    /// vertical's MSRV) settles the matter without an extra dependency
    /// like `futures-executor` for a single synchronous poll.
    fn poll_once<F: Future>(mut fut: Pin<&mut F>) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("test I/O must not return Pending"),
        }
    }

    /// The seam's byte stream — `futures-io`'s read and write halves plus
    /// [`Shutdown`] — with zero third-party dependencies beyond them:
    /// writes into a shared buffer, reads from that same buffer. Not a
    /// call-counting mock — working I/O, enough to actually push bytes
    /// through `TlsConnect::Stream<S>` and back.
    #[derive(Default)]
    struct Loopback {
        buf: VecDeque<u8>,
    }

    impl Read for Loopback {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            let n = buf.len().min(self.buf.len());
            for (slot, byte) in buf.iter_mut().zip(self.buf.drain(..n)) {
                *slot = byte;
            }
            Poll::Ready(Ok(n))
        }
    }

    impl Shutdown for Loopback {
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl Write for Loopback {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.buf.extend(data.iter().copied());
            Poll::Ready(Ok(data.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// A pass-through implementation of `TlsConnect::Stream<S>` — a
    /// wrapper around `S`, not `type Stream<S> = S`: a real adapter (Task
    /// 9, rustls) must wrap `S` in TLS session state, and an identity GAT
    /// wouldn't exercise that shape at all. Encrypts nothing, just
    /// forwards.
    struct PassThrough<S>(S);

    impl<S: Read + Unpin> Read for PassThrough<S> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl<S: Shutdown + Unpin> Shutdown for PassThrough<S> {
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    impl<S: Write + Unpin> Write for PassThrough<S> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, data)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }
        /// Forwards `poll_close` to `poll_close`, and the half-close to
        /// `poll_shutdown` in the [`Shutdown`] impl above — which is the
        /// distinction the seam exists to keep, so a double that folded
        /// the two would be the one place it could not be exercised.
        fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_close(cx)
        }
    }

    /// Encrypts nothing: reports the first proposed ALPN as "negotiated"
    /// and a fixed protocol version — exactly enough for the test to have
    /// something to check in `TlsInfo`, nothing more
    /// (`peer_certificates`/`cipher_suite` stay `None` — honestly, the
    /// stub has no way to produce them).
    struct NoOpTls(TlsConfigId);

    impl Default for NoOpTls {
        fn default() -> Self {
            Self(TlsConfigId::new_unique())
        }
    }

    impl TlsIdentity for NoOpTls {
        fn config_id(&self) -> TlsConfigId {
            self.0
        }
    }

    impl TlsConnect for NoOpTls {
        type Stream<S>
            = PassThrough<S>
        where
            S: Read + Write + Shutdown + Unpin;

        // `Ready`, so the fixture's `Send` follows from `S`'s exactly as
        // a real backend's does — a fixture that fixed the answer would
        // be the one place the seam's property could not be exercised.
        type Handshake<'a, S>
            = std::future::Ready<Result<(Self::Stream<S>, TlsInfo), Error>>
        where
            Self: 'a,
            S: Read + Write + Shutdown + Unpin + 'a;

        fn connect<'a, S>(&'a self, io: S, req: TlsRequest<'a>) -> Self::Handshake<'a, S>
        where
            S: Read + Write + Shutdown + Unpin + 'a,
        {
            let alpn = req.alpn.first().map(|proto| proto.to_vec());
            std::future::ready({
                Ok((
                    PassThrough(io),
                    TlsInfo {
                        client_cert: ClientCertAsk::Unobserved,
                        alpn,
                        peer_certificates: None,
                        protocol_version: Some("TLSv1.3".to_string()),
                        cipher_suite: None,
                    },
                ))
            })
        }
    }

    #[test]
    fn connect_wraps_the_stream_and_negotiates_alpn() {
        // The ALPN bytes are built from LOCAL `Vec<u8>`s, not `&'static
        // [u8]` literals — proof that `TlsRequest<'a>`, with the SAME `'a`
        // on both the outer slice and each element's bytes, is actually
        // constructible without `'static` and without `req` needing to be
        // stored anywhere longer than the `connect` call it was designed
        // for ("ALPN lives on the connect call" — see the field's doc
        // comment).
        let h2 = b"h2".to_vec();
        let http11 = b"http/1.1".to_vec();
        let alpn = [h2.as_slice(), http11.as_slice()];
        let req = TlsRequest::new("example.com", &alpn);

        // `io` already contains data BEFORE the handshake — proves below
        // that the returned `Stream<S>` actually wraps THIS `io`, rather
        // than substituting an independent source that just happens to
        // also implement `Read`/`Write`.
        let mut io = Loopback::default();
        io.buf.extend(*b"preexisting");

        let tls = NoOpTls::default();
        let fut = tls.connect(io, req);
        let mut fut = pin!(fut);
        let (mut stream, info) = poll_once(fut.as_mut()).unwrap();

        assert_eq!(info.alpn.as_deref(), Some(b"h2".as_slice()));
        assert_eq!(info.protocol_version.as_deref(), Some("TLSv1.3"));
        assert!(info.peer_certificates.is_none());
        assert!(info.cipher_suite.is_none());

        // Data that was sitting in `io` BEFORE `connect` is visible
        // through the returned `Stream<S>` — meaning it's a wrapper over
        // the passed-in `io`, not a new, disconnected stream.
        let mut preexisting = [0u8; 11];
        let read = poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, &mut preexisting));
        let n = poll_once(pin!(read).as_mut()).unwrap();
        assert_eq!(&preexisting[..n], b"preexisting");

        // `Stream<S>` actually implements `futures_io::AsyncWrite`, not just
        // types as one: write and read it back through the same shared
        // `Loopback` buffer.
        let write = poll_fn(|cx| Pin::new(&mut stream).poll_write(cx, b"ping"));
        let n = poll_once(pin!(write).as_mut()).unwrap();
        assert_eq!(n, 4);

        let mut echoed = [0u8; 4];
        let read = poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, &mut echoed));
        let n = poll_once(pin!(read).as_mut()).unwrap();
        assert_eq!(&echoed[..n], b"ping");
    }
}
