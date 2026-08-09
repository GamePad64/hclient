//! Spike: what shape lets a `!Send` future be spawned on the
//! `http_ng_rt::Spawn` seam without weakening the two impls that exist.
//!
//! Nothing under `crates/` is touched. Everything here is a *candidate*
//! that would eventually live in `http-ng-rt-tokio` / `http-ng-rt-smol`.

pub mod minipool;
pub mod reaper;

use http_ng_rt::{Blocking, Cancelled, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, Timer};
use http_ng_rt_smol::Smol;
use http_ng_rt_tokio::Tokio;
use std::future::Future;
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Candidate A: TokioLocal
// ---------------------------------------------------------------------------

/// A second runtime type that adds a `!Send` spawn to what [`Tokio`] does.
///
/// **Not a ZST, and that is the whole design.** `Tokio` is a ZST that picks
/// its runtime up from ambient thread-local state, which is why
/// `Tokio::spawn` panics outside a runtime. `TokioLocal` instead *owns* the
/// precondition: a `tokio::task::LocalSet`. There is no way to obtain a
/// `TokioLocal` without one, so `Spawn::spawn` here cannot fail for want of
/// a local executor — the failure moved from a panic deep inside a request
/// to a value the caller had to construct.
///
/// `LocalSet` (rather than `tokio::runtime::LocalRuntime`) on purpose: a
/// `LocalRuntime` held in an `Rc` can have its last handle dropped from
/// inside its own `block_on`, which is the "Cannot drop a runtime in a
/// context where blocking is not allowed" panic. A `LocalSet` is not a
/// runtime — it is a task set driven by an ordinary one — so the ambient
/// `Timer`/`TcpConnect`/`Blocking` of `Tokio` keep working underneath it
/// unchanged.
#[derive(Clone, Debug)]
pub struct TokioLocal {
    local: Rc<tokio::task::LocalSet>,
}

impl TokioLocal {
    pub fn new() -> Self {
        Self {
            local: Rc::new(tokio::task::LocalSet::new()),
        }
    }

    /// The other half of the bargain: somebody has to drive the local set.
    /// Tasks spawned on it make progress only while this is being polled.
    pub async fn run_until<F: Future>(&self, f: F) -> F::Output {
        self.local.run_until(f).await
    }
}

impl Default for TokioLocal {
    fn default() -> Self {
        Self::new()
    }
}

/// The impl the ZST cannot have: **no `Send` bound**.
impl<F: Future<Output = ()> + 'static> Spawn<F> for TokioLocal {
    fn spawn(&self, f: F) {
        self.local.spawn_local(f);
    }
}

// The price of a separate runtime type: it has to re-answer every other
// capability. Delegation is the cheapest form, and it is exact — the
// associated types are `Tokio`'s own, so `Native<TokioLocal, _, _>` and
// `Native<Tokio, _, _>` have the same IO type.
impl Timer for TokioLocal {
    type Instant = <Tokio as Timer>::Instant;
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> {
        Tokio.sleep(d)
    }
    fn now(&self) -> Self::Instant {
        Tokio.now()
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        Tokio.elapsed_since(earlier)
    }
}

impl TcpConnect for TokioLocal {
    type Stream = <Tokio as TcpConnect>::Stream;
    fn connect(
        &self,
        addr: SocketAddr,
        opts: &TcpOpts,
    ) -> impl Future<Output = std::io::Result<Self::Stream>> {
        Tokio.connect(addr, opts)
    }
}

impl TcpAdoptStd for TokioLocal {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream> {
        Tokio.adopt(std)
    }
}

impl Blocking for TokioLocal {
    fn run<T, F>(&self, f: F) -> impl Future<Output = Result<T, Cancelled>>
    where
        T: Send + 'static, // send-bound-exception: amendment-C5
        F: FnOnce() -> T + Send + 'static, // send-bound-exception: amendment-C5
    {
        Tokio.run(f)
    }
}

// ---------------------------------------------------------------------------
// Candidate A': TokioHandle — the same idea applied to the Send spawner
// ---------------------------------------------------------------------------

/// `Tokio` is a ZST, so `Tokio::spawn` reads ambient thread-local state and
/// panics when there is none. That is a *second* place where a default is
/// stronger than the truth, and it is fixed by the same move that fixes the
/// local case: carry the proof.
///
/// `TokioHandle` holds a `tokio::runtime::Handle`, which cannot be obtained
/// without a runtime. It is `Send + Sync`, so — unlike `TokioLocal` — it
/// costs `Native` none of its auto traits.
#[derive(Clone, Debug)]
pub struct TokioHandle(tokio::runtime::Handle);

