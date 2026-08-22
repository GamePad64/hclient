//! What this transport failed to reach, and for how long it remembers.
//!
//! [`altsvc`](crate::altsvc) is the memory of what an origin **said**; this
//! is the memory of what happened when we believed it. `hclient-native`'s
//! `discovery` module states the division and this is the half it left
//! open: *"the cache of what was advertised is Alt-Svc's, the cache of what
//! failed is the connector's"* — and the connector that would own an HTTP/3
//! failure is `hclient-h3`, which has no failure memory of any kind, and
//! could not usefully have one, because it is not the thing that would act
//! on it. `hclient-native`'s own `NegativeCache` is a **different fact** —
//! *a TCP connect through a discovered endpoint's port, hints, ALPN or ECH
//! failed* — and it never sees an HTTP/3 attempt at all, because when this
//! transport routes to QUIC the native one is not called.
//!
//! # Why it can exist now and could not before
//!
//! A suppression without a fallback degrades the caller rather than
//! protecting them: it would cost one **failed** request per window per
//! origin, where `hclient-native`'s own negative cache costs none because
//! native falls back to the origin's addresses inside the same connect.
//!
//! The staged connect removes it, and removes it without a race. Ask the
//! QUIC stack to *connect*; a failed connect routes a request that was
//! never handed to a transport at all — no retry, no `retry_kind()`, no
//! idempotence judgement — and `hclient-native`'s sentence is true of it
//! verbatim: *this is not a second request, it is the first one, which
//! never left.* So the memory costs a **slow** first request per window per
//! origin, not a failed one.
//!
//! # Scope, and it is a sharper question than Alt-Svc's
//!
//! The advertisement cache has the same problem — a laptop that moved
//! networks is advertising an alt-authority that was reachable somewhere
//! else — and the same answer: nothing is persisted, and
//! [`Selecting::network_changed`](crate::Selecting::network_changed) is the
//! only entry point, because RFC 7838 §2.2 conditions its own SHOULD on
//! *"information about network state"* that a `Transport` does not have.
//!
//! **What is different is what a network change does to each.** Alt-Svc
//! keeps `persist=1` entries, because that flag is the *origin's* claim
//! that its advertisement is a property of the origin rather than of the
//! path. Nothing says that about a failure: *"UDP/443 did not get through"*
//! is a fact about the network and about nothing else, and no peer ever
//! asked us to carry it. So [`H3Failures::network_changed`] clears
//! **everything**, with no `persist` notion to carry, and that asymmetry is
//! the decision rather than an oversight.
//!
//! The direction of being wrong is also different, and it is worth naming
//! because it is why a fixed window is honest here where a fixed lifetime
//! for an HTTPS record was not.
//! Remembering too long costs HTTP/3 at an origin that could now serve it;
//! forgetting too soon costs one bounded connect attempt, after which the
//! request still succeeds over TCP. Neither is a wrong answer, both are
//! bounded, and both self-correct — where an invented TTL for someone
//! else's DNS answer drifts silently against the resolver's.
//!
//! # What counts as a failure
//!
//! Any failure of `hclient_h3::StagedConnect::connect`, and — since the
//! race ([`crate::race`]) — **a QUIC connect that a hedge started `H` later
//! beat to a connection**. The second is a deliberate widening of the
//! first, and the honest name for what is stored is now *"HTTP/3 did not
//! produce a connection in time to be worth using"* rather than *"an
//! `H3::connect` failed"*.
//!
//! It is not an over-reach, and the arithmetic is the argument. A QUIC
//! handshake is one round trip where TCP-plus-TLS-1.3 is two, so an arm
//! that is still connecting when a TCP connect started a whole head start
//! later has finished is not a slow HTTP/3 — it is one that is not getting
//! through. And the alternative is the cost that made the race not worth
//! building at all: without it the head start is paid again on every
//! request to a blocked origin.
//!
//! What is *not* stored is the mirror of it, which is the half that keeps
//! the rule from reading as *"the hedge suppresses HTTP/3"*: a QUIC arm
//! that **won** teaches this nothing, so the next request to that origin
//! goes to QUIC exactly as this one did.
//!
//! Beyond that the *reason* is deliberately not read. Every reason a connect fails is a reason not to
//! spend the next request's time trying again immediately, and the
//! alternative — a list of `ErrorKind`s to admit — would be a third place
//! that has to be kept in step with two crates' error vocabularies for a
//! decision whose cost of being wrong is one missed opportunity to speak
//! HTTP/3. A response that fails **after** the connect is not a failure
//! here, and that is the line the staged seam draws for us: `exchange`'s
//! errors never reach this type.

