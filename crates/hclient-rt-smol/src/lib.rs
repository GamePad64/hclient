//! `hclient-rt` capabilities implemented on top of smol.
//!
//! **No `async-compat`.** It spins up a second runtime in-process if no
//! tokio context is found — which hides exactly the problem this vertical
//! is meant to surface.
#![forbid(unsafe_code)]

#[cfg(feature = "udp")]
mod udp;

#[cfg(feature = "udp")]
pub use udp::SmolUdpSocket;

use futures_core::future::BoxFuture;
use hclient_rt::{
    Blocking, Cancelled, Discard, FuturesIo, Spawn, TcpAdoptStd, TcpConnect, TcpOpts,
    TcpOptsSupport, Timer,
};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default)]
pub struct Smol;

impl Timer for Smol {
    type Instant = Instant;
    /// **The adapter is not redundant, and it is not a mistake.**
    /// `async_io::Timer` is a `Future` whose `Output` is the
    /// `std::time::Instant` at which it fired, not `()`. While
    /// `Timer::sleep` was an RPITIT this file wrote `async_io::Timer::
    /// after(d).await;` inside an `async fn` and the instant was dropped
    /// invisibly by the trailing semicolon. A named associated type has to
    /// state the conversion, and [`Discard`] is where it is stated — once,
    /// in `hclient-core`, shared with `hclient-fetch`, whose browser timer
    /// resolves to a `Result<JsValue, JsValue>` for the same reason.
    ///
    /// Nothing about the timing changed; the previous `async fn` form was
    /// chosen only to avoid `clippy::manual_async_fn`, which a plain
    /// constructor does not trip.
    type Sleep = Discard<async_io::Timer>;
    fn sleep(&self, d: Duration) -> Self::Sleep {
        Discard(async_io::Timer::after(d))
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn elapsed_since(&self, earlier: Instant) -> Duration {
        Instant::now().saturating_duration_since(earlier)
    }
}

impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for Smol {
    fn spawn(&self, f: F) {
        // `detach` is deliberate: the task's lifetime is tied to the
        // connection, not to the caller.
        smol_spawn(f);
    }
}

/// The executor every spawned task runs on, and the one thread that drives
/// it.
///
/// **They are two statics on purpose: one static is a race.** Spawning
/// the thread *inside* `OnceLock::get_or_init`'s closure, with its first
/// act `EXEC.get().expect("initialised")`, does not work —
/// `get_or_init` publishes nothing until the closure *returns*, so a
/// scheduler that ran the new thread first found `None` and the process
/// died on that `expect`. Seen twice in 48 runs of the `hclient-h3` suite,
/// and reproducible on demand by widening the window with a `sleep` before
/// the closure's last expression.
///
/// A [`LazyLock`](std::sync::LazyLock) removes the path rather than
/// narrowing it: the executor thread's own deref initialises it, so there
/// is no window in which a reader can observe it unset and no `expect` for
/// a scheduler to win. [`Once`](std::sync::Once) then keeps the thread
/// single without putting it back inside the initialiser.
static EXEC: std::sync::LazyLock<async_executor::Executor<'static>> =
    std::sync::LazyLock::new(async_executor::Executor::new);
static EXEC_THREAD: std::sync::Once = std::sync::Once::new();

fn smol_spawn<F: Future<Output = ()> + Send + 'static>(f: F) {
    EXEC_THREAD.call_once(|| {
        std::thread::Builder::new()
            .name("hclient-smol".into())
            .spawn(|| futures_lite::future::block_on(EXEC.run(std::future::pending::<()>())))
            .expect("spawn executor thread");
    });
    EXEC.spawn(f).detach();
}

