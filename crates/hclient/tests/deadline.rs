//! `Timeouts.total`'s replacement: a bound on the whole operation, against
//! real servers on loopback.
//!
//! The gap is a response that starts promptly and then dribbles just under
//! the `between_bytes` threshold: `connect`/`first_byte`/`between_bytes` all
//! hold, and the operation still runs for ever. So the headline test here
//! is a server that answers in milliseconds and then drips one byte at a
//! time until
//! the client gives up — **without the bound this test does not fail, it
//! hangs**, which is the only shape of test that can tell the feature
//! working apart from the feature being absent.
//!
//! Everything here runs against `hclient-native` over a real
//! `std::net::TcpListener`, not against a mock: the property being checked
//! is that a live socket stops, and a mock body cannot be wrong about that
//! in any interesting way.
//!
//! The whole-file gate is the same one `two_runtimes.rs` carries and for
//! the same reason: on `wasm32-*` neither the runtime nor `TcpListener`
//! exists, and the dev-dependencies this file needs are target-gated in
//! `Cargo.toml`.
#![cfg(not(target_family = "wasm"))]

use hclient::error::Phase;
use hclient::error::TotalTimeoutElapsed;
use hclient::{Client, ErrorKind};
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::error::Error as _;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

type NativeTransport = Native<Tokio, Rustls, SystemDns<Tokio>>;

fn transport() -> NativeTransport {
    // `with_webpki_roots`, like `two_runtimes.rs`: no handshake happens on
    // these plain-HTTP servers, but `Native::new` still needs a concrete
    // `TlsConnect`, and this one does not touch the system trust store.
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
}

/// Answers immediately, then drips one byte every 20ms for ever, under a
/// `Content-Length` far larger than it will ever send.
///
/// The flag it returns goes `true` when a write to the client fails —
/// which is how this test observes, from the server's side of the wire,
/// that the client's socket actually went away. That is the same technique
/// v0.2 W1's cancellation tests use, and for the same reason: the client's
/// own view of its own future proves nothing about the connection.
fn dribbling_server() -> (std::net::SocketAddr, Arc<AtomicBool>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let client_went_away = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&client_went_away);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            if s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000000\r\n\r\n")
                .is_err()
            {
                flag.store(true, Ordering::SeqCst);
                continue;
            }
            loop {
                if s.write_all(b"x").is_err() || s.flush().is_err() {
                    flag.store(true, Ordering::SeqCst);
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    });
    (addr, client_went_away)
}

/// Accepts the connection, reads the request, and answers nothing, ever.
/// The accepted sockets are kept alive so the client sees an open
/// connection rather than an EOF.
fn silent_server() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            held.push(s);
        }
    });
    addr
}

/// Answers the head after `head_delay` under a `Content-Length` it will
/// never satisfy, and then says **nothing at all**, for ever.
///
/// The difference from `dribbling_server` is the whole point of the tests
/// it serves. A dribbling body wakes the client every 20 ms, so an
/// elapsed-time check gets a second look at the clock for free; this one
/// never wakes it again, so only a sleep the client is holding itself can
/// end the transfer.
///
/// `head_delay` is what makes the *second* of those tests possible: with
/// the head costing a measurable part of the budget, a body sleeping for
/// the whole `total` rather than for what is left of it is a different
/// number rather than a rounding difference.
///
/// The flag goes `true` when the client's FIN arrives — `read` returning
/// `Ok(0)`. The observer is deliberately on the server's side of the wire
/// for the same reason the rest of this file's are: the client's view of
/// its own future proves nothing about the socket.
fn head_then_silence_server(head_delay: Duration) -> (std::net::SocketAddr, Arc<AtomicBool>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let client_went_away = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&client_went_away);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            std::thread::sleep(head_delay);
            if s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000000\r\n\r\n")
                .is_err()
                || s.flush().is_err()
            {
                flag.store(true, Ordering::SeqCst);
                continue;
            }
            // Not a `sleep` loop: blocking in `read` is what keeps the
            // socket open without sending a byte, and it is also how this
            // thread learns that the client hung up.
            let mut sink = [0u8; 64];
            loop {
                match s.read(&mut sink) {
                    Ok(0) | Err(_) => {
                        flag.store(true, Ordering::SeqCst);
                        break;
                    }
                    Ok(_) => continue,
                }
            }
        }
    });
    (addr, client_went_away)
}

