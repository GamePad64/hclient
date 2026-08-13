//! Pluggable TLS.
//!
//! The trait is typed on `hyper::rt::Read`/`Write`, and **not** on
//! futures-io or tokio-io. Consequence: there is no such thing as a
//! per-runtime TLS glue crate — one adapter (Task 9, rustls) serves every
//! runtime (`http-ng-rt-tokio`, `http-ng-rt-smol`, and any future one),
//! because `hyper::rt::{Read, Write}` is the one point every `S` in this
//! vertical is already normalized to (`http_ng_rt::FuturesIo`, `TokioIo`),
//! not one more layer stacked on top.
#![forbid(unsafe_code)]

use http_ng_core::{Error, ErrorKind, TlsSupport};
use std::future::Future;

/// Parameters for a single TLS connection.
///
/// ALPN lives on the **connect call**, not on the config: version pinning
/// and h2-prior-knowledge each require different ALPN sets for different
/// connections to the same origin (for example, one attempt forces
/// `h2`-only, the next falls back to `http/1.1`). An implementation for
/// which recomputing this on every connect is expensive is free to cache
/// its TLS config per concrete ALPN set internally — that's its own
/// business, not this trait's.
#[derive(Debug, Clone, Copy)]
pub struct TlsRequest<'a> {
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
    /// [`http_ng_core::bare_host`], before filling this field.
    ///
    /// **It is the caller's and not the backend's, and the reason is that
    /// a backend cannot know.** This field is a name, not a URI: a caller
    /// may have built it from a `Host` header, from a configuration file,
    /// or from a pinned identity that has nothing to do with the address
    /// dialled. A backend that stripped defensively would be guessing
    /// which of those it had, and would be the second place in the graph
    /// doing this normalisation — the first being the resolver, which has
    /// to strip too (`http_ng_dns::IpLiteralOnly::literal`,
    /// `http_ng_dns_doh`'s `ip_literal`). Two places normalising is how
    /// they come to disagree.
    ///
    /// The rule generalises past this field: **`Uri::host()`'s answer is
    /// URI syntax until someone takes the brackets off**, and the
    /// authority-shaped consumers — the `Host` header, HTTP/2's
    /// `:authority` — need them left on. Only the step *out* of URI-land
    /// strips.
    ///
    /// This was not written down when the field was added, and the gap
    /// cost three live defects: `http-ng-native`'s connector, and both of
    /// `http-ng-h3`'s two uses of the same string. Their tests are
    /// `http-ng-native`'s `tests/tls_server_name.rs` and `http-ng-h3`'s
    /// `tests/quic_server_name.rs`, each asserting a completed handshake
    /// against a certificate with an IP SAN.
    pub server_name: &'a str,
    pub alpn: &'a [&'a [u8]],
    /// RFC 9849 Encrypted Client Hello. The `EchConfigList` comes from an
    /// HTTPS/SVCB record (`http_ng_dns::SvcbEndpoint::ech_config_list`,
    /// Task 6). The slot is reserved up front, not bolted on once the
    /// first implementation needed it: adding a new field to a request
    /// struct later would be a breaking change for every already-written
    /// `TlsConnect` implementation.
    ///
    /// **Reserved is not the same as ignorable, and this field is the one
    /// where the difference is a security property.** No backend in this
    /// workspace implements ECH; all three refuse a non-`None` value with a
    /// typed error before a byte reaches the wire —
    /// `http-ng-tls-native-tls`, `http-ng-tls-rustls` on the TCP path, and
    /// the same crate's QUIC path. A backend that connected anyway would
    /// send in the clear the very name the caller asked to encrypt, and
    /// would report success while doing it: the caller cannot detect the
    /// difference from the response, which is what makes best-effort worse
    /// here than an error. A new `TlsConnect` implementation that does not
    /// honour this field owes the same refusal.
    pub ech: Option<&'a [u8]>,
    /// TLS 1.3 early data (0-RTT): `Some(n)` asks the backend to offer it
    /// and to accept up to `n` bytes of application data in the first
    /// flight, `None` asks for none. **Reserved, not implemented** — no
    /// backend in this workspace reads it today.
    ///
    /// Reserved for exactly the reason [`TlsRequest::ech`] above was: a
    /// new field on this struct later is a breaking change for every
    /// `TlsConnect` written against it, and a slot that costs nothing now
    /// costs a major version then.
    ///
    /// Three things whoever implements this needs, written down here so
    /// they are not rediscovered:
    ///
    /// 1. **0-RTT is replayable, and that makes it a client policy
    ///    question before it is a crypto one.** An attacker can replay
    ///    early data; which requests may go into it is therefore a
    ///    decision about the request, not about the connection. The
    ///    vocabulary for that decision already exists —
    ///    `http_ng_core::RequestBody::retry_kind()`, and the reasoning
    ///    around it that v0.2 W2's retry is built on. Start there.
    /// 2. **The floor rule applies here with unusual force.** Over-claiming
    ///    a capability normally costs a buffered copy or a lost
    ///    optimisation; over-claiming this one costs exposure to replay.
    ///    So whatever `Capabilities` end up saying about it must be the
    ///    value that holds on the worst case, exactly as
    ///    `full_duplex` is (see `http-ng-native`'s `Native::new`).
    /// 3. **`native-tls` will not be able to do it**, for the same reason
    ///    it cannot report ALPN — so the answer must come from the backend
    ///    ([`TlsConnect::reports_alpn`] is the shape), with the
    ///    conservative value as the default.
    ///
    /// One thing that is already half in place, and is not obvious:
    /// rustls keeps session resumption in `ClientConfig`
    /// (`ClientSessionStore`), and `http_ng_tls_rustls::Rustls::
    /// from_config` stores exactly one `Arc<ClientConfig>` — so the
    /// session cache is already scoped to one `Rustls` value, which is
    /// the same thing [`TlsConfigId`] identifies and which v0.2 W2 already
    /// put in the connection pool's key. **Half, not ready**: rustls keys
    /// its ticket store by `ServerName` alone, while a TLS 1.3 ticket also
    /// carries transport parameters, and `enable_early_data` sits on the
    /// config rather than on a per-connection request. The part that
    /// assembled itself is "which client may resume whose sessions"; the
    /// rest has not been designed.
    pub early_data: Option<usize>,
}

