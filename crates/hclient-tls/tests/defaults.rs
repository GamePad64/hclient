//! The seam's five defaulted members, and the six [`TlsInfo`] setters,
//! pinned from **outside** the crate that defines them.
//!
//! Outside is the load-bearing word for the setters — `TlsInfo` is
//! `#[non_exhaustive]`, so the door a reader has is not the door the
//! crate's own fixtures use. For the five defaults it is incidental, and
//! they are here because the fixture they need is the same one.
//!
//! Four of the five default to the **understating** value, and the
//! workspace states the rule for them in one sentence: *a constant
//! defaulted to the understating value, read by the layer above to decide
//! whether to ask.* Each has a named reader one crate up —
//! [`TlsConnect::reports_alpn`] by `hclient-native`'s `may_speak_h2`,
//! [`TlsConnect::applies_ech`] by its connector,
//! [`TlsIdentity::presents_client_certs`] by both of its capability
//! tables, and [`QuicTlsConnect::offers_early_data`] by `H3::new`. Those
//! readers are tested where they are.
//!
//! **What was not tested is the default itself.** A mutation sweep of this
//! crate left `presents_client_certs`, `applies_ech` and
//! `offers_early_data` flipped to `true` and `tls_support` dropped to
//! `TlsSupport::None`, with 6/6 tests passing on all four — because the
//! only implementation this crate ships is [`NoTls`], which overrides
//! `tls_support` and is not a client-certificate or ECH or early-data
//! backend by any reading, so nothing here ever *took* a default. The
//! fixture below is the piece that was missing: a backend that supplies
//! only what has no default — `config_id`, and `TlsConnect`'s two
//! associated types and its `connect` — and takes every defaulted member
//! as it comes, which is exactly the backend the defaults were written
//! for.
//!
//! Only `reports_alpn` was already pinned, in `no_tls.rs`, and the same
//! sweep caught its mutant — so this file is that test's siblings rather
//! than a new idea, and it deliberately does not restate it: see
//! [`a_backend_that_says_nothing_claims_no_capability_it_was_not_given`].
//!
//! # Why a backend that over-claims is the failure mode, not one that
//! under-claims
//!
//! Each `true` is a promise the layer above acts on. `applies_ech` is the
//! sharpest: a connector reading `true` fills [`TlsRequest::ech`] from an
//! HTTPS record, and every backend in this workspace *refuses* a non-`None`
//! value — so the flip does not weaken ECH, it makes every origin that
//! publishes an ECH config unreachable. `presents_client_certs` reaches
//! `Capabilities::client_certs`, which a caller reads before deciding
//! whether mTLS will work at all. `offers_early_data` over-claims replay
//! exposure, which its own doc calls a step stronger than the usual rule.
//!
//! [`TlsRequest::ech`]: hclient_tls::TlsRequest::ech

use hclient_core::caps::TlsSupport;
use hclient_core::error::Error;
use hclient_tls::{ClientCertAsk, TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use std::pin::Pin;
use std::task::{Context, Poll};

/// A backend that answers **only what it must** and takes every default.
///
/// `config_id` is the one member of [`TlsIdentity`] with no default and
/// `connect`/`Stream`/`Handshake` are [`TlsConnect`]'s; everything else is
/// left alone deliberately. That is the whole fixture: the value of this
/// file is what it does *not* write.
///
/// It is not [`NoTls`](hclient_tls::NoTls), which would answer these
/// questions truthfully and for its own reasons — a connector that
/// performs no TLS presents no client certificate and applies no ECH
/// because it does nothing, which would make the test pass for a reason
/// unrelated to the default. This one is a plausible TLS backend that
/// simply has not been told to say anything.
#[derive(Debug)]
struct SaysNothing(TlsConfigId);

impl Default for SaysNothing {
    fn default() -> Self {
        Self(TlsConfigId::new_unique())
    }
}

impl TlsIdentity for SaysNothing {
    fn config_id(&self) -> TlsConfigId {
        self.0
    }
}

/// Uninhabited for [`NoTls`](hclient_tls::NoTls)'s own reason: this
/// fixture is read for its answers, never connected through, so the type
/// system carries the fact that there is no stream rather than a `panic!`
/// carrying it at run time.
enum NoStream {}

impl futures_io::AsyncRead for NoStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        match *self {}
    }
}
impl futures_io::AsyncWrite for NoStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match *self {}
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match *self {}
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match *self {}
    }
}
impl hclient_rt::Shutdown for NoStream {
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match *self {}
    }
}