/// Redirects every request to itself after `per_hop`, for ever. The
/// counter is how many hops actually happened.
fn redirect_chain_server(per_hop: Duration) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let hops = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hops);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(per_hop);
            let body = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{addr}/next\r\nContent-Length: 0\r\n\r\n"
            );
            let _ = s.write_all(body.as_bytes());
        }
    });
    (addr, hops)
}

/// A complete, prompt response — the control for the timing assertions.
fn prompt_server() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone");
        }
    });
    addr
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("tokio runtime")
}

/// How long a test waits before calling the bound broken.
///
/// Three of the tests below CANNOT pass without the feature they check —
/// they would run for ever, since running for ever is precisely the
/// condition the bound exists to end. Left bare they would hang the suite,
/// which is a worse signal than a failure and costs a CI job its whole
/// budget, so each wraps itself in this outer guard and reports the hang
/// as an assertion instead. The guard is fifteen times the bound under
/// test: wide enough that a loaded machine cannot trip it, narrow enough
/// to be an answer.
const GUARD: Duration = Duration::from_secs(6);

/// The guard, applied — named so the panic says which wait never ended.
async fn guarded<F: std::future::Future>(what: &str, f: F) -> F::Output {
    match tokio::time::timeout(GUARD, f).await {
        Ok(v) => v,
        Err(_) => panic!(
            "{what}: nothing ended this within {GUARD:?}, so the \
             whole-operation bound did not fire at all"
        ),
    }
}

