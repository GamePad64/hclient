//! Proof of the same thesis that `http-ng-rt-pair-check` (Task 4) and
//! `http-ng-tls-rustls`'s `dual_runtime.rs` (Task 9) prove for their own
//! layers: ONE body — here, `connect_for_test`, exactly the code
//! `race_connect` runs through `drive`'s `Wait` branch (not just an
//! immediate success on the first `Start`) — works on both tokio and
//! smol with not a single `#[cfg]` and no task spawning. This is a
//! direct check of the brief's requirement "`race_connect` doesn't use
//! `spawn`," not just a type-level one: `smol` is single-threaded by
//! default under `futures_executor::block_on`, and if a `spawn`/`Send`
//! bound had leaked into `drive` anywhere through a combinator (rather
//! than through an explicit declaration), this file would fail to
//! compile, or fail to pass, right here.
mod net_fixtures;

use http_ng_native::testing::connect_for_test;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

// `dead_and_live()` lives in `net_fixtures` now, shared with
// `tests/connect.rs` — see that module's doc comment for why: this file's
// own copy of the same helper independently reproduced (dead == live.ip(),
// both listeners bound to 127.0.0.1, the closed listener's own port
// discarded in favor of the live one's) the exact bug found and fixed in
// `tests/connect.rs` twenty minutes earlier in the same review round. A
// shared, structurally-can't-return-a-live-address helper is the fix, not
// "be more careful this time" — vigilance already failed once in that same
// window.

#[tokio::test]
async fn same_connector_falls_over_to_a_live_address_on_tokio() {
    use http_ng_rt_tokio::Tokio;
    let (dead, live) = net_fixtures::dead_and_live();
    let conn = tokio::time::timeout(
        BOUND,
        connect_for_test(&Tokio, &[dead, live.ip()], live.port()),
    )
    .await
    .expect("must not hang");
    assert!(conn.is_ok());
}

#[test]
fn same_connector_falls_over_to_a_live_address_on_smol() {
    use http_ng_rt_smol::Smol;
    let (dead, live) = net_fixtures::dead_and_live();
    // `futures_executor::block_on` has no built-in timeout, and `Smol`'s
    // `Timer` doesn't compose with `tokio::time::timeout` (different
    // runtime) — so the bound here is a watchdog thread + a channel, same
    // discipline as the tokio test above, just implemented without a
    // runtime-provided combinator: a mutation that made `drive` loop
    // forever must fail this test, not hang the whole suite.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let conn =
            futures_executor::block_on(connect_for_test(&Smol, &[dead, live.ip()], live.port()));
        let _ = tx.send(conn.is_ok());
    });
    let ok = rx
        .recv_timeout(BOUND)
        .expect("connect_for_test must not hang on smol");
    assert!(ok);
}
