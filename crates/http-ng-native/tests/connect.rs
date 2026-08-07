//! Checks that the connector genuinely runs Happy Eyeballs: a dead address
//! is tried first, then a live one, and the connection succeeds; and when
//! everything is dead, the failure is reported as `ErrorKind::Connect`.
//!
//! **Why "dead address" here isn't TEST-NET-2.** This file's brief version
//! used `198.51.100.1` (TEST-NET-2, RFC 5737) on the theory that it's
//! "guaranteed not to answer." Checked before being taken on faith, not
//! after: this container has a `tun0` interface (visible in `ip route`)
//! that transparently proxies all outbound traffic — an attempt to reach
//! `198.51.100.1` genuinely SUCCEEDS in connecting, in about 40ms,
//! confirmed both through `cargo test` and separately with a raw
//! `socket.connect()` from Python in the same container. Keeping a test
//! that's red specifically here, and only here, is pointless: it stops
//! being a signal for anyone working here from day one (the same
//! conclusion review Task 4 reached for a similar situation with
//! TEST-NET-1). The property we actually care about — "a failed attempt
//! leads to the next address" / "every failed attempt is reported as
//! `ErrorKind::Connect`" — doesn't depend on WHY the address is dead; the
//! fixtures from `net_fixtures` (see `mod` below) genuinely fail in any
//! environment. Don't reinstate a TEST-NET version of this file — it's
//! red specifically here.
//!
//! Both tests below are wrapped in `tokio::time::timeout`, rather than
//! left as a bare `.await`: a "dead" address is, by construction, exactly
//! the spot where a mutation in `drive`'s loop (e.g. a lost
//! `mark_v6_done`/`mark_v4_done`, or a broken `Exhausted` condition) turns
//! the test not red but hung forever — Task 3 already found exactly this
//! shape of test, and it wasn't a hypothetical, it was a real finding
//! (see this vertical's Global Constraints). The bound is generous (30s)
//! but finite; on a closed local port the real time is much shorter —
//! `ECONNREFUSED` comes back from the kernel, not from the timeout.
mod net_fixtures;

use http_ng_native::testing::connect_for_test;
use http_ng_rt_tokio::Tokio;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

#[tokio::test]
async fn falls_over_from_a_dead_address_to_a_live_one() {
    let (dead, live) = net_fixtures::dead_and_live();
    let conn = tokio::time::timeout(
        BOUND,
        connect_for_test(&Tokio, &[dead, live.ip()], live.port()),
    )
    .await
    .expect("connect_for_test must not hang");
    assert!(conn.is_ok(), "must reach the live address");
}

#[tokio::test]
async fn reports_connect_kind_when_everything_is_dead() {
    let dead = net_fixtures::closed_port();
    let err = tokio::time::timeout(BOUND, connect_for_test(&Tokio, &[dead.ip()], dead.port()))
        .await
        .expect("connect_for_test must not hang")
        .expect_err("closed port must refuse");
    assert!(
        matches!(err.kind(), http_ng_core::ErrorKind::Connect),
        "{err}"
    );
}
