//! [`TokioHandle`]: the same capabilities as [`Tokio`], with the runtime
//! **carried as a value** instead of read out of a thread-local.
//!
//! # Why a second type rather than a fix to the first
//!
//! [`Tokio`] is a ZST. Everything it does it does through tokio's ambient
//! context, which means every one of its methods has a precondition —
//! "there is a runtime on this thread" — that the type does not state and
//! cannot check. Off a runtime thread the failure is a panic from inside
//! tokio, at a call site in *this* crate, naming neither the caller nor the
//! capability:
//!
//! ```text
//! there is no reactor running, must be called from the context of a Tokio 1.x runtime
//! ```
//!
//! `TokioHandle` holds a `tokio::runtime::Handle`, which cannot be obtained
//! without a runtime. The precondition is discharged once, at construction,
//! where it is a `Result` ([`TokioHandle::current`]) rather than a panic —
//! and it is then carried, so the capabilities are usable from a thread
//! that is not inside the runtime. This is the same move `hclient` makes
//! everywhere else: **a default must not be stronger than the truth**.
//!
//! `Tokio` is not deprecated by it, and is still what [`crate`]'s other
//! users get by default. It is a ZST — it costs a pointer less and it
//! composes with `Default` — and inside `#[tokio::main]`, which is where
//! almost all of this code runs, its precondition always holds.
//!
//! # Exactly what carrying the handle buys, measured
//!
//! Not "everything". Each capability was probed on tokio 1.53.1, from a
//! plain `std::thread` with no runtime entered, and the answers differ:
//!
//! | capability | off-runtime, via `Tokio` | off-runtime, via `TokioHandle` |
//! |---|---|---|
//! | [`Spawn::spawn`] | panics (`tokio::spawn` reads the context) | works, and the task runs on the runtime's threads |
//! | [`Blocking::run`] | panics | works |
//! | [`Timer::sleep`] | panics **at the call**, not at first poll | works — and the returned `Sleep` can then be polled by *any* executor |
//! | [`Timer::now`], [`Timer::elapsed_since`] | already worked | unchanged — `tokio::time::Instant::now()` never read the context |
//! | [`TcpAdoptStd::adopt`] | panics (`TcpStream::from_std` registers with the reactor) | works |
//! | [`TcpConnect::connect`] | panics | **still panics, and that is not fixable here** |
//! | [`IpcConnect::connect_ipc`](hclient_rt::IpcConnect::connect_ipc) | panics | still panics, for `connect`'s reason |
//!
//! The last row is the interesting one, so it is written down rather than
//! left for someone to rediscover. `connect` is an `async fn`: the reactor
//! registration happens in its body, at *first poll*, not at the call. A
//! `Handle::enter()` guard cannot cover that — a guard held across an
//! `.await` would be setting a thread-local on whichever thread happened to
//! poll next, which is wrong whenever that is a different thread, and
//! `EnterGuard` is `!Send` precisely so that the compiler says so. Measured:
//! an unconnected `tokio::net::TcpSocket` built *under* an `enter()` guard,
//! then `connect`ed on `futures_executor::block_on` off the runtime, panics
//! with the message above from `tokio-1.53.1/src/net/tcp/stream.rs`.
//!
//! Making it work would mean `Handle::spawn`ing the connect and awaiting a
//! `JoinHandle`, which drops cancellation on the floor: `Native`'s Happy
//! Eyeballs cancels losing attempts by dropping their futures, and a
//! spawned task ignores that. A capability that quietly stopped being
//! cancellable is worse than one that is honestly unchanged.
//!
//! So the rule for `TokioHandle` is: **entry points become callable from
//! anywhere; futures still have to be driven where they always were.**
//! [`Timer::sleep`] is the one place where those coincide, because tokio
//! captures the timer context when the `Sleep` is *built* — measured, a
//! 120 ms sleep built off-runtime under a guard and awaited on
//! `futures_executor::block_on` completed in 120.96 ms.

use super::{Tokio, TokioIo, classify};
use futures_core::future::BoxFuture;
use hclient_rt::{Blocking, Cancelled, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, TcpSupport, Timer};
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