impl TlsConnect for SaysNothing {
    type Stream<S>
        = NoStream
    where
        S: futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin;

    type Handshake<'a, S>
        = std::future::Ready<Result<(NoStream, TlsInfo), Error>>
    where
        Self: 'a,
        S: futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin + 'a;

    fn connect<'a, S>(&'a self, _io: S, _req: TlsRequest<'a>) -> Self::Handshake<'a, S>
    where
        S: futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin + 'a,
    {
        unreachable!("this fixture is read for its capabilities, never connected through")
    }
}

/// **`tls_support` is the one default that does not understate, and this
/// is the test that says so rather than a sentence that does.**
///
/// It answers [`TlsSupport::Full`] where the other four answer the
/// conservative value, and that is correct rather than an oversight: a
/// type implementing [`TlsConnect`] at all performs TLS, so `Full` is what
/// is true of every implementation but the documented exception, and the
/// exception overrides it — `NoTls::tls_support` is `TlsSupport::None`,
/// asserted in `no_tls.rs`.
///
/// **Which direction it is wrong in is what makes it worth pinning.**
/// `TlsSupport`'s own `#[default]` is `None`, so the mutation cargo-mutants
/// generates here — `Default::default()` — drops a real backend to
/// `None`, and `hclient-native` copies that answer straight into
/// `Capabilities::tls_config`. A caller reading it would be told this
/// client cannot do TLS by a client that can, which is the understating
/// direction and merely costs a request nobody makes. The dangerous
/// direction is unreachable from a mutation, and unreachable from a
/// backend too: there is no value a `TlsConnect` could return that claims
/// more than `Full`.
#[test]
fn a_tls_backend_that_says_nothing_still_says_it_does_tls() {
    assert_eq!(SaysNothing::default().tls_support(), TlsSupport::Full);
}

/// The two unpinned understating defaults on the TCP seam, asserted
/// together because they are one rule with two subjects — and each on its
/// own line, because a mutation flips exactly one and an
/// `assert!(!a && !b)` would name neither.
///
/// **`reports_alpn` is deliberately not the third.** It was written here
/// as one, on the argument that this fixture could plausibly negotiate
/// where `NoTls` cannot — and then measured: flipping that default to
/// `true` fails `no_tls.rs`'s
/// `reports_alpn_defaults_to_the_conservative_answer` **and** this test,
/// so the assertion killed nothing its sibling did not already kill. It is
/// the one of the five the original sweep already caught, and a second
/// statement of a fact that is already pinned is the thing this workspace
/// deletes rather than keeps: two checks of one fact are two places for
/// the rule to be changed in and one of them to be missed.
#[test]
fn a_backend_that_says_nothing_claims_no_capability_it_was_not_given() {
    let tls = SaysNothing::default();
    assert!(
        !tls.presents_client_certs(),
        "a backend that was never given an identity must not report one: \
         `Capabilities::client_certs` is read before a caller decides mTLS will work"
    );
    assert!(
        !tls.applies_ech(),
        "a backend that does not encrypt the ClientHello must say so: a connector \
         reading `true` fills `TlsRequest::ech` from an HTTPS record, and every \
         backend here refuses a non-`None` value — so the over-claim makes every \
         ECH-publishing origin unreachable rather than merely unencrypted"
    );
}