/// The outcome of a TLS handshake, as visible to the caller.
///
/// **Every field is `Option`**: native-tls (the only backend available
/// without picking a specific crypto library) hands back only the leaf
/// certificate, ALPN, and tls-server-end-point — not the full chain, not
/// the protocol version, not the cipher suite. Note that this describes
/// `native-tls` itself; `http-ng-tls-native-tls`, the backend built on it,
/// reports even less — its `alpn` is always `None`, because the async
/// wrapper it uses does not expose the negotiated protocol. See that
/// crate's module doc before relying on ALPN from it. The trait must allow for a
/// backend with that reduced a set. Symmetrically: a backend that can't
/// report a field must leave it `None`, not substitute a plausible-looking
/// value — a capability that lies about its own state is worse than a
/// capability that's simply absent (the same principle that split
/// `RedirectSupport::None`/`Transparent` in `http-ng-core` and
/// `supports_svcb()`/the empty stream in `http-ng-dns`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsInfo {
    /// The negotiated ALPN protocol for this connection — a single item,
    /// the result of negotiation, not the whole proposed list from
    /// `TlsRequest::alpn`.
    pub alpn: Option<Vec<u8>>,
    /// The peer's certificate chain, DER, in leaf → root order. Backends
    /// like native-tls hand back only the leaf — in that case a
    /// single-element `Vec`, not `None`: there is a certificate, the chain
    /// is just incomplete.
    pub peer_certificates: Option<Vec<Vec<u8>>>,
    /// The TLS protocol version negotiated on this connection.
    ///
    /// A `String`, not an enum defined by this crate: version enums differ
    /// across backends (rustls, native-tls over OpenSSL/SChannel/
    /// SecureTransport) in exactly which variants they carry, and defining
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
    /// Whether the server accepted the TLS 1.3 early data offered through
    /// [`TlsRequest::early_data`]. **Reserved, not implemented** — every
    /// backend here leaves it `None` today.
    ///
    /// `Option<bool>`, and all three states are distinct and all needed:
    /// `Some(true)` the early data counted, `Some(false)` it was rejected
    /// and whatever was in it has to be sent again, `None` this backend
    /// cannot tell — the same third state [`TlsInfo::alpn`] has, for the
    /// same backend, for the same reason. A caller that read `None` as
    /// `false` would resend needlessly; one that read it as `true` would
    /// drop a request on the floor. See [`TlsRequest::early_data`] for
    /// what an implementer needs to know before touching any of this.
    ///
    /// **This field answers for the streaming path — TLS 1.3 over TCP —
    /// and must not be read as the general contract for early data.**
    /// There the verdict is known by the time the handshake completes,
    /// which is what makes a field on a handshake result the honest shape.
    /// In QUIC it is not: measured in `docs/h3-research.md`, `into_0rtt()`
    /// returns at 1.3 ms, the response arrives at 8.5 ms and the
    /// acceptance verdict only at 8.6 ms — *after* the response. HTTP/3
    /// will therefore need a shape of its own for this (and has a third
    /// rejection path nobody here has, `425 Too Early`, RFC 8470); it must
    /// not be forced into this one.
    pub early_data_accepted: Option<bool>,
}

