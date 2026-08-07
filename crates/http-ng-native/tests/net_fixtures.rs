//! Shared network fixtures for this crate's integration tests. Doesn't
//! contain any `#[test]` itself — pulled in via `mod net_fixtures;` from
//! `connect.rs` and `dual_runtime.rs` (the same technique as `server.rs`
//! in `http-ng-tls-rustls/tests/`: an ordinary `tests/*.rs` file, which
//! cargo will compile as its own — empty — test binary regardless, but
//! which is also pulled in as a module wherever the shared logic is
//! needed).
//!
//! # Why this is a separate file, rather than just "be more careful everywhere"
//!
//! Review round 1 found the same class of bug twice within one task:
//! the first version of `tests/connect.rs`'s
//! `falls_over_from_a_dead_address_to_a_live_one` combined a closed
//! port's IP with the LIVE listener's port (both addresses need the same
//! port — `connect_for_test`/`connect` take ONE port for every candidate
//! address, Happy Eyeballs tries the same port on different IPs, not
//! different ports on one), which was caught and fixed. Twenty minutes
//! later, the same mistake independently reappeared in
//! `tests/dual_runtime.rs`'s `dead_and_live()`: it returned the closed
//! listener's IP together with a SEPARATE live listener's port — both
//! listeners were on `127.0.0.1`, so the "dead" address actually
//! coincided with the live one (`dead == live.ip()`), and the test
//! genuinely connected to the live listener in ~230µs, never exercising
//! the failure path at all.
//!
//! The lesson isn't "be more careful" — vigilance had already failed once
//! within that same twenty minutes. The lesson: the trap was in the
//! constructor's SHAPE (calling `TcpListener::bind` twice and manually
//! combining one's `.ip()` with the other's `.port()`), not in the one
//! spot where someone forgot to check it. `dead_and_live()` below removes
//! the shape itself: the "dead" IP is a LITERAL constant (`127.0.0.2`),
//! not derived from any listener, so combining it with a live listener's
//! port and accidentally getting a live address is simply not possible —
//! not "unchecked before use," but impossible to write. Both files get
//! this function from here rather than writing their own.
// `#![allow(dead_code)]`, not `#[cfg(test)]` tricks and not `#[expect]`:
// every `tests/*.rs` file that pulls this in via `mod net_fixtures;`
// recompiles it fresh as a SEPARATE binary, and not every one uses BOTH
// functions (`dual_runtime.rs` only takes `dead_and_live`, not
// `closed_port`) — `#[expect(dead_code)]` would be satisfied in some of
// those binaries and not others, and would just swap one warning for
// another (`unfulfilled_lint_expectations`) in the binaries where the
// function is actually used.
#![allow(dead_code)]

use std::net::{IpAddr, SocketAddr};

/// Binds an ephemeral port and immediately drops the listener, so the
/// address is (bar an astronomically unlikely race with another process on
/// this exact port) guaranteed to refuse connections for the rest of the
/// test process's lifetime. Same construction as `http-ng-rt-smol`'s
/// `adversarial_smol_connect.rs::closed_port` — a closed loopback port
/// refuses for real regardless of whatever proxies external egress.
///
/// Only safe to use for a SINGLE candidate address (its IP and port must
/// be used together) — see `dead_and_live` below for the two-address
/// fallback case, which needs a different construction entirely.
pub fn closed_port() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    addr
}

/// Returns `(dead, live)` such that connecting to `dead` at `live.port()`
/// is genuinely refused, and connecting to `live` itself succeeds (a
/// background thread accepts exactly one connection). Structurally cannot
/// return a live address for `dead`: `dead` is a hardcoded, different
/// loopback address (`127.0.0.2`) than the one `live` binds to
/// (`127.0.0.1`) — there is no listener whose own IP or port `dead` is
/// derived from, so there is nothing for a caller to accidentally reuse
/// the way the bug this function replaced did (see the module doc).
pub fn dead_and_live() -> (IpAddr, SocketAddr) {
    let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let live_addr = live.local_addr().unwrap();
    std::thread::spawn(move || {
        let _ = live.accept();
    });
    let dead: IpAddr = "127.0.0.2".parse().unwrap();
    (dead, live_addr)
}
