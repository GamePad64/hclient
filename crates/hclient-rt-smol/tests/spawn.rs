//! `Spawn` on smol, asserted **in this crate**.
//!
//! The property was already pinned — `hclient-rt-pair-check`'s
//! `pair_property_holds_for_smol` spawns a task and asserts it ran — and
//! that is where it belongs, because the point of that crate is that one
//! body runs on both runtimes. What was missing is a local statement:
//! `cargo mutants -p hclient-rt-smol` runs only this crate's tests, so
//! both `Spawn::spawn` and `smol_spawn` replaced by `()` were survivors of
//! a scoped sweep while being caught by a full-workspace run. Measured
//! both ways — with either mutation applied, this crate's own suite was
//! 21 of 21 green and `hclient-rt-pair-check` went 4 passed, 1 failed.
//!
//! That is not a false survivor to be waved away: a property asserted only
//! from another crate is one this crate's own suite cannot lose, and the
//! two spawn functions are where this crate's executor bootstrap lives.
use hclient_rt::Spawn;
use hclient_rt_smol::Smol;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Wait for a counter to reach `want`, or fail on a bound.
///
/// Polled rather than slept-then-asserted, so the test is not paying a
/// fixed cost on a fast machine and not flaky on a loaded one — the shape
/// `adversarial_smol_connect.rs` uses for every wait, and for its reason:
/// a regression must report a failure rather than wedge the binary.
fn wait_for(counter: &AtomicUsize, want: usize, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while counter.load(Ordering::SeqCst) < want {
        assert!(
            Instant::now() < deadline,
            "{what}: reached {} of {want} within the bound — the spawned task did not run",
            counter.load(Ordering::SeqCst)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A spawned future actually runs, on the executor thread this crate
/// starts for it.
///
/// The floor under both `Spawn::spawn` and `smol_spawn`: with either
/// replaced by `()` the counter never moves and this fails on its bound.
/// `detach` is what makes it observable at all — the task's lifetime is
/// tied to the connection rather than to any handle, so the only evidence
/// it ran is its effect.
#[test]
fn a_spawned_future_runs() {
    let ran = Arc::new(AtomicUsize::new(0));
    let r = Arc::clone(&ran);
    Smol.spawn(async move {
        r.fetch_add(1, Ordering::SeqCst);
    });
    wait_for(&ran, 1, "a single spawned future");
}

/// Several spawns share one executor and one thread, and every one of them
/// runs.
///
/// `EXEC` and `EXEC_THREAD` are two statics precisely because making them
/// one is a race — this crate's own doc records the `expect("initialised")`
/// that died when a scheduler ran the new thread before `get_or_init`
/// published. `Once` keeps the thread single; what says the executor still
/// serves every task is that spawns made from **different** threads, racing
/// that one-time initialisation, all complete.
///
/// It is a weak probe of a race by construction — a passing run does not
/// prove the window is closed — so it is written as a liveness assertion
/// rather than a claim about ordering: all sixteen must run, whichever
/// thread won.
#[test]
fn every_spawn_runs_when_several_threads_race_the_executor_bootstrap() {
    const THREADS: usize = 4;
    const PER_THREAD: usize = 4;
    let ran = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(std::sync::Barrier::new(THREADS));
    let mut joiners = Vec::new();
    for _ in 0..THREADS {
        let ran = Arc::clone(&ran);
        let start = Arc::clone(&start);
        joiners.push(std::thread::spawn(move || {
            // All four threads arrive together, so the first `spawn` calls
            // are as close to simultaneous as this can arrange.
            start.wait();
            for _ in 0..PER_THREAD {
                let r = Arc::clone(&ran);
                Smol.spawn(async move {
                    r.fetch_add(1, Ordering::SeqCst);
                });
            }
        }));
    }
    for j in joiners {
        j.join().expect("spawning thread");
    }
    wait_for(
        &ran,
        THREADS * PER_THREAD,
        "sixteen spawns from four threads",
    );
}

/// **A task that panics does not stop the ones spawned after it.**
///
/// There is one executor thread for the whole process, started once. If a
/// panicking task unwound through it, the thread would die, `Once` would
/// never start another, and every later `spawn` would be accepted and never
/// run — the one shape `hclient_rt::Spawn` forbids, and a hang rather than
/// an error for whoever spawned a connection driver.
///
/// It holds because `async-executor` 1.x builds each task with
/// `propagate_panic(true)`, catching the unwind into the task handle that
/// `detach` then discards. That is a claim about a third party, so it is
/// pinned rather than described: an `async-executor` that stopped catching
/// fails this line instead of hanging a client.
#[test]
fn a_panicking_task_does_not_stop_later_ones() {
    let before = Arc::new(AtomicUsize::new(0));
    let b = Arc::clone(&before);
    Smol.spawn(async move {
        b.fetch_add(1, Ordering::SeqCst);
        panic!("a spawned task that panics, on purpose");
    });
    wait_for(&before, 1, "the panicking task");
    // Give the unwind time to reach the executor thread, if it is going to.
    std::thread::sleep(Duration::from_millis(50));

    let after = Arc::new(AtomicUsize::new(0));
    let a = Arc::clone(&after);
    Smol.spawn(async move {
        a.fetch_add(1, Ordering::SeqCst);
    });
    wait_for(&after, 1, "a task spawned after one that panicked");
}