/// Which trust configuration a [`TlsConnect`] applies, as a value that can
/// be compared.
///
/// **A connection may be reused for a later request only if that request
/// would have been made with an equal identity.** Equal must therefore mean
/// *the same trust decisions*: the same roots, the same client certificate,
/// the same verifier, the same anything a peer's acceptance depends on. Two
/// clients with different roots sharing a socket is a security defect, not
/// a performance one, which is why this type exists at all — see
/// `http_ng_native`'s pool key.
///
/// # Why a token, and not a `TypeId` or a hash of the configuration
///
/// `TypeId` cannot work: two `Rustls` values built from different root
/// stores are the same type, so a `TypeId` would call them
/// interchangeable, which is exactly the defect.
///
/// A hash of the configuration's contents would work, and is not used
/// either. A collision would mean sharing a socket between two different
/// trust configurations — the same defect again, arriving quietly and at a
/// rate nobody measures — and hashing the contents correctly is work every
/// implementation would have to redo, with rustls's `ClientConfig` (a
/// verifier trait object among its fields) not offering a way to do it
/// completely.
///
/// So: a token, drawn from a process-wide counter by
/// [`TlsConfigId::new_unique`] **once, when the connector is constructed**,
/// and stored in it. Collisions cannot happen by construction. The cost is
/// in the other direction — two connectors built from the same
/// configuration by two separate calls get different identities and will
/// not share a socket — and that is the direction to be wrong in: less
/// reuse, never reuse across a trust boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TlsConfigId(u64);

impl TlsConfigId {
    /// A token distinct from every other one this process has produced.
    ///
    /// Call it **once per configuration**, in the constructor, and keep the
    /// result — an implementation that calls this from `config_id` itself
    /// would report a different identity on every call and pool nothing at
    /// all.
    pub fn new_unique() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        // Starts at 1: 0 belongs to `no_tls()` below.
        static NEXT: AtomicU64 = AtomicU64::new(1);
        // `Relaxed` is enough: nothing is published alongside this counter,
        // and the only property required of it is that two calls never
        // return the same value, which `fetch_add` gives on its own
        // regardless of ordering.
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The identity of a connector that performs no TLS at all ([`NoTls`]).
    ///
    /// A constant rather than a fresh token, and truthfully so: every
    /// `NoTls` makes the same trust decisions, namely none, so they are
    /// interchangeable in the only sense this type is about. It never
    /// reaches a pool key in practice — `NoTls::connect` returns an error
    /// instead of a stream, so there is no connection to key — but the
    /// method must return something, and returning something arbitrary
    /// would be a small lie in a type whose whole job is not to lie.
    pub const fn no_tls() -> Self {
        Self(0)
    }
}

