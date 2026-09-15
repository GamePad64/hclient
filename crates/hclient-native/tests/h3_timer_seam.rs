//! The QUIC timer seam, pinned by the only thing that can see it.
//!
//! `SeamTimer` in `src/http3/runtime.rs` is the `quinn::AsyncTimer` behind
//! **every** retransmission, PTO and idle deadline on the h3 path —
//! `SeamRuntime::new_timer` is its only constructor and `hclient-quinn`
//! carries no competing impl. It had no test, and the measurement that
//! says so is sharp: mutating `AsyncTimer::poll` to `Poll::Ready(())` —
//! every QUIC timer firing instantly — left **586 of 586 tests green**,
//! including the 103 h3 tests and the A/B pair in
//! [`super`](../h3_live.rs)'s `an_idle_connection_survives_only_because_of_the_keep_alive`
//! that `.notes/v03-acceptance.md` records as proving a 1500 ms gap kills
//! an unkept connection under a 1000 ms idle timeout.
//!
//! # Why nothing else could see it, which is the finding
//!
//! Read in `quinn-0.11.11/src/connection.rs:1160`, `drive_timer`:
//!
//! ```text
//! let now = self.runtime.now();
//! if now >= deadline { self.inner.handle_timeout(now); ... return true; }
//! ```
//!
//! **quinn decides from the clock and never from the timer**, with a
//! comment beside it saying exactly why (*"Use the clock rather than the
//! async timer to detect expiry: `Sleep::poll` respects Tokio's cooperative
//! budget and can return Pending for elapsed deadlines"*). A timer that
//! fires early therefore reaches `handle_timeout` with `now < deadline`,
//! which quinn-proto treats as a no-op, and `drive_timer` answers `true`
//! — *keep going* — so the driver loops. A timer that never re-arms keeps
//! its construction deadline, fires once, and thereafter answers `Ready`
//! for ever, which is the same loop one deadline later.
//!
//! So a broken timer here is not a wrong answer at any layer. It is a
//! **busy-spin**: the right bytes, at the right instants, with a core
//! burnt waiting for them. That is invisible to every assertion about a
//! status, a body, a count or a deadline — which is why 586 tests passed
//! over it — and it is a real defect on any network with latency, where
//! the waits this workspace's own h3 fixtures finish in milliseconds are
//! seconds long.
//!
//! # What the observable is, and why it is not a clock assertion
//!
//! CPU time, read from `/proc/self/stat` — the instrument
//! `CLAUDE.md` records for `hclient_native::testing::blocking_io`, where
//! an honest busy-spin measured *wall 600.4 ms, cpu 600 ms* against the
//! same exchange over a real reactor at *wall 601.1 ms, cpu 0 ms*.
//!
//! This is deliberately **not** a wall-clock assertion, because this
//! workspace has found four of those to be flakes. It is not one for a
//! structural reason rather than a hopeful one: `/proc/self/stat` reports
//! **this process's own** CPU, so load from every other test on the
//! machine is invisible to it by construction. Measured while the whole
//! `hclient-native` suite ran at `-j96` on 28 cores, the control read
//! 0–10 ms; unloaded and oversubscribed alone it read 0–20 ms. A loaded
//! runner makes the *wall* time longer, which only gives a spinning timer
//! more room to spend, so the margin widens under load rather than
//! narrowing.
//!
//! # The fixture: a black hole, because it is nothing but deadlines
//!
//! A QUIC handshake against a bound UDP socket that answers nothing has
//! **no incoming packet to wake its driver** — every datagram after the
//! first is a PTO, and a PTO is a pure deadline decision. Measured from
//! the socket's own side, the client's schedule is RFC 9002's exponential
//! backoff, 1.0 s / 3.0 s / 7.0 s, so a two-second window contains one
//! guaranteed ~1 s wait that a working timer sleeps through.
//!
//! # Dead ends, recorded so the next person does not re-derive them
//!
//! Three things were measured and do **not** discriminate:
//!
//! - **The wire.** Both mutants send seven datagrams at 1.0/3.0/7.0 s
//!   against the black hole, byte-for-byte identical to the control —
//!   because `handle_timeout` is clock-gated, the schedule is quinn's
//!   arithmetic and not the timer's. A datagram count or an arrival
//!   histogram, which is what `tests/race_cost.rs`'s black hole exists
//!   for, pins nothing here. An earlier reading of *3 against 5* was an
//!   artifact of a 3000 ms test deadline cutting the control's third
//!   volley; widening the window to 8 s made the two identical.
//! - **Starvation.** A spinning driver does not delay its neighbours:
//!   on a `current_thread` runtime, a task sleeping in 10 ms steps beside
//!   the spin completed 91 ticks per second against the control's 90.
//!   Tokio's cooperative budget is what keeps the spin fair, which is the
//!   same mechanism quinn's comment names.
//! - **The existing idle A/B.** `survives_gap`'s two arms both hold under
//!   either mutation: the kept arm pings *more* often than asked and
//!   survives, and the unkept arm dies as it is meant to. A timer that
//!   fires too eagerly cannot fail a test whose failure mode is a
//!   connection dying too soon.
//!
//! Linux only, for the instrument: `/proc/self/stat` is where the
//! measurement comes from, and a portable substitute would be a wall-clock
//! assertion, which is the thing this test exists not to be.
#![cfg(all(feature = "http3", target_os = "linux", not(target_family = "wasm")))]

