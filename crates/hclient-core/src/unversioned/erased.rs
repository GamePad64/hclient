//! Type-erased forms of [`crate::unversioned::Transport`] and [`Timer`], so
//! a facade can be **one concrete type** instead of two type parameters.
//!
//! A backend implements one method: [`BoxedTransport`]'s blanket impl is
//! over every [`crate::unversioned::SendTransport`], and
//! [`BoxedTimer`]'s over every `Timer`.
//!
//! # Everything boxed here declares `Send`
//!
//! The future, body, sleep and instant all carry it (amendments C14 and
//! C16), so a facade built on these hands back a request future and a
//! response body that cross a thread.
//!
//! **This module said the opposite for two verticals, and the reasoning
//! was sound at the time.** It read that declaring `Send` on the boxed
//! future would mean proving `Transport::execute`'s RPITIT `Send` for a
//! generic `T` — true, and unnameable — and that following the bound down
//! to where it *could* be proven would put it on seven seam methods,
//! excluding a single-threaded runtime such as `hclient-rt-embassy`.
//!
//! What that argument missed is the difference between **naming** a bound
//! and **requiring** one. The seams a transport awaits — `Resolve`,
//! `TcpConnect`, `TlsConnect`, `Blocking` — carry associated futures now,
//! so a consumer can name them while each implementor still answers for
//! itself; and [`crate::unversioned::SendTransport`] is a separate trait,
//! so an impl may carry bounds `Transport` does not. `hclient-rt-embassy`
//! is not excluded from anything: it names a plain box, and a `Native`
//! over it is a `Transport` and not a `SendTransport`.
//!
//! What is still true is the shape of the cost, moved rather than removed:
//! a backend that cannot promise `Send` loses the facade and keeps the
//! seam. `hclient-dns-doh` is the one that pays it today.
//!
//! # Where `Send + Sync` also appears
//!
//! [`SharedTransport`] and [`SharedTimer`], which a facade writes at its
//! own use site to hold these behind an `Arc`. A backend that cannot
//! satisfy the bound is **refused at the constructor** — a compile error
//! at the line that asked — rather than taxed at the seam.
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
use bytes::Bytes;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// A response body with its type erased, as an erased transport hands back.
///
/// **`Send`** (amendment C14), so a response body crosses a
/// `tokio::spawn`. One `BoxBody` serves every backend, and this bound is
/// payable only because every one of them satisfies it — which stopped
/// being a question when `hclient-fetch`'s body stopped holding a
/// `js_sys::JsFuture`. This doc said *not `Send`* for a vertical, directly
/// above the line that declares it.
pub type BoxBody = Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error> + Send>>; // send-bound-exception: amendment-C14

/// An erased exchange, as [`BoxedTransport`] hands one back.
pub type BoxExchange<'a> =
    Pin<Box<dyn Future<Output = Result<http::Response<BoxBody>, Error>> + Send + 'a>>; // send-bound-exception: amendment-C16

/// An erased sleep, as [`BoxedTimer`] hands one back.
///
/// Not `Send`, for [`BoxBody`]'s reason and inseparably from it: a response
/// body holds a sleep — that is how a total timeout cuts a silent body — so
/// the two answer the same question.
pub type BoxSleep = Pin<Box<dyn Future<Output = ()> + Send>>; // send-bound-exception: amendment-C14

/// Erase a body, mapping its error into [`Error`] on the way.
///
/// Written here rather than taken from `http-body-util`: `hclient-core`
/// depends on `http-body` and not on the util crate, and this is a dozen
/// lines against a dependency every backend would then carry.
pub fn box_body<B>(body: B) -> BoxBody
where
    B: http_body::Body<Data = Bytes> + Send + 'static, // send-bound-exception: amendment-C14
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
/// Implemented for every [`crate::unversioned::SendTransport`] whose error
/// and body error convert into [`Error`]. A backend author writes one
/// method — `SendTransport`'s, whose body at a concrete type is
/// `Box::pin(self.execute(req))`.
///
/// **It was over every `Transport` and cost nothing**, which is the trade
/// C16 made: a facade whose request future is `Send` in exchange for one
/// method per backend and the exclusion of a backend that cannot promise
/// it. `hclient-dns-doh`-resolving transports are the case that pays.
// The attribute is here as well as on `SendTransport`, and that is not a
// duplicate: `Client::builder` bounds on THIS trait, and the blanket impl
// below means the bound the compiler reports as unsatisfied is this one.
// Without it the error names `SendTransport` in a `note` and offers no way
// to act on it — measured by writing a transport from outside the
// workspace and reading what rustc actually printed.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot back an `hclient::Client`: it is a `Transport` but not a `SendTransport`",
    label = "this transport makes no `Send` claim",
    note = "`Client` boxes its transport `Send + Sync`, so it asks for the one claim `Transport` deliberately does not make.",
    note = "Implement `SendTransport` — one method, and at a concrete type its whole body is `Box::pin(self.execute(req))`, where `Send` is inferred rather than proved:",
    note = "    impl hclient_core::unversioned::SendTransport for {Self} {{",
    note = "        fn execute_send(&self, req: http::Request<RequestBody>)",
    note = "            -> hclient_core::unversioned::BoxSendExchange<'_, Self::Body, Self::Error>",
    note = "        {{ Box::pin(self.execute(req)) }}",
    note = "    }}",
    note = "If this transport genuinely cannot cross a thread — a browser one, or a runtime whose IO is `!Send` — do not implement it. `Transport` alone still works and only `hclient::Client` is out of reach."
)]
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
    T: crate::unversioned::SendTransport + Sync + 'static, // send-bound-exception: amendment-C16
    T::Body: Send + 'static,                               // send-bound-exception: amendment-C14
    <T::Body as http_body::Body>::Error: Into<Error>,
    T::Error: Into<Error>,
{
    fn execute_boxed<'a>(&'a self, req: http::Request<RequestBody>) -> BoxExchange<'a> {
        Box::pin(async move {
            match crate::unversioned::SendTransport::execute_send(self, req).await {
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
pub type BoxInstant = Box<dyn ErasedInstant + Send>; // send-bound-exception: amendment-C14

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
    Tm: Timer + Clone + Send + 'static, // send-bound-exception: amendment-C14
    Tm::Instant: Send,                  // send-bound-exception: amendment-C14
    Tm::Sleep: Send + 'static,          // send-bound-exception: amendment-C14
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