impl Blocking for Smol {
    /// `blocking::unblock`'s `Task<T>` is a `Future<Output = T>`: there's
    /// no `Result`, no `JoinError` analogue at all, because `blocking`'s
    /// background thread pool (`blocking::unblock`) is a lazily
    /// initialized process-global `static`, with no shutdown lifecycle
    /// tied to any particular executor or any particular `Smol` value.
    /// This pool has no "went away while the task was still queued" event
    /// — unlike `tokio::task::spawn_blocking`, which can race a whole
    /// runtime's `Runtime::shutdown_timeout`. `Cancelled` is structurally
    /// unreachable for this backend, not merely untested: there's really
    /// nowhere for a failure of that shape to come from. Always returning
    /// `Ok(..)` here is an honest reflection of that fact, not a fudge:
    /// the trait promises "IF a failure of exactly this shape occurs, it
    /// is typed", not "every backend must be capable of producing one".
    ///
    /// A panic in `f` also needs no special handling in this impl:
    /// `blocking::unblock` builds its task through
    /// `async_task::Builder::new().propagate_panic(true)`, and
    /// `async-task`'s own `Task::poll` re-raises the propagated panic via
    /// `std::panic::resume_unwind` with the original payload — the same
    /// mechanism, and the same "original payload, no stringifying"
    /// guarantee, that `hclient-rt-tokio`'s `classify()` assembles by
    /// hand. A plain `.await` on `Task<T>` already does what's needed.
    fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
        &self,
        f: F,
    ) -> BoxFuture<'_, Result<T, Cancelled>> {
        Box::pin(async move { Ok(blocking::unblock(f).await) })
    }
}

/// What a [`Smol`] connection is actually over.
///
/// An enum rather than a type parameter on the stream, because
/// `TcpConnect::Stream` is one associated type and both connects must
/// produce it — the same shape `hclient-rt-tokio`'s `Socket` has, and for
/// the reason `TcpConnect::connect_unix` gives for being one trait rather
/// than two.
#[derive(Debug)]
pub enum SmolSocket {
    Tcp(async_net::TcpStream),
    #[cfg(unix)]
    Unix(async_net::unix::UnixStream),
}

/// Delegates one poll to whichever socket is underneath — a macro for the
/// reason the tokio adapter's is one: the arms differ only in the name.
macro_rules! either {
    ($self:expr, $io:ident => $call:expr) => {
        match $self.get_mut() {
            SmolSocket::Tcp($io) => $call,
            #[cfg(unix)]
            SmolSocket::Unix($io) => $call,
        }
    };
}

impl SmolSocket {
    /// The TCP stream underneath, for reading applied [`TcpOpts`] back in
    /// tests and diagnostics.
    ///
    /// # Panics
    ///
    /// On a Unix-domain stream, where there is no TCP stream and every
    /// option this exists to read has no meaning — `hclient-rt-tokio`'s
    /// `TokioIo::get_ref` has the same shape and the same argument for
    /// why it is not a `Result`.
    pub fn tcp(&self) -> &async_net::TcpStream {
        match self {
            SmolSocket::Tcp(s) => s,
            #[cfg(unix)]
            SmolSocket::Unix(_) => panic!("tcp() on a Unix-domain stream: there is none"),
        }
    }
}

impl futures_lite::io::AsyncRead for SmolSocket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        either!(self, s => Pin::new(s).poll_read(cx, buf))
    }
}

/// `shutdown` on a socket whose peer has already gone is **not an error**,
/// and only the unixes say otherwise.
///
/// `shutdown(2)` returns `ENOTCONN` on macOS and the BSDs for an
/// `AF_UNIX` socket the peer has closed, where Linux returns success.
/// What the caller asked for is "my write half is closed"; a socket that
/// is not connected has certainly reached that state, so reporting a
/// failure turns a **completed** exchange into an error.
///
/// It is not hypothetical and it was not cheap: `Native::unix_socket` was
/// unusable on macOS against any server that closes first — which is every
/// server answering `Connection: close` — and it surfaced as
/// `ErrorKind::Connect`, naming the phase that had already succeeded.
/// Found only when `test (macos-latest)` started finishing runs again.
///
/// Applied to every socket kind rather than to the unix arm alone: the
/// argument is about what `shutdown` means, not about which address
/// family is asking, and a narrower fix would invite the same report for
/// TCP on the next BSD. `hclient-rt-tokio` carries the identical function,
/// and `hclient-rt-pair-check` is what keeps the two agreeing.
fn shutdown_is_done(r: std::io::Result<()>) -> std::io::Result<()> {
    match r {
        Err(e) if e.kind() == std::io::ErrorKind::NotConnected => Ok(()),
        other => other,
    }
}

