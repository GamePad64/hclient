//! A bound on the whole operation: the clock it is measured with, the
//! error it becomes, and the response body that carries it past
//! `Client::execute`.
//!
//! The gap this closes is written down in `docs/v01-acceptance.md`: with
//! only `connect`/`first_byte`/`between_bytes`, a response that starts
//! promptly and then dribbles just under the `between_bytes` threshold
//! runs unbounded. Nothing bounded the operation as a whole.

use crate::response::classify_body_error;
use bytes::Bytes;
use core::time::Duration;
use http_ng_core::unversioned::Timer;
use http_ng_core::{Error, ErrorKind, Phase};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// The source of an [`ErrorKind::Timeout`]`(`[`Phase::Total`]`)`.
///
/// A named type rather than a string, for the same reason
/// [`crate::InvalidBaseUrl`] is one: a caller has to be able to tell this
/// apart from any other timeout by
/// `Error::source().downcast_ref::<TotalTimeoutElapsed>()`, and to read
/// the bound that was actually in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the whole operation exceeded its total timeout of {0:?}")]
pub struct TotalTimeoutElapsed(pub Duration);

/// The clock slot of a client that was never given a clock.
///
/// # Why this implements `Timer` at all
///
/// [`crate::Client`] is generic over its clock, and `execute` needs *some*
/// clock type whether or not a bound was ever asked for. A client built
/// without the `default-transport` feature has no clock to name, and this
/// type fills the slot.
///
/// Its `sleep` never resolves and its clock never advances. That would be
/// a silent no-op if a total timeout could ever be set on such a client —
/// so none can be, and the guarantee is structural rather than a promise
/// in prose. There are exactly two ways to set a total timeout, and this
/// is the complete list:
///
/// - [`crate::ClientBuilder::total_timeout`], which takes the clock and
///   the bound **in the same call** and returns a builder over that clock.
///   It cannot leave `NoClock` in place.
/// - [`crate::Client::total_timeout`], which takes no clock because the
///   client already has one — it exists only for
///   [`crate::DefaultClock`], and only under the `default-transport`
///   feature, where that alias is a real clock (`Tokio` on native, a
///   `setTimeout` clock in the browser). Without the feature
///   `DefaultClock` *is* `NoClock`, and the method is `#[cfg]`-ed away
///   together with the feature that gives it something to measure with.
///
/// A no-argument setter written over `Tm: Timer` instead would exist on a
/// clockless client too and do nothing there, which is the silent no-op
/// this crate refuses; Rust has no way to write "any timer except this
/// one", so the restriction is expressed by which impl block the setter
/// lives in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoClock;

impl Timer for NoClock {
    /// `()`: `Copy + PartialOrd`, which is all [`Timer`] asks of an
    /// instant, and it cannot be mistaken for a real one.
    type Instant = ();

    fn sleep(&self, _: Duration) -> impl Future<Output = ()> {
        std::future::pending()
    }
    fn now(&self) -> Self::Instant {}
    fn elapsed_since(&self, _: Self::Instant) -> Duration {
        Duration::ZERO
    }
}

/// Runs `op` with a bound on the whole of it, and drops it if the bound
/// expires.
///
/// The order of the two polls is load-bearing: `op` first, so an operation
/// that completed in the same wake as the deadline expiring is a success
/// rather than a timeout. On expiry `op` is dropped when this function
/// returns — which is what actually stops the exchange, under the contract
/// on `Transport::execute` (v0.2 W1). Without that contract this would
/// bound the caller's wait and leave the request running.
pub(crate) async fn within<F, T, Tm>(op: F, timer: &Tm, total: Duration) -> Result<T, Error>
where
    F: Future<Output = Result<T, Error>>,
    Tm: Timer,
{
    let mut op = std::pin::pin!(op);
    // Constructed only on this branch, never when no bound was asked for:
    // `Tokio::sleep` is `tokio::time::sleep`, which panics outside a
    // runtime, and a client that never set a total must not need one.
    let mut sleep = std::pin::pin!(timer.sleep(total));
    std::future::poll_fn(move |cx| {
        if let Poll::Ready(v) = op.as_mut().poll(cx) {
            return Poll::Ready(v);
        }
        if sleep.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(Error::new(
                ErrorKind::Timeout(Phase::Total),
                TotalTimeoutElapsed(total),
            )));
        }
        Poll::Pending
    })
    .await
}