/// [`Tokio`] with the runtime handle carried rather than looked up.
///
/// `Send + Sync + Clone`, because `tokio::runtime::Handle` is: unlike a
/// `!Send` local-executor type, this costs `Native<TokioHandle, _, _>` none
/// of its auto traits. See the module doc for what it buys and what it
/// does not.
///
/// # Precondition: the runtime is still alive
///
/// A `Handle` does not keep its `Runtime` alive. While the runtime this
/// handle came from is running, every entry point here is callable from
/// any thread (the module doc's table says which futures must still be
/// polled on a runtime thread). **Once that `Runtime` has been dropped or shut down**, tokio
/// behaves differently per call, and none of it is this crate's choice:
///
/// - [`Spawn::spawn`] accepts the future and tokio discards it unpolled —
///   no panic and no error. `hclient_rt::Spawn` forbids exactly that
///   shape, so keeping the runtime alive for as long as anything built on
///   this handle (a `Native`, a `Client`) can spawn is the caller's side of
///   the contract.
/// - [`Blocking::run`] answers [`Cancelled`], as the seam asks.
/// - [`Timer::sleep`] and the connects panic inside tokio.
///
/// Pinned by `spawn_after_the_runtime_is_dropped_runs_nothing_and_says_nothing`,
/// so this paragraph fails a test rather than going stale if tokio changes
/// the first answer.
#[derive(Debug, Clone)]
pub struct TokioHandle(tokio::runtime::Handle);

impl TokioHandle {
    /// The precondition as a value: `Err` off a runtime, where [`Tokio`]
    /// would have panicked later, somewhere else.
    ///
    /// The error type is tokio's own `TryCurrentError` rather than one of
    /// ours. There is exactly one way to fail — no runtime — and wrapping
    /// it would add a name without adding information, while losing the
    /// `kind()` tokio already exposes.
    ///
    /// # Errors
    ///
    /// tokio's own [`tokio::runtime::TryCurrentError`] when this is called
    /// outside a tokio runtime — see above.
    pub fn current() -> Result<Self, tokio::runtime::TryCurrentError> {
        tokio::runtime::Handle::try_current().map(Self)
    }

    /// For a handle obtained from a runtime the caller built itself
    /// (`Runtime::handle().clone()`), which is the case
    /// [`TokioHandle::current`] cannot serve: the thread holding the
    /// `Runtime` is not inside it.
    pub fn from_handle(h: tokio::runtime::Handle) -> Self {
        Self(h)
    }

    /// The handle back out, for interop with code that takes tokio's own
    /// type. Borrowed, not cloned, so a caller pays for the clone only if
    /// it needs one.
    pub fn handle(&self) -> &tokio::runtime::Handle {
        &self.0
    }
}

impl Timer for TokioHandle {
    /// Delegated, exactly: an instant from `TokioHandle` and one from
    /// [`Tokio`] are the same value and can be compared. Anything else
    /// would make the two types silently non-interchangeable.
    type Instant = <Tokio as Timer>::Instant;
    type Sleep = <Tokio as Timer>::Sleep;

    /// The one capability where the handle changes what the *returned
    /// value* can do, not just where the call may happen: `tokio::time::
    /// sleep` reads the timer context when it builds the `Sleep`, so under
    /// the guard the context is captured and the future is afterwards
    /// pollable by any executor. Measured in
    /// `sleep_built_off_runtime_is_polled_by_a_foreign_executor`.
    fn sleep(&self, d: Duration) -> Self::Sleep {
        // A named binding, not `let _ =`: `let _ = self.0.enter()` drops
        // the guard at the end of the statement and the next line is once
        // again outside the runtime. Not hypothetical — it is the standard
        // way to get this wrong, and `enter`'s own docs say so.
        let _guard = self.0.enter();
        Tokio.sleep(d)
    }