/// [`TlsIdentity::config_id_for`] answers `None` for a label it does not
/// know, and a backend that says nothing knows none.
///
/// The reader is `hclient-native`, which turns `None` into an error naming
/// the label **before it opens a socket** — so this default is what makes
/// an unknown identity a refusal rather than a connection carrying the
/// default certificate to a server the caller never named it for.
///
/// It is the one member of the five whose surviving mutant is
/// **equivalent**: cargo-mutants' non-trivial variant, `Some(Default::
/// default())`, is unviable because [`TlsConfigId`] deliberately has no
/// `Default` — a token that could be defaulted would be a collision by
/// construction, which that type's doc is entirely about. What survived
/// deletes `let _ = name;` and returns the `None` the body already
/// returned. This test therefore pins the contract rather than killing a
/// mutant, and says so.
#[test]
fn an_unknown_label_resolves_to_no_identity_rather_than_to_the_default_one() {
    let tls = SaysNothing::default();
    assert!(
        tls.config_id_for("a-label-this-backend-never-registered")
            .is_none()
    );
    // The control: the backend *does* have an identity of its own, so
    // `None` here is a refusal to resolve a name rather than a backend
    // with nothing to offer.
    assert_eq!(tls.config_id(), tls.config_id());
    assert_ne!(tls.config_id(), SaysNothing::default().config_id());
}

/// **Each [`TlsInfo`] setter writes its own field and keeps the other
/// five**, and the two halves below are one property rather than two
/// tests.
///
/// The setters are not a convenience. `TlsInfo` is `#[non_exhaustive]`, so
/// a struct literal is a compile error outside the defining crate and a
/// backend elsewhere in the family has **no other way to produce the value
/// at all** — which the type's own doc says in as many words. That makes a
/// setter which drops what it is handed a backend reporting nothing while
/// believing it reported something, which is `TlsInfo`'s own *a capability
/// that lies about its own state is worse than one that is simply absent*
/// met at the point the value is built rather than where it is read.
///
/// **This is an integration test and not an in-crate one, and the reason
/// is the sentence above.** The crate's own `NoOpTls` fixture builds its
/// `TlsInfo` with a struct literal — legal inside the crate, impossible
/// for a reader — so an in-crate test could exercise these setters only by
/// choosing to, where from out here the setters are the only door. Before
/// this test **nothing called any of the six**: mutating all six at once
/// to `Default::default()` left the crate's 6 tests passing, and each one
/// alone also passed.
///
/// Each field is asserted on a chain that has already set the other four,
/// so a setter that discards its receiver is caught as well as one that
/// discards its argument, and each is read individually so that a setter
/// writing into the *wrong* field cannot pass either.
#[test]
fn every_tls_info_setter_writes_its_own_field_and_disturbs_no_other() {
    let full = TlsInfo::new()
        .alpn(Some(b"h2".to_vec()))
        .peer_certificates(Some(vec![vec![0xde, 0xad]]))
        .protocol_version(Some("TLSv1.3".to_string()))
        .cipher_suite(Some("TLS13_AES_128_GCM_SHA256".to_string()))
        .client_cert(ClientCertAsk::NotAsked);

    assert_eq!(full.alpn.as_deref(), Some(b"h2".as_slice()));
    assert_eq!(full.peer_certificates, Some(vec![vec![0xde, 0xad]]));
    assert_eq!(full.protocol_version.as_deref(), Some("TLSv1.3"));
    assert_eq!(
        full.cipher_suite.as_deref(),
        Some("TLS13_AES_128_GCM_SHA256")
    );
    assert_eq!(full.client_cert, ClientCertAsk::NotAsked);

    // The reverse direction, and it is the half a `Default::default()`
    // mutation alone would not distinguish: a setter handed `None` must
    // *clear* the field rather than leave what an earlier call put there.
    let cleared = full
        .clone()
        .alpn(None)
        .peer_certificates(None)
        .protocol_version(None)
        .cipher_suite(None);
    assert_eq!(cleared, TlsInfo::new().client_cert(ClientCertAsk::NotAsked));
}

