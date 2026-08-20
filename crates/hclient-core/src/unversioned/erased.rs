//! Type-erased forms of [] and [`Timer`], so a facade can be
//! **one concrete type** instead of two type parameters.
//!
//! # Why these are traits beside the seams rather than changes to them
//!
//! [] is an `async fn` in a trait, so its future is an
//! RPITIT and `dyn Transport` is not a thing. Naming that future's
//! auto-traits generically needs return type notation, which is still
//! `E0658` — re-measured on rustc 1.98.0, 2026-08-20.
//!
//! The way past it does not need RTN at all: a **second trait with no
//! default body**, which each backend implements in one expression. There
//! `Self` is concrete, so `Box::pin(self.execute(req))` is a future the
//! compiler can prove `Send` about. A backend that cannot produce one
//! simply does not implement this trait, and its inability is a compile
//! error at the line that asked for an erased client rather than a runtime
//! surprise.
//!
//! **That is why the `Send` bounds here do not break the core's rule.**
//! `scripts/no-send-or-sync-in-the-core-surface.sh` exists because
//! *declaring the bound in the seam forces it on backends that cannot
//! satisfy it* — and nothing here is the seam. [] and [`Timer`]
//! are untouched; these are opt-in siblings. Amendment C13.
//!
//! # The instant is erased as a question, not as a type
//!
//! [`Timer::Instant`] is `Copy + PartialOrd`, and `Copy` on a trait object
//! is not a thing. `docs/competitive-gaps.md` §G13 recorded that as a
//! permanent blocker on the strength of it, and it is not one: the way out
//! is to stop moving the instant across the boundary. [`ErasedInstant`]
//! answers the one question a client asks of a stamp — *how long ago was
//! this* — so the instant stays inside the timer that made it and `Copy` is
//! never asked of anything erased.
//!
//! Fixing `Instant` to a concrete type, which §G13 called the only way out,
//! is still impossible for the reason it gave: the three clocks that ship
//! here disagree (`tokio::time::Instant`, `std::time::Instant`, and
//! `NoClock`'s `()`), and `NoClock`'s is deliberate.

use crate::Capabilities;
use crate::Error;
use crate::RequestBody;
use crate::unversioned::Timer;
use bytes::Bytes;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// A stamp a [`BoxedTimer`] took, erased.
pub type BoxInstant = Box<dyn ErasedInstant + Send + Sync>; // send-bound-exception: amendment-C13

/// An erased sleep, as [`BoxedTimer`] hands one back.
///
/// A name rather than the type spelled at each site, and the reason is not
/// only brevity: the `send-bound-exception` marker is a trailing comment on
/// the **same line** as the bound, and `cargo fmt` moves a trailing comment
/// off a line that is too long — which silently unexcuses the site. It has
/// happened three times in this repository. A short signature is how the
/// marker stays put.
///
/// This is not the `BoxFuture` alias decision D6 refuses by name: that is a
/// general-purpose alias spread through the core's own signatures, where
/// this is one erased seam's own return type, used twice.
pub type BoxSleep = Pin<Box<dyn Future<Output = ()> + Send>>; // send-bound-exception: amendment-C13

/// An erased exchange, as [`BoxedTransport`] hands one back.
pub type BoxExchange<'a> =
    Pin<Box<dyn Future<Output = Result<http::Response<BoxBody>, Error>> + Send + 'a>>; // send-bound-exception: amendment-C13

/// A response body with its type erased, as an erased transport hands it
/// back.
pub type BoxBody = Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error> + Send>>; // send-bound-exception: amendment-C13

/// Erase a body, mapping its error into [`Error`] on the way.
///
/// Written here rather than taken from `http-body-util`: `hclient-core`
/// depends on `http-body` and not on the util crate, and this is twelve
/// lines against a dependency every backend would then carry.
pub fn box_body<B>(body: B) -> BoxBody
where
    B: http_body::Body<Data = Bytes> + Send + 'static, // send-bound-exception: amendment-C13
    B::Error: Into<Error>,
{
    Box::pin(MapErr(Box::pin(body)))
}

/// The inner body is held **already pinned**, so this needs no projection
/// and therefore no `unsafe` — `hclient-core` is `#![forbid(unsafe_code)]`,
/// and a newtype that has to project is how that gets quietly broken. The
/// cost is one allocation, on a path that is boxing anyway.
struct MapErr<B>(Pin<Box<B>>);

impl<B> http_body::Body for MapErr<B>
where
    B: http_body::Body<Data = Bytes>,
    B::Error: Into<Error>,
{
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        self.0
            .as_mut()
            .poll_frame(cx)
            .map(|o| o.map(|r| r.map_err(Into::into)))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.0.size_hint()
    }
}

/// [], with the future boxed by the backend that owns it.
///
/// **No default body, deliberately.** A blanket
/// `impl<T: Transport> BoxedTransport for T` cannot be written: proving
/// `T::execute(..)`'s future is `Send` for a generic `T` is exactly what
/// return type notation is for. Each backend writes the one expression
/// itself, where `Self` is concrete and the proof is available.
pub trait BoxedTransport {
    /// [], boxed.
    fn execute_boxed<'a>(&'a self, req: http::Request<RequestBody>) -> BoxExchange<'a>;

    /// [], unchanged — it was never generic.
    fn capabilities(&self) -> &Capabilities;
}

/// A moment a [`BoxedTimer`] recorded, which can be asked how long ago it
/// was and nothing else.
///
/// One method on purpose: it is what lets an erased clock exist at all.
/// See this module's own doc.
pub trait ErasedInstant {
    /// How long since this stamp was taken, on the clock that took it.
    fn elapsed(&self) -> Duration;
}

/// [`Timer`], with the sleep boxed and the instant behind
/// [`ErasedInstant`].
///
/// Unlike [`BoxedTransport`] this **does** have a blanket impl, and the
/// difference is worth knowing: `Tm::Sleep` is an ordinary associated type,
/// so `Tm::Sleep: Send` is an ordinary bound a caller can write. Only an
/// RPITIT needs RTN.
pub trait BoxedTimer {
    /// [`Timer::now`], as a stamp that outlives the borrow.
    fn now_boxed(&self) -> BoxInstant;

    /// [`Timer::sleep`], boxed.
    fn sleep_boxed(&self, d: Duration) -> BoxSleep;
}

/// The stamp a blanket [`BoxedTimer`] hands out: the clock and the moment,
/// together, so `elapsed` is answered by the clock that took it.
struct Stamp<Tm: Timer> {
    timer: Tm,
    at: Tm::Instant,
}

impl<Tm> ErasedInstant for Stamp<Tm>
where
    Tm: Timer + Send + Sync,  // send-bound-exception: amendment-C13
    Tm::Instant: Send + Sync, // send-bound-exception: amendment-C13
{
    fn elapsed(&self) -> Duration {
        self.timer.elapsed_since(self.at)
    }
}

impl<Tm> BoxedTimer for Tm
where
    Tm: Timer + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C13
    Tm::Instant: Send + Sync,                  // send-bound-exception: amendment-C13
    Tm::Sleep: Send + 'static,                 // send-bound-exception: amendment-C13
{
    fn now_boxed(&self) -> BoxInstant {
        Box::new(Stamp {
            timer: self.clone(),
            at: self.now(),
        })
    }

    fn sleep_boxed(&self, d: Duration) -> BoxSleep {
        Box::pin(self.sleep(d))
    }
}
