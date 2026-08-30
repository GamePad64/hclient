//! RFC 9113 §6.7 `PING`, on a **shared** connection and nowhere else.
//!
//! # Why this needs `multiplexed()` and could not exist before it
//!
//! An idle pooled h2 connection has no holder at all: the request future
//! that opened it is gone, and nothing polls it until the next checkout.
//! A `PING` sent by such a connection would never be written and its
//! `PONG` never read. [`crate::Native::multiplexed`] spawns a driver, and
//! that driver is the first thing in this transport that can hold a
//! clock beside a connection.
//!
//! # Two clocks, and only one of them is the WebSocket's
//!
//! [`hclient_tungstenite::WebSocketKeepAlive`] measures **silence** — any
//! inbound frame restarts its interval, which is what makes it free on a
//! busy connection. This cannot: `h2::client::Connection` reports no
//! traffic, and a driver polling it cannot tell a poll that moved bytes
//! from one that did not. So the interval here measures *time*, and a
//! busy connection pays one `PING` frame per interval — nine bytes,
//! against a keep-alive whose whole purpose is that the path sees
//! traffic.
//!
//! The second clock is the same in both: a probe that goes unanswered
//! within its bound ends the connection. `h2` makes that half easier —
//! `poll_pong` resolves for *our* ping and there is no unsolicited pong
//! to mistake for it, which is the mutation the WebSocket keep-alive had
//! to be taught.
//!
//! # One in flight, enforced by `h2` rather than here
//!
//! `PingPong::send_ping` answers `SendPingWhilePending` if the last ping
//! has not been answered, so "never two at once" is not a state this
//! module has to keep. It cannot happen anyway — the interval clock only
//! runs while no probe is outstanding.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use hclient_core::unversioned::Timer;

/// How often a shared HTTP/2 connection sends a `PING`, and how long the
/// peer then has to answer it.
///
/// Off by default, and the default is the decision: a client that pings
/// puts traffic on the wire nobody asked for, which is exactly what the
/// WebSocket keep-alive's own default records one crate over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H2KeepAlive {
    /// The gap between one `PING` and the next.
    ///
    /// **Time, not silence** — see this module's doc for why h2 cannot
    /// offer the second.
    pub every: Duration,
    /// How long the peer has to answer before the connection is closed.
    pub within: Duration,
}

impl H2KeepAlive {
    /// Both halves, because neither is useful alone: an interval with no
    /// bound never notices a peer that stopped answering, and a bound
    /// with no interval has nothing to bound.
    #[must_use]
    pub const fn new(every: Duration, within: Duration) -> Self {
        Self { every, within }
    }
}

/// Waiting out the interval, or waiting for the pong.
enum Phase<Tm: Timer> {
    Idle(Pin<Box<Tm::Sleep>>),
    Probing(Pin<Box<Tm::Sleep>>),
}

/// What the driver holds when a caller asked for keep-alive.
pub(super) struct KeepAlive<Tm: Timer> {
    ping: h2::PingPong,
    cfg: H2KeepAlive,
    timer: Tm,
    phase: Phase<Tm>,
}

/// Why a keep-alive ended the connection.
pub(super) enum Lapsed {
    /// The peer did not answer within [`H2KeepAlive::within`].
    NoPong,
    /// `h2` refused the ping or reported the connection gone.
    Broken(h2::Error),
}

impl<Tm: Timer> KeepAlive<Tm> {
    pub(super) fn new(ping: h2::PingPong, cfg: H2KeepAlive, timer: Tm) -> Self {
        let phase = Phase::Idle(Box::pin(timer.sleep(cfg.every)));
        Self {
            ping,
            cfg,
            timer,
            phase,
        }
    }

    /// `Ready(Lapsed)` means the connection must end; `Pending` means it
    /// is being kept.
    ///
    /// Polled **after** the connection, so a connection that ended on its
    /// own is reported by its own arm and this never runs on a corpse.
    pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Lapsed> {
        loop {
            match &mut self.phase {
                Phase::Idle(sleep) => {
                    std::task::ready!(sleep.as_mut().poll(cx));
                    if let Err(e) = self.ping.send_ping(h2::Ping::opaque()) {
                        return Poll::Ready(Lapsed::Broken(e));
                    }
                    self.phase = Phase::Probing(Box::pin(self.timer.sleep(self.cfg.within)));
                }
                Phase::Probing(deadline) => {
                    match self.ping.poll_pong(cx) {
                        Poll::Ready(Ok(_)) => {
                            self.phase = Phase::Idle(Box::pin(self.timer.sleep(self.cfg.every)));
                        }
                        Poll::Ready(Err(e)) => return Poll::Ready(Lapsed::Broken(e)),
                        // The pong is what clears the probe; the deadline
                        // is only consulted while it has not arrived.
                        Poll::Pending => {
                            std::task::ready!(deadline.as_mut().poll(cx));
                            return Poll::Ready(Lapsed::NoPong);
                        }
                    }
                }
            }
        }
    }
}
