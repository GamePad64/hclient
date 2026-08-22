//! The hedge, watched from the servers' side of the wire.
//!
//! Two real servers behind one authority, both alive in every test, so
//! *"which stack carried this request"* is a question a peer answers and
//! *"how many requests reached the origin"* is a number rather than an
//! argument.
//!
//! # The one claim this file exists for
//!
//! An earlier measurement of a race made of two `Transport::execute`
//! calls found that **with no head start the
//! losing arm delivers a complete, well-formed HTTP request to the
//! origin** — five of six arms. That is what made the head start a safety
//! mechanism and what stopped the race being built.
//!
//! [`with_no_head_start_both_stacks_connect_and_exactly_one_request_is_sent`]
//! is the negation, at the setting that used to be worst: both arms
//! connect, the servers say so, and **exactly one** of them is asked for
//! anything. Nothing else here means much without it.
//!
//! # What is asserted on a clock, and why only that
//!
//! One thing, in
//! [`a_blocked_origin_is_answered_without_waiting_for_quinn_to_give_up`],
//! because a duration genuinely is the feature: the whole point of the
//! hedge is that a UDP-blocked origin costs a head start rather than
//! quinn's 30 s `max_idle_timeout`. The bound is **5 s** — six times below
//! the 30.002–30.006 s that hop is measured to cost without the hedge
//! (§7.3, M1) and a hundred times above the ~50 ms it should cost with
//! it. Everything else is a counter, and most of them are deltas across a
//! hop.
#![cfg(not(target_family = "wasm"))]

mod fakedns;
mod servers;

use fakedns::{FakeDns, service_record};
use hclient_core::unversioned::Transport;
use hclient_core::{Error, ErrorKind, Phase, RequestBody, RequireVersion, Timeouts};
use hclient_h3::H3;
use hclient_native::Native;
use hclient_rt_tokio::TokioHandle;
use hclient_select::{DEFAULT_HEAD_START, Selecting};
use http_body_util::BodyExt;
use servers::{ORIGIN, Pair, Quic, Tcp};
use std::time::Duration;

/// Never an assertion — it turns a mutation that hangs into a red test
/// rather than an eternal one.
const BOUND: Duration = Duration::from_secs(20);

type Selector = Selecting<TokioHandle, hclient_tls_rustls::Rustls, FakeDns>;

fn plain(pair: &Pair, dns: FakeDns) -> Selector {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    Selecting::new(
        rt.clone(),
        Native::new(rt.clone(), servers::client_tls(&pair.cert_der), dns.clone()),
        H3::new(rt, servers::client_tls(&pair.cert_der), dns.clone()).expect("H3::new does no I/O"),
        dns,
    )
    .expect("the two stacks agree")
}

/// The same transport with the race switched on — which is the only way it
/// is ever on.
fn hedged(pair: &Pair, dns: FakeDns, head_start: Duration) -> Selector {
    plain(pair, dns).hedging(head_start)
}

/// An origin whose HTTPS record offers `h3`, so the fast tier sends the
/// very first request to QUIC and no advertisement has to be heard first.
fn offers_h3() -> FakeDns {
    FakeDns::with_records(vec![service_record(1, &[b"h3"])])
}

fn request(pair: &Pair, timeouts: Option<Timeouts>) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .uri(format!("https://{ORIGIN}:{}/hello", pair.port))
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    if let Some(t) = timeouts {
        req.extensions_mut().insert(t);
    }
    req
}

fn bound(connect: Duration) -> Timeouts {
    Timeouts {
        resolve: None,
        connect: Some(connect),
        ..Default::default()
    }
}

