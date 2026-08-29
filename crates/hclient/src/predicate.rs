//! A caller's own say over each redirect hop.
//!
//! [`RedirectPolicy`](hclient_proto::redirect::RedirectPolicy) answers *how
//! many* and this answers *whether this one* — a rule about where a client
//! may be sent, which no counter can express: refusing a hop to a private
//! address, to a different host, or away from the scheme the caller
//! started on.
//!
//! # Why it is not a variant of `RedirectPolicy`
//!
//! That was the obvious shape and it is wrong twice over.
//!
//! `RedirectPolicy` lives in **`hclient-proto`, which is sans-io and
//! clockless**, and `redirect::decide` is a pure function of six values —
//! a property this workspace leans on hard enough that the module's first
//! line says so. A closure variant would put a caller's arbitrary code
//! inside that function, and "pure" would become "pure except for
//! whatever the caller passed".
//!
//! And `RedirectPolicy` is `Copy + PartialEq + Eq`, which a boxed closure
//! ends. Those are not decorations: it is read out of a request's
//! extensions with `.copied()` and compared in tests.
//!
//! So `decide` is unchanged, and the predicate is asked **after** it, only
//! about a hop it already approved. That ordering is the useful one rather
//! than a concession: the predicate is handed the *resolved* target, the
//! method the hop would use and whether credentials are about to be
//! stripped — all of which are `decide`'s output, and none of which a
//! predicate consulted first could see.
//!
//! # What it costs
//!
//! One `Send + Sync` bound, on the opt-in setter and nowhere else — spec
//! amendment C12, whose subject is exactly this: a bound this crate
//! chooses so that a caller's own value reaches `Client` without becoming
//! a type parameter on it. Without it every `Client` would stop crossing a
//! `tokio::spawn`, configured predicate or not, which is a far larger
//! change than the one being asked for.

use std::fmt::Debug;
use std::sync::Arc;

/// What a [predicate](crate::ClientBuilder::redirect_predicate) says about
/// one hop.
///
/// Three answers and not two, because this crate already distinguishes the
/// last two everywhere else and folding them here would lose the
/// distinction at the one place it matters most. `RedirectPolicy::None`
/// hands the `3xx` back and an exceeded `Limited` is an error, under a
/// rule `redirect.rs` states outright: *"do not follow" is a `Stop`, not
/// an error: the 3xx is the caller's answer, not a failure to reach one.*
///
/// A predicate that could only [`Stop`](Self::Stop) would make an SSRF
/// guard hand back a `3xx` the caller must then remember to check — and a
/// caller who forgets gets a silent success where they asked for a
/// refusal. That is the shape of defect this workspace calls a capability
/// that lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectVerdict {
    /// Take the hop.
    Follow,
    /// Do not, and hand the `3xx` to the caller as the answer — exactly
    /// what [`RedirectPolicy::None`](hclient_proto::redirect::RedirectPolicy::None)
    /// does for a whole client.
    Stop,
    /// Do not, and fail:
    /// [`ErrorKind::Redirect`](hclient_core::ErrorKind::Redirect) with a
    /// [`RedirectRefused`] source naming the hop.
    Refuse,
}

/// The hop a predicate is being asked about: computed, not taken.
///
/// Everything here is already known by the time the question is asked, so
/// none of it costs a caller who configures no predicate.
#[derive(Debug, Clone, Copy)]
pub struct ProposedRedirect<'a> {
    from: &'a http::Uri,
    to: &'a http::Uri,
    status: http::StatusCode,
    method: &'a http::Method,
    cross_origin: bool,
    hops: u8,
    previous: &'a [http::Uri],
}

impl<'a> ProposedRedirect<'a> {
    pub(crate) fn new(
        from: &'a http::Uri,
        to: &'a http::Uri,
        status: http::StatusCode,
        method: &'a http::Method,
        cross_origin: bool,
        hops: u8,
        previous: &'a [http::Uri],
    ) -> Self {
        Self {
            from,
            to,
            status,
            method,
            cross_origin,
            hops,
            previous,
        }
    }

