//! Whether to send a request again, and how long to wait first.
//!
//! **Sans-io and clockless**, like every module here: [`RetryPolicy`] is a
//! rule, [`RetryPolicy::decide`] is a pure function of the rule and one
//! outcome, and nothing in this file reads a clock, draws entropy or
//! touches a socket. `hclient::Client` supplies the jitter, the sleep and
//! the remaining budget.
//!
//! # What makes this different from a middleware that wraps a client
//!
//! The retry crates people bolt onto other clients sit **above** the
//! transport, so they cannot tell a request that never left from one a
//! server received and acted on. Both look like an error. So they either
//! retry both — repeating a request the server may have processed — or
//! neither.
//!
//! Here the two are different values. [`Outcome::Unsent`] is a claim the
//! transport can make and does: `hclient-native` learns from hyper whether
//! the request was written before the connection failed, and hands back a
//! failure that says which. And whether a body *can* be sent twice is
//! `RequestBody::retry_kind()`, which is known **before** the first
//! attempt rather than discovered after it — a streaming body is
//! `RetryKind::Impossible` and no policy can override it.
//!
//! So the safe retry is expressible without asking the caller to promise
//! anything, and it is the default: [`RetryPolicy::default`] retries what
//! provably never arrived and nothing else.
//!
//! # A status retry is a different promise, and it is opt-in
//!
//! A response means the request reached a server. Retrying it may repeat
//! work that was already done — this codebase deliberately has no notion
//! of method safety (see `hclient-core`'s `RetryKind`, whose own doc says
//! *"can I send this again" and "may this be repeated" are different
//! questions*), so it cannot decide for the caller. [`RetryStatuses`] is
//! therefore a choice the caller makes, and `None` is what they get by
//! default.
//!
//! # `Retry-After` that cannot be honoured stops the retry
//!
//! A server asking for a longer wait than [`RetryPolicy::max_retry_after`]
//! is refused, **not rounded down**. Waiting less than a server asked is
//! the one behaviour the header exists to prevent, so a client that caps
//! the value and retries anyway has turned a limit into a violation. The
//! same rule covers the `HTTP-date` form, which this module deliberately
//! does not parse: an unreadable instruction is a reason to stop rather
//! than a reason to guess.

use core::time::Duration;

use crate::backoff::Backoff;

/// Which response statuses a caller is willing to have repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RetryStatuses {
    /// None. A response is an answer, and this client keeps it.
    ///
    /// The default, because a repeat can duplicate work the server has
    /// already done and only the caller knows whether that is acceptable.
    #[default]
    None,
    /// `408`, `429`, `500`, `502`, `503` and `504`.
    ///
    /// The set curl retries, and it is narrower than *every 5xx* on
    /// purpose: `501 Not Implemented` and `505 HTTP Version Not
    /// Supported` are statements about the request that a repeat cannot
    /// change, so retrying them is load with no chance of success.
    Transient,
    /// `408`, `429` and **every** `5xx`.
    ///
    /// What the popular middleware for other clients does. Wider than
    /// [`Self::Transient`] by exactly the statuses a repeat cannot fix,
    /// and offered because a deployment behind a proxy that answers `501`
    /// for a transient condition is a real thing and only its operator
    /// knows.
    AnyServerError,
}

impl RetryStatuses {
    /// Whether this set contains `status`.
    #[must_use]
    pub fn contains(self, status: http::StatusCode) -> bool {
        let code = status.as_u16();
        match self {
            Self::None => false,
            Self::Transient => matches!(code, 408 | 429 | 500 | 502 | 503 | 504),
            Self::AnyServerError => code == 408 || code == 429 || (500..600).contains(&code),
        }
    }
}

/// What one attempt produced.
///
/// `#[non_exhaustive]` and built through [`Outcome::status`], because a
/// third kind of outcome is a thing this module may learn about and a
/// caller matching exhaustively on it should be told rather than silently
/// take the wrong arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The attempt failed and the request **provably never reached a
    /// server** — the connection was refused, the name did not resolve,
    /// the connect timed out.
    ///
    /// This is the outcome a middleware above a transport cannot
    /// distinguish, and it is the only one [`RetryPolicy::default`]
    /// retries.
    Unsent,
    /// A response came back.
    ///
    /// `retry_after` is the header's `delta-seconds` form where the
    /// server sent one. Its `HTTP-date` form is deliberately not parsed
    /// here — see the module doc.
    Status {
        status: http::StatusCode,
        retry_after: Option<Duration>,
        /// The server sent a `Retry-After` this module could not read.
        ///
        /// Distinct from `retry_after: None`, which means the server said
        /// nothing: an instruction that exists and is unreadable stops
        /// the retry, where silence lets the backoff decide.
        retry_after_unreadable: bool,
    },
}

impl Outcome {
    /// A response, with what its `Retry-After` said.
    ///
    /// `retry_after` is [`retry_after_seconds`]'s answer and
    /// `retry_after_unreadable` is whether the header was **present**
    /// while that answer was `None` — the pair the module doc is about,
    /// and the reason this is a constructor rather than a literal: the
    /// two are easy to conflate and only one of them stops a retry.
    #[must_use]
    pub fn status(
        status: http::StatusCode,
        retry_after: Option<Duration>,
        retry_after_unreadable: bool,
    ) -> Self {
        Self::Status {
            status,
            retry_after,
            retry_after_unreadable,
        }
    }
}

