//! A bound on the whole operation: the clock it is measured with, the
//! error it becomes, and the response body that carries it past
//! `Client::execute`.
//!
//! The gap this closes: with only
//! `connect`/`first_byte`/`between_bytes`, a response that starts
//! promptly and then dribbles just under the `between_bytes` threshold
//! runs unbounded. Nothing bounded the operation as a whole.

use crate::error::TotalTimeoutElapsed;
use crate::response::classify_body_error;
use bytes::Bytes;
use core::time::Duration;
use hclient_core::unversioned::Timer;
use hclient_core::{Error, ErrorKind, Phase};
use std::error::Error as StdError;
use std::fmt::Debug;
use std::future::Future;
use std::future::poll_fn;
use std::pin::Pin;
use std::task::{Context, Poll};

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

    /// `Pending<()>`: the name of "never resolves", which is what this
    /// clock's sleep always was. Naming it changed nothing here but the
    /// signature — and it is worth noticing that the type now says out
    /// loud what the prose below explains.
    type Sleep = std::future::Pending<()>;

    fn sleep(&self, _: Duration) -> Self::Sleep {
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
/// on `Transport::execute`. Without that contract this would
/// bound the caller's wait and leave the request running.
pub(crate) async fn within<F, T>(
    op: F,
    timer: &hclient_core::unversioned::erased::SharedTimer,
    total: Duration,
) -> Result<T, Error>
where
    F: Future<Output = Result<T, Error>>,
{
    let mut op = std::pin::pin!(op);
    // Constructed only on this branch, never when no bound was asked for:
    // `Tokio::sleep` is `tokio::time::sleep`, which panics outside a
    // runtime, and a client that never set a total must not need one.
    let mut sleep = timer.sleep_boxed(total);
    poll_fn(move |cx| {
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
/// The shape is `hclient-wasi`'s: `Transport::execute` hands back
/// `http::Response<Self::Body>`, and a backend that needs something to
/// outlive `execute` carries it in that body — there an unfinished
/// request-body write, here the deadline. Nothing in the `Transport` seam
/// changes for it.
///
/// # What this bounds
///
/// The deadline is **raced against the body**, by two mechanisms that
/// answer two different questions:
///
/// - An elapsed-time check (`Timer::now`/`Timer::elapsed_since`) before
///   every poll of the wrapped body. This is what cuts a body that keeps
///   producing data past the deadline — the dribbling body the acceptance
///   names — at its first frame after the deadline.
/// - A **real sleep**, held in the `sleep` field below and polled whenever
///   the wrapped body answers `Pending`. This is what cuts a body that
///   goes **completely silent** after the head: no frame will ever arrive
///   to be checked on, so without a sleep registering the caller's waker
///   nothing would ever poll this wrapper again and the elapsed-time
///   check would never run a second time.
///
/// The response head, and every redirect hop before it, is bounded
/// separately by `within` in `Client::execute`, so a server that answers
/// nothing at all is cut before this type exists.
///
/// **This is still not `between_bytes`**, and the difference is not a
/// technicality: `between_bytes` bounds the GAP between two frames and
/// restarts on each one, so it cuts a stall at any point in an
/// arbitrarily long transfer. What is here bounds the operation once, from
/// `Client::execute`'s entry; a body that produces a byte every 50 ms for
/// an hour is fine by `between_bytes` and cut by this, and a transfer that
/// legitimately takes an hour and stalls for ten minutes in the middle is
/// the reverse.
///
/// `hclient-native` declares and enforces both `first_byte` and
/// `between_bytes`
/// (`hclient_native::IdleTimeout`, a body wrapper holding a sleep of its
/// own restarted on every frame). What has not changed is that the two
/// bounds are different questions and neither implies the other, so a
/// caller wanting the gap bounded must still ask for it — and must still
/// read `Capabilities::timeouts` rather than assume, because it is a
/// per-transport answer and the ambient backends give their own.
///
/// ## Why the sleep is here now, when it once could not be
///
/// This section used to describe the second bullet as a limitation, under
/// a reason that has since stopped being true. It read: "That is forced by
/// the shape of [`Timer`]: `sleep` returns `impl Future`, an RPITIT whose
/// type cannot be named and therefore cannot be a field of this struct.
/// The only way to store it is `Pin<Box<dyn Future>>`, which is `!Send` …
/// and that would make **every** response body `!Send`."
///
/// Both halves were right about the shape available at the time and wrong
/// as a permanent conclusion. [`Timer`] now has an associated
/// [`Timer::Sleep`], so the sleep **is** a field: `Pin<Box<Tm::Sleep>>` is
/// a box around a *concrete* type, and auto traits pass straight through
/// it — `Send` is inferred exactly as it is for `H1Body<I>`, rather than
/// lost as it would be through `dyn`. The `Pin<Box<dyn Future>>` half of
/// the old reasoning was checked and is correct; it simply was never the
/// only option once the type had a name. That `Send` survives is not left
/// to the eye: `tests/deadline.rs` moves a whole response body across a
/// `tokio::spawn`, which does not compile for a `!Send` body.
///
/// `Pin<Box<..>>` rather than a bare `Tm::Sleep` field, even though
/// `tokio::time::Sleep` and `async_io::Timer` both happen to be `Unpin`:
/// [`Timer::Sleep`] requires no such thing, and pinning an un-boxed field
/// in place needs either that bound (which would narrow the seam for every
/// runtime) or `unsafe` (which this crate forbids). One allocation per
/// bounded response is the price, and only for a response that asked for
/// a bound — `total: None` builds no sleep at all.
///
/// Measured before it was written, so the next reader does not have to
/// re-derive it: with a counting waker and no executor running at all, the
/// elapsed-time wrapper alone registers **zero** wakes after one `Pending`
/// poll on a silent body — nothing will ever poll it again, so the
/// deadline can never fire — while a wrapper holding the sleep registers
/// one and fires on its own deadline (302 ms against a 300 ms bound,
/// versus 1202 ms only because the harness happened to wake it).
///
/// # Firing drops the inner body
///
/// When the deadline fires the wrapped body is dropped, not merely
/// reported on. Dropping a response body before it ends is a cancellation
/// under `Transport::execute`'s contract, so the socket or
/// ambient exchange behind it is torn down rather than left draining. A
/// bound that returned an error and left the transfer running would be a
/// bound on the caller's patience, not on the operation.
pub struct Deadline<B> {
    /// `None` once the deadline has fired: the body is dropped there, and
    /// dropping it is what stops the exchange.
    inner: Option<B>,
    /// The stamp answers `elapsed` itself, so the clock is kept only to
    /// build a sleep — see `hclient_core::unversioned::erased`.
    ///
    /// **The clock itself is not kept**, and that falls out of erasure
    /// rather than being tidied away: the sleep is built in `new`, and
    /// `started` is a `BoxInstant`, which is the stamp *and* the clock that
    /// took it. So one `send-bound-exception` marker left this struct with
    /// the field.
    started: hclient_core::unversioned::erased::BoxInstant,
    /// `None` — no bound was set, and this wrapper is inert. It is still
    /// in the type, because a type cannot appear and disappear with a
    /// runtime value; the cost is one `Option` test per frame.
    total: Option<Duration>,
    /// The sleep that wakes a silent body, or `None`.
    ///
    /// `None` in exactly two situations, and they are not the same one:
    /// no bound was asked for (`total` is `None` too, and constructing a
    /// sleep would be a `tokio::time::sleep` outside a runtime for a
    /// client that never asked for a clock — see `within`); or the
    /// deadline has already fired, where it is dropped alongside `inner`
    /// so that a completed future is never polled again.
    sleep: Option<hclient_core::unversioned::erased::BoxSleep>,
}

impl<B> Deadline<B> {
    /// Builds the wrapper, and with it the sleep that will cut a silent
    /// body.
    ///
    /// The sleep is created **here**, not lazily on the first poll, and
    /// for the remaining budget rather than for the whole `total`: the
    /// head has already cost some of it, and a sleep of the full `total`
    /// would hand the body a second budget of its own. `saturating_sub`
    /// because the two clock reads are not atomic — an operation that
    /// spent its whole budget on the head gets a zero-length sleep, which
    /// fires at once, rather than an underflow.
    ///
    /// Creating it here is also what keeps a clockless client out of
    /// `tokio::time::sleep`'s panic: this is called from
    /// `Client::execute`, inside the runtime that is already polling it,
    /// and only ever with `total: Some(..)` for a client that named a
    /// clock in the same call that set the bound (see [`NoClock`]).
    ///
    /// **`total.map`, and never an unconditional sleep** — `Tokio::sleep`
    /// panics outside a runtime, and a client that never asked for a bound
    /// must not start requiring one. That is not a theory anyone has to
    /// take on trust: building the sleep unconditionally (`Duration::MAX`
    /// where there is no bound) turns eleven tests across `sse_reconnect`,
    /// `timeouts` and `two_runtimes` red at once, because they drive a
    /// `Client` on a bare `futures_executor` with `DefaultClock = Tokio`.
    /// Measured as one of the mutations over this change.
    pub(crate) fn new(
        inner: B,
        timer: &hclient_core::unversioned::erased::SharedTimer,
        started: hclient_core::unversioned::erased::BoxInstant,
        total: Option<Duration>,
    ) -> Self {
        let sleep = total.map(|t| {
            let left = t.saturating_sub(started.elapsed());
            timer.sleep_boxed(left)
        });
        Self {
            inner: Some(inner),
            started,
            total,
            sleep,
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
        (self.started.elapsed() >= total).then_some(total)
    }

    /// The deadline has expired: drop what is holding the exchange open
    /// and produce the error the caller will see.
    ///
    /// Dropping `inner` is the cancellation (see the type's last section),
    /// and it is what every test in `tests/deadline.rs` that watches a
    /// server sees.
    ///
    /// **`self.sleep = None` is not observable, and that is recorded
    /// rather than hidden.** Deleting that line survives the whole suite —
    /// one of fifteen mutations run over this change and the only
    /// survivor. It survives for a structural reason, not for want of a
    /// test: `inner` is `None` from here on, so every later `poll_frame`
    /// returns in the "already fired" branch and the sleep is never
    /// reached, whether or not it is still there. What the line actually
    /// buys is releasing the runtime's timer registration at the moment
    /// the deadline fires instead of whenever the caller drops the
    /// response, plus the invariant that a `Ready` future is not kept
    /// around to be polled again if this function's callers ever change.
    /// Neither is something a black-box test can see.
    fn fire(&mut self, total: Duration) -> Error {
        self.inner = None;
        self.sleep = None;
        Error::new(ErrorKind::Timeout(Phase::Total), TotalTimeoutElapsed(total))
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
impl<B: Unpin> Unpin for Deadline<B> {}

/// Hand-written: `#[derive(Debug)]` would require `Tm::Instant: Debug`,
/// which [`Timer`] does not ask for, so the derive would not compile for a
/// clock whose instant is not `Debug`. The instant is not printed.
impl<B: Debug> Debug for Deadline<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deadline")
            .field("inner", &self.inner)
            .field("total", &self.total)
            .finish()
    }
}

impl<B> http_body::Body for Deadline<B>
where
    B: http_body::Body<Data = Bytes> + Unpin,
    // The same `send-bound-exception: amendment-C1` point `Response::chunk`
    // already stands on: the error is re-classified into
    // `hclient_core::Error`, whose source is an `Arc<dyn Error + Send +
    // Sync>`.
    B::Error: StdError + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    type Data = Bytes;
    /// Not `B::Error`: a timeout has no `B::Error` to be, and one cannot be
    /// invented for a generic `B`. Re-classification goes through the same
    /// `classify_body_error` `Response::chunk` uses, so a body error that
    /// was already an `Error` keeps the category the backend gave it, and
    /// classifying it again in `Response::chunk` is idempotent.
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
            return Poll::Ready(Some(Err(this.fire(total))));
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
        // The body is polled BEFORE the sleep, the same order `within`
        // keeps one level up and for the same reason: a frame that arrived
        // in the very wake that expired the deadline is data, not a
        // timeout.
        match Pin::new(inner).poll_frame(cx) {
            Poll::Ready(o) => Poll::Ready(o.map(|r| r.map_err(classify_body_error))),
            // The body has nothing yet and has registered `cx`'s waker for
            // whenever it does. This is the only branch that can be the
            // last one ever taken — a body that never speaks again never
            // wakes anybody — so the sleep is polled here, on the same
            // waker, and that is what makes the deadline able to fire on
            // its own rather than only on the next frame.
            Poll::Pending => {
                if let Some(sleep) = this.sleep.as_mut()
                    && sleep.as_mut().poll(cx).is_ready()
                {
                    // Not `this.overrun()`: the sleep and the elapsed-time
                    // check are two readings of the same clock and need not
                    // agree to the nanosecond. The sleep firing IS the
                    // deadline, and reporting `total` is reporting the
                    // bound that was in force, not a measurement.
                    let total = this.total.unwrap_or_default();
                    return Poll::Ready(Some(Err(this.fire(total))));
                }
                Poll::Pending
            }
        }
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