/// What one hop did, as the **servers** saw it, plus what the caller got.
#[derive(Debug)]
struct Hop {
    /// QUIC connection attempts that reached a live endpoint.
    quic_tried: usize,
    /// HTTP/3 requests answered.
    quic_answered: usize,
    /// UDP datagrams that reached a [`Quic::BlackHole`] — the only thing a
    /// hole can be observed by, and what makes *"this hop did not try
    /// QUIC"* a delta.
    quic_datagrams: usize,
    /// HTTP/1.1 requests answered over TLS on TCP.
    tcp_answered: usize,
    /// TCP connections accepted, answered or not — the counter that sees a
    /// hedge which connected and was never spent.
    tcp_accepted: usize,
    elapsed: Duration,
    got: Result<String, Error>,
}

impl Hop {
    fn body(&self) -> &str {
        match &self.got {
            Ok(s) => s,
            Err(e) => panic!("expected one of the two servers to answer, got {e:?}"),
        }
    }

    /// Requests that reached the origin at all, whichever stack carried
    /// them. **This is the number the whole file is about.**
    fn requests_at_the_origin(&self) -> usize {
        self.quic_answered + self.tcp_answered
    }
}

fn counters(pair: &Pair) -> (usize, usize, usize, usize, usize) {
    (
        pair.quic_attempted(),
        pair.quic_answered(),
        pair.quic_datagrams(),
        pair.tcp_answered(),
        pair.tcp_accepted(),
    )
}

async fn hop(t: &Selector, pair: &Pair, req: http::Request<RequestBody>) -> Hop {
    let before = counters(pair);
    let began = std::time::Instant::now();
    let got = match tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("the request finished inside the bound")
    {
        Ok(resp) => {
            assert_eq!(resp.status(), 200);
            let bytes = resp
                .into_body()
                .collect()
                .await
                .expect("a complete body")
                .to_bytes();
            Ok(String::from_utf8(bytes.to_vec()).expect("utf-8"))
        }
        Err(e) => Err(e),
    };
    let elapsed = began.elapsed();
    let after = counters(pair);
    Hop {
        quic_tried: after.0 - before.0,
        quic_answered: after.1 - before.1,
        quic_datagrams: after.2 - before.2,
        tcp_answered: after.3 - before.3,
        tcp_accepted: after.4 - before.4,
        elapsed,
        got,
    }
}

/// Wait for a server thread to have caught up with a socket the kernel
/// already handed it.
///
/// Only ever used for assertions of the *at least* kind: a counter that has
/// not moved yet is a scheduling fact, where a counter that must **not**
/// move is read immediately and needs no waiting.
///
/// **A guard rather than a claim**, and ten seconds rather than one — but
/// not for the reason that was first written here. The bound was widened
/// on the theory that an oversubscribed run (`-j96` on 28 cores) had
/// starved the TCP fixture's thread; **it was measured and the theory was
/// wrong**, the same three-in-forty failure rate before and after. What
/// the widening did establish is that the connection never existed rather
/// than arriving late, which is what sent the search to the caller and
/// found a timing claim there. The generous bound is kept anyway: a guard
/// costs a passing run nothing, since the loop exits on the first poll
/// that sees the counter.
async fn eventually(mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..2_000 {
        if ready() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    ready()
}

/// Waits until the black hole's datagram counter has stopped moving.
///
/// **Between two hops, and it is a fixture problem rather than a client
/// one.** `hop` measures a delta across `execute`, and the end of that
/// future is not the end of the abandoned QUIC arm's UDP: measured, a hop
/// whose hedge wins puts **two** datagrams into the hole, and under an
/// oversubscribed run the second of them arrives after `execute` has
/// already returned — landing in the *next* hop's delta and reading as a
/// QUIC attempt the memory failed to stop.
///
/// Captured six times in forty full-workspace runs at `-j96` before this
/// existed, always the same shape: hop 1 with one datagram instead of its
/// usual two, hop 2 with the missing one.
///
/// A guard, not an assertion: it exits on the first pair of equal reads,
/// so a run where nothing is late pays two polls.
async fn settled(pair: &Pair) {
    let mut last = pair.quic_datagrams();
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let now = pair.quic_datagrams();
        if now == last {
            return;
        }
        last = now;
    }
}

