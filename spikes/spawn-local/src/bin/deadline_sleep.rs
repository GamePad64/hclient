//! Spike 4: **the second consumer of `type Sleep`, and it is not the
//! reaper.**
//!
//! `cargo run --bin deadline_sleep`
//!
//! `docs/v02-acceptance.md` records a shipped limitation of W4:
//!
//! > **`total` does not cut a body that goes completely silent after the
//! > head.** `Timer::sleep` is an RPITIT, so its future cannot be stored in
//! > a struct field, and boxing it would make *every* response body
//! > `!Send`. The body wrapper therefore checks elapsed time on each
//! > `poll_frame`, which catches a dribbling body on its next byte and
//! > never wakes for one that stops entirely.
//!
//! Two claims there, and this spike checks both rather than taking them:
//!
//! 1. that the elapsed-time wrapper really does hang on a silent body;
//! 2. that storing the sleep really would cost `Send` — it would, for
//!    `Pin<Box<dyn Future>>`, and it would **not** for a named
//!    `Tm::Sleep`, because a box around a *concrete* type is transparent
//!    to auto traits. That is the whole difference an associated type
//!    makes.
//!
//! `A` and `B` below are miniatures of `http_ng::deadline::Deadline`, with
//! its two fields that matter and its "firing drops the inner body" rule.

use bytes::Bytes;
use http_body::{Body, Frame};
use http_body_util::BodyExt;
use http_ng_rt::Timer;
use spawn_local_spike::reaper::NamedTimer;
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

const TOTAL: Duration = Duration::from_millis(300);
const PATIENCE: Duration = Duration::from_millis(1200);

/// A body that produced its head and then went completely silent: it
/// returns `Pending` and never registers a waker, so nothing will ever
/// poll it again. This is the case the acceptance doc says `total` misses.
struct SilentBody;

impl Body for SilentBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Pending
    }
}

#[derive(Debug)]
struct Expired;
impl std::fmt::Display for Expired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("total timeout elapsed")
    }
}
impl std::error::Error for Expired {}

// ---------------------------------------------------------------------------
// A — today's shape: no sleep, elapsed time checked per frame
// ---------------------------------------------------------------------------

struct DeadlineNow<B, Tm: Timer> {
    inner: Option<B>,
    timer: Tm,
    started: Tm::Instant,
    total: Duration,
}

/// Hand-written for the same reason `http_ng::deadline::Deadline` writes
/// its own: the auto-derivation would also demand `Unpin` of `Tm::Instant`,
/// which `Timer` does not require.
impl<B: Unpin, Tm: Timer> Unpin for DeadlineNow<B, Tm> {}

impl<B: Body<Data = Bytes> + Unpin, Tm: Timer + Unpin> Body for DeadlineNow<B, Tm>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let me = self.get_mut();
        if me.timer.elapsed_since(me.started) >= me.total {
            me.inner = None; // firing drops the inner body
            return Poll::Ready(Some(Err(Box::new(Expired))));
        }
        match &mut me.inner {
            None => Poll::Ready(None),
            Some(b) => Pin::new(b)
                .poll_frame(cx)
                .map(|o| o.map(|r| r.map_err(|e| Box::new(e) as Self::Error))),
        }
    }
}

// ---------------------------------------------------------------------------
// B — with `type Sleep`: the wrapper owns a real sleep
// ---------------------------------------------------------------------------

struct DeadlineSleep<B, Tm: NamedTimer> {
    inner: Option<B>,
    /// `Pin<Box<Tm::Sleep>>`, not `Pin<Box<dyn Future>>`. The box is only
    /// there because `tokio::time::Sleep` is `!Unpin`; it wraps a
    /// **concrete** type, so `Send` still passes through it. That is the
    /// entire difference between this and what the acceptance doc says
    /// storing the sleep would cost.
    sleep: Pin<Box<Tm::Sleep>>,
}

impl<B: Unpin, Tm: NamedTimer> Unpin for DeadlineSleep<B, Tm> {}

impl<B: Body<Data = Bytes> + Unpin, Tm: NamedTimer> DeadlineSleep<B, Tm> {
    fn new(inner: B, timer: &Tm, total: Duration) -> Self {
        Self {
            inner: Some(inner),
            sleep: Box::pin(timer.sleep_named(total)),
        }
    }
}

impl<B: Body<Data = Bytes> + Unpin, Tm: NamedTimer> Body for DeadlineSleep<B, Tm>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let me = self.get_mut();
        if me.inner.is_some() && me.sleep.as_mut().poll(cx).is_ready() {
            me.inner = None;
            return Poll::Ready(Some(Err(Box::new(Expired))));
        }
        match &mut me.inner {
            None => Poll::Ready(None),
            Some(b) => Pin::new(b)
                .poll_frame(cx)
                .map(|o| o.map(|r| r.map_err(|e| Box::new(e) as Self::Error))),
        }
    }
}