    /// Every URI already requested in this operation, oldest first, ending
    /// with [`from`](Self::from).
    ///
    /// **What this is for is a ring, which a count cannot see.** Two hosts
    /// a policy allows can redirect to each other for ever, and
    /// [`hops`](Self::hops) only bounds how long that takes — it cannot
    /// say the destination has been visited. `previous().contains(to)` can:
    ///
    /// ```
    /// # use hclient::redirect::{ProposedRedirect, RedirectVerdict};
    /// # fn check(hop: &ProposedRedirect<'_>) -> RedirectVerdict {
    /// if hop.previous().contains(hop.to()) {
    ///     return RedirectVerdict::Refuse;
    /// }
    /// # RedirectVerdict::Follow
    /// # }
    /// ```
    ///
    /// **It is one longer than [`hops`](Self::hops)**, and that is worth
    /// reading twice because the two are otherwise easy to swap: `hops`
    /// counts redirects already *followed*, `previous` lists URIs already
    /// *requested*, and the operation's original URI is in the second and
    /// not the first. At the first redirect decision `hops()` is `0` and
    /// `previous()` has one element.
    ///
    /// **Empty unless a predicate is installed.** The chain costs a `Uri`
    /// clone per hop, and a client that never asks about a hop should not
    /// pay for one — so `Client` accumulates it only when there is
    /// something to hand it to. A predicate always sees it filled.
    #[must_use]
    pub fn previous(&self) -> &[http::Uri] {
        self.previous
    }

    /// Where this hop would leave from — the URI of the request that was
    /// answered with the `3xx`, not the one the caller first asked for.
    pub fn from(&self) -> &http::Uri {
        self.from
    }

    /// Where it would go: the `Location` header **resolved** against
    /// [`from`](Self::from) by RFC 3986 §5.2, so a relative `Location` is
    /// already absolute here and a predicate never has to resolve one
    /// itself.
    pub fn to(&self) -> &http::Uri {
        self.to
    }

    /// The `3xx` that proposed it.
    pub fn status(&self) -> http::StatusCode {
        self.status
    }

    /// The method the hop would use — already downgraded to `GET` where
    /// the status calls for it, so a predicate reasoning about `POST`
    /// reads what would actually go out.
    pub fn method(&self) -> &http::Method {
        self.method
    }

    /// Whether the host, scheme or port changes, i.e. whether
    /// `Authorization`, `Cookie` and `Proxy-Authorization` are about to be
    /// stripped.
    ///
    /// The same value that drives the stripping rather than one computed
    /// again beside it, so a predicate refusing cross-origin hops and the
    /// client removing credentials cannot disagree about what an origin is.
    pub fn cross_origin(&self) -> bool {
        self.cross_origin
    }

    /// How many hops this chain has already taken. `0` on the first `3xx`.
    ///
    /// **One less than [`previous`](Self::previous)'s length**, because
    /// this counts redirects *followed* where that lists URIs
    /// *requested*, and the operation's original URI has been requested
    /// without a redirect having been followed to reach it. Use this for
    /// a cap and `previous` for a ring; the pair is pinned by
    /// `tests/redirect.rs`.
    pub fn hops(&self) -> u8 {
        self.hops
    }
}

/// The predicate refused a hop.
#[derive(Debug, thiserror::Error)]
#[error("the redirect policy refused a {status} to {to}")]
#[non_exhaustive]
pub struct RedirectRefused {
    pub to: http::Uri,
    pub status: http::StatusCode,
}

/// A caller's redirect predicate, as `Client` holds it.
///
/// `Fn` and not `FnMut`: this is shared by every clone of the client and
/// every request in flight, so a `FnMut` would need a lock, and a lock
/// taken on every hop of every request for the sake of predicates that
/// mostly hold no state is the wrong default. A predicate that does need
/// state uses its own interior mutability and pays for it alone.
#[derive(Clone)]
pub struct RedirectPredicate(
    Arc<dyn Fn(&ProposedRedirect<'_>) -> RedirectVerdict + Send + Sync>, // send-bound-exception: amendment-C12
);

impl RedirectPredicate {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&ProposedRedirect<'_>) -> RedirectVerdict + Send + Sync + 'static, // send-bound-exception: amendment-C12
    {
        Self(Arc::new(f))
    }

    pub(crate) fn ask(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        (self.0)(hop)
    }
}

/// Hand-written, for [`crate::erased::AnyList`]'s reason one module over: a trait
/// object has no `Debug`, and there is nothing honest to print about a
/// closure anyway.
impl Debug for RedirectPredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RedirectPredicate(..)")
    }
}

