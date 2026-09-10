//! The seam a backend implements: one request in, one response out.
//!
//! [`Transport`] is two required methods and two associated types — plus
//! a defaulted [`Transport::to_error`], which a backend overrides only if
//! its own error type is not `Send + Sync`. Its future
//! is an RPITIT — deliberately unnameable, so nothing can demand `Send` of
//! it and a single-threaded backend can exist. [`SendTransport`] is the
//! separate promise for whoever can make it: an impl may carry bounds the
//! trait does not, which is what lets `hclient::Client` box a transport
//! `Send + Sync` without excluding the ones that are not.
//!
//! What a backend does **not** implement is everything a client does —
//! redirects, a cookie jar, a cache, retries, decompression. Those live
//! above this seam, which is why a transport that reports
//! [`crate::caps::RedirectSupport::Internal`] is refused a redirect policy
//! at `build()` rather than silently ignoring one.
use crate::body::RequestBody;
use crate::caps::Capabilities;
use crate::error::{Error, ErrorKind};
use bytes::Bytes;
use std::error::Error as StdError;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// The one seam between hclient and real HTTP.
///
/// The shape is taken from `wasi:http/client.send` — the poorest of the
/// ambient APIs. Anything richer degrades to it cleanly; the reverse isn't
/// true.
///
/// No `poll_ready`, no `&mut self`, no `Send`: Send-ness is inferred by
/// auto-traits through the returned `impl Future`.
pub trait Transport {
    type Body: http_body::Body<Data = Bytes>;
    type Error: StdError + 'static;

    /// Send the request.
    ///
    /// **On `Timeouts` in `req.extensions()`: presence isn't intent.**
    /// `hclient::Client::execute` puts the result of merging its own
    /// configuration with the request there (`effective_timeouts`)
    /// UNCONDITIONALLY — including when no timeout
    /// at all was set, in which case a `Timeouts` with every field `None`
    /// sits there. The correct read is `.get::<Timeouts>().copied().
    /// unwrap_or_default()` and then field by field: "no extension" and
    /// "extension present, every field `None`" must be the same observation
    /// to the backend. Branching on `extensions.get::<Timeouts>().is_some()`
    /// as "the caller asked for timeouts" is not allowed — that will be
    /// true always, for every request that comes through `Client`.
    ///
    /// # Dropping the returned future cancels the exchange
    ///
    /// **Dropping this future before it completes MUST stop the exchange,
    /// as far as this transport controls it.** No further request bytes are
    /// written, no response is waited for, and whatever carries the
    /// exchange — a socket this transport owns, or an operation an ambient
    /// host is running on its behalf — is torn down rather than left to run
    /// to completion. A drop is a cancellation, never a way to detach a
    /// request into the background.
    ///
    /// This is a claim about **this side**, and deliberately not about the
    /// server's:
    ///
    /// - The request may already have arrived and already have been acted
    ///   on. Cancellation is not a rollback, and a cancelled `POST` is not
    ///   a `POST` that did not happen. A caller that needs to know reaches
    ///   for idempotency keys, not for this.
    /// - `Drop` must not block. There is no async destructor in Rust, and a
    ///   backend that needs to wait for a peer to acknowledge anything must
    ///   stop waiting rather than stall the dropping task. Every backend
    ///   here satisfies this by construction: closing a socket, calling
    ///   `AbortController::abort()`, and the Component Model's
    ///   `subtask.cancel` are all non-blocking.
    /// - The exchange does not end at `execute`. Once this future has
    ///   returned `Ok`, the same duty passes to `Self::Body`: dropping the
    ///   response body before it ends is also a cancellation, on the same
    ///   terms, and must not leave a connection being drained in the
    ///   background either.
    ///
    /// **A backend that cannot honour this says so, in `Capabilities`.**
    /// [`cancel_on_drop`](crate::caps::Capabilities::cancel_on_drop) set
    /// to `false` is the one honest way out, and it is what a backend that never fills the field
    /// in already says, since it is the value
    /// [`Capabilities::default()`](crate::caps::Capabilities) returns. What is
    /// not allowed is the third option this method's documentation used to
    /// take: saying nothing at all, and leaving a caller to find out per
    /// target that a dropped future means three different things.
    ///
    /// **Why a MUST rather than a plain capability with no default duty.**
    /// The alternative — "each backend does what it does, read the field" —
    /// pushes a branch into every caller that races a request against
    /// anything, and there is no useful code to write in the `None` arm: a
    /// caller who cannot cancel cannot un-send the request either. So the
    /// duty belongs on the implementer, who can actually discharge it, and
    /// the field exists for the case where they genuinely cannot. It is
    /// also what makes connection reuse possible at all: a pool
    /// may only take back a connection whose exchange finished, and
    /// "finished" is not a property anyone can establish if a dropped
    /// future leaves an exchange running.
    fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>;

