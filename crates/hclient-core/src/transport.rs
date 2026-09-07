use crate::{Capabilities, Error, ErrorKind, RequestBody};
use bytes::Bytes;
use std::error::Error as StdError;
use std::future::Future;

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
    /// [`CancelSupport::None`](crate::CancelSupport::None) is the one
    /// honest way out, and it is what a backend that never fills the field
    /// in already says, since it is the value
    /// [`Capabilities::default()`](crate::Capabilities) returns. What is
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
    /// `Error::new`, which requires `Send + Sync + 'static` from the source
    /// (amendment-C1 — erasure into `Arc<dyn Error>` doesn't let
    /// auto-traits through). A default "for any `Self::Error`" cannot
    /// exist.
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
/// Named as an alias rather than written out at each site for the reason
/// `docs/exceptions.md` records under C12: `cargo fmt` reflows the long
/// form and carries the marker comment away with it.
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
