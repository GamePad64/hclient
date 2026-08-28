//! Dropping the future `execute_send` handed back must stop the work.
//!
//! `SendTransport::execute_send` runs the exchange in a `spawn_local` task
//! so that no JS handle crosses a thread. That buys `Send` and puts a
//! contract at risk in the same move: `Transport::execute` promises that
//! **dropping the future cancels the exchange**, and a spawned task does
//! not stop because its spawner went away — nothing about a spawn says the
//! work should end.
//!
//! `crate::deliver` is the race that keeps it, and these two tests are the
//! assertion. The pair is the point: a `deliver` that ignored cancellation
//! passes the first and fails the second.

#![cfg(target_arch = "wasm32")]

use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

/// Stands in for the `AbortOnDrop` the real work owns: it records that it
/// was dropped, which is exactly what the guard does when it aborts.
struct Guard(Rc<Cell<bool>>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

#[wasm_bindgen_test]
async fn a_completed_exchange_is_delivered() {
    let dropped = Rc::new(Cell::new(false));
    let guard = Guard(Rc::clone(&dropped));
    let (tx, rx) = futures_channel::oneshot::channel();

    // The work finishes immediately, so the value must arrive.
    wasm_bindgen_futures::spawn_local(hclient_fetch::testing::deliver(
        async move {
            let _held = guard;
            42u32
        },
        tx,
    ));
    assert_eq!(rx.await.unwrap(), 42);
    assert!(
        dropped.get(),
        "the work ran to completion, so its guard is gone"
    );
}

#[wasm_bindgen_test]
async fn dropping_the_receiver_drops_work_that_would_never_finish() {
    let dropped = Rc::new(Cell::new(false));
    let guard = Guard(Rc::clone(&dropped));
    let (tx, rx) = futures_channel::oneshot::channel::<u32>();

    wasm_bindgen_futures::spawn_local(hclient_fetch::testing::deliver(
        async move {
            let _held = guard;
            // A promise the browser will never settle — the shape of a
            // request to a server that never answers.
            std::future::pending::<u32>().await
        },
        tx,
    ));

    // Let the task reach its first poll, so this is a cancellation of work
    // in flight rather than of work never started.
    yield_to_the_event_loop().await;
    assert!(
        !dropped.get(),
        "the work is in flight, and nothing has cancelled it yet"
    );

    drop(rx);
    yield_to_the_event_loop().await;
    assert!(
        dropped.get(),
        "dropping the receiver must drop the work — otherwise a fetch runs to completion \
         behind a caller who walked away, which `Transport::execute`'s contract forbids"
    );
}

/// One turn of the browser's event loop, so the spawned task is polled.
///
/// A bare `await` on a ready future is not enough: `spawn_local` queues a
/// microtask, and the assertion is about what that task has done.
async fn yield_to_the_event_loop() {
    let p = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED);
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}