    /// The transport's capabilities, determined once — at construction —
    /// and unchanged for this object ever since. This is not a "right now"
    /// check: the signature returns `&Capabilities` rather than computing it
    /// fresh on every call (recomputing on every call doesn't compile —
    /// `E0515` — and any alternative that does compile leaks memory on every
    /// call). A backend whose capabilities can change over the process's
    /// lifetime needs to rebuild the transport from scratch.
    fn capabilities(&self) -> &Capabilities;

    /// How a transport error becomes a library error.
    ///
    /// The default is wrapping with `ErrorKind::Other`: a backend that has
    /// nothing to say about the category owes nothing further.
    ///
    /// # An error that's ALREADY `Error` passes through
    ///
    /// The default first asks whether `Self::Error` is exactly [`Error`],
    /// and if so returns it unwrapped. So a backend whose error is already
    /// classified gets the correct behaviour from the default and cannot
    /// forget it — the earlier design wrapped unconditionally, and a
    /// backend that did not override the hook silently lost its whole
    /// taxonomy with the compiler and its own tests all green.
    ///
    /// # What the default still can't do
    ///
    /// A backend whose error is ITS OWN type carrying the category inside
    /// it (`MyError::Timeout` and the like) must override `to_error`: no
    /// default can guess a foreign enum, and without an override such an
    /// error honestly becomes [`ErrorKind::Other`]. Nothing degrades
    /// silently — the category was never in [`Error`] — but nothing is
    /// classified either.
    ///
    /// The backends here override the hook with an explicit identity even
    /// though the default now covers them: it states intent where it is
    /// read, and survives a change to the default.
    ///
    /// Getting this wrong is expensive, which is why the hook exists. With
    /// the classification discarded one layer up, every `is_*` predicate on
    /// the facade answers `false` for any transport error and `kind()` is
    /// `Other` alike for DNS, TLS, connect-timeout and host-unreachable —
    /// forty lines of `hclient-wasi`'s `wasi_err`, sorting 39 `ErrorCode`
    /// variants into eight `ErrorKind`s, thrown away.
    ///
    /// **Why a defaulted method, and not `Transport::Error: Into<Error>` or
    /// `Error` as the seam's error type.**
    ///
    /// `Into<Error>` would cost a `!Send` backend its TYPED source: such an
    /// error can satisfy the bound, but only by stringifying itself, since
    /// `Error::source` requires `Send + Sync`. It would also force every
    /// backend with nothing to say about the category to write a
    /// conversion anyway. Making `Error` the seam's error type is worse
    /// again — it requires `Send + Sync` from every backend. The defaulted
    /// method requires neither.
    ///
    /// Amendment C1 deliberately kept a transport with a genuinely `!Send`
    /// error representable: it can't use `Client`, but it does implement
    /// `Transport` (see `non_send_transport_still_satisfies_the_trait` and
    /// `a_transport_whose_error_is_not_send_still_implements_the_trait` in
    /// `tests/shape.rs`). The default preserves this — the where-clause sits
    /// on the method, so such a transport simply can't CALL `to_error`
    /// (though it's free to define an override — verified: an override's
    /// body isn't required to call `Error::new`); and it breaks no backend
    /// that doesn't need categorization.
    ///
    /// The where-clause is unavoidable here: the default's body calls
    /// `Error::new`, which requires `Send + Sync + 'static` from the
    /// source, because erasure into `Arc<dyn Error>` does not let auto
    /// traits through. A default "for any `Self::Error`" cannot exist.
    ///
    /// The name is `to_error`, not `into_error`: by Rust convention `into_*`
    /// consumes `self`, and here it is `&self` — the backend is making a
    /// decision, not converting a value, and `execute` takes `&self` too.
    fn to_error(&self, e: Self::Error) -> Error
    where
        Self::Error: Send + Sync, // send-bound-exception: amendment-C1
    {
        // The box is needed because `Any` can only GIVE BACK a value out of
        // a `Box`: `downcast_ref`/`downcast_mut` would hand back a
        // reference, and we need ownership — otherwise we'd have to require
        // `Clone` from a foreign error. `dyn Any` without `+ Send + Sync`:
        // the erased object doesn't need auto-traits here at all
        // (`downcast` exists on a bare `Box<dyn Any>` too), and `Error::new`
        // below draws them from the method's where-clause. Writing them
        // here would mean declaring a bound that buys nothing, and
        // spending a `send-bound-exception` marker on it — the
        // `no-declared-send` CI check catches such a line, and rightly so.
        let boxed: Box<dyn core::any::Any> = Box::new(e);
        match boxed.downcast::<Error>() {
            // `Self::Error` is exactly our `Error`: the category was
            // already set by the backend, nothing to wrap.
            Ok(already_ours) => *already_ours,
            // A foreign type: wrap it, keeping the source whole.
            Err(foreign) => Error::new(
                ErrorKind::Other,
                *foreign.downcast::<Self::Error>().unwrap_or_else(|_| {
                    // Unreachable, and not an invariant between two
                    // far-apart places (the crate tries not to have that
                    // class of invariant), but a fact established three
                    // lines above in the same expression: we boxed exactly
                    // `Self::Error`, the first `downcast` missed, so the
                    // second must hit.
                    unreachable!("boxed a Self::Error three lines above")
                }),
            ),
        }
    }
}