impl futures_lite::io::AsyncWrite for SmolSocket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        either!(self, s => Pin::new(s).poll_write(cx, buf))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        either!(self, s => Pin::new(s).poll_flush(cx))
    }
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let p = either!(self, s => Pin::new(s).poll_close(cx));
        Poll::Ready(shutdown_is_done(std::task::ready!(p)))
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        either!(self, s => Pin::new(s).poll_write_vectored(cx, bufs))
    }
}

impl TcpConnect for Smol {
    type Stream = FuturesIo<SmolSocket>;

    /// `cfg!(unix)`, which is what `async_net::unix` compiles on.
    const SUPPORTS_UNIX: bool = cfg!(unix);

    /// Every field, and `build_socket` below is where each one is applied.
    /// Stated rather than left to the trait's `NONE` default, which would
    /// understate this runtime — see `TcpConnect::APPLIES`.
    /// **No longer `TcpOptsSupport::ALL`, and that is the point.** Two of
    /// the fields are Linux socket options with no counterpart elsewhere —
    /// `SO_BINDTODEVICE` on Linux/Android/Fuchsia, `TCP_USER_TIMEOUT` on
    /// those plus Cygwin — and a constant claiming them on macOS or
    /// Windows would be a capability that lies, refused at the wrong
    /// moment or not at all. `ALL` still means *every field*; it is simply
    /// no longer a value this runtime can honestly claim on every target
    /// it builds for.
    ///
    /// The direction of the `cfg` matters: an understated `APPLIES` costs
    /// a caller a named `Unsupported` error, an overstated one costs them
    /// an option silently not applied.
    const APPLIES: TcpOptsSupport = TcpOptsSupport {
        bind_device: cfg!(any(
            target_os = "android",
            target_os = "fuchsia",
            target_os = "linux"
        )),
        user_timeout: cfg!(any(
            target_os = "android",
            target_os = "fuchsia",
            target_os = "linux",
            target_os = "cygwin"
        )),
        // `socket2::TcpKeepalive::with_retries` does not exist on these
        // three, so neither does this claim.
        keepalive_retries: !cfg!(any(
            target_os = "openbsd",
            target_os = "redox",
            target_os = "solaris"
        )),
        ..TcpOptsSupport::ALL
    };

    /// A `Send` box, like `hclient_rt_tokio::Tokio`'s and for the same reason:
    /// everything awaited here is `async-io`'s, and a consumer proving a
    /// `Send` future has to be able to name this one.
    type Connecting<'a> =
        std::pin::Pin<Box<dyn Future<Output = std::io::Result<Self::Stream>> + Send + 'a>>;