/// The QUIC seam's one defaulted member, on the same fixture shape.
///
/// `src/quic.rs` had **no test file at all** before this, which its own
/// module doc makes worth more than a coverage number: the argument for a
/// second trait is that an adapter between `TlsConnect` and
/// `quinn_proto::crypto::Session` *"type-checks with an empty body"*, and
/// a seam whose failure mode is compiling is a seam whose members have to
/// be asserted rather than read.
mod quic {
    use super::SaysNothing;
    use hclient_core::error::Error;
    use hclient_tls::quic::{QuicCryptoConfig, QuicTlsConnect, QuicTlsRequest};

    /// `unreachable!` rather than a real config, which is
    /// `hclient-native`'s `http3::tests::StubTls` verbatim and for its
    /// stated reason: the caller reads `offers_early_data` and nothing
    /// else, so producing a real [`QuicCryptoConfig`] would be building a
    /// crypto provider to answer a question that never asks for one.
    ///
    /// **This fixture names no QUIC stack at all, and `()` is the proof
    /// of it.** The newtype this replaced kept quinn out of the seam's
    /// *signature* while this crate still linked it to hold the value —
    /// so a backend that only reports its capabilities compiled against
    /// a graph carrying `quinn-proto`, `ring` and `chacha20`. With a
    /// declarative config and an opaque `Session`, the type a backend
    /// names is its own, and a backend that never connects can name
    /// nothing at all.
    impl QuicTlsConnect for SaysNothing {
        type Session = ();

        fn quic_client_config(&self, _: QuicTlsRequest<'_>) -> Result<QuicCryptoConfig, Error> {
            unreachable!("this fixture is read for its capabilities, never connected through")
        }

        fn quic_session(&self, _: &QuicCryptoConfig) -> Result<Self::Session, Error> {
            unreachable!("this fixture is read for its capabilities, never connected through")
        }
    }

    /// **The understating rule at its strongest**, in this member's own
    /// words: over-claiming a capability normally costs a buffered copy or
    /// a lost optimisation, and over-claiming this one costs *replay
    /// exposure*, because early data is data an attacker can capture and
    /// send again. `H3::new` reads it into `Capabilities::early_data`.
    #[test]
    fn a_quic_backend_that_says_nothing_offers_no_early_data() {
        assert!(!SaysNothing::default().offers_early_data());
    }

    /// **The builder's defaults are the understating ones**, which is the
    /// half `#[non_exhaustive]` makes load-bearing: a field added later
    /// is only safe to add because a caller who never names it gets the
    /// safe answer. Early data is the one that matters — it is replayable,
    /// so a request must never end up offering it because nobody said
    /// otherwise.
    #[test]
    fn a_request_offers_nothing_but_its_alpn_until_asked() {
        let req = QuicTlsRequest::new(&[b"h3"]);
        assert_eq!(req.alpn, &[b"h3"]);
        assert!(!req.early_data, "early data is opt-in, never a default");
        assert!(req.ech.is_none());
        assert!(req.identity.is_none());
    }

    /// And each setter reaches its own field, which is what says the
    /// builder is a translation rather than four names over one value.
    /// Written as one request carrying all three, because a setter that
    /// overwrote a neighbour would pass three separate assertions.
    #[test]
    fn each_setter_reaches_its_own_field() {
        let req = QuicTlsRequest::new(&[b"h3"])
            .early_data(true)
            .ech(Some(b"ech"))
            .identity(Some("corp"));
        assert!(req.early_data);
        assert_eq!(req.ech, Some(b"ech".as_slice()));
        assert_eq!(req.identity, Some("corp"));
        assert_eq!(req.alpn, &[b"h3"], "the constructor's argument survives");
    }
}