use crate::altsvc::Origin;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a failed QUIC connect keeps this transport off an origin's
/// HTTP/3.
///
/// Five minutes, and it is `hclient_native::SVCB_FAILURE_TTL`'s number
/// arrived at by the same argument rather than that constant imported: the
/// two are different facts about different protocols, and importing one
/// would make a later change to either silently change the other. What is
/// shared is the shape of the question — *how long is a connection failure
/// worth believing* — and the answer this workspace already gives it.
pub const H3_FAILURE_TTL: Duration = Duration::from_secs(300);

/// The origins whose HTTP/3 has already cost this transport a failed
/// connect.
///
/// Cheap to clone (an `Arc` bump) and every clone is the same memory — it
/// lives on one [`Selecting`](crate::Selecting) and is shared by every
/// request that transport makes, which is the whole point: a memory that
/// lasted one request would be no memory at all.
///
/// It reads no clock of its own. `now` arrives as a parameter, the shape
/// `AltSvcCache` and `hclient-native`'s `NegativeCache` both have, which is
/// what makes a five-minute window testable without waiting five minutes.
#[derive(Clone, Default)]
pub struct H3Failures {
    /// Origin -> the elapsed time, on the owning transport's `Timer` and
    /// from its epoch, past which this failure is forgotten.
    entries: Arc<Mutex<HashMap<Origin, Duration>>>,
}

impl std::fmt::Debug for H3Failures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H3Failures")
            .field(
                "suppressed",
                &self.entries.lock().map(|m| m.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl H3Failures {
    /// Whether HTTP/3 at `origin` is currently held off — and, in the same
    /// pass, forgetting the entry whose window has run out.
    ///
    /// Expiry is applied here rather than by a background sweep for
    /// `AltSvcCache`'s reason: this is the only place that asks, and an
    /// entry nobody looks up costs one small map slot until the next
    /// lookup for that origin.
    ///
    /// The comparison is strict, so an entry whose window has exactly
    /// closed is gone. That agrees with the cache next door, which is worth
    /// more than either choice is on its own: two memories consulted for
    /// one request should not disagree about what "expired" means.
    pub fn suppressed(&self, origin: &Origin, now: Duration) -> bool {
        let mut entries = self.entries.lock().expect("h3 failure memory poisoned");
        match entries.get(origin) {
            Some(until) if now < *until => true,
            Some(_) => {
                entries.remove(origin);
                false
            }
            None => false,
        }
    }

    /// A QUIC connect to `origin` failed.
    ///
    /// The window restarts from `now` on every failure rather than
    /// extending from the previous deadline, so an origin that fails once
    /// and then is never tried again is forgotten [`H3_FAILURE_TTL`] later
    /// and not longer.
    pub fn note(&self, origin: &Origin, now: Duration) {
        self.entries
            .lock()
            .expect("h3 failure memory poisoned")
            // Saturating for `AltSvcCache`'s reason: an elapsed time near
            // `Duration::MAX` is not a case to panic on.
            .insert(origin.clone(), now.saturating_add(H3_FAILURE_TTL));
    }

    /// The caller has seen a network configuration change: forget every
    /// failure, without exception.
    ///
    /// **No `persist` here, and that is the decision** — see the module
    /// doc. `persist=1` is an origin's claim about its own advertisement;
    /// nothing ever claimed that a failure to reach it belongs to the
    /// origin rather than to the path, and a failure remembered across a
    /// network change is exactly the entry that is now certainly wrong.
    pub fn network_changed(&self) {
        self.entries
            .lock()
            .expect("h3 failure memory poisoned")
            .clear();
    }
}