/// Which trust configuration a TLS backend applies — see [`TlsConfigId`].
///
/// # Why this is a trait of its own rather than a method on [`TlsConnect`]
///
/// It was a method on `TlsConnect` until HTTP/3 (v0.3). QUIC needs a
/// *second* TLS trait — `http_ng_tls_quic::QuicTlsConnect` — because the
/// intersection of `TlsConnect`'s methods with what a QUIC stack asks of a
/// TLS session is empty: QUIC wants per-encryption-level key schedules and
/// CRYPTO-frame payloads, and `TlsConnect` can only hand back a wrapped
/// byte stream. Both traits need the identity, for the same reason and with
/// the same meaning, and `http-ng-tls-rustls` implements both.
///
/// Declaring `config_id` on each of them would give such a backend two
/// inherent methods of the same name, so every concrete-typed call site
/// becomes `E0034`. Declaring it once, here, and making both traits require
/// it costs each implementation a three-line `impl` and **costs consumers
/// nothing**: a call through a `T: TlsConnect` bound still resolves through
/// the supertrait, so no code that reads an identity moved when this was
/// extracted.
///
/// The alternative — leaving `config_id` on `TlsConnect` alone and having
/// the QUIC side require `T: TlsConnect + QuicTlsConnect` — is cheaper
/// today and forecloses a TLS backend that speaks QUIC and not TCP.
/// SChannel and Security.framework both support QUIC natively, so that is
/// not a hypothetical shape, merely an absent one.
pub trait TlsIdentity {
    /// Which trust configuration this connector applies — see
    /// [`TlsConfigId`].
    ///
    /// The answer must be **fixed for this connector's lifetime** and equal
    /// only to itself: return a [`TlsConfigId::new_unique`] drawn once in
    /// the constructor and stored, not one drawn here.
    ///
    /// # Why this has no default, when [`TlsConnect::tls_support`] does
    ///
    /// `tls_support` is defaulted because getting it wrong understates what
    /// the transport can do: a capability weaker than the truth, which
    /// costs a caller an opportunity. Getting *this* wrong hands one
    /// client's socket to another client's trust configuration. A default
    /// would let an implementation be wrong by saying nothing, and this is
    /// not a field to be wrong about by silence — so every implementation
    /// answers, and adding one is a compile error until it does.
    fn config_id(&self) -> TlsConfigId;

    /// Whether this connector presents a **client certificate** when a
    /// server asks for one.
    ///
    /// Defaulted to the understating value, exactly as
    /// [`TlsConnect::reports_alpn`] and [`TlsConnect::applies_ech`] are and
    /// for the same reason: a backend that says nothing costs a caller an
    /// opportunity, where one that over-claims costs them a handshake they
    /// were told would work.
    ///
    /// # Why it lives on `TlsIdentity` and not on either connect trait
    ///
    /// Because it is the same fact on both paths. `TlsConnect` and
    /// [`QuicTlsConnect`](https://docs.rs/http-ng-tls-quic) share this
    /// trait precisely because a connector has **one** configuration
    /// identity rather than two, and for `http-ng-tls-rustls` the QUIC
    /// config is a clone of the TCP one — so a method on each would be two
    /// places to answer one question, and the second place is where the
    /// answer goes stale.
    ///
    /// # This replaced a constant, and the code that held it stated the
    /// rule it broke
    ///
    /// `http-ng-h3` set `Capabilities::client_certs = true` two lines
    /// under a comment reading *"Read from the TLS backend, never from a
    /// constant: the capability has to come from the component that
    /// knows"* — while `http-ng-native` had no line at all, so it took
    /// `Capabilities::none()`'s `false`. Both were wrong and in opposite
    /// directions: `http-ng-tls-native-tls` has an `identity` setter, and
    /// `Rustls::from_config` accepts a `rustls::ClientConfig` built with
    /// `with_client_auth_cert`, so the TCP path *can* present one; and the
    /// QUIC path claimed it whatever `T` was, including a `T` that cannot.
    fn presents_client_certs(&self) -> bool {
        false
    }
}

