//! Pluggable TLS.
//!
//! The trait is typed on `futures_io::{AsyncRead, AsyncWrite}` plus
//! [`hclient_rt::Shutdown`], and **not** on tokio-io or on an HTTP
//! implementation's own IO traits. Consequence: there is no such thing as
//! a per-runtime TLS glue crate — one adapter serves every runtime
//! (`hclient-rt-tokio`, `hclient-rt-smol`, and any future one), because
//! that trio is the one point every `S` in this vertical is already
//! normalized to, not one more layer stacked on top.
//!
//! **It was `hyper::rt::Read`/`Write` until the seam was frozen**, and the
//! sentence above was the whole of the argument — true, and silent about
//! *whose major version the seam promises*. A public bound naming
//! `hyper::rt::Read` puts hyper's major in the manifest of every
//! implementor, which for a TLS backend written outside this workspace is
//! a dependency it never chose. `hclient-dns` paid the same cost through
//! one `pub fn` and it is recorded as *the leak outlived the decoder it
//! leaked*.
//!
//! hyper is a dependency this workspace may one day replace; `futures-io`
//! is not. So the conversion to `hyper::rt` lives in `hclient-native`, the
//! one crate that hands a stream to
//! `hyper::client::conn::http1::handshake`.
//!
//! # Where things are
//!
//! Two seams, peers, each in its own module, and what they share here:
//!
//! - `tcp` — [`TlsConnect`], a handshake over a byte stream, with
//!   [`TlsRequest`], [`TlsInfo`] and the [`NoTls`] backend. A private
//!   module whose items are named from this root, where every consumer
//!   has always named them: one path per type, since a stable crate
//!   promises every path it publishes.
//! - [`quic`] — [`QuicTlsConnect`](quic::QuicTlsConnect), what a QUIC stack
//!   asks of TLS, which is not a handshake over a stream at all.
//! - here — [`TlsIdentity`] and [`TlsConfigId`], the configuration
//!   identity both seams require, so one connector has one identity rather
//!   than two.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod quic;
mod tcp;

pub use hclient_core::hooks::{ClientCertAsk, ClientCertRequest};
pub use tcp::{NoStream, NoTls, TlsConnect, TlsInfo, TlsRequest};

/// Which trust configuration a [`TlsConnect`] applies, as a value that can
/// be compared.
///
/// **A connection may be reused for a later request only if that request
/// would have been made with an equal identity.** Equal must therefore mean
/// *the same trust decisions*: the same roots, the same client certificate,
/// the same verifier, the same anything a peer's acceptance depends on. Two
/// clients with different roots sharing a socket is a security defect, not
/// a performance one, which is why this type exists at all — see
/// `hclient_native`'s pool key.
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
/// *second* TLS trait — [`quic::QuicTlsConnect`] — because the
/// intersection of `TlsConnect`'s methods with what a QUIC stack asks of a
/// TLS session is empty: QUIC wants per-encryption-level key schedules and
/// CRYPTO-frame payloads, and `TlsConnect` can only hand back a wrapped
/// byte stream. Both traits need the identity, for the same reason and with
/// the same meaning, and `hclient-tls-rustls` implements both.
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
/// `SChannel` and Security.framework both support QUIC natively, so that is
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

    /// The identity the caller named, or `None` — this backend has none
    /// by that name.
    ///
    /// **A backend that answers `Some` here owes that identity at
    /// connect time**, and owes a *refusal* rather than a substitution if
    /// it cannot serve it after all. Connecting with the default identity
    /// instead is how one tenant's certificate reaches another tenant's
    /// server, and the layer above cannot see it happen: it resolved the
    /// label, put the id in its pool key, and has no way to learn that
    /// the handshake used a different one.
    ///
    /// **Defaulted to refusing every name**, which is the understating
    /// direction and `reports_alpn`'s rule: a backend that knows nothing
    /// about labels says so, and the layer above turns that into an error
    /// naming the label. Connecting with the default identity instead is
    /// how one tenant's certificate reaches another tenant's server.
    ///
    /// The returned id is what isolates connections: it is already a
    /// component of `hclient-native`'s pool key, so two labels resolving
    /// to two ids cannot share a connection by construction rather than
    /// by a check.
    fn config_id_for(&self, name: &str) -> Option<TlsConfigId> {
        let _ = name;
        None
    }

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
    /// [`QuicTlsConnect`](quic::QuicTlsConnect) share this
    /// trait precisely because a connector has **one** configuration
    /// identity rather than two, and for `hclient-tls-rustls` the QUIC
    /// config is a clone of the TCP one — so a method on each would be two
    /// places to answer one question, and the second place is where the
    /// answer goes stale.
    ///
    /// # This replaced a constant, and the code that held it stated the
    /// rule it broke
    ///
    /// `hclient-h3` set `Capabilities::client_certs = true` two lines
    /// under a comment reading *"Read from the TLS backend, never from a
    /// constant: the capability has to come from the component that
    /// knows"* — while `hclient-native` had no line at all, so it took
    /// `Capabilities::default()`'s `false`. Both were wrong and in opposite
    /// directions: `hclient-tls-native-tls` has an `identity` setter, and
    /// `Rustls::from_config` accepts a `rustls::ClientConfig` built with
    /// `with_client_auth_cert`, so the TCP path *can* present one; and the
    /// QUIC path claimed it whatever `T` was, including a `T` that cannot.
    fn presents_client_certs(&self) -> bool {
        false
    }
}