impl TokioHandle {
    /// The precondition, as a value rather than a panic.
    pub fn current() -> Result<Self, tokio::runtime::TryCurrentError> {
        tokio::runtime::Handle::try_current().map(Self)
    }
    pub fn from_handle(h: tokio::runtime::Handle) -> Self {
        Self(h)
    }
}

impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for TokioHandle {
    fn spawn(&self, f: F) {
        self.0.spawn(f);
    }
}

impl Timer for TokioHandle {
    type Instant = <Tokio as Timer>::Instant;
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> {
        Tokio.sleep(d)
    }
    fn now(&self) -> Self::Instant {
        Tokio.now()
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        Tokio.elapsed_since(earlier)
    }
}

// ---------------------------------------------------------------------------
// Candidate B: SmolLocal
// ---------------------------------------------------------------------------

/// The smol counterpart, on `async_executor::LocalExecutor`.
///
/// Same shape, same reason: the executor is owned, not ambient. `Smol`'s
/// `Spawn` reaches a *global* executor on a background thread (hence
/// `Send + 'static`); this one reaches an executor the caller is running
/// on this very thread.
#[derive(Clone)]
pub struct SmolLocal {
    ex: Rc<async_executor::LocalExecutor<'static>>,
}

impl std::fmt::Debug for SmolLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmolLocal").finish_non_exhaustive()
    }
}

impl SmolLocal {
    pub fn new() -> Self {
        Self {
            ex: Rc::new(async_executor::LocalExecutor::new()),
        }
    }

    /// Drive the executor until `f` completes, on this thread.
    pub fn block_on<F: Future>(&self, f: F) -> F::Output {
        futures_lite::future::block_on(self.ex.run(f))
    }
}

impl Default for SmolLocal {
    fn default() -> Self {
        Self::new()
    }
}

impl<F: Future<Output = ()> + 'static> Spawn<F> for SmolLocal {
    fn spawn(&self, f: F) {
        self.ex.spawn(f).detach();
    }
}

impl Timer for SmolLocal {
    type Instant = <Smol as Timer>::Instant;
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> {
        Smol.sleep(d)
    }
    fn now(&self) -> Self::Instant {
        Smol.now()
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        Smol.elapsed_since(earlier)
    }
}

impl TcpConnect for SmolLocal {
    type Stream = <Smol as TcpConnect>::Stream;
    fn connect(
        &self,
        addr: SocketAddr,
        opts: &TcpOpts,
    ) -> impl Future<Output = std::io::Result<Self::Stream>> {
        Smol.connect(addr, opts)
    }
}

impl TcpAdoptStd for SmolLocal {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream> {
        Smol.adopt(std)
    }
}

impl Blocking for SmolLocal {
    fn run<T, F>(&self, f: F) -> impl Future<Output = Result<T, Cancelled>>
    where
        T: Send + 'static, // send-bound-exception: amendment-C5
        F: FnOnce() -> T + Send + 'static, // send-bound-exception: amendment-C5
    {
        Smol.run(f)
    }
}

// ---------------------------------------------------------------------------
// The coherence question, behind `--features overlap`
// ---------------------------------------------------------------------------

/// Can ONE type carry both impls? Turn on `--features overlap` and read
/// the error. Kept in the tree so the claim is reproducible, not asserted.
#[cfg(feature = "overlap")]
mod overlap {
    use super::*;

    pub struct OneType;

    impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for OneType {
        fn spawn(&self, f: F) {
            let _ = f;
        }
    }

    impl<F: Future<Output = ()> + 'static> Spawn<F> for OneType {
        fn spawn(&self, f: F) {
            let _ = f;
        }
    }
}

// ---------------------------------------------------------------------------
// What library code would actually write
// ---------------------------------------------------------------------------

/// The shape a library function needs: the future type is a *parameter*,
/// so a caller with a `!Send` future picks a runtime whose impl allows it,
/// and a caller with a `Send` one keeps `Tokio`. No `Send` anywhere in
/// this signature — the trait has none, so neither does this.
pub fn background<R, F>(rt: &R, f: F)
where
    R: Spawn<F>,
    F: Future<Output = ()>,
{
    rt.spawn(f);
}
