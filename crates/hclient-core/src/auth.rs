//! Authentication as a seam: a scheme, and one exchange's state.
//!
//! **Here rather than in `hclient` because a scheme is written by
//! somebody else.** NTLM and Negotiate need a platform's own security
//! provider, and those crates have real users while no HTTP glue over any
//! of them is published — `reqwest` and `hyper` have nowhere to put one.
//! So this crate does not grow a Kerberos dependency; it grows the two
//! traits a third party needs, in the crate that already holds every
//! other seam a third party implements.
//!
//! It moved out of the facade for the reason `Transport`, `Hooks` and
//! `Capabilities` were never in it: a consumer *configures*
//! authentication, and `hclient` re-exports these under
//! `hclient::auth` so nothing a consumer writes moves — but an
//! **implementor** should not have to depend on a whole HTTP client to
//! reach a two-method trait. Measured before the move: reaching it
//! through `hclient` cost 32 crates against this crate's 16.
//!
//! What stayed behind is what belongs to the client rather than to the
//! seam: `MAX_LEGS`, which is a bound `Client::run` enforces, the
//! `Digest` scheme, and the error a flow that never finishes produces.

use crate::BodyView;

/// What a flow says after seeing a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStep {
    /// This response is the answer.
    Done,
    /// Send the request again; [`AuthFlow::authorize`] will be called
    /// first and may set a different header.
    Again,
}

/// The request a flow is about to authenticate.
///
/// A named type rather than three parameters, because the list grew once
/// already: [`body`](Self::body) is here for schemes that sign what they
/// send, and a fourth would otherwise be a fourth breaking change to
/// every implementor.
#[derive(Debug)]
pub struct AuthRequest<'a> {
    method: &'a http::Method,
    uri: &'a http::Uri,
    body: BodyView<'a>,
}

impl<'a> AuthRequest<'a> {
    /// Built by the client. A flow receives one; nothing constructs one
    /// except `hclient::Client` and a test — this crate holds the seam and
    /// never the client that drives it.
    #[must_use]
    pub fn new(method: &'a http::Method, uri: &'a http::Uri, body: BodyView<'a>) -> Self {
        Self { method, uri, body }
    }

    /// The method going on the request line. Digest hashes it into `A2`.
    #[must_use]
    pub fn method(&self) -> &http::Method {
        self.method
    }

    /// The full URI. **A scheme that signs the request-target wants the
    /// path and query rather than this** — RFC 7616 §3.4.2, and hashing
    /// the whole URL gives a server a different `A2` and a second `401`
    /// nobody can explain.
    #[must_use]
    pub fn uri(&self) -> &http::Uri {
        self.uri
    }

    /// What the body is, to a scheme that has to sign it.
    ///
    /// **Three states, because two would make "there is nothing to hash"
    /// and "there is something and you cannot have it" the same answer** —
    /// and a signing scheme must tell them apart: AWS SigV4 hashes the
    /// empty string for the first and writes `UNSIGNED-PAYLOAD` for the
    /// second, and getting that backwards produces a signature the server
    /// rejects with nothing in the rejection to say why.
    ///
    /// **What a flow sees is what the replay snapshot sees**, and that is
    /// more than it sounds: `Client` takes the snapshot before building
    /// the flow, so an ordinary `Rewindable` body — the shape a retryable
    /// upload has — arrives as its **bytes**, with no second call to its
    /// factory. This was written the other way round first, and the test
    /// that fixed it is
    /// `a_rewindable_body_is_shown_as_bytes_without_a_second_factory_call`.
    ///
    /// [`BodyView::Opaque`] is left to a snapshot that has no bytes
    /// either: a `Streaming` body, which has none until it is pumped, and
    /// a `Rewindable` whose factory hands back another one, whose bytes
    /// are behind a call this client will not make. Neither is a refusal
    /// it could lift by trying harder — buffering a stream is a cost
    /// every caller would pay for a scheme most do not use.
    ///
    /// It is a **view**: reading it consumes nothing and allocates
    /// nothing.
    #[must_use]
    pub fn body(&self) -> BodyView<'a> {
        self.body
    }
}

/// One exchange's authentication state.
///
/// Made by [`Auth::start`], used for one hop, and dropped. A scheme with
/// one leg keeps no state and ignores that; a scheme with three needs it.
pub trait AuthFlow {
    /// Add credentials to the request about to be sent.
    ///
    /// Called before **every** attempt including the first, so a scheme
    /// that can authenticate pre-emptively — Basic, or Digest with a
    /// cached nonce — does it here and never needs a second leg.
    ///
    /// A header carrying a secret should be marked with
    /// [`http::HeaderValue::set_sensitive`], which is what keeps it out
    /// of a `Debug`.
    ///
    /// **The request arrives as a value and the headers as a borrow**, so
    /// that a scheme may read what it signs and change only what it adds.
    /// [`AuthRequest::body`] is what makes signing schemes writable here
    /// at all — see [`AuthRequest::body`] for what it can and cannot show.
    fn authorize(&mut self, req: &AuthRequest<'_>, headers: &mut http::HeaderMap);

    /// Look at what came back.
    ///
    /// Only the head is offered. A scheme that needed the body would need
    /// this client to buffer every response before deciding, which is a
    /// cost every caller would pay for a case no shipped scheme has.
    fn on_response(&mut self, status: http::StatusCode, headers: &http::HeaderMap) -> AuthStep;
}

/// The configuration a flow is made from.
///
/// **Neither trait declares an auto trait**, which is this workspace's
/// rule for a seam: the demands live where the facade *stores* the value,
/// in [`BoxedFlow`] and in the `Arc` `RequestBuilder::auth` wraps a scheme
/// into, and are amendment C12's shape —
/// a bound on a value the caller hands over at an opt-in call.
///
/// The rule was reached the hard way here. A `pub trait AuthFlow: Send`
/// carries its `send-bound-exception` marker on the same line as the
/// bound, and `cargo fmt` moves a trailing comment off a `{` line; the
/// obvious repair — a trait-level `where Self: Send,` — is **worse**,
/// because `cargo fmt` deletes that comment outright rather than moving
/// it. Reproduced in isolation before it was believed. Declaring nothing
/// removes the question along with the bound.
pub trait Auth: std::fmt::Debug {
    /// A fresh flow for one hop.
    fn start(&self) -> BoxedFlow;
}

impl<T: Auth + ?Sized> Auth for std::sync::Arc<T> {
    fn start(&self) -> BoxedFlow {
        (**self).start()
    }
}

/// A flow, as the client holds it.
///
/// `Send` because the client keeps it across an await, and
/// `Client::execute`'s future is `Send` — a property this workspace spent
/// the whole erasure effort recovering, and which a flow that was not
/// would take back from every caller.
pub type BoxedFlow = Box<dyn AuthFlow + Send>; // send-bound-exception: amendment-C12
