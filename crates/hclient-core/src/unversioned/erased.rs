//! Type-erased forms of [`crate::unversioned::Transport`] and [`Timer`], so
//! a facade can be **one concrete type** instead of two type parameters.
//!
//! A backend implements nothing here: [`BoxedTransport`] and [`BoxedTimer`]
//! have blanket impls over every `Transport` and every `Timer`.
//!
//! # Nothing boxed here declares `Send`
//!
//! The boxed future, body, sleep and instant carry no auto trait, and that
//! is what makes the blanket impls possible: proving
//! `Transport::execute`'s RPITIT `Send` for a generic `T` needs return type
//! notation, unstable as of rustc 1.98. Following the bound down to where
//! it *can* be proven would put it on seven seam methods, which excludes a
//! single-threaded runtime such as `hclient-rt-embassy`, whose `connect`
//! future holds a `RefCell` and always will.
//!
//! The consequence a caller meets: **nothing a request produces is
//! `Send`** — not the future, not the response body. One `BoxBody` serves
//! every backend, and a browser's body holds a `dyn Stream` with no auto
//! trait, so declaring `Send` would exclude that backend rather than weaken
//! it.
//!
//! # Where `Send + Sync` does appear
//!
//! [`SharedTransport`] and [`SharedTimer`], which a facade writes at its
//! own use site to hold these behind an `Arc` and cross a `tokio::spawn`.
//! A backend that cannot satisfy the bound is **refused at the
//! constructor** — a compile error at the line that asked — rather than
//! taxed at the seam. `hclient-rt-embassy` is that backend: `RefCell`
//! throughout, because embassy's executor is single-threaded, so an
//! embedded caller uses `Transport` directly rather than a facade.
//!
//! # The instant is erased as a question, not as a type
//!
//! [`Timer::Instant`] is `Copy + PartialOrd`, and `Copy` on a trait object
//! is not a thing. [`ErasedInstant`] answers the one question a client asks
//! of a stamp — *how long ago was this* — so the instant stays inside the
//! clock that made it and `Copy` is asked of nothing erased.

use crate::Error;
use crate::RequestBody;
use crate::unversioned::Timer;
use alloc::boxed::Box;
use bytes::Bytes;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;

/// A response body with its type erased, as an erased transport hands back.
///
/// **Not `Send`**, so it cannot cross a `tokio::spawn`. One `BoxBody`
/// serves every backend and a browser's body holds a `dyn Stream` with no
/// auto trait, so the bound would exclude that backend rather than weaken
/// it. A caller who needs a spawnable body reaches past the facade for the
/// concrete transport's own body type.
pub type BoxBody = Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error>>>;

/// An erased exchange, as [`BoxedTransport`] hands one back.
pub type BoxExchange<'a> =
    Pin<Box<dyn Future<Output = Result<http::Response<BoxBody>, Error>> + 'a>>;

/// An erased sleep, as [`BoxedTimer`] hands one back.
///
/// Not `Send`, for [`BoxBody`]'s reason and inseparably from it: a response
/// body holds a sleep — that is how a total timeout cuts a silent body — so
/// the two answer the same question.
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

    /// The transport as [`core::any::Any`], so a caller can ask for its
    /// concrete type back.
    ///
    /// Erasure is what makes a facade one type rather than two parameters,
    /// and the price is exactly this: the type is gone. A caller who needs
    /// it back — to inspect a mock's recorded requests, or to lend a
    /// `Native` to a WebSocket connector — downcasts through here, and the
    /// `Option` is the honest answer, because the client holds whatever
    /// backend it was built with and nothing checked it against this
    /// caller's guess.
    fn as_any(&self) -> &dyn core::any::Any;
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

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// A transport a facade can share between threads, erased.
///
/// **The bound lives on this alias rather than at the use sites, and that
/// is a rule rather than a style.** `cargo fmt` moves a trailing comment
/// off a line it reflows and deletes one from a `where` clause outright,
/// so a `send-bound-exception` marker cannot survive on a long signature.
/// A short named type is a line fmt has no reason to touch, so every use
/// site writes `Box<SharedTransport>` and carries no marker at all.
///
/// The bound is amendment C12's criterion: one this crate chooses so a
/// caller's value reaches a facade by erasure rather than by a type
/// parameter, said at the use site and never on the trait. A backend that
/// cannot satisfy it is refused at a constructor rather than taxed at the
/// seam.
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
/// Not `Send`, for [`BoxSleep`]'s reason: the same body holds the stamp the
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
