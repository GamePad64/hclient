//! `Timeouts::between_bytes` — a bound on the **gap** between two reads of
//! a response body, enforced by racing a real sleep rather than by looking
//! at a clock.
//!
//! # Why this cannot be done by checking elapsed time
//!
//! It is the same measurement `hclient::Deadline`'s doc comment records,
//! from the other end. A wrapper that checks the clock on each
//! `poll_frame` can only fire when something polls it, and nothing polls a
//! body whose server has gone completely silent: the inner body returns
//! `Pending`, its waker is parked on a socket that will never be readable
//! again, and no wake is ever delivered. Measured there with a counting
//! waker and no executor at all: **zero** wakes after one `Pending` poll,
//! so such a wrapper can never fire, against one wake and a timely firing
//! for a wrapper that holds a sleep.
//!
//! That is exactly the case `total` deliberately does not cover, and
//! exactly what `between_bytes` is for. Holding a sleep is possible here
//! because [`hclient_core::unversioned::Timer`] carries an associated
//! `Sleep` type: `Pin<Box<Tm::Sleep>>` is a box around a **concrete**
//! type, so auto traits pass straight through it and
//! `IdleTimeout<NativeBody<..>, Tokio>` stays `Send` exactly as
//! `NativeBody` alone was (`tests/shape.rs`).
//!
//! # What "between bytes" means here, precisely
//!
//! The clock starts when the inner body first answers `Pending` — that is,
//! when this transport has asked for more of the response and the peer has
//! nothing to give — and is thrown away and restarted on **every** frame.
//! Two consequences worth stating, because a caller can act on both:
//!
//! - The gap between the response head and the first body frame is
//!   covered. A server that sends a complete head and then falls silent is
//!   the case this exists for, and it is bounded from the moment the body
//!   is first polled.
//! - Time in which the **caller** is not polling is not counted. A
//!   consumer that reads a frame and then goes away for a minute has not
//!   been kept waiting by the server, and cutting its transfer would be
//!   reporting the caller's own delay as the peer's fault.
//!
//! # A separate wrapper rather than a field of `NativeBody`
//!
//! `NativeBody` is generic over the IO and nothing else, deliberately —
//! `crate::Native`'s epoch/`Duration` bookkeeping exists so that neither
//! it nor the pool needs a second type parameter for the runtime. A body
//! that holds a sleep needs the clock, so it needs that parameter; keeping
//! it in a wrapper leaves `NativeBody` alone and makes this one testable
//! on any inner body at all, which is what `tests/idle.rs` does with a body
//! that has no socket under it.

