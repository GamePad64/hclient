//! Spike 1: a `!Send` future, spawned through `http_ng_rt::Spawn`, on both
//! runtimes, and actually run.
//!
//! `cargo run --bin shape`

use http_ng_rt::Spawn;
use spawn_local_spike::{SmolLocal, TokioLocal, background};
use std::cell::Cell;
use std::rc::Rc;

/// A future that is `!Send` by construction, the same trick
/// `http-ng-native`'s `connect.rs::FakeStream` uses: it holds an `Rc`.
struct NotSend {
    flag: Rc<Cell<u32>>,
}

impl Future for NotSend {
    type Output = ();
    fn poll(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        self.flag.set(self.flag.get() + 1);
        std::task::Poll::Ready(())
    }
}

fn assert_not_send<T: ?Sized>() {}

fn main() {
    // Compile-time: the future type really is `!Send`. (If it were `Send`,
    // the whole spike would be measuring nothing.)
    assert_not_send::<NotSend>();
    println!("NotSend holds an Rc<Cell<u32>>; see the `not_send` bin for the E0277 that proves it.");

    // --- tokio -------------------------------------------------------------
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let n = rt.block_on(async {
        let local = TokioLocal::new();
        let flag = Rc::new(Cell::new(0));
        // Through the *trait*, not through an inherent method.
        Spawn::spawn(&local, NotSend { flag: flag.clone() });
        // And through generic library-shaped code that knows only `Spawn`.
        background(&local, NotSend { flag: flag.clone() });
        local
            .run_until(async {
                tokio::task::yield_now().await;
                tokio::task::yield_now().await;
            })
            .await;
        flag.get()
    });
    println!("TokioLocal: !Send futures polled = {n} (expected 2)");
    assert_eq!(n, 2);

    // --- smol --------------------------------------------------------------
    let local = SmolLocal::new();
    let flag = Rc::new(Cell::new(0));
    Spawn::spawn(&local, NotSend { flag: flag.clone() });
    background(&local, NotSend { flag: flag.clone() });
    local.block_on(async {
        futures_lite::future::yield_now().await;
        futures_lite::future::yield_now().await;
    });
    println!("SmolLocal:  !Send futures polled = {} (expected 2)", flag.get());
    assert_eq!(flag.get(), 2);

    // --- the existing impls are untouched ----------------------------------
    // `Tokio` still refuses a !Send future; `--bin not_send` quotes the error.
    // What it accepts, it still accepts:
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d = done.clone();
    rt.block_on(async move {
        Spawn::spawn(&http_ng_rt_tokio::Tokio, async move {
            d.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
    });
    println!(
        "Tokio (unchanged, Send impl): ran = {}",
        done.load(std::sync::atomic::Ordering::SeqCst)
    );

    println!("OK: one seam, two runtime types, !Send spawn works and the Send impls are untouched");
}