/// A pluggable TLS handshake over an arbitrary transport.
///
/// One method, `connect`, not separate "handshake" and "wrap" steps:
/// there's nothing to gain from splitting them — no caller anywhere in
/// this vertical wants a bare handshake without a wrapped stream, or the
/// reverse.
pub trait TlsConnect: TlsIdentity {
    /// The wrapped stream after the handshake. `S: hyper::rt::Read + Write
    /// + Unpin` appears in both places (on the type itself and in its
    /// where clause) — an implementation can't promise a wrapper for only
    /// some possible `S`; every `S` capable of `connect` must get back a
    /// working `Stream<S>` too.
    type Stream<S>: hyper::rt::Read + hyper::rt::Write + Unpin
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    /// Performs a TLS handshake over an already established `io` (a TCP
    /// socket from `http_ng_rt::TcpConnect`, wrapped in
    /// `FuturesIo`/`TokioIo` — `connect` itself knows nothing about the
    /// transport) and returns the encrypted stream along with whatever
    /// negotiated parameters the implementation can honestly report.
    fn connect<S>(
        &self,
        io: S,
        req: TlsRequest<'_>,
    ) -> impl Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    /// What a transport built on this implementation should advertise in
    /// [`Capabilities::tls_config`].
    ///
    /// Defaulted to `Full` so that adding this method broke no existing
    /// implementation — every one of them does perform TLS. It exists for
    /// the one that does not: [`NoTls`] returns `None`, and a transport
    /// that asks instead of assuming cannot end up advertising TLS it will
    /// refuse to perform.
    ///
    /// The same shape as [`http_ng_dns::Resolve::supports_svcb`], and for
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
    /// This is not hypothetical. `http-ng-tls-native-tls` **sends** the
    /// ALPN list it is given (`native_tls`'s `request_alpns`) and cannot
    /// read the selection back, because `async-native-tls` does not expose
    /// it — see that crate's module doc. It is exactly the backend the
    /// `false` default describes, and it does not override this method.
    ///
    /// The same shape as [`tls_support`](Self::tls_support) and
    /// [`http_ng_dns::Resolve::supports_svcb`], for the same reason: a
    /// capability has to come from the component that knows, not from
    /// whoever assembles it.
    fn reports_alpn(&self) -> bool {
        false
    }

    /// Whether a non-`None` [`TlsRequest::ech`] would actually be applied
    /// — that is, whether this backend encrypts the ClientHello with the
    /// config it is handed, rather than refusing the request.
    ///
    /// **`false` today for every backend in this workspace**, and that is
    /// the truth rather than a placeholder: all three refuse ECH by name
    /// (see [`TlsRequest::ech`]). The method exists because a *caller* now
    /// has an ECH config to offer — `http-ng-native`'s connector reads
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

impl hyper::rt::Read for NoStream {
    fn poll_read(
        self: core::pin::Pin<&mut Self>,
        _: &mut core::task::Context<'_>,
        _: hyper::rt::ReadBufCursor<'_>,
    ) -> core::task::Poll<std::io::Result<()>> {
        match *self {}
    }
}

impl hyper::rt::Write for NoStream {
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
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    async fn connect<S>(&self, _io: S, req: TlsRequest<'_>) -> Result<(NoStream, TlsInfo), Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        Err(Error::new(
            ErrorKind::Tls,
            std::io::Error::other(format!(
                "this client was built without TLS support (NoTls); cannot secure a connection to {}",
                req.server_name
            )),
        ))
    }

    fn tls_support(&self) -> TlsSupport {
        TlsSupport::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::rt::{Read, ReadBufCursor, Write};
    use std::collections::VecDeque;
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

    /// `hyper::rt::Read + Write` with zero third-party dependencies:
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
            mut buf: ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            let n = buf.remaining().min(self.buf.len());
            let chunk: Vec<u8> = self.buf.drain(..n).collect();
            buf.put_slice(&chunk);
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
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
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
            buf: ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
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
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
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
            S: Read + Write + Unpin;

        fn connect<S>(
            &self,
            io: S,
            req: TlsRequest<'_>,
        ) -> impl Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>>
        where
            S: Read + Write + Unpin,
        {
            let alpn = req.alpn.first().map(|proto| proto.to_vec());
            async move {
                Ok((
                    PassThrough(io),
                    TlsInfo {
                        alpn,
                        peer_certificates: None,
                        protocol_version: Some("TLSv1.3".to_string()),
                        cipher_suite: None,
                        early_data_accepted: None,
                    },
                ))
            }
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
        let req = TlsRequest {
            server_name: "example.com",
            alpn: &alpn,
            ech: None,
            early_data: None,
        };

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
        let mut rb = hyper::rt::ReadBuf::new(&mut preexisting);
        let read = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, rb.unfilled()));
        poll_once(pin!(read).as_mut()).unwrap();
        assert_eq!(&preexisting, b"preexisting");

        // `Stream<S>` actually implements `hyper::rt::Write`, not just
        // types as one: write and read it back through the same shared
        // `Loopback` buffer.
        let write = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_write(cx, b"ping"));
        let n = poll_once(pin!(write).as_mut()).unwrap();
        assert_eq!(n, 4);

        let mut echoed = [0u8; 4];
        let mut rb = hyper::rt::ReadBuf::new(&mut echoed);
        let read = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, rb.unfilled()));
        poll_once(pin!(read).as_mut()).unwrap();
        assert_eq!(&echoed, b"ping");
    }
}