// --- the premise ---------------------------------------------------------

/// **The claim the staged connect made true and this file exists for.**
///
/// No head start at all, and two working servers: **exactly one request
/// reaches the origin**. The measurement this replaces had a complete
/// request delivered by the *losing* arm in five of six arms at this
/// setting.
///
/// It does not assert which arm wins, and must not: on loopback there is no
/// round trip, so the two stacks are being compared on CPU alone and TCP is
/// the faster of them since `TCP_NODELAY` landed (re-measured for this
/// work: TCP by name 1.4–2.6 ms against QUIC's 2.5–7.8 ms). What is
/// asserted is the thing that holds whichever wins, because neither
/// `connect` writes a request byte.
///
/// # It used to assert that both stacks connect, and that was a clock
///
/// The name said *both stacks connect*, and the body asserted
/// `tcp_accepted >= 1`. That is a timing claim wearing a counter's
/// clothes: at a head start of zero the hedge is only *started*, and a
/// QUIC arm that finishes first cancels it — possibly before its `SYN`
/// leaves. Captured twice under an oversubscribed run, with the counters
/// in the failure: `body="h3" quic_answered=1 tcp_accepted=0
/// elapsed=3.0ms`. Widening the wait to ten seconds changed nothing,
/// which is what says the connection never existed rather than arriving
/// late.
///
/// Nothing is lost by dropping it. That the hedge runs and answers when
/// QUIC cannot is asserted **causally** by
/// [`a_blocked_origin_is_answered_without_waiting_for_quinn_to_give_up`],
/// and that its connection is the one used by
/// [`the_connection_the_hedge_made_is_the_one_the_request_is_sent_on`] —
/// neither of which depends on who is faster.
#[tokio::test(flavor = "multi_thread")]
async fn with_no_head_start_exactly_one_request_reaches_the_origin() {
    let pair = servers::start();
    let t = hedged(&pair, offers_h3(), Duration::ZERO);

    let one = hop(&t, &pair, request(&pair, None)).await;

    assert_eq!(
        one.requests_at_the_origin(),
        1,
        "one request was made, so one request reaches the origin — the \
         losing arm connected and sent nothing"
    );
    assert!(
        matches!(one.body(), "h1" | "h3"),
        "and the caller got a real answer from whichever server won"
    );
    // The QUIC arm is safe to assert on where the hedge is not: `select`
    // polls it first and nothing cancels it before it has begun.
    assert!(
        eventually(|| pair.quic_attempted() >= 1).await,
        "with no head start the QUIC arm certainly ran"
    );
}

/// The other end of the same knob, and the reason the default is not zero.
///
/// At [`DEFAULT_HEAD_START`] against a working QUIC origin the hedge is
/// never started: **no TCP socket is opened at all**. The margin is stated
/// rather than assumed — a cold QUIC exchange on this host is 2.5–7.8 ms
/// median and 19.6 ms at its worst sample, against a 250 ms head start, so
/// this is an order of magnitude and a half of room.
///
/// `quic_tried == 1` is the other half: the connection the race made is the
/// one the request is sent on, not a second one — the QUIC side of
/// [`the_connection_the_hedge_made_is_the_one_the_request_is_sent_on`].
#[tokio::test(flavor = "multi_thread")]
async fn the_head_start_keeps_the_hedge_from_firing_when_quic_answers() {
    let pair = servers::start();
    let t = hedged(&pair, offers_h3(), DEFAULT_HEAD_START);

    let one = hop(&t, &pair, request(&pair, None)).await;

    assert_eq!(
        (one.body(), one.tcp_accepted),
        ("h3", 0),
        "QUIC answered long before the head start elapsed, so the hedge \
         never opened a socket"
    );
    assert_eq!(
        one.quic_tried, 1,
        "and the request went out on the connection the race made, not on a second one"
    );
}

// --- what the hedge buys -------------------------------------------------