/// Whether a retry the policy approved may actually happen.
///
/// **Two verdicts, where [`RedirectVerdict`] has three**, and the
/// difference is that the third arm there had a subject and here does
/// not. A predicate refusing a redirect needs `Refuse` because the
/// alternative — handing back the `3xx` — is a *success* the caller might
/// forget to check. Declining a retry hands back what the attempt already
/// produced: the server's own status, or the transport's own error. Both
/// are the honest answer to the request, so there is nothing an error of
/// ours would add and a status it could hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryVerdict {
    /// Send it again.
    Retry,
    /// Keep what this attempt produced.
    Stop,
}

/// A retry the [`RetryPolicy`](hclient_proto::retry::RetryPolicy) has
/// already approved, offered to the caller's predicate.
///
/// **Everything here is the policy's *output*, which is why the predicate
/// is asked afterwards** — the same order and the same reason as
/// [`ProposedRedirect`]: what a predicate wants to see is the decision
/// that was reached, not the inputs it was reached from.
#[derive(Debug, Clone, Copy)]
pub struct ProposedRetry<'a> {
    method: &'a http::Method,
    uri: &'a http::Uri,
    attempt: u32,
    outcome: hclient_proto::retry::Outcome,
    delay: std::time::Duration,
}

impl<'a> ProposedRetry<'a> {
    pub(crate) fn new(
        method: &'a http::Method,
        uri: &'a http::Uri,
        attempt: u32,
        outcome: hclient_proto::retry::Outcome,
        delay: std::time::Duration,
    ) -> Self {
        Self {
            method,
            uri,
            attempt,
            outcome,
            delay,
        }
    }

    /// The method of the request that would be sent again.
    ///
    /// **This is the field the predicate exists for.** This workspace
    /// deliberately has no notion of method safety — `RetryKind` answers
    /// *can this be sent again*, never *may this be repeated*, and those
    /// are different questions that disagree on the same request: a
    /// `POST /transfer` with a buffered body is trivially replayable and
    /// is exactly what must not be repeated. Only the caller can answer
    /// the second, so the client hands them the method rather than
    /// guessing a rule.
    #[must_use]
    pub fn method(&self) -> &http::Method {
        self.method
    }

    /// The target, as the hop was addressed — absolute.
    #[must_use]
    pub fn uri(&self) -> &http::Uri {
        self.uri
    }

    /// Attempts already made, so the first proposed retry reports `1`.
    #[must_use]
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// What the attempt produced — a status, or a failure the transport
    /// says never reached a server.
    #[must_use]
    pub fn outcome(&self) -> hclient_proto::retry::Outcome {
        self.outcome
    }

    /// How long the policy decided to wait, `Retry-After` included.
    ///
    /// Offered so a predicate can refuse a wait it finds too long for
    /// this particular target. It cannot *change* it: a predicate that
    /// returned a duration would be a second policy, and then two things
    /// would decide one number.
    #[must_use]
    pub fn delay(&self) -> std::time::Duration {
        self.delay
    }
}

/// The caller's own say over each retry.
///
/// `Fn` rather than `FnMut`, for [`RedirectPredicate`]'s reason: it is
/// shared by every clone of the client and every request in flight, so
/// `FnMut` would mean a lock taken on every attempt of every request for
/// the sake of predicates that mostly hold no state.
#[derive(Clone)]
pub struct RetryPredicate(
    Arc<dyn Fn(&ProposedRetry<'_>) -> RetryVerdict + Send + Sync>, // send-bound-exception: amendment-C12
);

impl RetryPredicate {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&ProposedRetry<'_>) -> RetryVerdict + Send + Sync + 'static, // send-bound-exception: amendment-C12
    {
        Self(Arc::new(f))
    }

    pub(crate) fn ask(&self, proposed: &ProposedRetry<'_>) -> RetryVerdict {
        (self.0)(proposed)
    }
}

impl Debug for RetryPredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RetryPredicate(..)")
    }
}