/// What [`SendTransport::execute_send`] hands back: the same exchange
/// [`Transport::execute`] produces, in a form whose `Send` has a **name**.
///
/// An alias rather than the written-out form at each site, because the
/// long form is a line `cargo fmt` reflows — and a reflowed line carries
/// its trailing comment away with it.
pub type BoxSendExchange<'a, B, E> =
    std::pin::Pin<Box<dyn Future<Output = Result<http::Response<B>, E>> + Send + 'a>>; // send-bound-exception: amendment-C16

/// A transport whose exchange can cross a thread, said in a way a
/// consumer can rely on.
///
/// # Why this is a second trait and not a bound on the first
///
/// [`Transport::execute`] returns `impl Future`, which has no name — so a
/// consumer that must *prove* its own future `Send` cannot ask for this
/// one to be. Return type notation is the language feature for naming an
/// RPITIT; it is unstable, and across a crate boundary it makes the
/// compiler ICE (measured — see CLAUDE.md).
///
/// A separate trait sidesteps all of it, because **an impl may carry
/// bounds the trait does not**. `hclient-native` implements this for every
/// `Native` whose runtime, TLS backend and resolver name `Send` associated
/// futures, and for no other — so `Native` over `hclient-rt-embassy` is
/// still a `Transport` and simply not a `SendTransport`. Nothing is
/// excluded from the seam; something is excluded from a promise.
///
/// # What it costs a backend
///
/// One method, and at a concrete type its body is `Box::pin(self.execute(
/// req))` — `Send` is *inferred* there rather than proved, which is the
/// asymmetry the whole design rests on. A backend that cannot make the
/// claim does not implement this, and loses `hclient::Client` while
/// keeping everything else.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is a `Transport` but not a `SendTransport`, so it cannot back an `hclient::Client`",
    label = "this transport makes no `Send` claim",
    note = "implement it — one method, and at a concrete type its whole body is `Box::pin(self.execute(req))`:",
    note = "    impl SendTransport for {Self} {{",
    note = "        fn execute_send(&self, req: http::Request<RequestBody>)",
    note = "            -> BoxSendExchange<'_, Self::Body, Self::Error>",
    note = "        {{ Box::pin(self.execute(req)) }}",
    note = "    }}",
    note = "`Send` is inferred there rather than proved. If this transport genuinely cannot cross a thread — a browser one, or a runtime whose IO is `!Send` — do not implement it: `Transport` alone still works, and only `hclient::Client` is out of reach."
)]
pub trait SendTransport: Transport {
    /// [`Transport::execute`], boxed with its `Send` named.
    fn execute_send(
        &self,
        req: http::Request<RequestBody>,
    ) -> BoxSendExchange<'_, Self::Body, Self::Error>;
}

// ── erasure ─────────────────────────────────────────────────────────────
//
// `hclient::Client` is one concrete type rather than two type parameters,
// and this is how: a boxed form of the trait above, beside the trait, the
// way `futures_core` keeps `BoxFuture` beside `Future`. A backend writes
// none of it — the blanket impl below is over every `SendTransport`.

