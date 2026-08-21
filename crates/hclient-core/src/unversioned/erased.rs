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
//! site, [`SharedTransport`] and [`SharedTimer`]. A backend that cannot
//! satisfy that is refused at the constructor rather than taxed at the seam.
//!
//! **That refusal is a real loss for one backend, and this doc comment
//! claimed otherwise for one commit.** `hclient-rt-embassy` is `RefCell`
//! throughout, because embassy's executor is single-threaded, and the
//! sentence here read *"`Embassy` is refused there today for the same
//! reason it always was"*. It was not: the generic `Client` built over
//! Embassy perfectly well and merely could not cross a thread. Being
//! `!Send` and being **refused** are different things, and only the second
//! costs a backend its `Client`. The embedded scenarios use `Transport`
//! directly now; `docs/erased-client.md` has the measurement.
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
///
/// **`Send` is here and on nothing else in this module**, and it is free
/// rather than a concession: a facade that boxes a transport already asks
/// `Send + Sync` of the transport at its own use site, so a backend whose
/// body is `!Send` was excluded one line earlier anyway. What it buys is
/// the thing a caller notices — a response body that crosses a
/// `tokio::spawn`, which is ordinary and which the future above cannot do.
pub type BoxBody = Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error>>>;

/// An erased exchange, as [`BoxedTransport`] hands one back.
pub type BoxExchange<'a> =
    Pin<Box<dyn Future<Output = Result<http::Response<BoxBody>, Error>> + 'a>>;

/// An erased sleep, as [`BoxedTimer`] hands one back.
///
/// `Send` for [`BoxBody`]'s reason, and it is the same fact one layer in:
/// a response body holds a sleep — that is how a total timeout cuts a
/// silent body — so a `!Send` sleep would make the body `!Send` however
/// the body itself were declared.
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

    /// The transport as [`std::any::Any`], so a caller can ask for its
    /// concrete type back.
    ///
    /// Erasure is what makes a facade one type rather than two parameters,
    /// and the price is exactly this: the type is gone. A caller who needs
    /// it back — to inspect a mock's recorded requests, or to lend a
    /// `Native` to a WebSocket connector — downcasts through here, and the
    /// `Option` is the honest answer, because the client holds whatever
    /// backend it was built with and nothing checked it against this
    /// caller's guess.
    fn as_any(&self) -> &dyn std::any::Any;
}

impl<T> BoxedTransport for T
where
    T: crate::unversioned::Transport + 'static,
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A transport a facade can share between threads, erased.
///
/// **The `Send + Sync` lives on this line and on `SharedTimer` below, and
/// that is a rule rather than a style.** `cargo fmt` moves a trailing
/// comment off a line it has to reflow, and deletes one from a `where`
/// clause outright, so a `send-bound-exception` marker cannot survive on a
/// long signature — this workspace has lost one that way four times. A
/// short named type is a line fmt has no reason to touch, so every use
/// site writes `Box<SharedTransport>` and carries no marker at all.
///
/// The bound itself is amendment C12's own criterion: a bound this crate
/// chooses so that a caller's value reaches a facade by erasure rather
/// than by a type parameter, said at the use site and never on the trait.
/// A backend that cannot satisfy it is refused at a constructor rather
/// than taxed at the seam.
pub type SharedTransport = dyn BoxedTransport + Send + Sync; // send-bound-exception: amendment-C12

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
///
/// `Send` for [`BoxSleep`]'s reason: the same body holds the stamp the
/// sleep was computed from.
pub type BoxInstant = Box<dyn ErasedInstant>;

/// [`Timer`], with the sleep boxed and the instant behind [`ErasedInstant`].
pub trait BoxedTimer {
    /// [`Timer::now`], as a stamp that outlives the borrow.
    fn now_boxed(&self) -> BoxInstant;

    /// [`Timer::sleep`], boxed.
    fn sleep_boxed(&self, d: Duration) -> BoxSleep;
}

/// A clock a facade can share between threads, erased.
///
/// [`SharedTransport`]'s reasoning, for the other seam.
pub type SharedTimer = dyn BoxedTimer + Send + Sync; // send-bound-exception: amendment-C12

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