#[path = "h3_server.rs"]
mod server;

use hclient_core::body::RequestBody;
use hclient_core::transport::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::H3;
use hclient_rt_tokio::TokioHandle;
use std::time::Duration;

/// How long the handshake is left to run. Two seconds spans the initial
/// flight and the first PTO volley at ~1.0 s, so it contains one
/// guaranteed second of waiting.
const WINDOW: Duration = Duration::from_millis(2000);

/// The most CPU this process may spend inside [`WINDOW`].
///
/// A ceiling with room rather than today's number, which is the rule
/// `tests/future_size.rs` states for its own bound: the measurements are
/// control 0–20 ms (0–10 ms with the whole suite loading 28 cores) against
/// 920 ms for a timer that never re-arms and 2.37 s for one that fires
/// instantly. 300 ms is 15x the noisiest control and 3x below the quietest
/// mutant, so neither a slow runner nor a faster QUIC stack moves it.
const BUDGET: Duration = Duration::from_millis(300);

fn h3(
    cert: &rustls::pki_types::CertificateDer<'static>,
) -> H3<TokioHandle, hclient_tls_rustls::Rustls, IpLiteralOnly> {
    H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(cert),
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O")
}

fn get(addr: std::net::SocketAddr, path: &str) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(format!("https://{addr}{path}"))
        .body(RequestBody::Empty)
        .unwrap()
}

/// This process's own CPU time, user plus system.
///
/// Fields 14 and 15 of `/proc/self/stat` in `proc(5)`'s numbering. The
/// split is on `") "` rather than on whitespace because field 2 is the
/// executable name in parentheses and may itself contain spaces — after
/// that separator field 3 (`state`) is index 0, so `utime` is index 11.
///
/// The unit is clock ticks, 100 Hz on every Linux this runs on, so the
/// resolution is 10 ms. That is coarse and it is what makes the reading
/// trustworthy: a control that measures `0` has not been rounded down
/// from something, it spent under one tick.
fn cpu() -> Duration {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
    let after_comm = stat
        .rsplit(") ")
        .next()
        .expect("rsplit always yields one field");
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let ticks: u64 =
        fields[11].parse::<u64>().expect("utime") + fields[12].parse::<u64>().expect("stime");
    Duration::from_millis(ticks * 10)
}

/// A bound UDP socket that answers nothing — a `DROP`, which is what a
/// firewall blocking UDP/443 does, rather than the `REJECT` an unbound
/// port would give. Held by the caller: dropping it makes the kernel send
/// ICMP port-unreachable and the handshake fails promptly instead of
/// waiting, which would leave this test measuring nothing.
fn black_hole() -> (std::net::UdpSocket, std::net::SocketAddr) {
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let addr = sock.local_addr().expect("local_addr");
    (sock, addr)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_quic_handshake_that_is_waiting_for_a_deadline_is_asleep_and_not_spinning() {
    let (_hole, addr) = black_hole();
    // A certificate with no server behind it: nothing here gets far enough
    // to check one, and starting a server would give the handshake
    // something to complete against — and something other than a timer to
    // wake its driver, which is the whole fixture.
    let id = server::identity();
    let t = h3(&id.cert_der);

    let before = cpu();
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(WINDOW, t.execute(get(addr, "/x"))).await;
    let (spent, wall) = (cpu().saturating_sub(before), started.elapsed());

    // The premise, asserted rather than assumed: if the handshake had
    // finished or failed early there would be no waiting to measure, and a
    // low CPU reading would say nothing at all. This is
    // `without_the_bound_the_same_handshake_is_still_going`'s control,
    // which the CPU assertion below needs for the same reason.
    assert!(
        outcome.is_err(),
        "the handshake must still be in flight when the window closes, or \
         there was no deadline for the timer to wait on: finished in {wall:?}"
    );

    assert!(
        spent < BUDGET,
        "a QUIC connection waiting ~1s for its first PTO spent {spent:?} of \
         CPU over {wall:?} of wall time, which is a busy-spin rather than a \
         sleep: `SeamTimer` is either firing before its deadline or not \
         re-arming, and quinn decides from the clock so nothing else in this \
         suite can see it (budget {BUDGET:?})"
    );
}