/// **The feature.** An origin that offers `h3` on a network where UDP does
/// not get through is answered over TCP in about a head start, rather than
/// after quinn's `max_idle_timeout`.
///
/// This is the one assertion in the file that is on a clock, and the
/// margins are the argument: this exact hop measures **30.002–30.006 s**
/// without a hedge, six runs across two
/// profiles with a spread of four milliseconds; with the hedge it is the
/// 50 ms head start plus a TCP exchange. **5 s** sits two orders of
/// magnitude above the second and six times below the first.
#[tokio::test(flavor = "multi_thread")]
async fn a_blocked_origin_is_answered_without_waiting_for_quinn_to_give_up() {
    let pair = servers::start_with_quic(Quic::BlackHole);
    let t = hedged(&pair, offers_h3(), Duration::from_millis(50));

    let one = hop(&t, &pair, request(&pair, None)).await;

    assert_eq!(
        (one.body(), one.tcp_answered),
        ("h1", 1),
        "the hedge answered where QUIC never will"
    );
    assert!(
        one.quic_datagrams >= 1,
        "and QUIC really was tried — a hop that skipped it proves nothing"
    );
    assert!(
        one.elapsed < Duration::from_secs(5),
        "the whole point is not waiting out quinn's 30 s idle timeout; this \
         hop took {:?}",
        one.elapsed
    );
}

/// The hand-off from the winning arm to the request goes through the
/// **pool**, and this is what says so.
///
/// The hedge connects, its handle is dropped, and `hclient_native::Staged`'s
/// own `Drop` checks that connection in warm. The request is then sent on
/// it. If the handle's connection were discarded instead — or if the routed
/// request dialled afresh — the listener would have accepted **two**
/// connections for one request.
///
/// This fixture's TCP server answers once and closes, so a second accept
/// would be plainly visible rather than hidden by reuse.
#[tokio::test(flavor = "multi_thread")]
async fn the_connection_the_hedge_made_is_the_one_the_request_is_sent_on() {
    let pair = servers::start_with_quic(Quic::BlackHole);
    let t = hedged(&pair, offers_h3(), Duration::from_millis(50));

    let one = hop(&t, &pair, request(&pair, None)).await;

    assert_eq!(
        (one.tcp_accepted, one.tcp_answered, one.body()),
        (1, 1, "h1"),
        "one connection, one request — the hedge's connection is the request's"
    );
}

// --- it is off until it is asked for -------------------------------------

/// **Genuinely optional, and this is the A/B that says so.**
///
/// One fixture, one bound, one difference: whether
/// [`Selecting::hedging`](hclient_select::Selecting::hedging) was called.
/// Without it the request spends its whole `Timeouts::connect` on a QUIC
/// arm that will never answer and fails — which is exactly the behaviour
/// this crate had before the race existed, and which
/// `h3_failure::the_fallback_spends_what_is_left_of_the_connect_bound_and_no_more`
/// already pins from the other side. With it, the same request is answered.
///
/// A default that opened UDP sockets and TCP sockets for one request would
/// be a decision about what a plain client does on a network that blocks
/// UDP/443. This is that decision, made visible.
#[tokio::test(flavor = "multi_thread")]
async fn the_race_is_off_until_it_is_asked_for() {
    let connect = bound(Duration::from_millis(300));

    // Arm A — no hedge. The QUIC arm spends the whole bound and the
    // sequential fallback has nothing left to connect with.
    let pair = servers::start_with_quic(Quic::BlackHole);
    let t = plain(&pair, offers_h3());
    let a = hop(&t, &pair, request(&pair, Some(connect))).await;
    assert_eq!(
        a.tcp_accepted, 0,
        "a transport that was not asked to race must not open a TCP connection"
    );
    assert_eq!(
        *a.got.expect_err("nothing was left to connect with").kind(),
        ErrorKind::Timeout(Phase::Connect),
    );

    // Arm B — the same everything, hedged.
    let pair = servers::start_with_quic(Quic::BlackHole);
    let t = hedged(&pair, offers_h3(), Duration::from_millis(50));
    let b = hop(&t, &pair, request(&pair, Some(connect))).await;
    assert_eq!(
        (b.body(), b.tcp_answered),
        ("h1", 1),
        "and one that was gets an answer out of the same bound"
    );
}

