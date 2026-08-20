//! Type-erased forms of [`crate::unversioned::Transport`] and [`Timer`], so
//! a facade can be **one concrete type** instead of two type parameters.
//!
//! # Nothing here declares `Send`, and that is the whole design
//!
//! An earlier attempt at this put `Send` on the boxed future, so that an
//! erased client could be spawned. It does not work and the measurement is
//! worth keeping: proving `Transport::execute`'s RPITIT `Send` for a generic
//! `T` needs return type notation (`E0658` on rustc 1.98.0), and following
//! that down to where it *can* be proven means the bound on seven seam
//! methods — at which point `hclient-rt-embassy` is excluded, because its
//! `connect` future holds `RefCell<embassy_net::Inner>` and always will.
//!
//! **The bound was never needed.** `Native::execute`'s future is already
//! `!Send` — one unmarked box around the resolver's stream, pinned by a
//! doctest on `Native` — so a caller cannot spawn a request today either.
//! Erasing without `Send` costs nothing that exists, and buys two things
//! the `Send` version could not: a **blanket impl** over every `Transport`,
//! since there is nothing left to prove, and a core that declares no auto
//! trait at all.
//!
//! Where the erased client wants `Send + Sync` — it holds these behind an
//! `Arc` and is meant to cross a `tokio::spawn` — it says so at its own use
//! site, `Arc<dyn BoxedTransport + Send + Sync>`. A backend that cannot
//! satisfy that is refused at the constructor rather than taxed at the seam,
//! and `Embassy` is refused there today for the same reason it always was:
//! it is `RefCell` throughout, so `Client<Native<Embassy, ..>>` is already
//! `!Send`.
//!
//! # The instant is erased as a question, not as a type
//!
//! [`Timer::Instant`] is `Copy + PartialOrd`, and `Copy` on a trait object
//! is not a thing. `docs/competitive-gaps.md` §G13 called that permanent;
//! it is not. [`ErasedInstant`] answers the one question a client asks of a
//! stamp — *how long ago was this* — so the instant stays inside the clock
//! that made it and `Copy` is asked of nothing erased. Fixing `Instant` to a
//! concrete type, which §G13 called the only way out, is still impossible
//! for the reason it gave, and is no longer needed.

use crate::Error;
use crate::RequestBody;
use crate::unversioned::Timer;
use bytes::Bytes;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// A response body with its type erased, as an erased transport hands back.
pub type BoxBody = Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error>>>;

/// An erased exchange, as [`BoxedTransport`] hands one back.
pub type BoxExchange<'a> =
    Pin<Box<dyn Future<Output = Result<http::Response<BoxBody>, Error>> + 'a>>;

/// An erased sleep, as [`BoxedTimer`] hands one back.
pub type BoxSleep = Pin<Box<dyn Future<Output = ()>>>;

/// Erase a body, mapping its error into [`Error`] on the way.
///
/// Written here rather than taken from `http-body-util`: `hclient-core`
/// depends on `http-body` and not on the util crate, and this is a dozen
/// lines against a dependency every backend would then carry.
pub fn box_body<B>(body: B) -> BoxBody
where
    B: http_body::Body<Data = Bytes> + 'static,
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

/// [`crate::unversioned::Transport`], with the future and the body boxed.
///
/// Implemented for every `Transport` whose error and body error convert
/// into [`Error`], which is every backend in this workspace. A backend
/// author writes nothing.
pub trait BoxedTransport {
    /// [`crate::unversioned::Transport::execute`], boxed.
    fn execute_boxed<'a>(&'a self, req: http::Request<RequestBody>) -> BoxExchange<'a>;

    /// [`crate::unversioned::Transport::capabilities`], unchanged — it was
    /// never generic.
    fn capabilities(&self) -> &crate::Capabilities;
}

impl<T> BoxedTransport for T
where
    T: crate::unversioned::Transport,
    T::Body: 'static,
    <T::Body as http_body::Body>::Error: Into<Error>,
    T::Error: Into<Error>,
{
    fn execute_boxed<'a>(&'a self, req: http::Request<RequestBody>) -> BoxExchange<'a> {
        Box::pin(async move {
            match crate::unversioned::Transport::execute(self, req).await {
                Ok(resp) => Ok(resp.map(box_body)),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn capabilities(&self) -> &crate::Capabilities {
        crate::unversioned::Transport::capabilities(self)
    }
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

/// A stamp a [`BoxedTimer`] took, erased.
pub type BoxInstant = Box<dyn ErasedInstant>;

/// [`Timer`], with the sleep boxed and the instant behind [`ErasedInstant`].
pub trait BoxedTimer {
    /// [`Timer::now`], as a stamp that outlives the borrow.
    fn now_boxed(&self) -> BoxInstant;

    /// [`Timer::sleep`], boxed.
    fn sleep_boxed(&self, d: Duration) -> BoxSleep;
}

/// The stamp the blanket [`BoxedTimer`] hands out: the clock and the moment
/// together, so `elapsed` is answered by the clock that took it.
struct Stamp<Tm: Timer> {
    timer: Tm,
    at: Tm::Instant,
}

impl<Tm: Timer> ErasedInstant for Stamp<Tm> {
    fn elapsed(&self) -> Duration {
        self.timer.elapsed_since(self.at)
    }
}

impl<Tm> BoxedTimer for Tm
where
    Tm: Timer + Clone + 'static,
    Tm::Sleep: 'static,
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