fn assert_send<T: Send>() {}

/// A waker that counts. Nothing else in this program can wake the body, so
/// the count answers the question directly: **will anything ever poll this
/// wrapper again?**
struct Counter(std::sync::atomic::AtomicUsize);

impl std::task::Wake for Counter {
    fn wake(self: std::sync::Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Poll `body` exactly once with a counting waker, wait out the deadline
/// **without any executor at all**, and report whether the body ever asked
/// to be polled again.
///
/// `Smol` rather than `Tokio` here on purpose: `async_io::Timer` is driven
/// by async-io's own reactor thread, so it can fire while nothing is
/// running this task. A `tokio::time::Sleep` would need the tokio runtime
/// to be driven, and then the runtime — not the body — would be the thing
/// doing the waking, which is exactly the confound this probe removes.
fn will_anything_poll_it_again<B>(label: &str, mut body: B)
where
    B: Body<Data = Bytes> + Unpin,
{
    let c = std::sync::Arc::new(Counter(std::sync::atomic::AtomicUsize::new(0)));
    let w = std::task::Waker::from(c.clone());
    let mut cx = Context::from_waker(&w);
    match Pin::new(&mut body).poll_frame(&mut cx) {
        Poll::Pending => {}
        Poll::Ready(_) => {
            println!("  {label}: ready on the first poll — not the case under test");
            return;
        }
    }
    std::thread::sleep(TOTAL * 2);
    let n = c.0.load(std::sync::atomic::Ordering::SeqCst);
    println!(
        "  {label}: after one Pending poll and {}ms of nothing running, wakes = {n} -> {}",
        (TOTAL * 2).as_millis(),
        if n == 0 {
            "NOBODY will ever poll it again; the deadline can never fire"
        } else {
            "it woke itself; the next poll fires the deadline"
        }
    );
}

fn main() {
    // The `Send` half of the claim, at compile time. `Deadline`'s doc says
    // storing the sleep would make every response body `!Send`; with a
    // named associated type it does not.
    assert_send::<DeadlineSleep<SilentBody, http_ng_rt_tokio::Tokio>>();
    assert_send::<DeadlineNow<SilentBody, http_ng_rt_tokio::Tokio>>();
    println!(
        "compile-time: DeadlineSleep<SilentBody, Tokio> is Send, with tokio::time::Sleep stored in it"
    );
    println!(
        "              the `Pin<Box<dyn Future>>` variant is not — `--bin deadline_dyn --features must-fail`\n"
    );

    println!(
        "1. the decisive question, with no executor running at all (total = {}ms)",
        TOTAL.as_millis()
    );
    will_anything_poll_it_again(
        "A  elapsed-per-frame (today's shape)",
        DeadlineNow {
            inner: Some(SilentBody),
            timer: http_ng_rt_smol::Smol,
            started: http_ng_rt_smol::Smol.now(),
            total: TOTAL,
        },
    );
    will_anything_poll_it_again(
        "B  stored Tm::Sleep                 ",
        DeadlineSleep::new(SilentBody, &http_ng_rt_smol::Smol, TOTAL),
    );

    println!(
        "\n2. the same two under a real runtime, with an outer patience of {}ms",
        PATIENCE.as_millis()
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let t0 = Instant::now();
        let body = DeadlineNow {
            inner: Some(SilentBody),
            timer: http_ng_rt_tokio::Tokio,
            started: http_ng_rt_tokio::Tokio.now(),
            total: TOTAL,
        };
        let r = tokio::time::timeout(PATIENCE, body.collect()).await;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  A: {} at {ms:.0}ms — {}",
            match &r {
                Ok(Err(_)) => "cut",
                Ok(Ok(_)) => "ended",
                Err(_) => "outer patience gave up",
            },
            "and the {PATIENCE}ms is the tell: the wake came from the harness, not from the body.              Part 1 is the honest measurement; on its own this line looks like a success"
                .replace("{PATIENCE}", &PATIENCE.as_millis().to_string())
        );
    });

    rt.block_on(async {
        let t0 = Instant::now();
        let body = DeadlineSleep::new(SilentBody, &http_ng_rt_tokio::Tokio, TOTAL);
        let r = tokio::time::timeout(PATIENCE, body.collect()).await;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  B: {} at {ms:.0}ms — on its own deadline, {}ms before the harness would have woken it",
            match &r {
                Ok(Err(_)) => "cut",
                Ok(Ok(_)) => "ended",
                Err(_) => "outer patience gave up",
            },
            (PATIENCE - TOTAL).as_millis()
        );
    });
}