/// Polls a flag for up to `limit`, so a failure is a failed assertion
/// rather than a hung test.
fn wait_for(flag: &AtomicBool, limit: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if flag.load(Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

const TOTAL: Duration = Duration::from_millis(400);

/// The headline property, and the one the acceptance document named.
///
/// The response head arrives in milliseconds, so `connect` and
/// `first_byte` are satisfied; bytes keep arriving every 20ms, so
/// `between_bytes` at any sane threshold is satisfied too. Only a bound on
/// the operation as a whole ends this. **Delete the bound and this test
/// does not go red — it never returns**, which is exactly why it is
/// written against a server that drips rather than one that stalls.
#[test]
fn a_body_that_dribbles_for_ever_is_cut_at_the_total_deadline() {
    let (addr, _) = dribbling_server();
    let c = Client::builder(transport())
        .total_timeout(Tokio, TOTAL)
        .build()
        .expect("the native transport supports this configuration");

    let started = Instant::now();
    let (kind, elapsed) = rt().block_on(guarded("a body that dribbles for ever", async {
        let mut resp = c
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("the head arrives promptly — this is not a stalled server");
        assert_eq!(resp.status(), 200);

        let mut last = None;
        while let Some(frame) = resp.chunk().await {
            match frame {
                Ok(_) => continue,
                Err(e) => {
                    last = Some(e);
                    break;
                }
            }
        }
        let e = last.expect("the body must end in an error, not in a clean EOF");
        (e, started.elapsed())
    }));

    assert_eq!(
        *kind.kind(),
        ErrorKind::Timeout(Phase::Total),
        "the failure must be typed as a total-timeout, not flattened into \
         ErrorKind::Body: {kind}"
    );
    assert!(
        kind.source()
            .and_then(|s| s.downcast_ref::<TotalTimeoutElapsed>())
            .is_some(),
        "the source must name the bound that was in force: {kind}"
    );
    assert!(
        elapsed >= TOTAL,
        "cut before the deadline it was given ({elapsed:?} < {TOTAL:?})"
    );
    assert!(
        elapsed < TOTAL * 10,
        "the bound is supposed to be tight, not eventual ({elapsed:?})"
    );
}

/// The half of the bound that did not exist before the `Deadline` wrapper
/// held a sleep of its own, and the reason this file grew a fourth server.
///
/// The head arrives in milliseconds, and then the server sends **nothing,
/// ever**, under a `Content-Length` of ten million. Nothing will wake the
/// body wrapper again, so an implementation that only checks elapsed time
/// on each `poll_frame` has no second chance to look at the clock: it
/// waits for a frame that never comes, and `GUARD` fires at six seconds
/// with the message that says so. Only a sleep the wrapper is holding —
/// registered on the caller's own waker while the body answers `Pending` —
/// can end this.
///
/// This case was written down as impossible, then as merely undecided. It
/// is neither, and this test is what makes the difference observable
/// rather than asserted.
#[test]
fn a_body_that_goes_silent_for_ever_after_the_head_is_cut_at_the_total_deadline() {
    let (addr, went_away) = head_then_silence_server(Duration::ZERO);
    let c = Client::builder(transport())
        .total_timeout(Tokio, TOTAL)
        .build()
        .expect("the native transport supports this configuration");

    let started = Instant::now();
    let (resp, err, elapsed) = rt().block_on(guarded("a body that goes silent for ever", async {
        let mut resp = c
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("the head arrives promptly — this is not a stalled server");
        assert_eq!(resp.status(), 200);

        let err = loop {
            match resp.chunk().await {
                Some(Ok(_)) => continue,
                Some(Err(e)) => break e,
                None => panic!(
                    "a body under a Content-Length of ten million must not end \
                     cleanly after zero bytes"
                ),
            }
        };
        (resp, err, started.elapsed())
    }));

    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::Total),
        "the failure must be typed as a total-timeout: {err}"
    );
    assert!(
        err.source()
            .and_then(|s| s.downcast_ref::<TotalTimeoutElapsed>())
            .is_some(),
        "the source must name the bound that was in force: {err}"
    );
    assert!(
        elapsed >= TOTAL,
        "cut before the deadline it was given ({elapsed:?} < {TOTAL:?})"
    );
    assert!(
        elapsed < TOTAL * 10,
        "a bound that fires only because something else woke the body is not \
         a bound ({elapsed:?})"
    );
    // The `Response` is still alive here on purpose, exactly as in the
    // test below: what closed the socket can then only be the wrapper
    // dropping the transport's body when its sleep fired.
    assert!(
        wait_for(&went_away, Duration::from_secs(10)),
        "the server never saw the connection close, so the timed-out \
         exchange was left running"
    );
    drop(resp);
}

/// The body does **not** get a budget of its own: the sleep runs for what
/// the head left over, not for the whole `total` a second time.
///
/// The head costs 600 ms of an 800 ms bound, and then the server goes
/// silent. A correct client cuts at ≈800 ms from the start; one whose body
/// sleeps for the full `total` cuts at ≈1400 ms, and both are "a bound
/// that fired" to any assertion loose enough to accept the first. Hence
/// the upper bound here is `TOTAL_SLOW + HEAD/2`, not the `TOTAL * 10` the
/// other tests can afford — this is the one place where a fifteen-hundred-
/// millisecond answer is wrong for a reason that has nothing to do with a
/// loaded machine.
#[test]
fn the_body_races_what_is_left_of_the_bound_rather_than_a_second_copy_of_it() {
    const HEAD: Duration = Duration::from_millis(600);
    const TOTAL_SLOW: Duration = Duration::from_millis(800);

    let (addr, _) = head_then_silence_server(HEAD);
    let c = Client::builder(transport())
        .total_timeout(Tokio, TOTAL_SLOW)
        .build()
        .expect("supported");

    let started = Instant::now();
    let (err, elapsed) = rt().block_on(guarded("a slow head then silence", async {
        let mut resp = c
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("the head arrives, late but inside the bound");
        let err = loop {
            match resp.chunk().await {
                Some(Ok(_)) => continue,
                Some(Err(e)) => break e,
                None => panic!("the body must not end cleanly"),
            }
        };
        (err, started.elapsed())
    }));

    assert_eq!(*err.kind(), ErrorKind::Timeout(Phase::Total), "{err}");
    assert!(
        elapsed >= TOTAL_SLOW,
        "cut before the deadline ({elapsed:?} < {TOTAL_SLOW:?})"
    );
    assert!(
        elapsed < TOTAL_SLOW + HEAD / 2,
        "the body was given a fresh {TOTAL_SLOW:?} of its own instead of \
         what the head left of it ({elapsed:?})"
    );
}

