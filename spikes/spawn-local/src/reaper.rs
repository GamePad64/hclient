//! Two shapes that get past the "the future has no name" wall the
//! `unnameable` bin quotes, plus the reaper itself.

use crate::minipool::{Inner, sweep};
use http_ng_rt::{Spawn, Timer};
use std::future::Future;
use std::pin::Pin;
use std::sync::Weak;
use std::task::{Context, Poll};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Shape B: name the sleep, and the reaper becomes nameable too
// ---------------------------------------------------------------------------

// This section used to declare a `NamedTimer` extension trait, because at
// the time `Timer::sleep` was an RPITIT: the future existed but had no
// name, so no struct could declare a field of that type and no `where`
// clause could mention it. The extension trait was the way to try the shape
// without touching anything under `crates/`.
//
// **That shape shipped.** `Timer` itself now carries `type Sleep`, so the
// extension trait is gone from this file and the code below names
// `R::Sleep` on the real trait. `TcpConnect::Stream` was the precedent, and
// this is the same idea applied to the timer.
//
// `Discard` shipped with it, into `http-ng-core` and re-exported from
// `http-ng-rt`, for the cost the associated type made visible:
// `async_io::Timer` resolves to an `Instant`, not to `()`, so the smol side
// needs a two-line adapter that `async { t.await; }` used to perform
// silently. This file no longer defines its own copy; it uses the shipped
// one, which is what keeps this spike a check on the real seam rather than
// a rehearsal of it.

/// The reaper, as a **named struct** rather than an `async` block — which
/// is what makes `R: Spawn<Reaper<R, I>>` writable at all.
///
/// `Send`ness is inferred, never declared: `Reaper<R, I>` is `Send` when
/// `R`, `R::Sleep` and `I` are, which is exactly the rule `h1.rs`'s
/// module doc already applies to `H1Body<I>`.
pub struct Reaper<R: Timer, I> {
    rt: R,
    pool: Weak<Inner<I>>,
    period: Duration,
    /// Boxed so `Reaper` is `Unpin` and needs no unsafe projection. A box
    /// around a *concrete* type is transparent to auto traits — the same
    /// argument `h1.rs` makes about `Failed::NotSent`.
    sleep: Pin<Box<R::Sleep>>,
    /// Spike-only: how many sweeps ran, so the bins can report a number
    /// instead of an opinion.
    pub swept: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl<R: Timer, I> Reaper<R, I> {
    pub fn new(
        rt: R,
        pool: Weak<Inner<I>>,
        period: Duration,
        swept: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        let sleep = Box::pin(rt.sleep(period));
        Self {
            rt,
            pool,
            period,
            sleep,
            swept,
        }
    }
}

impl<R: Timer + Unpin, I: Unpin> Future for Reaper<R, I> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let me = self.get_mut();
        loop {
            match me.sleep.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(()) => {}
            }
            me.sleep = Box::pin(me.rt.sleep(me.period));
            match sweep(&me.pool) {
                // The pool is gone: so is the reason to exist. This is the
                // half a `Weak` buys and an `Arc` would lose.
                None => return Poll::Ready(()),
                Some(n) => {
                    me.swept.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }
    }
}

/// The call `http-ng-native` would make. Note what is *not* here: no
/// `Send`, no `'static` written by hand, no `Box<dyn>`. The bound names a
/// concrete type, and each runtime's own `Spawn` impl decides whether it
/// qualifies.
pub fn start_reaper<R, I>(
    rt: R,
    pool: Weak<Inner<I>>,
    period: Duration,
    swept: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) where
    R: Timer + Unpin + Clone + Spawn<Reaper<R, I>>,
    I: Unpin,
{
    let r = Reaper::new(rt.clone(), pool, period, swept);
    rt.spawn(r);
}

// ---------------------------------------------------------------------------
// Shape C: a trait with a generic method
// ---------------------------------------------------------------------------

/// The other way past the naming wall: quantify the future in the
/// *method* instead of the trait.
///
/// This makes any `'static` future spawnable from generic code with no
/// naming and no seam change to `Timer`. The price is that the `Send`
/// choice moves back into the trait declaration, which is exactly what
/// `Spawn<F>`'s doc comment says it was avoiding: a runtime whose spawner
/// needs `Send` — i.e. `Tokio` and `Smol` — **cannot implement this at
/// all**. See the `local_only` must-fail bin.
pub trait LocalSpawn {
    fn spawn_local<F: Future<Output = ()> + 'static>(&self, f: F);
}

impl LocalSpawn for crate::TokioLocal {
    fn spawn_local<F: Future<Output = ()> + 'static>(&self, f: F) {
        Spawn::spawn(self, f);
    }
}

impl LocalSpawn for crate::SmolLocal {
    fn spawn_local<F: Future<Output = ()> + 'static>(&self, f: F) {
        Spawn::spawn(self, f);
    }
}

/// The same reaper written the easy way, for the runtimes that can take
/// it. No named sleep, no struct, no `Unpin` — an ordinary `async` block.
pub fn start_reaper_local<R, I>(
    rt: R,
    pool: Weak<Inner<I>>,
    period: Duration,
    swept: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) where
    R: Timer + LocalSpawn + Clone + 'static,
    I: 'static,
{
    let r = rt.clone();
    rt.spawn_local(async move {
        loop {
            r.sleep(period).await;
            match sweep(&pool) {
                None => return,
                Some(n) => {
                    swept.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }
    });
}
