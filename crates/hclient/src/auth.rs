//! Multi-leg authentication as a seam.
//!
//! # Why a seam rather than more built-in schemes
//!
//! Digest is implemented here and NTLM and Negotiate are not, because
//! they need a platform's own security provider — and the measurement
//! says that is exactly where a seam beats a feature. `sspi` is at
//! **561,893** downloads a month, `libgssapi` at **565,785** and
//! `cross-krb5` at **496,315**; the primitives have real users. What does
//! not exist, on crates.io, is any HTTP glue over them: there is no
//! `reqwest-ntlm` and no `hyper-ntlm`, because those clients have nowhere
//! to put one.
//!
//! So this crate does not grow a Kerberos dependency. It grows the two
//! traits somebody else needs in order to write one in their own crate.
//!
//! # The shape, and why it has two traits
//!
//! [`Auth`] is the configuration — a username and password, a credentials
//! handle — and is shared by every clone of the client and every request
//! in flight. [`AuthFlow`] is **one exchange's state**, made fresh per
//! hop, because a scheme with more than one leg has to remember which leg
//! it is on and a shared value cannot.
//!
//! That is the same split the redirect and retry policies make for the
//! same reason: the policy holds the rule, the client holds the state.
//! Here the state is per hop rather than per operation, so it is the flow
//! that is created rather than a counter the client keeps.
//!
//! ```text
//!   flow = auth.start()
//!   loop {
//!       flow.authorize(method, uri, &mut headers)   // before every send
//!       response = send()
//!       match flow.on_response(status, headers) {
//!           Done  => the response is the answer
//!           Again => go round, at most MAX_LEGS times
//!       }
//!   }
//! ```
//!
//! It is httpx's generator-based `Auth` written as a state machine, which
//! is what Rust has instead of `response = yield request`.
//!
//! # Three rules the client enforces and a flow cannot override
//!
//! **A body that cannot be replayed ends it.** `RequestBody::retry_kind()`
//! is asked before the second leg, exactly as it is for `425` and for a
//! retry, and a `Streaming` body means the first response stands.
//!
//! **Credentials do not cross an origin.** The flow is per hop and is not
//! carried across one that changed host or scheme — the rule digest
//! already followed, now true of every scheme.
//!
//! **The legs are bounded.** [`MAX_LEGS`] attempts and then a named
//! error: a flow that never says [`AuthStep::Done`] would otherwise be an
//! infinite exchange against a server that keeps challenging.

//! # What does not need a scheme, and the one thing this cannot do
//!
//! **Basic does not.** [`crate::RequestBuilder::basic_auth`] sets one
//! header and there is no challenge to answer; wrapping that in an `Arc`
//! and a per-hop flow would buy nothing. What the seam *does* make
//! expressible, and was not before, is Basic that waits to be
//! challenged — curl's `--anyauth` — which matters because sending it
//! pre-emptively hands the password to whatever is at that URL. Fifteen
//! lines here, and not shipped, because shipping it would be
//! speculation.
//!
//! **OAuth 2 does not either, and the ecosystem has already settled
//! that.** The token exchange belongs to `oauth2` (11,633,197 downloads a
//! month), `yup-oauth2` (4,774,159) or `openidconnect` (3,870,380);
//! sending the result is [`crate::RequestBuilder::bearer_auth`]. There is
//! no `reqwest-oauth2` on crates.io, and the reason is not that it is
//! impossible — it is that nobody needs one.
//!
//! **What this seam cannot do is refresh a token from inside a flow.**
//! [`AuthFlow::on_response`] is synchronous, so a flow cannot await an
//! HTTP request of its own on a `401`. httpx supports that
//! (`async_auth_flow`), so the want is real; here it would cost a boxed
//! future in the seam — auto traits lost, amendment C1's tax — paid by
//! every caller, for a case the practical pattern avoids: both `oauth2`
//! and `yup-oauth2` hand out a token with an expiry and are refreshed
//! **by that expiry before the request**, not in reaction to the answer.
//! `yup-oauth2::Authenticator::token()` is awaited by the caller and its
//! result passed in.
//!
//! So the cost of the absence is one loop in a caller's code, and the
//! cost of the presence would be `Client::execute`'s future — a property
//! this workspace spent the erasure work recovering. Stated here rather
//! than discovered later.

