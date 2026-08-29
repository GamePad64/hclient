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

/// The predicate refused a hop.
#[derive(Debug, thiserror::Error)]
#[error("the redirect policy refused a {status} to {to} after {after_hops} hops: {why}")]
#[non_exhaustive]
pub struct RedirectRefused {
    pub to: http::Uri,
    pub status: http::StatusCode,
    /// The policy's own words — `"redirect limit reached"`,
    /// `"redirect leaves the origin"`, or whatever a caller's own policy
    /// returned. It replaces the separate `TooMany` error, which said one
    /// of these things and could not say the others.
    pub why: &'static str,
    /// How many hops had already been taken.
    ///
    /// The client's fact rather than the policy's, which is why it is
    /// here and not in the verdict: a `&'static str` cannot carry a
    /// number without allocating, and the number a reader wants is *how
    /// far did this get* — which is true of every refusal, not only a
    /// limit. It is what the old `TooMany(u8)` said, generalised.
    pub after_hops: u8,
}

/// Whether a retry the policy approved may actually happen.
///
/// **Two verdicts, where [`RedirectVerdict`](hclient_proto::redirect::RedirectVerdict) has three**, and the
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
/// [`ProposedRedirect`](hclient_proto::redirect::ProposedRedirect): what a predicate wants to see is the decision
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
/// `Fn` rather than `FnMut`, for the redirect policy's reason: it is
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
