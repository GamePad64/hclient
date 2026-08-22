//! Reviewer-written adversarial test suite for `Smol::connect`'s
//! non-blocking connect dance, independent
//! of the implementer's work.
//!
//! Exercises `begin_connect`'s classification of `connect()` outcomes
//! against a real socket rather than a mock: a closed port refuses the
//! connection for real, over loopback, and the assertion is that the
//! refusal surfaces through `TcpConnect::connect`'s `Result` as an `Err`
//! (specifically `ErrorKind::ConnectionRefused`) rather than as an `Ok`
//! stream that would only fail on first use.
//!
//! `begin_connect`/`build_socket` are private to `hclient-rt-smol::lib`, so
//! this can only observe the public contract (`TcpConnect::connect`'s
//! `Result`), not which private match arm fired internally. Which arm fires
//! (immediate `Err(ECONNREFUSED)` vs. `WouldBlock`/`EINPROGRESS` followed by
//! an async `SO_ERROR` readback) is host/kernel-timing dependent even for
//! the same test on the same machine - loopback ECONNREFUSED is commonly
//! synchronous on Linux but the public contract must hold either way, so
//! the test asserts the outcome, not the internal path.
//!
//! Instrumentation (`eprintln!` in each match arm of `begin_connect`, not
//! kept) showed the closed-port refusal in this
//! sandbox resolves via the `EINPROGRESS` branch (`raw_os_error=Some(115)`,
//! `kind=InProgress`), not the synchronous-`Err` branch - i.e. this sandbox
//! exercises the async `writable().await` + `take_error()` path, not just
//! the immediate-classification path in `begin_connect`. Mutation-tested
//! (temporarily replacing three of `build_socket`'s six setters with
//! no-ops, one at a time, restored via `cp`): `local_address`,
//! `reuse_address`, and `nodelay` mutations each turned exactly their own
//! named test in `smol_socket_opts_tests.rs` red and nothing else.
//!
//! To re-run: drop this file into `crates/hclient-rt-smol/tests/` in a
//! scratch clone and `cargo test -p hclient-rt-smol --test
//! adversarial_smol_connect --all-features`.
use hclient_rt::{TcpConnect, TcpOpts};
use hclient_rt_smol::Smol;
use std::net::SocketAddr;
use std::time::Duration;

/// Every wait in this file is bounded: an unbounded `.await` on a hung
/// connect would wedge the whole test binary (and the CI job) with no
/// diagnosis instead of failing. Ten seconds is orders of magnitude more
/// than a loopback connect/refusal needs.
const BOUND: Duration = Duration::from_secs(10);

async fn bounded<T>(
    fut: impl std::future::Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    futures_lite::future::or(fut, async {
        async_io::Timer::after(BOUND).await;
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("did not resolve within {BOUND:?} - treating a stall as a regression"),
        ))
    })
    .await
}

/// Binds an ephemeral port and immediately drops the listener, so the
/// address is (bar an astronomically unlikely race with another process on
/// this exact port) guaranteed to refuse connections for the rest of the
/// test process's lifetime.
fn closed_port() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    addr
}

#[test]
fn connect_to_a_closed_port_surfaces_as_a_connect_error_not_a_stream() {
    futures_executor::block_on(async {
        let addr = closed_port();
        let result = bounded(Smol.connect(addr, &TcpOpts::default())).await;
        match result {
            Ok(_stream) => panic!(
                "connect to a closed port ({addr}) unexpectedly returned Ok - the refusal must \
                 surface as an Err from connect(), not as a stream that fails on first use"
            ),
            Err(e) => {
                assert_eq!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused,
                    "expected ConnectionRefused from a closed loopback port, got {e:?}"
                );
            }
        }
    });
}

#[test]
fn connect_to_an_accepting_listener_still_succeeds() {
    // Negative control for the test above: a real, accepting listener must
    // still succeed - otherwise "every connect fails" would trivially pass
    // the refused-port test too.
    futures_executor::block_on(async {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });
        let result = bounded(Smol.connect(addr, &TcpOpts::default())).await;
        assert!(
            result.is_ok(),
            "connect to a real listener failed: {result:?}"
        );
    });
}

// A third test attempting to connect to 192.0.2.1:9 (TEST-NET-1, RFC 5737 -
// reserved, must never be routable) was tried here to force the
// writable().await + take_error() path for a connect that stays
// in-progress rather than resolving synchronously. Dropped: in this
// sandbox it is not reliable - repeated runs alternated between "times out
// at the 10s bound" and "connect unexpectedly succeeds", which means
// something in this network environment sometimes answers on behalf of
// that address (a transparent proxy/NAT, most likely) rather than the
// destination being consistently unroutable. That is an environment
// property, not a property of `Smol::connect`, so it does not belong in a
// suite meant to be re-run later - a flaky assertion is worse than no
// assertion. The closed-port test above still exercises the async
// (EINPROGRESS-then-writable-then-SO_ERROR) path reliably in this same
// sandbox: probing `begin_connect` directly (reviewer-only instrumentation,
// not kept) showed the closed-port refusal itself resolves via the
// EINPROGRESS branch here, not the synchronous-Err branch, so the
// async path is already covered without relying on an external address.