    fn connect<'a>(&'a self, addr: SocketAddr, opts: &TcpOpts) -> Self::Connecting<'a> {
        let opts = opts.clone();
        Box::pin(async move {
            // Options are applied here, on the `socket2::Socket`, BEFORE the
            // descriptor is ever handed to the runtime — the same seam as
            // `hclient-rt-tokio::build_socket`, and deliberately the same
            // order of operations: the runtime only adopts an already
            // configured socket.
            let sock = build_socket(addr, &opts)?;
            sock.set_nonblocking(true)?;
            begin_connect(&sock, addr)?;

            let std_stream: std::net::TcpStream = sock.into();
            // `async_io::Async::new_nonblocking` registers the descriptor with
            // smol's reactor WITHOUT setting non-blocking mode again —
            // `sock.set_nonblocking(true)` two lines above already did that
            // once. `Async::new` (the plain, non-`_nonblocking` variant) is
            // itself built as `set_nonblocking(io) + Self::new_nonblocking(io)`
            // (`async-io-2.6.0/src/lib.rs:658-663` and `:752-757`, for both the
            // unix and windows `impl` blocks — read, not assumed): calling it
            // here would mean paying a second, redundant syscall on every
            // `connect()`. `new_nonblocking` is an ordinary `pub fn` on both
            // `impl` blocks, exported alongside `new`.
            let async_stream = async_io::Async::new_nonblocking(std_stream)?;
            // The socket becomes writable once the non-blocking `connect()`
            // finishes — whether with success or an error. The same technique
            // `async_io::Async::<TcpStream>::connect` uses (the only reason
            // this CAN be borrowed instead of reinvented: `async-io` itself
            // gives no way to pass in an already configured socket — it has no
            // `connect` overload that takes a `socket2::Socket`).
            async_stream.writable().await?;
            // A non-blocking socket's `connect()` doesn't guarantee that
            // "became writable" means "connected successfully" — an error also
            // makes the socket writable. `take_error` is the only reliable way
            // to tell them apart.
            if let Some(err) = async_stream.get_ref().take_error()? {
                return Err(err);
            }

            Ok(FuturesIo::new(SmolSocket::Tcp(async_net::TcpStream::from(
                async_stream,
            ))))
        })
    }

    #[cfg(unix)]
    async fn connect_unix(&self, path: &std::path::Path) -> std::io::Result<Self::Stream> {
        // No `TcpOpts` and no `socket2` dance, for the reason the trait's
        // own doc gives: `AF_UNIX` has none of those options, so there is
        // nothing to set before the connect.
        Ok(FuturesIo::new(SmolSocket::Unix(
            async_net::unix::UnixStream::connect(path).await?,
        )))
    }
}

impl TcpAdoptStd for Smol {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream> {
        std.set_nonblocking(true)?;
        Ok(FuturesIo::new(SmolSocket::Tcp(
            async_net::TcpStream::try_from(std)?,
        )))
    }
}

/// Initiates a non-blocking `connect(2)` on an already configured socket
/// and classifies the immediate result: `Ok(())` (rare, but happens for
/// localhost) — the connect already finished; `WouldBlock`/`EINPROGRESS` —
/// the connect is in progress, waited on externally via `writable()`;
/// anything else — a real error.
///
/// `EINPROGRESS` can't be observed through `std::io::ErrorKind`
/// (`ErrorKind::InProgress` is still `#[unstable]`), so we compare
/// `raw_os_error()` directly — the same way `socket2`'s own
/// `Socket::connect_timeout` solves this exact problem.
fn begin_connect(sock: &socket2::Socket, addr: SocketAddr) -> std::io::Result<()> {
    match sock.connect(&addr.into()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(libc::EINPROGRESS) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Identical to `hclient_rt_tokio::build_socket` in the list and order of
/// options — deliberately: both functions implement the same `TcpOpts`
/// contract, and a divergence between them would mean one of the two
/// runtimes is lying about some option. The only difference is the type
/// the socket ends up wrapped in.
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
    // **One call for three fields**, because `SO_KEEPALIVE` is switched on
    // by `set_tcp_keepalive` and each part left unset keeps the OS's — see
    // `TcpOpts::keepalive`, where that is stated because the field names do
    // not say it.
    if opts.keepalive.is_some()
        || opts.keepalive_interval.is_some()
        || opts.keepalive_retries.is_some()
    {
        let mut k = socket2::TcpKeepalive::new();
        if let Some(d) = opts.keepalive {
            k = k.with_time(d);
        }
        if let Some(d) = opts.keepalive_interval {
            k = k.with_interval(d);
        }
        #[cfg(not(any(target_os = "openbsd", target_os = "redox", target_os = "solaris")))]
        if let Some(n) = opts.keepalive_retries {
            k = k.with_retries(n);
        }
        sock.set_tcp_keepalive(&k)?;
    }
    // Linux, Android and Fuchsia only, which is why `APPLIES` is a
    // `cfg` and not a constant: on every other target a caller who set
    // this is refused before the connect rather than having it ignored.
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    if let Some(dev) = &opts.bind_device {
        sock.bind_device(Some(dev.as_bytes()))?;
    }
    #[cfg(any(
        target_os = "android",
        target_os = "fuchsia",
        target_os = "linux",
        target_os = "cygwin"
    ))]
    if let Some(d) = opts.user_timeout {
        sock.set_tcp_user_timeout(Some(d))?;
    }
    Ok(sock)
}

