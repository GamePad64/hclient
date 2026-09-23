use crate::error::Cancelled;
use futures_core::future::BoxFuture;
use std::future::Future;

/// The shape is deliberately copied from `hyper::rt::Executor`: generic
/// over the future, zero bounds in the declaration. `Send` is added by the
/// `impl`, not the trait, so single-threaded runtimes can implement it
/// honestly.
pub trait Spawn<F: Future<Output = ()>> {
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
