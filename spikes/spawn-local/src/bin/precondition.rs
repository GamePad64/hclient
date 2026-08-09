//! Spike 2: what the precondition is, and whether it fails loudly.
//!
//! `cargo run --bin precondition`
//!
//! Three spawners, three preconditions:
//!
//! - `tokio::task::spawn_local` — ambient `LocalSet`. Panics without one.
//! - `http_ng_rt_tokio::Tokio` (the shipped `Spawn`) — ambient runtime.
//!   Panics without one. This is already the case today, documented on the
//!   type, and never exercised because nothing calls `spawn`.
//! - `TokioLocal`/`SmolLocal` (this spike) — the executor is a field, so
//!   the precondition is discharged at construction. `spawn` is total.

use http_ng_rt::Spawn;
use spawn_local_spike::{SmolLocal, TokioLocal};
use std::cell::Cell;
use std::rc::Rc;

fn panic_message<F: FnOnce()>(what: &str, f: F) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    match r {
        Ok(()) => println!("  {what}: NO PANIC"),
        Err(e) => {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<non-string panic payload>".into());
            println!("  {what}: PANIC: {msg}");
        }
    }
}

fn main() {
    println!("A. the ambient spawners, with their precondition absent");

    // `tokio::task::spawn_local` needs a LocalSet. There is a runtime here
    // (so this isolates the LocalSet, not the runtime).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        panic_message("tokio::task::spawn_local, runtime but no LocalSet", || {
            let flag = Rc::new(Cell::new(0u32));
            tokio::task::spawn_local(async move {
                flag.set(1);
            });
        });
    });

    // The shipped `Spawn` impl, outside any runtime at all.
    panic_message("http_ng_rt_tokio::Tokio::spawn, no runtime", || {
        Spawn::spawn(&http_ng_rt_tokio::Tokio, async {});
    });

    println!("\nB. the shape this spike proposes: the executor is a field");

    // Nothing ambient exists yet — no runtime has been entered on this
    // thread for this value, and the tasks are queued before any driver.
    let local = TokioLocal::new();
    let flag = Rc::new(Cell::new(0u32));
    let f = flag.clone();
    panic_message("TokioLocal::spawn, before any runtime is entered", || {
        Spawn::spawn(&local, async move {
            f.set(f.get() + 1);
        });
    });
    // ... and the queued task runs once somebody drives the set.
    rt.block_on(local.run_until(async {
        tokio::task::yield_now().await;
    }));
    println!("  TokioLocal: the queued !Send task ran = {}", flag.get() == 1);

    let sl = SmolLocal::new();
    let flag = Rc::new(Cell::new(0u32));
    let f = flag.clone();
    panic_message("SmolLocal::spawn, before block_on", || {
        Spawn::spawn(&sl, async move {
            f.set(f.get() + 1);
        });
    });
    sl.block_on(async { futures_lite::future::yield_now().await });
    println!("  SmolLocal:  the queued !Send task ran = {}", flag.get() == 1);

    println!("\nC. the residual truth gap: a spawner nobody drives");
    // This is what a `Capabilities`-style claim would have to be honest
    // about. The task is accepted, and never runs, because nothing polls
    // the set. No panic, no error — a silent no-op.
    let orphan = TokioLocal::new();
    let flag = Rc::new(Cell::new(0u32));
    let f = flag.clone();
    Spawn::spawn(&orphan, async move { f.set(1) });
    rt.block_on(async {
        tokio::task::yield_now().await;
    });
    println!(
        "  spawned on a TokioLocal that is never run_until'd: ran = {} (silently dropped when the set is)",
        flag.get() == 1
    );
    drop(orphan);
    println!(
        "  after the TokioLocal is dropped:                   ran = {}",
        flag.get() == 1
    );
}