/// The response body with the operation's deadline still attached.
///
/// The shape is `http-ng-wasi`'s: `Transport::execute` hands back
/// `http::Response<Self::Body>`, and a backend that needs something to
/// outlive `execute` carries it in that body — there an unfinished
/// request-body write, here the deadline. Nothing in the `Transport` seam
/// changes for it.
///
/// # What this bounds, and what it does not
///
/// The deadline is checked on every poll, against `Timer::now`/
/// `Timer::elapsed_since` — it does **not** hold a sleep of its own. That
/// is forced by the shape of [`Timer`]: `sleep` returns `impl Future`, an
/// RPITIT whose type cannot be named and therefore cannot be a field of
/// this struct. The only way to store it is `Pin<Box<dyn Future>>`, which
/// is `!Send` — proving `Send` through an RPITIT needs return type
/// notation (rust-lang/rust#109417, the same wait `http-ng-tower`
/// documents) — and that would make **every** response body `!Send`, so
/// `tokio::spawn(client.get(u).send())` would stop compiling. That
/// property is not worth trading for this one.
///
/// So, precisely:
///
/// - A body that keeps producing data past the deadline — the dribbling
///   body the acceptance names — is cut at its first frame after the
///   deadline. The response head, and every redirect hop before it, is
///   bounded by a real sleep in `Client::execute`, so a server that
///   answers nothing at all is cut too.
/// - A body that goes **completely silent** after the head is not cut:
///   nothing polls this wrapper again, and there is nobody to wake it.
///   That is `between_bytes`'s job — declared `false` by `http-ng-native`
///   today (v0.2 W4's middle bullet), so a caller who needs it should read
///   `Capabilities::timeouts` rather than assume `total` covers it.
///
/// Claiming "we bound the operation as a whole" without that second
/// bullet would be stronger than the truth.
///
/// # Firing drops the inner body
///
/// When the deadline fires the wrapped body is dropped, not merely
/// reported on. Dropping a response body before it ends is a cancellation
/// under `Transport::execute`'s contract (v0.2 W1), so the socket or
/// ambient exchange behind it is torn down rather than left draining. A
/// bound that returned an error and left the transfer running would be a
/// bound on the caller's patience, not on the operation.
pub struct Deadline<B, Tm: Timer> {
    /// `None` once the deadline has fired: the body is dropped there, and
    /// dropping it is what stops the exchange.
    inner: Option<B>,
    timer: Tm,
    started: Tm::Instant,
    /// `None` — no bound was set, and this wrapper is inert. It is still
    /// in the type, because a type cannot appear and disappear with a
    /// runtime value; the cost is one `Option` test per frame.
    total: Option<Duration>,
}

impl<B, Tm: Timer> Deadline<B, Tm> {
    pub(crate) fn new(inner: B, timer: Tm, started: Tm::Instant, total: Option<Duration>) -> Self {
        Self {
            inner: Some(inner),
            timer,
            started,
            total,
        }
    }

    /// The bound in force for this response, if any.
    pub fn total_timeout(&self) -> Option<Duration> {
        self.total
    }

    /// `true` once the deadline has fired and the inner body has been
    /// dropped.
    pub fn is_expired(&self) -> bool {
        self.inner.is_none()
    }

    fn overrun(&self) -> Option<Duration> {
        let total = self.total?;
        (self.timer.elapsed_since(self.started) >= total).then_some(total)
    }
}

/// `Unpin` whenever the wrapped body is, stated rather than inferred.
///
/// The auto-derivation would also demand it of the clock and of
/// `Tm::Instant`, neither of which [`Timer`] requires — a clock with a
/// `!Unpin` field would make every response body `!Unpin` and lock
/// `Response::chunk` and `SseStream` out of it. Writing the impl is sound
/// here because nothing in this type is ever pinned in place: the only
/// projection is `Pin::new(&mut inner)`, which needs `B: Unpin` on its
/// own, and no `unsafe` appears in this crate at all (`#![forbid]`).
impl<B: Unpin, Tm: Timer> Unpin for Deadline<B, Tm> {}

/// Hand-written: `#[derive(Debug)]` would require `Tm::Instant: Debug`,
/// which [`Timer`] does not ask for, so the derive would not compile for a
/// clock whose instant is not `Debug`. The instant is not printed.
impl<B: std::fmt::Debug, Tm: Timer> std::fmt::Debug for Deadline<B, Tm> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deadline")
            .field("inner", &self.inner)
            .field("total", &self.total)
            .finish()
    }
}

impl<B, Tm> http_body::Body for Deadline<B, Tm>
where
    B: http_body::Body<Data = Bytes> + Unpin,
    // The same `send-bound-exception: amendment-C1` point `Response::chunk`
    // already stands on: the error is re-classified into
    // `http_ng_core::Error`, whose source is an `Arc<dyn Error + Send +
    // Sync>`.
    B::Error: std::error::Error + Send + Sync + 'static,
    Tm: Timer,
{
    type Data = Bytes;
    /// Not `B::Error`: a timeout has no `B::Error` to be, and one cannot be
    /// invented for a generic `B`. Re-classification goes through the same
    /// `classify_body_error` `Response::chunk` uses, so a body error that
    /// was already an `Error` keeps the category the backend gave it
    /// (finding B2, one seam later), and classifying it again in
    /// `Response::chunk` is idempotent.
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        // `B: Unpin`, so no projection and no `unsafe` — the crate forbids
        // it. Every consumer of a body in this crate (`Response::chunk`,
        // `SseStream`) already requires `Unpin` of it, so this adds no
        // constraint that was not there.
        let this = self.get_mut();
        if let Some(total) = this.overrun() {
            // Dropping the inner body is the cancellation; doing it before
            // returning means the exchange is already stopping when the
            // caller sees the error.
            this.inner = None;
            return Poll::Ready(Some(Err(Error::new(
                ErrorKind::Timeout(Phase::Total),
                TotalTimeoutElapsed(total),
            ))));
        }
        let Some(inner) = this.inner.as_mut() else {
            // Already fired. The deadline has not become un-expired, so
            // saying so again is the honest answer; `Response::chunk`
            // seals after the first `Err` and never gets here.
            let total = this.total.unwrap_or_default();
            return Poll::Ready(Some(Err(Error::new(
                ErrorKind::Timeout(Phase::Total),
                TotalTimeoutElapsed(total),
            ))));
        };
        Pin::new(inner)
            .poll_frame(cx)
            .map(|o| o.map(|r| r.map_err(classify_body_error)))
    }

    fn is_end_stream(&self) -> bool {
        // An expired body has not "ended": it failed. Saying `true` here
        // would let a caller conclude the response was complete.
        self.inner.as_ref().is_some_and(|b| b.is_end_stream())
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self.inner.as_ref() {
            Some(b) => b.size_hint(),
            None => http_body::SizeHint::default(),
        }
    }
}