/// The other mechanism, in the one situation where it is the only one:
/// a body that never answers `Pending` at all.
///
/// The sleep is polled from the `Pending` branch and nowhere else, which
/// is correct — that is the branch that can be the last one — but it means
/// a body handing back frame after frame with no wait in between never
/// reaches it. An already-buffered response is exactly that shape, and
/// `MockTransport`'s bodies are exactly that shape. Only the elapsed-time
/// check before each poll can cut this one, and deleting that check makes
/// the whole response arrive intact.
///
/// `TestTimer`'s clock is the sum of the sleeps asked of it, so
/// `Client::execute`'s own `within` puts the virtual clock a full `total`
/// past the start before the body is ever touched. That is what makes the
/// deadline already expired here without a millisecond of real waiting —
/// a property of the fake clock, used deliberately, not a claim about a
/// real one.
#[cfg(feature = "test-util")]
#[test]
fn a_body_that_never_yields_is_cut_by_the_elapsed_check_alone() {
    use hclient::mock::{MockTransport, TestTimer};

    let m = MockTransport::new();
    m.push_response_frames(
        http::Response::builder()
            .status(200)
            .body(vec!["one", "two", "three"])
            .unwrap(),
    );
    let c = Client::builder(m)
        .total_timeout(TestTimer::new(), Duration::from_millis(1))
        .build()
        .expect("the mock supports this configuration");

    let err = futures_executor::block_on(async {
        let mut resp = c.get("https://a/x").send().await.expect("answered");
        assert_eq!(resp.status(), 200);
        loop {
            match resp.chunk().await {
                Some(Ok(_)) => continue,
                Some(Err(e)) => break Some(e),
                None => break None,
            }
        }
    });

    let err = err.expect(
        "an expired bound must cut a body that is already in memory too — \
         nothing will ever poll a sleep on its behalf",
    );
    assert_eq!(*err.kind(), ErrorKind::Timeout(Phase::Total), "{err}");
}

/// **The response body no longer crosses a `tokio::spawn`, and this test
/// is the record of what that cost and why it was paid.**
///
/// It used to assert the opposite, and the assertion was right for a
/// `Client` that named its transport: `Deadline` held a
/// `Pin<Box<Tm::Sleep>>`, a box around a *concrete* type, so auto traits
/// passed through and `hclient-native`'s bodies were `Send`.
///
/// Erasure ends it. One `ClientBody` has to serve every backend, and
/// `hclient-fetch`'s body holds a `dyn Stream` with no auto trait — so a
/// `Send` on the erased body does not weaken the browser backend, it
/// **excludes** it. Measured rather than argued: with `BoxBody` declared
/// `Send`, `cargo test -p hclient-fetch --target wasm32-unknown-unknown
/// --no-run` refuses `Client::builder(Fetch::new())` outright.
///
/// So: every backend can be a `Client`, and no response body can be
/// spawned. A caller who needs the second gets it by reaching past the
/// facade — `Client::transport_as::<Native<..>>()` hands back the concrete
/// transport, whose own bodies are unchanged.
///
/// The bound still works on a body held across an await; what it cannot do
/// is cross a thread. That is what this test now pins, with the
/// `compile_fail` for the negative living in `shape.rs` beside the rest of
/// the erasure's `Send` story.
#[test]
fn a_bounded_response_body_survives_being_held_across_an_await() {
    let addr = prompt_server();
    let c = Client::builder(transport())
        .total_timeout(Tokio, TOTAL)
        .build()
        .expect("supported");

    let body = rt().block_on(async {
        let resp = c
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("responds");
        // Held across a yield rather than moved to another task: the
        // `total_timeout` is set, so this really is a body carrying a live
        // sleep, not the inert `total: None` shape.
        tokio::task::yield_now().await;
        resp.collect()
            .await
            .expect("body collects inside the bound")
            .text()
            .expect("utf-8")
    });
    assert_eq!(body, "done");
}

