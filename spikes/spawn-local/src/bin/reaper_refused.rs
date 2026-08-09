//! Negative control 4: **the refusal is a compile error, not a silent
//! no-op.** A runtime whose spawner needs `Send` cannot start a reaper
//! over a `!Send` connection. Must NOT compile.
//!
//! `cargo build --bin reaper_refused --features must-fail`
//!
//! This is the shape the "a default must never be stronger than the truth"
//! rule asks for: `start_reaper` is offered only where it can actually
//! run, and the mismatch is caught where the caller wrote it rather than
//! discovered as a reaper that never fires.

use spawn_local_spike::minipool::Pool;
use spawn_local_spike::reaper::start_reaper;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

struct NotSendConn(#[allow(dead_code)] std::rc::Rc<()>);

fn main() {
    let pool: Pool<NotSendConn> = Pool::new();
    // The control, one line up from the failure: with a `Send` connection
    // the SHIPPED `Tokio` takes the same reaper. This line compiles.
    let ok: Pool<u32> = Pool::new();
    start_reaper(
        http_ng_rt_tokio::Tokio,
        ok.weak(),
        Duration::from_millis(1),
        Arc::new(AtomicUsize::new(0)),
    );
    // And with a `!Send` one it does not.
    start_reaper(
        http_ng_rt_tokio::Tokio,
        pool.weak(),
        Duration::from_millis(1),
        Arc::new(AtomicUsize::new(0)),
    );
}