use bytes::Bytes;
use hclient_core::unversioned::Timer;
use hclient_core::{Error, ErrorKind, Phase};
use http_body::{Body, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// The source of an [`ErrorKind::Timeout`]`(`[`Phase::BetweenBytes`]`)`.
///
/// A named type rather than a string, for the same reason
/// `hclient::TotalTimeoutElapsed` is one: a caller must be able to tell
/// this apart from any other timeout with
/// `Error::source().downcast_ref()`, and to read the bound that was
/// actually in force rather than parse it out of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the response body sent nothing for {0:?}, its between_bytes timeout")]
pub struct BetweenBytesElapsed(pub Duration);

/// A response body that fails if the peer stops sending for longer than
/// its `between_bytes` bound — see the module doc.
///
/// With no bound set it is a pass-through and never builds a sleep at all,
/// which matters beyond the cost of an allocation: `Tokio::sleep` panics
/// outside a runtime, and a request that asked for nothing must not need
/// one.
pub struct IdleTimeout<B, Tm: Timer> {
    /// `None` once the bound has fired: the body is dropped there, and
    /// dropping it is what stops the exchange — under `Transport::
    /// execute`'s contract (v0.2 W1) that is a cancellation, so the socket
    /// is torn down rather than left to drain into nobody. A bound that
    /// reported an error and left the transfer running would be a bound on
    /// the caller's patience.
    inner: Option<B>,
    timer: Tm,
    /// `None` — no bound was asked for, and this wrapper is inert. Still in
    /// the type, because a type cannot appear and disappear with a runtime
    /// value; the cost is one `Option` test per frame.
    every: Option<Duration>,
    /// The sleep now running against the current gap. Dropped on every
    /// frame, so each gap gets the whole bound rather than what is left of
    /// one started earlier.
    ///
    /// `Pin<Box<..>>` because `tokio::time::Sleep` is `!Unpin` and this
    /// workspace forbids `unsafe`, so there is no projection to be had —
    /// the same reason, and the same non-consequence for auto traits, as
    /// `crate::pool::Reaper`'s.
    sleep: Option<Pin<Box<Tm::Sleep>>>,
}

impl<B, Tm: Timer> IdleTimeout<B, Tm> {
    pub(crate) fn new(inner: B, timer: Tm, every: Option<Duration>) -> Self {
        Self {
            inner: Some(inner),
            timer,
            every,
            sleep: None,
        }
    }

    /// The bound in force for this response, if any.
    pub fn between_bytes_timeout(&self) -> Option<Duration> {
        self.every
    }

    /// `true` once the bound has fired and the inner body has been
    /// dropped.
    pub fn is_expired(&self) -> bool {
        self.inner.is_none()
    }

    fn elapsed(&self) -> Error {
        Error::new(
            ErrorKind::Timeout(Phase::BetweenBytes),
            BetweenBytesElapsed(self.every.unwrap_or_default()),
        )
    }
}

/// `Unpin` whenever the wrapped body is, stated rather than inferred — the
/// same reasoning, in the same words, as `hclient::Deadline`'s: the
/// derivation would also demand it of the clock, which [`Timer`] does not
/// require, and a clock with a `!Unpin` field would make every response
/// body `!Unpin` and lock `Response::chunk` and `SseStream` out of it.
/// Sound here because nothing in this type is ever pinned in place: the
/// only projections are `Pin::new(&mut inner)`, which needs `B: Unpin` on
/// its own, and the sleep, which is behind its own `Pin<Box<_>>`.
impl<B: Unpin, Tm: Timer> Unpin for IdleTimeout<B, Tm> {}

/// Hand-written for the reason `hclient::Deadline`'s is: `#[derive(Debug)]`
/// would demand `Debug` of the clock, which [`Timer`] does not ask for.
impl<B: std::fmt::Debug, Tm: Timer> std::fmt::Debug for IdleTimeout<B, Tm> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdleTimeout")
            .field("inner", &self.inner)
            .field("between_bytes", &self.every)
            .finish()
    }
}

impl<B, Tm> Body for IdleTimeout<B, Tm>
where
    B: Body<Data = Bytes, Error = Error> + Unpin,
    Tm: Timer,
{
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        // `B: Unpin`, so no projection and no `unsafe` — the crate forbids
        // it, and every consumer of a body in this workspace already
        // requires `Unpin` of it.
        let this = self.get_mut();
        let Some(inner) = this.inner.as_mut() else {
            // Already fired. The gap has not become un-elapsed, so saying
            // so again is the honest answer — the same choice, for the
            // same reason, as `hclient::Deadline`'s.
            return Poll::Ready(Some(Err(this.elapsed())));
        };

        // The inner body **first**, always, and on both branches: a frame
        // that arrived in the same wake as the deadline expiring is data
        // the peer really sent, and reporting a timeout instead would
        // discard it. This is the same ordering rule `hclient::within`
        // states for the total bound.
        let polled = Pin::new(inner).poll_frame(cx);
        let Some(every) = this.every else {
            return polled;
        };
        match polled {
            Poll::Ready(out) => {
                // Any answer at all — a frame, a clean end, an error —
                // ends this gap. Dropping the sleep is what gives the next
                // gap the whole bound rather than the remains of this one.
                this.sleep = None;
                Poll::Ready(out)
            }
            Poll::Pending => {
                if this.sleep.is_none() {
                    this.sleep = Some(Box::pin(this.timer.sleep(every)));
                }
                let sleep = this.sleep.as_mut().expect("just set");
                if sleep.as_mut().poll(cx).is_ready() {
                    // Dropped before the error is returned, so the exchange
                    // is already stopping when the caller sees it.
                    this.inner = None;
                    this.sleep = None;
                    return Poll::Ready(Some(Err(this.elapsed())));
                }
                Poll::Pending
            }
        }
    }

    /// An expired body has not "ended": it failed. Saying `true` would let
    /// a caller conclude the response was complete.
    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().is_some_and(|b| b.is_end_stream())
    }

    fn size_hint(&self) -> SizeHint {
        match self.inner.as_ref() {
            Some(b) => b.size_hint(),
            None => SizeHint::default(),
        }
    }
}