/// Firing does not merely report: it drops the body, and dropping a body
/// is a cancellation under `Transport::execute`'s contract (v0.2 W1).
///
/// The `Response` is deliberately still alive while the assertion runs —
/// so what closes the socket can only be the wrapper having dropped the
/// transport's body when the deadline fired, not the response going out of
/// scope. Without that, this server would go on writing into a connection
/// nobody reads, and the flag would stay `false`.
#[test]
fn firing_drops_the_body_so_the_server_sees_the_connection_go_away() {
    let (addr, went_away) = dribbling_server();
    let c = Client::builder(transport())
        .total_timeout(Tokio, TOTAL)
        .build()
        .expect("supported");

    let resp = rt().block_on(guarded("firing drops the body", async {
        let mut resp = c.get(format!("http://{addr}/")).send().await.expect("head");
        while let Some(frame) = resp.chunk().await {
            if frame.is_err() {
                break;
            }
        }
        resp
    }));

    assert!(
        wait_for(&went_away, Duration::from_secs(10)),
        "the server never saw the connection close, so the timed-out \
         exchange was left running"
    );
    // Held to here on purpose: see the doc comment.
    drop(resp);
}

/// The other end of the same bound: a server that accepts and then answers
/// nothing at all. Here the deadline has to fire against a real sleep,
/// because no frame will ever arrive to be checked on — which is why
/// `Client::execute` races the head rather than only stamping the body.
#[test]
fn a_server_that_never_answers_is_cut_at_the_head() {
    let addr = silent_server();
    let c = Client::builder(transport())
        .total_timeout(Tokio, TOTAL)
        .build()
        .expect("supported");

    let started = Instant::now();
    let err = rt().block_on(guarded("a server that never answers", async {
        c.get(format!("http://{addr}/")).send().await.unwrap_err()
    }));
    let elapsed = started.elapsed();

    assert_eq!(*err.kind(), ErrorKind::Timeout(Phase::Total), "{err}");
    assert!(elapsed >= TOTAL, "{elapsed:?}");
    assert!(elapsed < TOTAL * 10, "{elapsed:?}");
}

/// The bound covers the OPERATION, not a hop — the property that decides
/// where this lives. A `tower` layer sits under `Client` and is entered
/// once per hop, so its clock would restart on each redirect and this
/// chain would never be cut.
///
/// Sensitive without hanging: a per-hop bound (or none) lets the default
/// ten-hop limit run out, and the error is then `ErrorKind::Redirect`
/// rather than `Timeout(Phase::Total)`. Each hop is well inside the bound
/// on its own, so nothing but the accumulated total can end it.
#[test]
fn the_deadline_spans_redirect_hops_rather_than_restarting_on_each() {
    let per_hop = Duration::from_millis(120);
    let (addr, hops) = redirect_chain_server(per_hop);
    let c = Client::builder(transport())
        .redirect(hclient::redirect::Limit::new(10))
        .total_timeout(Tokio, TOTAL)
        .build()
        .expect("supported");

    let err = rt().block_on(async { c.get(format!("http://{addr}/")).send().await.unwrap_err() });

    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::Total),
        "a per-hop bound would have run the chain to its ten-hop limit and \
         reported ErrorKind::Redirect instead: {err}"
    );
    let seen = hops.load(Ordering::SeqCst);
    assert!(
        (2..10).contains(&seen),
        "the test is only meaningful if several hops happened and the limit \
         was not the thing that stopped it; saw {seen}"
    );
}

