//! `http-ng-rt` capabilities implemented on top of tokio.
#![forbid(unsafe_code)]

mod io;

pub use io::TokioIo;

use http_ng_rt::{Blocking, Cancelled, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, Timer};
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

/// ZST: the tokio handle is picked up from the ambient runtime, the same
/// way reqwest does it. Outside a runtime, `spawn`/`sleep` panic —
/// documented behavior.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tokio;

impl Timer for Tokio {
    type Instant = tokio::time::Instant;
    /// `tokio::time::Sleep` already resolves to `()`, so this side needs
    /// no adapter — unlike smol's, see `http-ng-rt-smol`.
    type Sleep = tokio::time::Sleep;
    fn sleep(&self, d: Duration) -> Self::Sleep {
        tokio::time::sleep(d)
    }
    fn now(&self) -> Self::Instant {
        tokio::time::Instant::now()
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        tokio::time::Instant::now().saturating_duration_since(earlier)
    }
}

impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for Tokio {
    fn spawn(&self, f: F) {
        tokio::spawn(f);
    }
}

impl Blocking for Tokio {
    /// `tokio::task::spawn_blocking` returns `JoinError` for two distinct
    /// cases, and `caps::Blocking`'s contract (fix round 1, coordinator)
    /// requires not conflating them: the closure panicked, OR the
    /// background thread pool went away before the task got to start (not
    /// hypothetical — happens when a `spawn_blocking` task is still queued
    /// on the pool and the runtime starts shutting down). The first is
    /// re-raised as a panic (`resume_unwind`, the original payload reaches
    /// the calling code as-is); the second is a typed `Cancelled`, not a
    /// panic: it isn't a bug in the calling code, but an ordinary runtime
    /// lifecycle event.
    ///
    /// `classify` is factored out into its own function and covered by a
    /// unit test against a REAL `JoinError` — see
    /// `tests::classify_reports_cancelled_for_a_join_error_that_is_not_a_panic`
    /// and its comment on why the `JoinError` there is obtained via
    /// `AbortHandle::abort()` rather than by racing a whole-runtime
    /// shutdown.
    async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
        &self,
        f: F,
    ) -> Result<T, Cancelled> {
        classify(tokio::task::spawn_blocking(f).await)
    }
}

fn classify<T>(r: Result<T, tokio::task::JoinError>) -> Result<T, Cancelled> {
    match r {
        Ok(v) => Ok(v),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(_) => Err(Cancelled),
    }
}

impl TcpConnect for Tokio {
    type Stream = TokioIo;

    async fn connect(&self, addr: SocketAddr, opts: &TcpOpts) -> std::io::Result<TokioIo> {
        // Options are applied on the `socket2::Socket` **once**, and the
        // runtime adopts the finished descriptor. This is exactly the seam
        // `TcpAdoptStd` provides: without it, every runtime crate would
        // rewrite this whole rigmarole again.
        let sock = build_socket(addr, opts)?;
        sock.set_nonblocking(true)?;
        let std_stream: std::net::TcpStream = sock.into();
        let tcp = tokio::net::TcpSocket::from_std_stream(std_stream)
            .connect(addr)
            .await?;
        Ok(TokioIo::new(tcp))
    }
}

impl TcpAdoptStd for Tokio {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<TokioIo> {
        std.set_nonblocking(true)?;
        Ok(TokioIo::new(tokio::net::TcpStream::from_std(std)?))
    }
}