    fn now(&self) -> Self::Instant {
        // No guard: `tokio::time::Instant::now()` never reads the context
        // — measured, it does not panic off a runtime. A guard here would
        // suggest a requirement that does not exist.
        Tokio.now()
    }

    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        Tokio.elapsed_since(earlier)
    }
}

/// The impl this type exists for. `Handle::spawn` needs no guard and does
/// not panic, and the task runs on the runtime's own threads — while that
/// runtime is alive, which is [`TokioHandle`]'s stated precondition.
impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for TokioHandle {
    fn spawn(&self, f: F) {
        self.0.spawn(f);
    }
}

impl Blocking for TokioHandle {
    /// `Handle::spawn_blocking` is total in the same way `Handle::spawn`
    /// is, so this needs no guard either — only the same `classify`,
    /// which is shared with [`Tokio`] rather than repeated, so the
    /// panic-vs-`Cancelled` distinction cannot drift between the two.
    fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
        &self,
        f: F,
    ) -> BoxFuture<'_, Result<T, Cancelled>> {
        Box::pin(async move { classify(self.0.spawn_blocking(f).await) })
    }
}

impl TcpConnect for TokioHandle {
    type Stream = TokioIo;

    /// **Exactly what [`Tokio`] declares, and both other answers were
    /// bugs.**
    /// `connect` below delegates to `Tokio::connect`, which applies every
    /// option on the `socket2::Socket` — so the options really are applied
    /// here. Without this line the trait's `NONE` default stood, and
    /// `TcpOpts::reject_unsupported` turned a `nodelay: true` a caller had
    /// asked for into a refused connect: a capability understating its own
    /// code, which is the shape this workspace has caught repeatedly.
    ///
    /// It mattered more than a stray default because `TokioHandle` is the
    /// runtime the HTTP/3 race was measured on, and v0.4's race measurement
    /// found Nagle costing 41 ms on the head of every connection made
    /// without it.
    ///
    /// The second was the line that fixed the first: it declared
    /// `TcpSupport::ALL` while `Tokio` declared a per-target set, so on
    /// macOS and Windows this claimed `bind_device` and `user_timeout`,
    /// `reject_unsupported` let them through, and `Tokio::connect` did
    /// not apply them. A delegate's claim is its delegate's.
    const TCP_SUPPORT: TcpSupport = <Tokio as TcpConnect>::TCP_SUPPORT;

    /// [`Tokio`]'s, forwarded — including the `Send`, which is what lets
    /// a handle stand in for the ambient runtime wherever one is proved.
    type Connecting<'a> =
        std::pin::Pin<Box<dyn Future<Output = std::io::Result<TokioIo>> + Send + 'a>>;

    /// **Delegated to [`Tokio`]'s, with no `EnterGuard`.** See the module
    /// doc's last table row: the registration a guard would have to cover
    /// happens at first poll, inside the async body, where no guard can
    /// reach it — so this future must be polled on a runtime thread, as
    /// `Tokio`'s must. Delegating rather than copying keeps the option
    /// check and `build_socket`'s "all options, once, on the
    /// `socket2::Socket`" stated in exactly one place.
    fn connect<'a>(&'a self, addr: SocketAddr, opts: &TcpOpts) -> Self::Connecting<'a> {
        let opts = opts.clone();
        Box::pin(async move { Tokio.connect(addr, &opts).await })
    }
}

impl hclient_rt::IpcConnect for TokioHandle {
    /// [`Tokio`]'s, for the reason `TCP_SUPPORT` is: `connect_ipc` below
    /// delegates to it, and a delegate's claim is its delegate's.
    const IPC_SUPPORT: hclient_rt::IpcSupport = <Tokio as hclient_rt::IpcConnect>::IPC_SUPPORT;

    type ConnectingIpc<'a>
        = std::pin::Pin<Box<dyn Future<Output = std::io::Result<TokioIo>> + Send + 'a>>
    where
        Self: 'a;

