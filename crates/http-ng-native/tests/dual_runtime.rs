//! Доказательство того же тезиса, что `http-ng-rt-pair-check` (Task 4) и
//! `http-ng-tls-rustls`'s `dual_runtime.rs` (Task 9) доказывают для своих
//! слоёв: ОДНО тело — здесь `connect_for_test`, ровно тот код, который
//! `race_connect` гоняет через `Wait`-ветку `drive` (не только мгновенный
//! успех на первом `Start`) — работает и на tokio, и на smol без единого
//! `#[cfg]` и без спавна задач. Это прямая, а не только типовая проверка
//! требования брифа "`race_connect` не использует `spawn`": `smol`
//! однопоточен по умолчанию у `futures_executor::block_on`, и если бы где-то
//! в `drive` протёк `spawn`/`Send`-бонд через комбинатор (а не через явное
//! объявление), этот файл не скомпилировался и не прошёл бы здесь.
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