/// The whole `TcpOpts` list is applied here, on the `socket2::Socket`,
/// BEFORE the descriptor is ever handed to tokio — exactly what the doc
/// comment on `TcpConnect::connect` promises: the runtime only adopts a
/// finished socket.
///
/// Fix round 1 (coordinator): previously, `nodelay`/`keepalive` were
/// applied in a separate step AFTER `connect()`, on the already
/// tokio-wrapped `TcpStream` (`apply_post_connect`), even though this
/// comment already promised "once, on the `socket2::Socket`" at the time.
/// Not a bug (`TCP_NODELAY`/`SO_KEEPALIVE` behave identically whether set
/// before or after `connect()`), but a mismatch between the text and the
/// code — and Task 4 (`http-ng-rt-smol`) was going to copy this exact
/// file. Both `nodelay` and `keepalive` can be set on a `socket2::Socket`
/// before `connect()`; no exception turned up that would have to stay
/// post-connect — the whole list now lives in one place.
fn build_socket(addr: SocketAddr, opts: &TcpOpts) -> std::io::Result<socket2::Socket> {
    let domain = socket2::Domain::for_address(addr);
    let sock = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
    if opts.reuse_address {
        sock.set_reuse_address(true)?;
    }
    if let Some(size) = opts.send_buffer_size {
        sock.set_send_buffer_size(size)?;
    }
    if let Some(size) = opts.recv_buffer_size {
        sock.set_recv_buffer_size(size)?;
    }
    if let Some(ip) = opts.local_address {
        sock.bind(&SocketAddr::new(ip, 0).into())?;
    }
    if opts.nodelay {
        sock.set_tcp_nodelay(true)?;
    }
    if let Some(d) = opts.keepalive {
        sock.set_tcp_keepalive(&socket2::TcpKeepalive::new().with_time(d))?;
    }
    Ok(sock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_rt::{Blocking, Cancelled, TcpConnect, TcpOpts, Timer};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[tokio::test]
    async fn timer_sleeps_and_measures() {
        let t = Tokio;
        let start = t.now();
        t.sleep(Duration::from_millis(20)).await;
        assert!(t.elapsed_since(start) >= Duration::from_millis(20));
    }

    #[tokio::test]
    async fn blocking_runs_off_the_reactor() {
        let out = Tokio.run(|| 6 * 7).await;
        assert_eq!(out, Ok(42));
    }

    #[tokio::test]
    #[should_panic(expected = "boom")]
    async fn blocking_propagates_the_original_panic_payload() {
        // Checks `resume_unwind`, not the text of `.expect(...)`: if `run`
        // simply did `.expect("blocking task panicked")`, the panic
        // message here would be that string, not `"boom"` — the closure's
        // original payload would be lost.
        let _ = Tokio.run(|| panic!("boom")).await;
    }

    #[test]
    fn classify_reports_cancelled_for_a_join_error_that_is_not_a_panic() {
        // `Tokio::run`'s own `spawn_blocking` handle is private — there's no
        // hook from outside to call `.abort()` on it specifically. Instead,
        // the test builds a REAL `tokio::task::JoinError` with
        // `is_cancelled() == true, is_panic() == false` — exactly the shape
        // `classify` must turn into `Cancelled` — the same way tokio itself
        // produces it: a `spawn_blocking` task still queued on the pool is
        // cancelled before a worker thread gets to pick it up.
        //
        // Racing an ACTUAL runtime shutdown was tried first and rejected:
        // with `max_blocking_threads(1)` occupied by its single thread, a
        // second `spawn_blocking` gets queued, an observer task
        // (`tokio::spawn(async { Tokio.run(f).await })`) awaits it, and
        // `Runtime::shutdown_timeout` is called immediately after — but
        // shutdown consistently killed the observer task itself before it
        // could report its result, EVEN in runs where the blocking closure
        // had already executed (`BLOCKING_RAN=true`, yet the channel was
        // still `Disconnected`). Confirmed empirically (5/5 runs) with a
        // throwaway probe on tokio 1.53.1 outside this crate — racing a
        // whole-runtime shutdown is not a reliable basis for a test.
        // `AbortHandle::abort()` on a task not yet picked up from the queue
        // is a deterministic, documented way to get the same `JoinError`
        // shape without that race (also confirmed empirically, 5/5).
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            // Occupy the single blocking thread so the second
            // `spawn_blocking` is guaranteed to queue up rather than start
            // running immediately.
            let (started_tx, started_rx) = mpsc::channel::<()>();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let occupier = tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            started_rx.recv().unwrap();

            let ran = std::sync::Arc::new(AtomicBool::new(false));
            let ran_inner = ran.clone();
            let handle = tokio::task::spawn_blocking(move || {
                ran_inner.store(true, Ordering::SeqCst);
            });
            // Give the scheduler time to actually push the task into the
            // pool's queue (in this version it happens synchronously
            // inside `spawn_blocking`, but a generous margin doesn't hurt).
            tokio::time::sleep(Duration::from_millis(20)).await;

            handle.abort();
            // Release the "occupier" so the worker thread actually reaches
            // the queue and processes the cancelled task.
            let _ = release_tx.send(());
            let join_err = handle
                .await
                .expect_err("a task aborted before it ran must return a JoinError");

            assert!(
                !ran.load(Ordering::SeqCst),
                "the closure should never have run"
            );
            assert!(join_err.is_cancelled());
            assert!(!join_err.is_panic());

            assert_eq!(classify::<()>(Err(join_err)), Err(Cancelled));

            let _ = occupier.await;
        });
    }

    #[tokio::test]
    async fn connects_to_a_local_listener_with_options() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });

        let opts = TcpOpts {
            nodelay: true,
            ..Default::default()
        };
        let s = Tokio.connect(addr, &opts).await.expect("connect");
        // Not just "the connection succeeded" (that would pass even if
        // `build_socket` silently ignored `opts`), but that `nodelay: true`
        // actually reached the socket: read the option back from the
        // `TcpStream` itself, rather than relying on the call having
        // happened.
        assert!(
            s.get_ref().nodelay().expect("nodelay query"),
            "TcpOpts::nodelay was not applied to the connected socket"
        );
    }

    #[tokio::test]
    async fn connects_with_keepalive_enabled() {
        // The same principle as the nodelay test above, for the second
        // option that moved into `build_socket` in this round of fixes:
        // `keepalive` is read back, not just checked for `connect()`
        // returning `Ok`. `tokio::net::TcpStream` gives no direct getter
        // for `SO_KEEPALIVE` — use `socket2::SockRef`, the same type
        // `apply_post_connect` (in the earlier version of this file) used,
        // just for reading instead of writing.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });

        let opts = TcpOpts {
            keepalive: Some(Duration::from_secs(30)),
            ..Default::default()
        };
        let s = Tokio.connect(addr, &opts).await.expect("connect");
        let enabled = socket2::SockRef::from(s.get_ref())
            .keepalive()
            .expect("keepalive query");
        assert!(
            enabled,
            "TcpOpts::keepalive was not applied to the connected socket"
        );
    }
}