/// A `RequireVersion(HTTP_3)` demand is not hedged, and the reason is the
/// same one that keeps it from falling back: a caller who demanded HTTP/3
/// cannot be answered over TCP, so a TCP connection opened for this request
/// is a connection opened for nothing at all.
///
/// The two conditions on the hedge are separate — *a race was asked for*
/// and *this request may go over TCP* — and this is the second one. The
/// test above is the first.
#[tokio::test(flavor = "multi_thread")]
async fn a_demand_for_http_3_is_not_hedged() {
    let pair = servers::start_with_quic(Quic::BlackHole);
    let t = hedged(&pair, offers_h3(), Duration::from_millis(50));

    let mut req = request(&pair, Some(bound(Duration::from_millis(300))));
    req.extensions_mut()
        .insert(RequireVersion(http::Version::HTTP_3));
    let one = hop(&t, &pair, req).await;

    assert_eq!(
        one.tcp_accepted, 0,
        "a demand for HTTP/3 must not open a TCP connection, hedge or no hedge"
    );
    assert!(
        one.quic_datagrams >= 1,
        "it was tried over QUIC, which is what the caller asked for"
    );
    assert_eq!(
        *one.got.expect_err("QUIC never answered").kind(),
        ErrorKind::Timeout(Phase::Connect),
    );
}

/// And a request the two tiers sent to TCP is not hedged either, because
/// there is nothing to hedge: the race lives inside the QUIC arm, and a
/// record that does not offer `h3` never reaches it.
///
/// The QUIC half of this fixture is a black hole, so *"QUIC was not tried"*
/// is a datagram count of zero rather than an absence of evidence.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_the_record_sent_to_tcp_never_touches_the_hedge() {
    let pair = servers::start_with_quic(Quic::BlackHole);
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h2", b"http/1.1"])]);
    let t = hedged(&pair, dns, Duration::from_millis(50));

    let one = hop(&t, &pair, request(&pair, None)).await;

    assert_eq!(
        (one.body(), one.tcp_answered, one.quic_datagrams),
        ("h1", 1, 0),
        "the record chose TCP and nothing raced"
    );
}

// --- the budget ----------------------------------------------------------

/// `H >= C` — a caller's bound with no room for two connects in it — leaves
/// the sequential fallback, and does not double the bound to make room.
///
/// Refusing the **request** would be worse than the thing
/// it replaces: what the caller gets is what they got before the race
/// existed. What must not happen is a hedge started at 250 ms inside a
/// 100 ms bound, which is a bound the transport doubled on its own
/// initiative.
#[tokio::test(flavor = "multi_thread")]
async fn a_head_start_that_does_not_fit_the_bound_leaves_the_sequential_fallback() {
    let pair = servers::start_with_quic(Quic::BlackHole);
    let t = hedged(&pair, offers_h3(), DEFAULT_HEAD_START);

    let one = hop(
        &t,
        &pair,
        request(&pair, Some(bound(Duration::from_millis(100)))),
    )
    .await;

    assert_eq!(
        one.tcp_accepted, 0,
        "there was no room inside the caller's bound for a second connect"
    );
    assert_eq!(
        *one.got
            .expect_err("the QUIC arm spent the whole bound")
            .kind(),
        ErrorKind::Timeout(Phase::Connect),
    );
    assert!(
        one.elapsed < Duration::from_secs(1),
        "and the bound was the caller's, not the caller's plus a head start; \
         this hop took {:?}",
        one.elapsed
    );
}