/// A response body with its type erased, as an erased transport hands back.
///
/// **`Send`**, so a response body crosses a `tokio::spawn`. One `BoxBody`
/// serves every backend, and the bound is payable only because every one
/// of them satisfies it — including the browser's, whose body holds no JS
/// handle across an await.
pub type BoxBody = Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error> + Send>>; // send-bound-exception: amendment-C14

/// An erased exchange, as [`BoxTransport`] hands one back.
pub type BoxExchange<'a> =
    Pin<Box<dyn Future<Output = Result<http::Response<BoxBody>, Error>> + Send + 'a>>; // send-bound-exception: amendment-C16

/// Erase a body, mapping its error into [`Error`] on the way.
///
/// Written here rather than taken from `http-body-util`: `hclient-core`
/// depends on `http-body` and not on the util crate, and this is a dozen
/// lines against a dependency every backend would then carry.
///
/// **`pub(crate)`, and it was `pub` until it was asked who calls it.** Its
/// doc named a reader outside this workspace — an author writing a
/// backend — and that reader does not exist, because the only call is
/// [`BoxTransport`]'s blanket impl below, which erases a backend's body
/// *for* it. A backend declares `type Body` and hands back its own; it
/// never boxes one itself.
///
/// Checked rather than reasoned about, and the check had to be a consumer
/// rather than a grep: a whole backend written outside this workspace —
/// its own `Transport`, `SendTransport` and body type, reaching
/// `hclient::Client` — compiles without naming this function, and goes on
/// compiling with it private. The two neighbours that look identical to a
/// grep, [`BoxTimer`] and [`BoxInstantOf`], are the counterexample that
/// makes the method worth stating: both have **zero** mentions outside
/// this crate too, and both are load-bearing `pub` — making either private
/// is `E0624` at a call in `hclient`, because their blanket impls are how
/// a caller's own `Timer` reaches the erasure. Absence from a grep is not
/// absence of a caller.
pub(crate) fn box_body<B>(body: B) -> BoxBody
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

/// [`crate::transport::Transport`], with the future and the body boxed.
///
/// Implemented for every [`crate::transport::SendTransport`] whose error
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
    note = "If `{Self}` is a `Result`, this is a missing `?` rather than a missing impl: `Client::new` and `default_transport` are fallible on native and infallible in a browser, so portable code differs by exactly that one character.",
    note = "`Client` boxes its transport behind `Send` and `Sync`, so it asks for the one claim `Transport` deliberately does not make.",
    note = "Implement `SendTransport` — one method, and at a concrete type its whole body is `Box::pin(self.execute(req))`, where `Send` is inferred rather than proved:",
    note = "    impl hclient_core::SendTransport for {Self} {{",
    note = "        fn execute_send(&self, req: http::Request<RequestBody>)",
    note = "            -> hclient_core::BoxSendExchange<'_, Self::Body, Self::Error>",
    note = "        {{ Box::pin(self.execute(req)) }}",
    note = "    }}",
    note = "If this transport genuinely cannot cross a thread — a browser one, or a runtime whose IO is `!Send` — do not implement it. `Transport` alone still works and only `hclient::Client` is out of reach."
)]
pub trait BoxTransport {
    /// [`crate::transport::Transport::execute`], boxed.
    fn execute_boxed<'a>(&'a self, req: http::Request<RequestBody>) -> BoxExchange<'a>;

    /// [`crate::transport::Transport::capabilities`], unchanged — it was
    /// never generic.
    fn capabilities(&self) -> &crate::caps::Capabilities;

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

impl<T> BoxTransport for T
where
    T: crate::transport::SendTransport + Sync + 'static, // send-bound-exception: amendment-C16
    T::Body: Send + 'static,                             // send-bound-exception: amendment-C14
    <T::Body as http_body::Body>::Error: Into<Error>,
    T::Error: Into<Error>,
{
    fn execute_boxed<'a>(&'a self, req: http::Request<RequestBody>) -> BoxExchange<'a> {
        Box::pin(async move {
            match crate::transport::SendTransport::execute_send(self, req).await {
                Ok(resp) => Ok(resp.map(box_body)),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn capabilities(&self) -> &crate::caps::Capabilities {
        crate::transport::Transport::capabilities(self)
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
/// The bound is one this crate chooses so a caller's value reaches a
/// facade by erasure rather than by a type parameter — said at the use
/// site and never on the trait. A backend that
/// cannot satisfy it is refused at a constructor rather than taxed at the
/// seam.
pub type SharedTransport = dyn BoxTransport + Send + Sync; // send-bound-exception: amendment-C12