    /// **Delegated to [`Tokio`]'s, with no `EnterGuard`**, for the reason
    /// `connect` gives: the socket registers with the reactor at first
    /// poll, inside the async body, so this future must be polled on a
    /// runtime thread. What the handle adds is that `Native::unix_socket`
    /// accepts it at all.
    fn connect_ipc<'a>(&'a self, addr: &hclient_rt::IpcAddr) -> Self::ConnectingIpc<'a> {
        let addr = addr.clone();
        Box::pin(async move { hclient_rt::IpcConnect::connect_ipc(&Tokio, &addr).await })
    }
}

impl TcpAdoptStd for TokioHandle {
    /// Unlike `connect`, this one is a plain `fn`: all of it — including
    /// `TcpStream::from_std`'s reactor registration — happens before it
    /// returns, so a guard covers all of it. Measured: `Tokio::adopt` off
    /// a runtime panics, this does not.
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<TokioIo> {
        let _guard = self.0.enter();
        Tokio.adopt(std)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Everything below asks the same question — "does this work on a
    /// thread with no runtime?" — so the runtime must not be *entered* on
    /// the thread running the test body. `#[tokio::test]` enters one, so
    /// these are plain `#[test]`s that build a runtime, take its handle
    /// with [`TokioHandle::from_handle`], and do the work on a spawned
    /// `std::thread`. A spawned thread and not just "outside `block_on`",
    /// because a `Runtime` value on the current thread is enough for some
    /// of tokio's context lookups.
    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime")
    }