/// The most attempts one hop's authentication may take.
///
/// Digest needs two — the challenge and the answer. NTLM and Negotiate
/// need three. Four leaves room for a scheme with one more leg and still
/// bounds a flow that never finishes, which is the failure this exists
/// to make impossible rather than merely unlikely.
pub const MAX_LEGS: u8 = 4;

/// What a flow says after seeing a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStep {
    /// This response is the answer.
    Done,
    /// Send the request again; [`AuthFlow::authorize`] will be called
    /// first and may set a different header.
    Again,
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
    fn authorize(&mut self, method: &http::Method, uri: &http::Uri, headers: &mut http::HeaderMap);

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
/// in [`BoxedFlow`] and [`SharedAuth`], and are amendment C12's shape —
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

/// A scheme, as the client stores it — shared by every clone.
pub type SharedAuth = std::sync::Arc<dyn Auth + Send + Sync>; // send-bound-exception: amendment-C12

/// A flow ran out of legs.
#[derive(Debug, thiserror::Error)]
#[error("authentication did not finish in {MAX_LEGS} attempts")]
pub struct TooManyLegs;

/// RFC 7616 digest, as an [`Auth`].
///
/// The scheme this crate implements itself, and the one the seam was
/// generalised out of: it was a hard-coded `401` branch before there was
/// anywhere else for a scheme to live.
#[cfg(feature = "digest-auth")]
#[derive(Debug, Clone)]
pub struct Digest {
    user: String,
    password: String,
}

#[cfg(feature = "digest-auth")]
impl Digest {
    #[must_use]
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            password: password.into(),
        }
    }
}

#[cfg(feature = "digest-auth")]
impl Auth for Digest {
    fn start(&self) -> BoxedFlow {
        Box::new(DigestFlow {
            user: self.user.clone(),
            password: self.password.clone(),
            seen: None,
            answer: None,
            answered: false,
        })
    }
}

/// Two legs: the challenge, then the answer.
///
/// **It stashes the method and target in `authorize`**, because RFC 7616
/// §3.4.2 hashes both into `A2` and `on_response` is deliberately given
/// only the response. That is the state machine's ordinary shape — the
/// trait's contract is that `authorize` runs before every send, so the
/// facts are always in hand by the time a challenge arrives — and it is
/// the reason `on_response` does not also take them: a flow that needs
/// them has already seen them.
#[cfg(feature = "digest-auth")]
struct DigestFlow {
    user: String,
    password: String,
    /// The last request's method and request-target, from `authorize`.
    seen: Option<(http::Method, String)>,
    /// Set by `on_response` from a `401`, read by the next `authorize`.
    answer: Option<http::HeaderValue>,
    /// Whether a challenge has already been answered, so a server that
    /// keeps sending `401` gets two attempts rather than `MAX_LEGS`.
    answered: bool,
}

#[cfg(feature = "digest-auth")]
impl AuthFlow for DigestFlow {
    fn authorize(&mut self, method: &http::Method, uri: &http::Uri, headers: &mut http::HeaderMap) {
        // The **request-target**, not the URL: §3.4.2 hashes what goes on
        // the request line, and a full URL would give the server a
        // different `A2` and a second `401` nobody could explain.
        let target = uri
            .path_and_query()
            .map_or_else(|| uri.path().to_owned(), ToString::to_string);
        self.seen = Some((method.clone(), target));
        if let Some(v) = self.answer.take() {
            headers.insert(http::header::AUTHORIZATION, v);
        }
    }

    fn on_response(&mut self, status: http::StatusCode, headers: &http::HeaderMap) -> AuthStep {
        // **One answer per hop, by construction rather than by a
        // counter.** A server wedged on `401` gets two attempts and the
        // caller gets the second `401` — the same shape the `425` replay
        // has, and the reason `MAX_LEGS` is a backstop here rather than
        // the bound that does the work.
        if status != http::StatusCode::UNAUTHORIZED || self.answered {
            return AuthStep::Done;
        }
        let Some((method, target)) = self.seen.as_ref() else {
            return AuthStep::Done;
        };
        let Ok(challenge) =
            crate::digest::best_challenge(headers.get_all("www-authenticate").iter())
        else {
            return AuthStep::Done;
        };
        let value = crate::digest::answer(
            &challenge,
            &self.user,
            &self.password,
            method,
            target,
            &crate::digest::cnonce(),
        );
        let Ok(mut v) = http::HeaderValue::from_str(&value) else {
            return AuthStep::Done;
        };
        v.set_sensitive(true);
        self.answer = Some(v);
        self.answered = true;
        AuthStep::Again
    }
}
