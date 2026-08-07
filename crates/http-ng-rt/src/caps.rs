use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// The shape is deliberately copied from `hyper::rt::Executor`: generic
/// over the future, zero bounds in the declaration. `Send` is added by the
/// `impl`, not the trait, so single-threaded runtimes can implement it
/// honestly.
pub trait Spawn<F: Future<Output = ()>> {
    fn spawn(&self, f: F);
}

/// Socket options are applied in http-ng **once**, on the `socket2::Socket`,
/// and the runtime only adopts the descriptor (`TcpAdoptStd`). Otherwise
/// every runtime crate would rewrite this whole rigmarole again.
#[derive(Debug, Clone, Default)]
pub struct TcpOpts {
    pub nodelay: bool,
    pub keepalive: Option<Duration>,
    pub local_address: Option<IpAddr>,
    pub send_buffer_size: Option<usize>,
    pub recv_buffer_size: Option<usize>,
    pub reuse_address: bool,
}

pub trait TcpConnect {
    type Stream: hyper::rt::Read + hyper::rt::Write + Unpin;

    fn connect(
        &self,
        addr: SocketAddr,
        opts: &TcpOpts,
    ) -> impl Future<Output = std::io::Result<Self::Stream>>;
}

/// On platforms with file descriptors, the whole set of socket options is
/// applied outside the runtime, and the runtime only adopts the finished
/// socket.
pub trait TcpAdoptStd: TcpConnect {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream>;
}

/// A separate trait, not a method: `getaddrinfo` blocks, and wasm and
/// embedded have no blocking pool at all. The absence of the capability
/// must be a compile error, not an `unimplemented!()` in the runtime.
///
/// **The one place in the whole project where we declare `Send` ourselves**,
/// and here it's honest: both `tokio::task::spawn_blocking` and
/// `blocking::unblock` require `Send + 'static`, and the `Blocking`
/// capability doesn't exist on wasm at all — there's nothing for it to
/// infect. The justification is `amendment-C5`
/// (`docs/superpowers/specs/2026-08-05-http-ng-design.md`), an amendment
/// separate from C1/C2: those two are about erasing auto-traits in `dyn
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
    fn run<T, F>(&self, f: F) -> impl Future<Output = Result<T, Cancelled>>
    where
        T: Send + 'static, // send-bound-exception: amendment-C5
        F: FnOnce() -> T + Send + 'static; // send-bound-exception: amendment-C5
}

/// The background thread pool that `Blocking::run` was supposed to run on
/// went away before the task got to start — for example, the runtime is
/// shutting down while the task is still queued. No payload: this is not a
/// failure of `f` (`f` never ran at all), but a signal from the runtime
/// that there will be no result.
///
/// A panic in `f`, by contrast, does NOT become `Cancelled` — it is
/// re-raised as a panic by the `Blocking` implementation, see the trait's
/// doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("blocking task pool went away before the work started")
    }
}

impl std::error::Error for Cancelled {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_opts_default_is_conservative() {
        // All SIX fields, not four: a hand-written `Default` that set
        // `send_buffer_size`/`recv_buffer_size` to `Some(1 << 20)` would
        // pass this test unnoticed if only the other four were checked
        // (carried finding, review Task 1). Today `#[derive(Default)]`
        // gives `None` by construction, but the test's name promises the
        // whole struct — so the test must check the whole struct.
        let o = TcpOpts::default();
        assert!(!o.nodelay, "the user turns nodelay on, not us");
        assert!(o.keepalive.is_none());
        assert!(o.local_address.is_none());
        assert!(o.send_buffer_size.is_none());
        assert!(o.recv_buffer_size.is_none());
        assert!(!o.reuse_address);
    }

    #[test]
    fn spawn_is_generic_over_the_future_not_boxed() {
        // The shape is copied from hyper::rt::Executor: generic over F,
        // zero bounds in the declaration. Send is added by the impl, not
        // the trait.
        struct Immediate;
        impl<F: std::future::Future<Output = ()>> Spawn<F> for Immediate {
            fn spawn(&self, f: F) {
                futures_executor::block_on(f)
            }
        }
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let d = done.clone();
        // !Send future — the trait allows this.
        Immediate.spawn(async move { d.set(true) });
        assert!(done.get());
    }
}