    fn panicked<F: FnOnce() + Send + 'static>(f: F) -> bool {
        std::thread::spawn(move || {
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            std::panic::set_hook(prev);
            r.is_err()
        })
        .join()
        .expect("probe thread")
    }

    #[test]
    fn current_reports_a_missing_runtime_as_a_value_rather_than_a_panic() {
        // The whole reason the type exists: the precondition is checkable.
        assert!(
            !panicked(|| {
                TokioHandle::current().expect_err("no runtime on a bare thread");
            }),
            "TokioHandle::current must not panic off a runtime"
        );

        let rt = rt();
        let inside = rt.block_on(async { TokioHandle::current() });
        assert!(
            inside.is_ok(),
            "TokioHandle::current must succeed inside a runtime"
        );
    }

    #[test]
    fn spawn_is_total_off_a_runtime_where_the_zst_panics() {
        let rt = rt();
        let h = TokioHandle::from_handle(rt.handle().clone());

        // The control. Without it this test would pass just as well
        // against a `Tokio` that had somehow become total, and would stop
        // saying anything about the difference between the two types.
        assert!(
            panicked(|| Spawn::spawn(&Tokio, async {})),
            "the ZST's spawn is supposed to panic off a runtime — if this \
             stops being true, the reason TokioHandle exists has changed"
        );

        let ran = Arc::new(AtomicBool::new(false));
        let r = ran.clone();
        let h2 = h.clone();
        assert!(
            !panicked(move || Spawn::spawn(&h2, async move { r.store(true, Ordering::SeqCst) })),
            "TokioHandle::spawn must not panic off a runtime"
        );

        // Accepting the task is not running it: a spawner that swallowed
        // the future would pass the assertion above. Drive the runtime
        // until it has actually run.
        rt.block_on(async {
            for _ in 0..200 {
                if ran.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        assert!(
            ran.load(Ordering::SeqCst),
            "the task spawned through the handle never ran"
        );
    }

    #[test]
    fn blocking_is_total_off_a_runtime_where_the_zst_panics() {
        let rt = rt();
        let h = TokioHandle::from_handle(rt.handle().clone());

        assert!(
            panicked(|| {
                let _ = futures_executor::block_on(Blocking::run(&Tokio, || 6 * 7));
            }),
            "the ZST's blocking run is supposed to panic off a runtime"
        );

        let out = std::thread::spawn(move || futures_executor::block_on(h.run(|| 6 * 7)))
            .join()
            .expect("probe thread");
        assert_eq!(
            out,
            Ok(42),
            "the handle's blocking run must produce the closure's value off a runtime"
        );
    }

    #[test]
    fn sleep_built_off_runtime_is_polled_by_a_foreign_executor() {
        let rt = rt();
        let h = TokioHandle::from_handle(rt.handle().clone());

        assert!(
            panicked(|| {
                // `drop`, not `let _ =`: the claim under test is that the
                // PANIC happens at the call, so the future is never polled
                // and clippy's `let_underscore_future` is right to object.
                drop(Tokio.sleep(Duration::from_millis(1)));
            }),
            "tokio::time::sleep is supposed to panic at the CALL off a \
             runtime — if it became lazy, TokioHandle::sleep's guard is \
             covering nothing and the comment on it is wrong"
        );

        // No tokio in this block_on at all: `futures_executor`. The claim
        // is that the context is captured when the `Sleep` is built, so
        // afterwards any executor can drive it.
        let elapsed = std::thread::spawn(move || {
            let s = h.sleep(Duration::from_millis(120));
            let t0 = std::time::Instant::now();
            futures_executor::block_on(s);
            t0.elapsed()
        })
        .join()
        .expect("probe thread");

        assert!(
            elapsed >= Duration::from_millis(120),
            "a 120ms sleep returned after {elapsed:?} — it did not wait"
        );
        // The upper bound is what fails if the sleep resolves immediately
        // AND the lower bound is somehow met by scheduling noise; it is
        // generous because CI machines are not real-time.
        assert!(
            elapsed < Duration::from_secs(5),
            "a 120ms sleep took {elapsed:?} — it was never woken"
        );
    }

    #[test]
    fn adopt_is_total_off_a_runtime_where_the_zst_panics() {
        let rt = rt();
        let h = TokioHandle::from_handle(rt.handle().clone());

        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("local_addr");
        let acceptor = std::thread::spawn(move || {
            let mut kept = Vec::new();
            for _ in 0..2 {
                if let Ok((s, _)) = l.accept() {
                    kept.push(s);
                }
            }
            kept
        });

        assert!(
            panicked(move || {
                let s = std::net::TcpStream::connect(addr).expect("connect");
                let _ = Tokio.adopt(s);
            }),
            "the ZST's adopt is supposed to panic off a runtime — \
             TcpStream::from_std registers with the reactor"
        );

        let adopted = std::thread::spawn(move || {
            let s = std::net::TcpStream::connect(addr).expect("connect");
            h.adopt(s).is_ok()
        })
        .join()
        .expect("probe thread");
        assert!(adopted, "TokioHandle::adopt must work off a runtime");

        drop(acceptor.join());
        drop(rt);
    }

    /// The delegation is exact, so a value from one clock is comparable
    /// with a value from the other. This is what stops the two types from
    /// being silently non-interchangeable as a runtime for `Native`.
    ///
    /// Both directions, deliberately. The first version of this test
    /// measured only `Tokio::elapsed_since(h.now())`, and a mutant making
    /// `TokioHandle::elapsed_since` return a constant hour SURVIVED it:
    /// the handle's own method was never called.
    #[test]
    fn the_two_clocks_are_the_same_clock() {
        let rt = rt();
        let h = TokioHandle::from_handle(rt.handle().clone());
        let a = Tokio.now();
        let b = h.now();
        assert!(b >= a, "the handle's clock ran backwards against the ZST's");

        // The handle measuring an instant the ZST produced...
        let by_handle = h.elapsed_since(a);
        // ...and the ZST measuring one the handle produced.
        let by_zst = Tokio.elapsed_since(b);
        assert!(
            by_handle < Duration::from_secs(1),
            "TokioHandle::elapsed_since reported {by_handle:?} for an instant \
             taken microseconds earlier — it is not reading the same clock"
        );
        assert!(
            by_zst < Duration::from_secs(1),
            "Tokio::elapsed_since reported {by_zst:?} for an instant the \
             handle produced — the two Instant types are not the same clock"
        );

        // **And a lower bound, which the two upper bounds above cannot
        // give.** A mutant answering `Duration::ZERO` from
        // `TokioHandle::elapsed_since` satisfies `< 1s` — the same shape
        // as the constant hour the first version missed, from the other
        // side. Measured: that mutant survived a crate-scoped sweep until
        // this line existed.
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            h.elapsed_since(a) >= Duration::from_millis(20),
            "TokioHandle::elapsed_since must measure time that passed"
        );
    }

    /// **"Total" holds while the runtime the handle came from is alive,
    /// and not after.** A `tokio::runtime::Handle` does not keep its
    /// `Runtime` alive: once the `Runtime` is dropped, `Handle::spawn`
    /// accepts the future and tokio discards it unpolled — no panic, no
    /// error, nothing run. Measured on tokio 1.x from an outside consumer
    /// before this test was written.
    ///
    /// That is the one shape `hclient_rt::Spawn` forbids — *accept the
    /// future and drop it* — so it is stated on [`TokioHandle`] as its
    /// precondition, and pinned here so that the statement goes stale
    /// loudly if tokio ever changes the answer (to a panic, say, which
    /// would move this type's precondition rather than remove it).
    #[test]
    fn spawn_after_the_runtime_is_dropped_runs_nothing_and_says_nothing() {
        let rt = rt();
        let h = TokioHandle::from_handle(rt.handle().clone());
        drop(rt);

        let ran = Arc::new(AtomicBool::new(false));
        let r = ran.clone();
        assert!(
            !panicked(move || Spawn::spawn(&h, async move { r.store(true, Ordering::SeqCst) })),
            "tokio started panicking on a spawn after shutdown — TokioHandle's \
             precondition moved; update its documentation"
        );
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !ran.load(Ordering::SeqCst),
            "a future spawned onto a dropped runtime ran — tokio changed, and \
             TokioHandle's stated precondition is stale"
        );
    }

    /// `TokioHandle` delegates `connect` to `Tokio`, so it applies every
    /// option — and until v0.4 it declared none, because it left
    /// `TcpConnect::TCP_SUPPORT` to the trait's `NONE` default. The effect was
    /// not cosmetic: `TcpOpts::reject_unsupported` turned a `nodelay: true`
    /// the caller asked for into a refused connect.
    ///
    /// Shaped like `Tokio`'s own pair of tests rather than asserting the
    /// constant: the socket is asked what it got, and the declaration is
    /// compared against *that*. A constant checked against itself is a
    /// constant nobody checks.
    #[tokio::test]
    async fn the_handle_declares_the_options_it_actually_applies() {
        use hclient_rt::{TcpConnect, TcpOpts};

        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            let _ = l.accept();
        });

        let rt = TokioHandle::current().expect("inside #[tokio::test]");
        let opts = TcpOpts::default().nodelay(true);
        let s = rt.connect(addr, &opts).await.expect(
            "a runtime that applies nodelay must not refuse it -- if this \
             fails with an unsupported-option error, TCP_SUPPORT has drifted \
             below what connect does",
        );
        let applied = s.get_ref().nodelay().expect("nodelay query");
        assert!(applied, "nodelay did not reach the socket");
        assert_eq!(<TokioHandle as TcpConnect>::TCP_SUPPORT.nodelay, applied);
    }

    /// `TokioHandle` reaches a Unix-domain socket as `Tokio` does, and
    /// claims exactly `Tokio`'s support. The connection is checked from
    /// the listener's side, so a `connect_ipc` that answered without
    /// dialing would fail here.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_handle_dials_a_unix_socket_and_claims_what_tokio_claims() {
        use hclient_rt::IpcConnect;

        let dir = std::env::temp_dir().join(format!(
            "hclient-tokio-handle-unix-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("a clock after 1970")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let accepted = std::thread::spawn(move || listener.accept().is_ok());

        let rt = TokioHandle::current().expect("inside #[tokio::test]");
        let _s = rt
            .connect_ipc(&hclient_rt::IpcAddr::unix(&path))
            .await
            .expect("connect_ipc through the handle");
        assert!(
            accepted.join().expect("listener thread"),
            "the listener saw no connection"
        );
        assert_eq!(
            <TokioHandle as IpcConnect>::IPC_SUPPORT,
            <Tokio as IpcConnect>::IPC_SUPPORT
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