/// The control. The same bound, a server that answers properly: the
/// response arrives whole and the deadline never fires. Without this, a
/// `total` implementation that failed every request would pass every other
/// test in this file.
#[test]
fn a_prompt_response_is_untouched_by_the_same_bound() {
    let addr = prompt_server();
    let c = Client::builder(transport())
        .total_timeout(Tokio, TOTAL)
        .build()
        .expect("supported");

    let body = rt().block_on(async {
        c.get(format!("http://{addr}/"))
            .send()
            .await
            .expect("responds")
            .collect()
            .await
            .expect("body collects inside the bound")
            .text()
            .expect("utf-8")
    });
    assert_eq!(body, "done");
}

/// And the same server with no bound at all: the wrapper is in the type
/// either way, and it must not invent a deadline of its own.
#[test]
fn a_client_with_no_total_is_not_bounded_by_the_wrapper_being_present() {
    let addr = prompt_server();
    let c = Client::builder(transport()).build().expect("supported");

    let (body, total) = rt().block_on(async {
        let resp = c
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("responds");
        // Two `into_inner()`s: the byte limit added outside everything,
        // then the decompression wrapper v0.2 W5 added outside this one.
        // The `Deadline` this file is about is inside `ClientBody`, which
        // forwards its two answers rather than being peeled — the chain
        // went private when the alias became a newtype, and the order is
        // pinned by a compile-time assertion in `src/client_body.rs`.
        let client_body = resp.into_parts().1;
        let total = client_body.total_timeout();
        (client_body, total)
    });
    assert_eq!(
        total, None,
        "an unconfigured client must carry no bound at all, not a large one"
    );
    assert!(!body.is_expired());
}

/// **`Client::new()`'s clock is a real one** — the half of the default
/// that no compile-shape test can reach.
///
/// `tests/deadline_client_type.rs` pins that `Client::new()?
/// .total_timeout(d)` is still a `Client` and that the value is stored.
/// Both of those stay true if `DefaultClock` were quietly `NoClock`: the
/// setter would compile, the number would be stored, and the timeout would
/// simply never fire. Only running one catches that, so this test does —
/// against a local server that answers nothing, over plain HTTP, so
/// nothing beyond loopback is involved.
///
/// It is what makes `Client::new()?.total_timeout(d)` a feature rather
/// than a well-typed no-op.
#[cfg(feature = "default-transport")]
#[test]
fn the_clock_client_new_carries_by_default_actually_fires() {
    let addr = silent_server();
    let c = Client::new()
        .expect("default transport supports the default config")
        .total_timeout(TOTAL);

    let started = Instant::now();
    let err = rt().block_on(guarded("the default clock", async {
        c.get(format!("http://{addr}/")).send().await.unwrap_err()
    }));

    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::Total),
        "the default clock must be able to measure, not merely to exist: {err}"
    );
    assert!(started.elapsed() >= TOTAL);
}

/// The race is decided in favour of the operation.
///
/// `within` polls the operation before the deadline, so a request that
/// finishes in the same wake as its deadline expiring is a success and not
/// a timeout. Nothing else in this file can see that: every other test
/// here has a bound that expires long before its server answers.
///
/// The setup makes the tie certain rather than likely — `TestTimer::sleep`
/// resolves immediately, so the deadline is already expired on the first
/// poll, and `MockTransport` answers on that same first poll. Reverse the
/// two polls in `within` and every request through this client becomes a
/// timeout.
#[cfg(feature = "test-util")]
#[test]
fn an_operation_that_finishes_in_the_same_wake_as_its_deadline_wins() {
    use hclient::mock::{MockTransport, TestTimer};

    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());
    let c = Client::builder(m)
        .total_timeout(TestTimer::new(), Duration::from_millis(1))
        .build()
        .expect("the mock supports this configuration");

    let resp = futures_executor::block_on(c.get("https://a/x").send())
        .expect("an answered request is not a timeout, however expired the clock");
    assert_eq!(resp.status(), 200);
}
