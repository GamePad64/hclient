//! What this transport tells a [`Hooks`], and the two facts QUIC does not
//! have.
//!
//! The vocabulary is `hclient_core::unversioned::{Hooks, Event}` and it is
//! **unchanged** here: this crate is the second backend to implement it,
//! which is the test of whether the event set was a shape or one backend's
//! habits. What follows is the three places the answer was not mechanical.
//!
//! # `ConnectTiming` has four fields and QUIC has three numbers
//!
//! `dns` and `total` mean exactly what they mean over TCP. The other two
//! do not divide the same way, because **QUIC's handshake is TLS** — there
//! is no interval in which a transport connection exists and a TLS session
//! does not, and the first packet a client sends already carries its
//! `ClientHello`.
//!
//! So:
//!
//! - **`tcp` is the attempt itself**: from the moment
//!   `quinn::Endpoint::connect_with` was called to the moment the
//!   connection could carry a request. Its *name* is wrong here and its
//!   *definition* — "the winning attempt, from the moment it was launched
//!   to the moment it connected" — is exactly right, so it holds a real
//!   measured interval rather than a `Duration::ZERO` standing in for a
//!   phase that does not exist.
//! - **`tls` is always `None`**, and that is forced rather than chosen.
//!   `into_0rtt()` hands back a usable connection *before* the handshake
//!   completes, so on a 0-RTT connection there is no completed handshake
//!   to time when the request goes out — reporting one would mean waiting
//!   for the round trip 0-RTT exists to skip. A field that was `Some` on
//!   one path and `None` on the other would mean two different things
//!   under one name, and a `Some` that duplicated `tcp` would break the
//!   `dns + tcp + tls <= total` invariant `ConnectTiming` documents.
//!
//! `None` there reads as "this connection has no TLS", which is false of
//! every QUIC connection ever made. That is a defect in the seam's
//! wording rather than in this backend, and it is written down in
//! `docs/v03-acceptance.md` rather than fixed by editing `hclient-core`
//! for one backend's benefit.
//!
//! Everything between the phases — binding the endpoint, building the
//! crypto configuration, and h3's own SETTINGS exchange on top of the
//! finished handshake — is time inside `total` that belongs to no phase,
//! which is what `ConnectTiming`'s own doc means by "three measurements,
//! not a decomposition".
//!
//! # `CloseReason::Ended` has no emitter here, and that is a finding
//!
//! Over HTTP/1 a connection ends because an exchange ended it — the peer
//! closed after the response, or the response said `Connection: close`.
//! **Nothing in HTTP/3 works that way.** A QUIC connection outlives its
//! streams by construction; it goes away because it timed out, because the
//! peer sent `CONNECTION_CLOSE`, or because something failed. So `Ended`,
//! whose own doc says *"the exchange finished it"*, has no subject in this
//! crate, and this crate emits `Stale` and `Failed`.
//!
//! The one place a *graceful* end could be seen promptly is the spawned
//! connection driver, and it is shut to a hook by P13's own answer:
//! [`crate::QuinnTask`] is `Pin<Box<dyn Future + Send>>` because quinn
//! declares `Runtime::spawn` that way, and `Hooks` deliberately promises
//! no `Send` — so a future capturing an `H` does not coerce. Calling a
//! hook from there would cost every hook a `Send` bound, which is the
//! thing P13 was asked to avoid. **A close is therefore discovered rather
//! than observed**: at the next checkout that finds the connection dead,
//! or at the request or body that fails on it.
//!
//! # A shared connection can be reported closed by several requests
//!
//! `hclient-native` reports at most once per body, and that is at most
//! once per connection because an h2 connection is checked out
//! exclusively. Here a connection is *shared*, so two bodies can meet its
//! death in the same millisecond. [`ConnState`] carries the "already told"
//! flag with the connection rather than with the request, so a caller
//! counting connections is not made wrong in the direction that looks like
//! a leak.

use hclient_core::Error;
use hclient_core::unversioned::{CloseReason, Closed, ConnectionId, Event, Hooks};
use hclient_rt::Timer;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A stopwatch that does not exist when nobody is watching.
///
/// Every clock read whose only purpose is an event goes through this, and
/// `H::WATCHING` is a `const`, so on `NoHooks` the `then` is a compile-time
/// `false` and there is nothing left to remove — not a branch, not a call.
/// `crates/hclient-h3/tests/hooks_cost.rs` counts the reads from outside,
/// on a runtime whose clock reports how often it was asked.
///
/// The same two functions `hclient-native` has, written again rather than
/// shared: they are four lines, and the alternative is a dependency from
/// the QUIC crate on the TCP one for the sake of them.
pub(crate) fn mark<H: Hooks, R: Timer>(rt: &R) -> Option<R::Instant> {
    H::WATCHING.then(|| rt.now())
}

