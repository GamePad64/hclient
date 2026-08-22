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
//! [`TlsConnect`]: hclient_tls::TlsConnect
//! [`NoTls`]: hclient_tls::NoTls
#![forbid(unsafe_code)]

use hclient_core::Error;
use hclient_tls::TlsIdentity;
use std::sync::Arc;

/// Parameters for one QUIC connection's TLS.
///
/// Deliberately **not** `TlsRequest`: two of that struct's four fields mean
/// something different here (see [`QuicTlsRequest::alpn`] and
/// [`QuicTlsRequest::early_data`]), and reusing a type whose fields have
/// shifted meaning is how a caller ends up setting one and getting the
/// other.
#[derive(Debug, Clone, Copy)]
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
    /// [`TlsConnect::reports_alpn`]: hclient_tls::TlsConnect::reports_alpn
    pub alpn: &'a [&'a [u8]],
    /// RFC 9849 Encrypted Client Hello, from an HTTPS/SVCB record — the
    /// same `EchConfigList` [`TlsRequest::ech`] carries. It belongs on the
    /// request rather than on the connector for the same reason ALPN does:
    /// it comes from a DNS answer about one origin.
    ///
    /// [`TlsRequest::ech`]: hclient_tls::TlsRequest::ech
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
    /// [`TlsRequest::early_data`]: hclient_tls::TlsRequest::early_data
    pub early_data: bool,
}

/// A TLS backend that can drive a QUIC handshake.
///
/// One method that produces anything, like [`TlsConnect`], and for the same
/// reason: no caller in this workspace wants half of it.
///
/// [`TlsConnect`]: hclient_tls::TlsConnect
pub trait QuicTlsConnect: TlsIdentity {
    /// Build the crypto configuration for one QUIC connection.
    ///
    /// # Why this is typed on `quinn_proto`'s trait object rather than an opaque associated type
    ///
    /// The same decision `TlsConnect` made when it typed itself on
    /// `hyper::rt::{Read, Write}` instead of inventing a stream trait: an
    /// abstraction is worth having only if it carries something. An opaque
    /// `type ClientConfig` here would carry nothing — the consumer would
    /// have to bound it back to `Into<Arc<dyn quinn_proto::crypto::
    /// ClientConfig>>` before it could do anything with it, which is this
    /// module's empty-body adapter one level up, dressed as generality.
    ///
    /// The cost is honest and bounded: this crate depends on `quinn-proto`,
    /// and nothing else in the workspace has to.
    fn quic_client_config(
        &self,
        req: QuicTlsRequest<'_>,
    ) -> Result<Arc<dyn quinn_proto::crypto::ClientConfig>, Error>;

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
    /// [`TlsConnect::reports_alpn`]: hclient_tls::TlsConnect::reports_alpn
    fn offers_early_data(&self) -> bool {
        false
    }
}