/// Why a retry is not happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stop {
    /// This outcome is not one the policy retries.
    NotRetryable,
    /// The body cannot be sent a second time.
    ///
    /// Known from `RequestBody::retry_kind()` before the first attempt,
    /// not discovered after it.
    BodyCannotBeReplayed,
    /// [`Backoff::max_attempts`] is spent.
    OutOfAttempts,
    /// The server asked for a longer wait than
    /// [`RetryPolicy::max_retry_after`], or sent one this module cannot
    /// read. Refused rather than rounded down.
    RetryAfterTooLong,
}

/// The answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Wait this long, then send the request again.
    After(Duration),
    /// Keep what you have.
    Stop(Stop),
}

/// When to send a request again.
///
/// See the module doc for why the default is what it is.
///
/// **Deliberately not `#[non_exhaustive]`**, copying `TcpOpts` and
/// `H2Opts`: its whole use is
/// `RetryPolicy { statuses: .., ..Default::default() }`, and the
/// attribute forbids exactly that expression — functional update included
/// — from outside this crate, leaving per-field setters that exist only
/// to work around it. The cost is that adding a field is a major version;
/// that is the trade this workspace makes wherever the caller is the one
/// building the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// How long to wait, and how many times.
    pub backoff: Backoff,
    /// Retry a failure whose request provably never reached a server.
    ///
    /// `true` by default: there is nothing at the server to duplicate, so
    /// this needs no promise from the caller.
    pub retry_unsent: bool,
    /// Which response statuses to repeat. [`RetryStatuses::None`] by
    /// default.
    pub statuses: RetryStatuses,
    /// The longest `Retry-After` this client will wait for.
    ///
    /// A server asking for longer stops the retry rather than being
    /// rounded down to this.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            backoff: Backoff {
                base: Duration::from_millis(100),
                max: Duration::from_secs(10),
                max_attempts: Some(3),
            },
            retry_unsent: true,
            statuses: RetryStatuses::None,
            max_retry_after: Duration::from_secs(60),
        }
    }
}

impl RetryPolicy {
    /// The default, plus [`RetryStatuses::Transient`].
    #[must_use]
    pub fn transient() -> Self {
        Self {
            statuses: RetryStatuses::Transient,
            ..Self::default()
        }
    }

    /// Whether to send the request again, and after how long.
    ///
    /// `attempt` counts attempts **already made**, so the first call
    /// after the first send passes `1`. `jitter` is a fraction in `0..=1`
    /// taken as a parameter rather than drawn here, for
    /// [`Backoff::delay`]'s own reason: a function that reads live
    /// entropy can only be tested statistically.
    ///
    /// `body_replayable` is `RequestBody::retry_kind().may_replay()` —
    /// passed in rather than inspected, because this crate is sans-io and
    /// a body is not.
    #[must_use]
    pub fn decide(
        &self,
        outcome: Outcome,
        attempt: u32,
        jitter: f64,
        body_replayable: bool,
    ) -> Verdict {
        // Asked before anything else, because it is the one condition no
        // policy may override and no server may ask us past.
        if !body_replayable {
            return Verdict::Stop(Stop::BodyCannotBeReplayed);
        }

        let asked = match outcome {
            Outcome::Unsent => {
                if !self.retry_unsent {
                    return Verdict::Stop(Stop::NotRetryable);
                }
                None
            }
            Outcome::Status {
                status,
                retry_after,
                retry_after_unreadable,
            } => {
                if !self.statuses.contains(status) {
                    return Verdict::Stop(Stop::NotRetryable);
                }
                // An unreadable instruction is refused rather than
                // ignored: ignoring it retries sooner than the server
                // asked, which is the one thing the header forbids.
                if retry_after_unreadable {
                    return Verdict::Stop(Stop::RetryAfterTooLong);
                }
                retry_after
            }
        };

        let Some(backoff) = self.backoff.delay(attempt, jitter) else {
            return Verdict::Stop(Stop::OutOfAttempts);
        };

        match asked {
            // The server named a wait. It is a floor and not a hint —
            // taking `max(backoff, asked)` rather than `asked` alone, so
            // a server asking for less than our own backoff does not
            // shorten it.
            Some(asked) if asked > self.max_retry_after => Verdict::Stop(Stop::RetryAfterTooLong),
            Some(asked) => Verdict::After(asked.max(backoff)),
            None => Verdict::After(backoff),
        }
    }
}

/// Reads `Retry-After`'s `delta-seconds` form.
///
/// **`None` means the server gave an instruction this module cannot
/// follow**, never that it said nothing — the absence of the header is a
/// different code path, and the two must not be merged: silence lets the
/// backoff decide, where an unreadable instruction stops the retry.
///
/// The date form is not read here because doing so needs a calendar, and
/// this crate is the sans-io leaf whose dependency count is guarded. It
/// is a narrowing with a direction: the failure is to stop rather than to
/// retry sooner than asked.
///
pub fn retry_after_seconds(value: &str) -> Option<Duration> {
    let v = value.trim();
    // RFC 9110 §10.2.3 is `delta-seconds = 1*DIGIT`, so a sign, a decimal
    // point or anything else is the date form or a malformation, and both
    // land in the same place.
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    v.parse::<u64>().ok().map(Duration::from_secs)
}