/// The other half of [`mark`]: the interval since one, or `ZERO` when there
/// was no mark to measure from.
///
/// The `ZERO` is never reported — a build that produced `None` above has no
/// hook to hand it to.
pub(crate) fn since<R: Timer>(rt: &R, at: Option<R::Instant>) -> Duration {
    match at {
        Some(t) => rt.elapsed_since(t),
        None => Duration::ZERO,
    }
}

/// A connection's identity for the observability seam, and whether its end
/// has been told.
///
/// Lives in the pool beside the connection and travels into every response
/// body opened on it, so the `Closed` naming a connection is the same
/// number its `Connected` did — and so that the end is announced **once**
/// however many requests are sharing it.
#[derive(Debug)]
pub(crate) struct ConnState {
    id: ConnectionId,
    told: AtomicBool,
}

impl ConnState {
    /// A state for a connection about to be made, or `None` when nobody
    /// will read it — so a no-hook build does not touch the process-wide
    /// counter and does not allocate once per connection.
    pub(crate) fn new<H: Hooks>() -> Option<Arc<Self>> {
        H::WATCHING.then(|| {
            Arc::new(Self {
                id: ConnectionId::next(),
                told: AtomicBool::new(false),
            })
        })
    }

    /// The id to put on an event about this connection.
    pub(crate) fn id(state: Option<&Arc<Self>>) -> ConnectionId {
        state.map_or(ConnectionId::UNWATCHED, |s| s.id)
    }

    /// Tell the hook this connection is over — **at most once**, across
    /// every request that shares it.
    ///
    /// The `swap` is the "at most once" and it is not bookkeeping for its
    /// own sake: a connection carrying three requests fails all three, and
    /// three `Closed` events for one connection would make a caller
    /// counting connections wrong.
    ///
    /// Never called from a `Drop` impl and never with a lock held — the two
    /// rules `hclient_core::unversioned::hooks`'s module doc states.
    pub(crate) fn closed<H: Hooks>(&self, hooks: &H, reason: CloseReason<'_>) {
        if self.told.swap(true, Ordering::SeqCst) {
            return;
        }
        hooks.on(Event::Closed(Closed {
            id: self.id,
            reason,
        }));
    }
}

/// What a response body needs in order to report the end of the connection
/// underneath it: the hook, the connection to ask, and the state to report
/// through.
///
/// Boxed behind an `Option` where it is held, so a request that wants none
/// of this carries eight bytes rather than the whole of it — and so that
/// `H3Body` stays `Unpin` for every `H`, which `hclient-select` requires of
/// it.
pub(crate) struct Watch<H> {
    hooks: H,
    conn: quinn::Connection,
    state: Arc<ConnState>,
}

// Hand-written: `H` is not required to be `Debug`, and requiring it would
// be a bound on the caller's hook for the benefit of a formatter.
impl<H> std::fmt::Debug for Watch<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch").field("id", &self.state.id).finish()
    }
}

impl<H: Clone> Clone for Watch<H> {
    fn clone(&self) -> Self {
        Self {
            hooks: self.hooks.clone(),
            conn: self.conn.clone(),
            state: self.state.clone(),
        }
    }
}

impl<H: Hooks> Watch<H> {
    pub(crate) fn new(hooks: H, conn: quinn::Connection, state: Arc<ConnState>) -> Self {
        Self { hooks, conn, state }
    }

    /// Report a failure **if it was the connection's and not one stream's**.
    ///
    /// `close_reason()` is the discriminator and there is no other: a
    /// `RESET_STREAM` for one request, a server that stopped reading a
    /// body, an h3 stream error — all of those fail a request on a
    /// connection that is still carrying its neighbours, and announcing
    /// them as a close would be the loudest possible lie about a transport
    /// whose whole point is that one stream's death is not the
    /// connection's.
    pub(crate) fn failed(&self, e: &Error) {
        if self.conn.close_reason().is_some() {
            self.state.closed(&self.hooks, CloseReason::Failed(e));
        }
    }
}
