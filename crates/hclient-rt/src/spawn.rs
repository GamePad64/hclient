use crate::error::Cancelled;
use futures_core::future::BoxFuture;
use std::future::Future;

/// Run `f` to completion in the background.
///
/// The shape is deliberately copied from `hyper::rt::Executor`: generic
/// over the future, zero bounds in the declaration. `Send` is added by the
/// `impl`, not the trait, so single-threaded runtimes can implement it
/// honestly.
///
/// # `spawn` does not fail
///
/// It returns `()`, and that is the contract rather than an omission: **a
/// runtime that implements this trait accepts every future it is handed.**
/// Whatever it needs in order to do that is a precondition it states on
/// its own type and discharges where it can — at construction, as a
/// `Result`, is the form to reach for. `hclient-rt-tokio` shows both
/// ways of stating one: `TokioHandle` carries a handle to its runtime and
/// is total from any thread **for as long as that runtime is alive** —
/// which it says on its own type, because a tokio `Handle` does not keep
/// its `Runtime` alive and a spawn after shutdown is discarded unrun,
/// something no caller can detect — while the `Tokio` ZST reads an
/// ambient runtime and says, on its own doc, that calling it off one
/// panics.
///
/// [`Blocking::run`] answers [`Cancelled`] for the neighbouring case and
/// this does not, and the asymmetry is about the caller rather than the
/// runtime. A blocking call's caller is waiting on the answer and has an
/// error path to put a refusal on. The callers of `spawn` — a connection
/// driver, a pool reaper — are handing over work that keeps something
/// alive *after* the call returns, mid-way through a connect that has
/// already happened; a refusal there has no better answer than the one
/// the runtime was in a position to give earlier, at construction.
///
/// So a runtime that can find itself unable to spawn has two honest
/// options: make that impossible at the type (carry what it needs, as
/// `hclient-rt-smol`'s `Smol` does — its executor is process-wide and never
/// shuts down), or state the precondition where a caller reads it, as both
/// of `hclient-rt-tokio`'s runtimes do. **Carrying a handle is not the same
/// as carrying what it needs**, and `TokioHandle` is the example: this
/// paragraph named it as the total one until an audit dropped the runtime
/// under it and watched a spawned future vanish.
/// What it must not do is accept the future and drop it: the caller cannot
/// tell, and a driver that never runs is a connection that hangs.
///
/// **A runtime whose spawn can run out should not implement this trait**,
/// and that is the one real cost of the shape. Embassy's executor is the
/// example: tasks come from a pool whose size is fixed at compile time,
/// so its own spawn answers an error when the pool is full — a condition
/// of the moment, which no constructor can discharge.
/// `hclient-rt-embassy` therefore leaves `Spawn` unimplemented (its module
/// doc has the alternative, a leaked task per call). Nothing essential is
/// lost: `hclient_native::Native` needs `Spawn` only for its opt-ins —
/// `multiplexed()`, the HTTP/3 arm and the pool reaper — and a runtime
/// without it meets a compile error at the line that asked, which is the
/// honest form of *cannot*.
pub trait Spawn<F: Future<Output = ()>> {
    /// Hand `f` to the runtime. See the trait documentation for why this
    /// cannot refuse.
    fn spawn(&self, f: F);
}

/// A separate trait, not a method: `getaddrinfo` blocks, and wasm and
/// embedded have no blocking pool at all. The absence of the capability
/// must be a compile error, not an `unimplemented!()` in the runtime.
///
/// **The one place in the whole project where we declare `Send` ourselves**,
/// and here it's honest: both `tokio::task::spawn_blocking` and
/// `blocking::unblock` require `Send + 'static`, and the `Blocking`
/// capability doesn't exist on wasm at all — there's nothing for it to
/// infect. The justification is `amendment-C5` (`docs/exceptions.md`), an
/// amendment separate from C1/C2: those two are about erasing auto-traits in `dyn
/// Trait` on the `Client -> Transport` path, whereas here the bound is
/// declared directly in the signature of a capability trait that simply
/// doesn't exist on wasm.
///
/// The bounds live in `where`, not in the generic parameter list `fn
/// run<T: Send + …>`, so each one can carry its own `send-bound-exception`
/// marker on its own line: the CI `no-declared-send` job matches bound
/// declarations line by line, and a single shared comment after the
/// generic list wouldn't cover it.
///
/// Two distinct failure modes of `f` are not conflated into one channel:
///
/// - A panic in `f` is a bug in the calling code. It must be re-raised as a
///   panic (`std::panic::resume_unwind`, with the original payload), not
///   quietly turned into a value that can be `?`-propagated — otherwise the
///   implementation hides a defect in the caller's code behind a `Result`.
/// - The background thread pool going away (for example, the runtime
///   shutting down while a task is still queued and hasn't started
///   running) is not a bug in the calling code, but an ordinary runtime
///   lifecycle event. The implementation must return [`Cancelled`], not
///   panic: a library panicking on a normal (if rare) runtime-shutdown
///   scenario would contradict the rest of the project ("no silent
///   no-ops... typed error, never a discarded value" — the same principle
///   applied here, just to failure instead of success).
pub trait Blocking {
    /// **A named future, not an RPITIT, and the `Send` costs nobody
    /// anything.** A consumer that has to *prove* its own future `Send` —
    /// `hclient-native`, so that `hclient::Client`'s can be — must be able
    /// to **name** this one, and `impl Future` has no name. A boxed
    /// `Send` future is the honest form here rather than an associated
    /// type per implementor, because this trait already requires
    /// `T: Send` and `F: Send`: a pool that cannot be handed work from
    /// another thread is not one, which is amendment C5's whole argument.
    /// So there is no implementor for whom the weaker form would be true,
    /// and nothing is excluded that was not already.
    ///
    /// The cost is one allocation per blocking call — set against handing
    /// work to a thread pool, which is what the call is for.
    fn run<T, F>(&self, f: F) -> BoxFuture<'_, Result<T, Cancelled>>
    where
        T: Send + 'static, // send-bound-exception: amendment-C5
        F: FnOnce() -> T + Send + 'static; // send-bound-exception: amendment-C5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_is_generic_over_the_future_not_boxed() {
        // The shape is copied from hyper::rt::Executor: generic over F,
        // zero bounds in the declaration. Send is added by the impl, not
        // the trait.
        struct Immediate;
        impl<F: std::future::Future<Output = ()>> Spawn<F> for Immediate {
            fn spawn(&self, f: F) {
                futures_executor::block_on(f);
            }
        }
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let d = done.clone();
        // !Send future — the trait allows this.
        Immediate.spawn(async move { d.set(true) });
        assert!(done.get());
    }
}