#[cfg(test)]
mod tests {
    /// The `ENOTCONN`-on-shutdown decision, checked where it can be:
    /// Linux never produces the error, so the platform cannot be the test.
    /// What is testable is our own rule, in all three directions.
    #[test]
    fn a_shutdown_of_a_socket_that_is_already_gone_is_success() {
        use std::io::ErrorKind;
        assert!(super::shutdown_is_done(Ok(())).is_ok());
        assert!(
            super::shutdown_is_done(Err(std::io::Error::from(ErrorKind::NotConnected))).is_ok(),
            "macOS reports ENOTCONN for a peer that closed first, and the write \
             half it asks about is closed either way"
        );
        // The control, and the half that makes this more than
        // `|_| Ok(())`: every other error still travels.
        assert_eq!(
            super::shutdown_is_done(Err(std::io::Error::from(ErrorKind::BrokenPipe)))
                .unwrap_err()
                .kind(),
            ErrorKind::BrokenPipe
        );
    }

    use super::*;
    use hclient_rt::{Blocking, TcpConnect, TcpOpts, Timer};

    #[test]
    fn timer_sleeps_and_measures() {
        futures_executor::block_on(async {
            let t = Smol;
            let start = t.now();
            t.sleep(Duration::from_millis(20)).await;
            assert!(t.elapsed_since(start) >= Duration::from_millis(20));
        });
    }

    #[test]
    fn blocking_runs_off_the_reactor_and_returns_ok() {
        let out = futures_executor::block_on(Smol.run(|| 6 * 7));
        assert_eq!(out, Ok(42));
    }

    #[test]
    #[should_panic(expected = "boom-from-smol")]
    fn blocking_propagates_the_original_panic_payload() {
        // `blocking::unblock`'s `propagate_panic(true)` gives us this for
        // free, with no `classify()` analogue needed in this
        // implementation.
        let _ = futures_executor::block_on(Smol.run(|| panic!("boom-from-smol")));
    }

    #[test]
    fn connects_to_a_local_listener_with_nodelay() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });
        futures_executor::block_on(async {
            let s = Smol
                .connect(
                    addr,
                    &TcpOpts {
                        nodelay: true,
                        ..Default::default()
                    },
                )
                .await
                .expect("connect");
            // Not just "the connection succeeded" (that would pass even if
            // `build_socket` silently ignored `opts`), but that
            // `nodelay: true` actually reached the socket: read the option
            // back, rather than relying on the call having happened.
            let applied = s.get_ref().tcp().nodelay().expect("nodelay query");
            assert!(
                applied,
                "TcpOpts::nodelay was not applied to the connected socket"
            );
            // And `APPLIES` is not a free-floating claim: it is compared
            // against what the socket just said, so the constant is
            // checked by the test that measures the behaviour rather than
            // by nobody.
            assert_eq!(<Smol as TcpConnect>::APPLIES.nodelay, applied);
        });
    }

    #[test]
    fn connects_with_keepalive_enabled() {
        // The same principle as the nodelay test above, for the second
        // option applied in `build_socket` before `connect()`.
        // `async_net::TcpStream` gives no direct getter for
        // `SO_KEEPALIVE` — use `socket2::SockRef`, the same as
        // `hclient-rt-tokio`'s test of the same name.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });
        futures_executor::block_on(async {
            let s = Smol
                .connect(
                    addr,
                    &TcpOpts {
                        keepalive: Some(Duration::from_secs(30)),
                        ..Default::default()
                    },
                )
                .await
                .expect("connect");
            let enabled = socket2::SockRef::from(s.get_ref().tcp())
                .keepalive()
                .expect("keepalive query");
            assert!(
                enabled,
                "TcpOpts::keepalive was not applied to the connected socket"
            );
            assert_eq!(<Smol as TcpConnect>::APPLIES.keepalive, enabled);
        });
    }
}