/// **The race is one connect phase and is charged for once**, and this is
/// where that is visible: both arms fail, so the request reaches the end of
/// the race with nothing left of `Timeouts::connect` and the QUIC arm's own
/// failure is the answer.
///
/// The hedge is refused causally — one round trip, a TLS handshake that
/// meets EOF — so it is the QUIC arm that spends the bound, exactly as in
/// `h3_failure::the_fallback_spends_what_is_left_of_the_connect_bound_and_no_more`
/// one layer up. **`tcp_accepted == 1` is the assertion**: a race that
/// handed the request a fresh copy of the bound would go on to make a
/// second TCP connect, which this server would refuse in its turn and which
/// the caller's bound never had room for.
///
/// It is also the only test here that reaches the arm where the **hedge**
/// resolves first and loses: a hedge that failed is not an answer, so the
/// QUIC arm is awaited alone rather than the race ending on it.
#[tokio::test(flavor = "multi_thread")]
async fn the_race_spends_the_connect_bound_once_when_both_arms_fail() {
    let pair = servers::start_with(Quic::BlackHole, Tcp::Rejecting);
    let t = hedged(&pair, offers_h3(), Duration::from_millis(50));

    let one = hop(
        &t,
        &pair,
        request(&pair, Some(bound(Duration::from_millis(300)))),
    )
    .await;

    assert_eq!(
        one.tcp_accepted, 1,
        "the hedge connected once; there was no bound left for a second attempt"
    );
    assert_eq!(
        *one.got.expect_err("both arms failed").kind(),
        ErrorKind::Timeout(Phase::Connect),
        "and the answer is the arm that spent the bound"
    );
}

// --- what the race teaches -----------------------------------------------

/// **A QUIC arm that loses the race teaches the failure memory**, which is
/// what stops the head start being paid on every request to a blocked
/// origin — the cost that made the race not worth building without it.
///
/// Hop 1 races and the hedge wins. Hop 2 is not raced at all, and the
/// black hole's datagram counter is what says so: a hop the memory held
/// back sends no UDP. Hop 3 is the control — the memory is the only thing
/// holding QUIC back, so clearing it brings the attempt straight back with
/// the record unchanged.
#[tokio::test(flavor = "multi_thread")]
async fn a_quic_arm_that_lost_the_race_teaches_the_memory() {
    let pair = servers::start_with_quic(Quic::BlackHole);
    let t = hedged(&pair, offers_h3(), Duration::from_millis(50));

    let one = hop(&t, &pair, request(&pair, None)).await;
    assert_eq!(one.body(), "h1");
    assert!(
        one.quic_datagrams >= 1,
        "this hop must actually have tried QUIC, or hop 2 proves nothing"
    );

    settled(&pair).await;
    let two = hop(&t, &pair, request(&pair, None)).await;
    assert_eq!(
        (two.body(), two.quic_datagrams),
        ("h1", 0),
        "the origin's HTTP/3 is not tried again inside the window, so the \
         head start is paid once and not once per request"
    );

    t.network_changed();

    let three = hop(&t, &pair, request(&pair, None)).await;
    assert_eq!(three.body(), "h1");
    assert!(
        three.quic_datagrams >= 1,
        "a reported network change forgets it, and the record still offers h3"
    );
}

/// The other direction of the same decision, and it is what keeps the one
/// above from reading as *"the hedge suppresses QUIC"*: an origin whose
/// QUIC **answers** teaches the memory nothing, so the next request goes to
/// QUIC again.
///
/// Without this the pair is one rule; with only this, the memory would look
/// like it was never written.
#[tokio::test(flavor = "multi_thread")]
async fn a_quic_arm_that_won_teaches_it_nothing() {
    let pair = servers::start_with_quic(Quic::Working);
    let t = hedged(&pair, offers_h3(), DEFAULT_HEAD_START);

    for hop_number in 1..=2 {
        let one = hop(&t, &pair, request(&pair, None)).await;
        assert_eq!(
            (one.body(), one.tcp_accepted),
            ("h3", 0),
            "hop {hop_number} went over QUIC and opened no TCP connection"
        );
    }
}
