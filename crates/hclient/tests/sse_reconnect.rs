//! `Client::sse` — the reconnecting SSE stream.
//!
//! `#![cfg(feature = "test-util")]` — same reason as `tests/sse.rs`: this
//! file pulls in `hclient::mock::{MockTransport, TestTimer}`, which only
//! exist behind `test-util`.
//!
//! Every reconnecting test below supplies `TestTimer` to
//! `SseBuilder::with_timer` — it records the `Duration`s `sleep` was called
//! with and resolves immediately, so nothing here actually waits, no matter
//! what `Backoff` is configured. This is the same "controllable clock
//! instead of a real one" trick `hclient-native/src/connect.rs`'s `FakeRt`
//! already uses for connect-timeout tests; `TestTimer` is `hclient`'s own
//! instance of it, next to `MockTransport` in `mock.rs`, specifically
//! because an earlier version of this file needed a real ambient clock and
//! that design was reworked away (see the report).
#![cfg(feature = "test-util")]

use hclient::mock::{MockTransport, TestTimer};
use hclient::sse::SseEvent;
use hclient::sse::SseOptions;
use hclient::{Client, ErrorKind};
use hclient_proto::backoff::Backoff;
use std::time::Duration;

fn sse(body: &'static str) -> http::Response<&'static str> {
    http::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}

/// A small, easy-to-read `Backoff` for tests that need a bounded
/// `max_attempts` — readability, not speed: `TestTimer` never actually
/// waits, so the actual `Duration` values here don't affect how long any
/// test takes to run.
fn bounded_backoff() -> Backoff {
    Backoff {
        base: Duration::from_millis(1),
        max: Duration::from_millis(20),
        max_attempts: Some(5),
    }
}

fn bounded_options() -> SseOptions {
    SseOptions {
        backoff: bounded_backoff(),
        ..Default::default()
    }
}

#[test]
fn reconnects_and_sends_last_event_id() {
    let m = MockTransport::new();
    m.push_response(sse("id: 7\ndata: first\n\n")); // stream breaks on clean EOF
    m.push_response(sse("data: second\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(bounded_options())
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();

    let mut got = Vec::new();
    for _ in 0..2 {
        if let Some(Ok(e)) = futures_executor::block_on(s.next()) {
            got.push(e)
        }
    }
    assert_eq!(
        got.len(),
        2,
        "both events, across the reconnect, must surface"
    );
    assert_eq!(
        got[0],
        SseEvent::Message {
            event: None,
            data: "first".into(),
            id: Some("7".into())
        }
    );
    assert_eq!(
        got[1],
        SseEvent::Message {
            event: None,
            data: "second".into(),
            id: Some("7".into())
        },
        "id persists across a reconnect until a new one is seen"
    );

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(seen.len(), 2, "the second request is the reconnect");
    assert_eq!(
        seen[1].headers.get("last-event-id").unwrap(),
        "7",
        "the reconnect must fill in the last id"
    );
    assert!(
        seen[0].headers.get("last-event-id").is_none(),
        "there's no id yet on the first request — can't send an empty one"
    );
}

#[test]
fn stops_forever_on_204() {
    let m = MockTransport::new();
    m.push_response(sse("data: x\n\n"));
    m.push_response(
        http::Response::builder()
            .status(204)
            .header("content-type", "text/event-stream")
            .body("")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(bounded_options())
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();
    while futures_executor::block_on(s.next()).is_some() {}

    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        2,
        "204 means \"stop,\" not \"try again\" — exactly one reconnect attempt, never a third"
    );
}

#[test]
fn a_204_reconnect_surfaces_one_terminal_error_not_a_silent_stop() {
    // Same scenario as `stops_forever_on_204`, but pins WHAT `next()` hands
    // back, not just how many requests went out: "no silent no-ops" — the
    // caller must be able to see that the stream ended because the server
    // said stop, not guess from the outside.
    let m = MockTransport::new();
    m.push_response(sse("data: x\n\n"));
    m.push_response(
        http::Response::builder()
            .status(204)
            .header("content-type", "text/event-stream")
            .body("")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(bounded_options())
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();

    let first = futures_executor::block_on(s.next()).unwrap().unwrap();
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "x".into(),
            id: None
        }
    );

    let second = futures_executor::block_on(s.next())
        .expect("the terminal status must surface as an item, not vanish");
    assert!(
        second.is_err(),
        "a non-200 (204 included) is a stop, not a success"
    );
    assert_eq!(second.unwrap_err().kind(), &ErrorKind::Status);

    assert!(futures_executor::block_on(s.next()).is_none());
    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "terminal must mean forever"
    );
}

#[test]
fn honours_server_sent_retry_over_the_policy() {
    // The server's `retry: 1000` (1s) replaces the configured
    // `Backoff::base` (10s) — checked two ways: the events still all
    // arrive (behavioral), AND `TestTimer` recorded a delay no larger than
    // the server's 1s rather than the policy's 10s (exact — `Backoff::
    // delay` only ever REDUCES via jitter, never grows past its base, so
    // this bound holds regardless of the actual jitter draw).
    //
    // `1000`, not `1` (review round 1, Minor-2): with a 1ms server-supplied
    // base the lower-bound check below could flake at roughly 1-in-10^6 —
    // `Backoff`'s fixed-point jitter quantization floors a 1ms base to
    // exactly `Duration::ZERO` for any jitter draw past ≈0.9999995
    // (measured against this toolchain: `Backoff{base:1ms,..}.delay(0,
    // 0.9999995) == Some(Duration::ZERO)`), which is common enough to hit
    // in CI over time. A 1s base pushes that same floor-to-zero event down
    // to roughly 1-in-10^9 — see the lower-bound assertion's own comment
    // for the (now honestly stated, not claimed-impossible) residual.
    let m = MockTransport::new();
    m.push_response(sse("retry: 1000\ndata: x\n\n"));
    m.push_response(sse("data: y\n\n"));
    m.push_response(sse("data: z\n\n"));

    let c = Client::builder(m).build().unwrap();
    let slow_base_but_server_overrides = SseOptions {
        backoff: Backoff {
            base: Duration::from_secs(10),
            max: Duration::from_secs(10),
            max_attempts: Some(5),
        },
        ..Default::default()
    };
    let timer = TestTimer::new();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(slow_base_but_server_overrides)
            .with_timer(timer.clone())
            .connect(),
    )
    .unwrap();

    // `next()` also yields the `Retry` directive itself as a real item
    // (first-class, per the design doc — same treatment as `Comment`), so
    // a plain "collect N items" loop would count it as one of the three
    // and never actually reach `data: z`. Filter to `Message` events
    // specifically, and bound the iteration count generously (not
    // unboundedly) so a real regression fails instead of hanging.
    let mut messages = Vec::new();
    for _ in 0..8 {
        if messages.len() == 3 {
            break;
        }
        if let Some(Ok(SseEvent::Message { data, .. })) = futures_executor::block_on(s.next()) {
            messages.push(data);
        }
    }
    assert_eq!(
        messages,
        vec!["x".to_string(), "y".to_string(), "z".to_string()],
        "all three messages, across two reconnects, must arrive"
    );

    let sleeps = timer.sleeps();
    assert_eq!(sleeps.len(), 2, "exactly two reconnects happened");
    for d in sleeps {
        assert!(
            d <= Duration::from_secs(1),
            "recorded sleep {d:?} exceeds the server's 1000ms retry — the \
             configured 10s base leaked through instead of being replaced"
        );
        // Lower bound too, not just upper — catches a wiring bug that
        // calls `sleep` with a hardcoded zero (which would trivially
        // satisfy the upper-bound check above too) instead of the
        // actually-computed delay.
        //
        // NOT actually impossible, and this comment used to claim
        // otherwise (review round 1, Minor-2): `jitter()` is documented
        // (see its doc comment in `sse.rs`) to be able to return exactly
        // `1.0`, at which point `Backoff::delay` returns exactly
        // `Duration::ZERO` regardless of `base` — and even short of that,
        // `Backoff`'s fixed-point jitter quantization floors a small-
        // enough scaled base to `Duration::ZERO` on its own for a jitter
        // draw close enough to (but short of) `1.0`. Both are real,
        // measured floors, not eliminated by this fix — only pushed far
        // enough out (the 1s base here puts the combined probability
        // around 1-in-10^9 per sleep) that a real CI run flaking on it is
        // not a realistic outcome, unlike the 1ms base this test used to
        // configure.
        assert!(
            d > Duration::ZERO,
            "recorded sleep was exactly zero — sleep() was likely called \
             with a hardcoded value instead of the computed delay"
        );
    }
}

#[test]
fn oversized_event_is_fatal_and_not_retried() {
    let m = MockTransport::new();
    let big = "data: 0123456789abcdefghijklmnop\n\n";
    m.push_response(sse(big));
    m.push_response(sse("data: never\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(SseOptions {
                max_event_size: 8,
                ..bounded_options()
            })
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();

    let err = futures_executor::block_on(s.next())
        .expect("the size violation must surface")
        .expect_err("it's an error, not a value");
    assert_eq!(
        err.kind(),
        &ErrorKind::Decode,
        "EventTooLarge is a decode-layer fatality"
    );
    assert!(futures_executor::block_on(s.next()).is_none());
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1,
        "reconnecting after a fatal decode error is forbidden"
    );
}

#[test]
fn a_transient_body_error_is_retried_transparently_without_surfacing_as_an_event() {
    // `ErrorKind::Body` (a foreign, uncategorized body-read failure — the
    // MockTransport equivalent of a connection reset) must be retried
    // silently: a caller iterating `next()` for `SseEvent`s should not see
    // a spurious `Err` for a hiccup that got automatically recovered from.
    let m = MockTransport::new();
    m.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: a\n\n"])
            .unwrap(),
        hclient::Error::new(ErrorKind::Body, std::io::Error::other("connection reset")),
    );
    m.push_response(sse("data: b\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(bounded_options())
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();

    let mut got = Vec::new();
    for _ in 0..2 {
        let item = futures_executor::block_on(s.next());
        assert!(
            matches!(item, Some(Ok(_))),
            "a retryable failure must never reach the caller as Err: got {item:?}"
        );
        if let Some(Ok(e)) = item {
            got.push(e);
        }
    }
    assert_eq!(got.len(), 2);
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        2,
        "one reconnect happened"
    );
}

#[test]
fn connect_without_with_timer_makes_exactly_one_attempt_regardless_of_error_kind() {
    // Without `with_timer`, `SseBuilder::connect` returns a plain
    // `SseStream` (vertical 1's type, not a reconnecting wrapper at all) —
    // this checks the WIRING through `Client::sse` -> `open` -> a single
    // `Client::execute` call behaves like `SseStream::new`'s own contract
    // (a failed construction is `Err`, full stop), independent of
    // `is_retryable`'s classification: even a kind that WOULD be silently
    // retried by the reconnecting variant (`Body`, here) still fails this
    // plain `connect()` outright, because there is no timer to wait a
    // backoff delay with and therefore no reconnect loop at all.
    let m = MockTransport::new();
    m.push_transport_error(hclient::Error::new(
        ErrorKind::Body,
        std::io::Error::other("down before the first byte"),
    ));

    let c = Client::builder(m).build().unwrap();
    let err = futures_executor::block_on(c.sse("https://a/stream").connect())
        .expect_err("connect() must fail outright on the very first attempt, no retry");
    assert_eq!(err.kind(), &ErrorKind::Body);
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1,
        "no retry was attempted — there is no timer to retry with"
    );
}

/// Review round 1, Important-1: `ReconnectingSseBuilder::connect`'s own doc
/// comment states this exact contract ("makes exactly one connection
/// attempt — no retry, regardless of whether the failure would later be
/// classified as retryable"), but nothing exercised it — the test above
/// drives the *plain* `SseBuilder::connect`, a different function on a
/// different type, and the reviewer showed (mutation M19) that rewriting
/// `ReconnectingSseBuilder::connect` to enter the reconnect loop on a
/// retryable first-connect failure instead of returning `Err` left the
/// entire workspace suite green. This test drives the type that doc
/// comment is actually on: `.with_timer(..)` is supplied, and the initial
/// failure (`Body`, retryable once a stream is already live — see
/// `a_transient_body_error_is_retried_transparently_...` above) must STILL
/// fail the very first connection outright, with no reconnect attempt,
/// because `connect` is a single try before any `ReconnectingSseStream`
/// — with its own `attempt`/backoff state — exists to retry with at all.
#[test]
fn with_timer_connect_also_makes_exactly_one_attempt_regardless_of_error_kind() {
    let m = MockTransport::new();
    m.push_transport_error(hclient::Error::new(
        ErrorKind::Body,
        std::io::Error::other("down before the first byte"),
    ));

    let c = Client::builder(m).build().unwrap();
    let err = futures_executor::block_on(
        c.sse("https://a/stream")
            .with_timer(TestTimer::new())
            .connect(),
    )
    .expect_err("with_timer(..).connect() must also fail outright on the first attempt");
    assert_eq!(err.kind(), &ErrorKind::Body);
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1,
        "no reconnect attempt for a failure on the INITIAL connection — \
         the reconnect loop only exists once a stream has been live at least once"
    );
}

#[test]
fn an_unsupported_capability_error_is_terminal_not_retried() {
    // `ErrorKind::Unsupported` means the backend fundamentally can't do
    // something the client asked for — waiting and asking again changes
    // nothing. Distinguishing this from `Body`
    // (`a_transient_body_error_is_retried_transparently_...` above) is the
    // point: not every non-decode error is retryable, and not every error
    // is terminal — the classification is per-kind. Triggered MID-STREAM
    // (after a successful connect through `with_timer`), not at the
    // initial connection: the initial connection fails outright on ANY
    // kind regardless of classification (see the plain-`connect()` test
    // above), so only a mid-stream failure actually exercises
    // `is_retryable`.
    let m = MockTransport::new();
    m.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: a\n\n"])
            .unwrap(),
        hclient::Error::new(
            ErrorKind::Unsupported,
            std::io::Error::other("capability not available"),
        ),
    );
    m.push_response(sse("data: never\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(bounded_options())
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();

    let first = futures_executor::block_on(s.next()).unwrap().unwrap();
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "a".into(),
            id: None
        }
    );

    let second = futures_executor::block_on(s.next())
        .expect("a terminal error must surface as an item, not vanish");
    let err = second.expect_err("Unsupported must not be silently retried");
    assert_eq!(err.kind(), &ErrorKind::Unsupported);

    assert!(futures_executor::block_on(s.next()).is_none());
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1,
        "no reconnect attempt for a terminal kind — the second queued response is never touched"
    );
}

#[test]
fn gives_up_after_max_attempts_with_one_distinguishable_error_not_silence() {
    let m = MockTransport::new();
    m.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: a\n\n"])
            .unwrap(),
        hclient::Error::new(ErrorKind::Body, std::io::Error::other("down")),
    );
    // No further response queued: every reconnect attempt after the first
    // failure hits `MockTransport`'s empty-queue error, `ErrorKind::Other`,
    // which is retryable, so the backoff runs out of `max_attempts` rather
    // than hitting a terminal kind.

    let c = Client::builder(m).build().unwrap();
    let opts = SseOptions {
        backoff: Backoff {
            base: Duration::from_millis(1),
            max: Duration::from_millis(5),
            max_attempts: Some(2),
        },
        ..Default::default()
    };
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(opts)
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();

    let first = futures_executor::block_on(s.next()).unwrap().unwrap();
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "a".into(),
            id: None
        }
    );

    // The stream must eventually give up and say so exactly once — not
    // hang, not silently return None with no explanation.
    let mut gave_up = None;
    for _ in 0..2 {
        match futures_executor::block_on(s.next()) {
            Some(Err(e)) => {
                gave_up = Some(e);
                break;
            }
            Some(Ok(e)) => panic!("unexpected extra event: {e:?}"),
            None => {}
        }
    }
    let e = gave_up.expect("giving up must be an observable Err, not a quiet None");
    assert!(
        std::error::Error::source(&e)
            .expect("Error::new always sets a source")
            .to_string()
            .to_lowercase()
            .contains("attempt"),
        "the error must say it's a gave-up-retrying condition"
    );

    assert!(futures_executor::block_on(s.next()).is_none());
    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "exhaustion is forever, not a one-shot glitch"
    );
}

#[test]
fn connect_without_with_timer_returns_a_plain_non_reconnecting_stream() {
    // No runtime flag controls this anymore (an earlier version had
    // `SseOptions::reconnect: bool`) — whether the stream reconnects is a
    // type-level fact now, decided solely by whether `with_timer` was
    // called. This test exercises the "not called" side: `Client::sse(url)
    // .connect()` must behave exactly like `SseStream`'s own vertical-1
    // contract (already covered in depth by `tests/sse.rs`) — a clean EOF
    // just ends the stream, no error, no reconnect attempt.
    let m = MockTransport::new();
    m.push_response(sse("data: only\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(c.sse("https://a/stream").connect()).unwrap();

    let first = futures_executor::block_on(s.next()).unwrap().unwrap();
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "only".into(),
            id: None
        }
    );
    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "a clean EOF with no timer supplied just ends the stream, no error"
    );
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1,
        "without with_timer there is no reconnect attempt at all"
    );
}

#[test]
fn header_carries_a_custom_header_across_every_reconnect() {
    let m = MockTransport::new();
    m.push_response(sse("data: first\n\n"));
    m.push_response(sse("data: second\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .header("x-api-key", "secret")
            .options(bounded_options())
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();

    for _ in 0..2 {
        futures_executor::block_on(s.next());
    }

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(seen.len(), 2);
    for r in &seen {
        assert_eq!(
            r.headers.get("x-api-key").unwrap(),
            "secret",
            "a caller-supplied header must survive every reconnect, not just the first request"
        );
    }
}

/// Review round 1, Important-2: `SseBuilder::error` exists specifically so
/// an invalid `(name, value)` pair passed to `.header(..)` becomes an `Err`
/// from `connect()` rather than the brief's own reference code, which
/// silently dropped it — the project's headline "no silent no-ops" rule,
/// applied here. Nothing exercised it (mutation M20: reverting `header` to
/// silently drop an invalid pair left the whole suite green). No response
/// is queued on the mock transport for either half of this test — the
/// error must short-circuit `connect()` before any request is even
/// attempted, so `requests().len() == 0` is exactly as load-bearing as the
/// `Err` itself: a version that queued the invalid header but still sent
/// the request would fail differently (a real request going out with a
/// dropped header) and this assertion is what would catch that.
#[test]
fn an_invalid_header_is_an_error_not_a_silently_dropped_pair() {
    let m = MockTransport::new();
    let c = Client::builder(m).build().unwrap();

    let err =
        futures_executor::block_on(c.sse("https://a/stream").header("bad name", "v").connect())
            .expect_err("an invalid header name must fail connect(), not be silently dropped");
    assert_eq!(err.kind(), &ErrorKind::Other);
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        0,
        "the invalid header must short-circuit before any request is sent"
    );
}

/// The FIRST invalid pair wins and survives later calls — the contract
/// `SseBuilder::header`'s doc comment states, held by its
/// `if self.error.is_some() { return self; }` guard and pinned by nothing
/// until now.
///
/// The two failures have to be distinguishable for this test to say
/// anything, so it uses an invalid header *name* first and an invalid
/// header *value* second: `http` reports those differently. Deleting the
/// guard lets the second overwrite the first, and the assertion below
/// fails.
#[test]
fn the_first_invalid_header_wins_and_later_ones_do_not_overwrite_it() {
    let m = MockTransport::new();
    let c = Client::builder(m).build().unwrap();

    let err = futures_executor::block_on(
        c.sse("https://a/stream")
            .header("bad name", "fine")
            .header("x-fine", "bad\nvalue")
            .connect(),
    )
    .expect_err("two invalid headers must still fail connect()");

    let msg = err.to_string();
    assert!(
        msg.contains("header name"),
        "the first failure (an invalid NAME) must be the one reported, not the later invalid value: {msg}"
    );
}

/// The same mechanism, through the reconnecting builder — `SseBuilder::
/// error` is shared state consulted by BOTH `SseBuilder::connect` and
/// `ReconnectingSseBuilder::connect` (the latter via `self.builder.error`),
/// and only the first was ever tested.
#[test]
fn an_invalid_header_is_an_error_through_with_timer_too() {
    let m = MockTransport::new();
    let c = Client::builder(m).build().unwrap();

    let err = futures_executor::block_on(
        c.sse("https://a/stream")
            .header("bad name", "v")
            .with_timer(TestTimer::new())
            .connect(),
    )
    .expect_err("an invalid header name must fail with_timer(..).connect() too");
    assert_eq!(err.kind(), &ErrorKind::Other);
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        0,
        "the invalid header must short-circuit before any request is sent"
    );
}

#[test]
fn an_explicit_empty_id_clears_last_event_id_and_no_header_is_sent_on_reconnect() {
    // WHATWG: an `id:` field with an EMPTY value clears the last event ID
    // buffer — that's a real, observable state ("no id, on purpose"), not
    // the same as "no `id:` field was ever seen" even though both cases
    // agree on one externally visible thing: neither sends a
    // `Last-Event-ID` header (empty headers are forbidden by the spec, and
    // there's nothing to send for "never seen" either). First get a real
    // id, then explicitly clear it, THEN break the connection — the
    // reconnect must not resurrect the old id.
    //
    // `push_response_frames`, not `push_response`: `SseDecoder::push` is
    // eager — fed the "first" and "second" blocks as ONE chunk, it would
    // process (and dispatch) both in a single `push` call, so
    // `last_event_id()` read right after `next()` returns "first" would
    // already reflect the SECOND block's clearing `id:` line, one that
    // hasn't even been returned to the caller yet. That's a property of
    // batching a whole multi-event body into one chunk (pre-existing,
    // orthogonal to reconnect — a real connection delivers bytes
    // incrementally as they arrive, not as one pre-assembled blob), not a
    // reconnect defect, and separate frames are what actually match that
    // real incremental delivery.
    let m = MockTransport::new();
    m.push_response_frames(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["id: 7\ndata: first\n\n", "id:\ndata: second\n\n"])
            .unwrap(),
    );
    m.push_response(sse("data: third\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(bounded_options())
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();

    let first = futures_executor::block_on(s.next()).unwrap().unwrap();
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "first".into(),
            id: Some("7".into())
        }
    );
    assert_eq!(s.last_event_id(), Some("7"));

    let second = futures_executor::block_on(s.next()).unwrap().unwrap();
    assert_eq!(
        second,
        SseEvent::Message {
            event: None,
            data: "second".into(),
            id: Some("".into())
        },
        "an id: field with an empty value clears it — Some(\"\"), not Some(\"7\")"
    );
    assert_eq!(
        s.last_event_id(),
        Some(""),
        "cleared, not unset — distinct from never having seen an id at all"
    );

    // Clean EOF -> reconnect.
    let third = futures_executor::block_on(s.next()).unwrap().unwrap();
    assert_eq!(
        third,
        SseEvent::Message {
            event: None,
            data: "third".into(),
            // The cleared id (`Some("")`) carries forward across the
            // reconnect for a message the new connection doesn't set one
            // on — NOT the stale "7", but also not `None`: "cleared" and
            // "never seen" stay distinct states even across a reconnect.
            id: Some("".into())
        },
    );

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(seen.len(), 2, "one reconnect happened");
    assert!(
        seen[1].headers.get("last-event-id").is_none(),
        "the cleared (empty) id must not be sent as Last-Event-ID — an \
         empty header is exactly what WHATWG forbids, and what \
         reqwest-eventsource gets wrong"
    );
}

/// Review round 1, Minor-4: WHATWG requires `EventSource` to send
/// `Accept: text/event-stream` — a content-negotiating server without it
/// could answer with something else entirely and there would be no way to
/// tell the server "no, specifically this." Deleting the line that sets it
/// (`open`, `sse.rs`) left the whole suite green (mutation M21); nothing
/// looked at the header before this test.
#[test]
fn accept_header_is_sent_and_survives_a_reconnect() {
    let m = MockTransport::new();
    m.push_response(sse("data: first\n\n"));
    m.push_response(sse("data: second\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(bounded_options())
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();

    for _ in 0..2 {
        futures_executor::block_on(s.next());
    }

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(seen.len(), 2, "one reconnect happened");
    for (i, r) in seen.iter().enumerate() {
        assert_eq!(
            r.headers.get("accept").map(|v| v.to_str().unwrap()),
            Some("text/event-stream"),
            "request {i} is missing Accept: text/event-stream"
        );
    }
}

/// Review round 1, Minor-5: nothing pinned that `Client::sse` actually
/// resolves its URL against a configured `base_url` — `effective_uri`
/// itself is well covered (`tests/base_url.rs`), but replacing `open`'s
/// `effective_uri(client.config().base_url.as_ref(), url)` with
/// `effective_uri(None, url)` left the whole suite green (mutation M22):
/// nothing routes an SSE connection through the client's base at all in
/// any existing test, since every other test in this file passes an
/// already-absolute URL. A relative path against a configured base is
/// exactly the case a caller who set `base_url` expects to work.
///
/// **Deliberately a MULTI-segment relative path (`"a/events"`), not a
/// single bare segment (`"events"`).** The first version of this fix used
/// `"events"` and did NOT catch the mutation it was written for, found by
/// actually re-running M22, not by reasoning: `"events"` alone happens to
/// parse as a valid (if unusual) bare `http::Uri` — a path-only URI with
/// no scheme or authority — even with no base at all. So under the
/// mutation, `open`'s own resolution silently "succeeds" (producing that
/// bare-path URI), the request still goes out, and `Client::execute`'s
/// OWN, entirely separate `effective_uri` call (`client.rs`, applied to
/// every request as a second, redundant safety net) re-resolves it against
/// the SAME base anyway — masking `open`'s mutated line completely. A
/// multi-segment reference like `"a/events"` does NOT parse as a bare
/// `http::Uri` at all (confirmed: `"a/events".parse::<http::Uri>()` is
/// `Err`, the same as the crate's own documented `"v1/things"` example) —
/// so under the mutation, `open`'s own now-baseless resolution fails
/// immediately with `InvalidUri`, `connect()` returns `Err`, and this
/// test's `.unwrap()` panics, which a passing run does not. This is what
/// actually distinguishes "`open` applies the base" from "`Client::
/// execute` quietly compensates regardless" — re-verified by re-running
/// M22 against this version specifically.
#[test]
fn client_sse_resolves_a_relative_url_against_the_configured_base() {
    let m = MockTransport::new();
    m.push_response(sse("data: hello\n\n"));

    let c = Client::builder(m)
        .base_url("https://api.example/v1/".parse().unwrap())
        .build()
        .unwrap();
    let mut s = futures_executor::block_on(c.sse("a/events").connect()).unwrap();

    let first = futures_executor::block_on(s.next()).unwrap().unwrap();
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "hello".into(),
            id: None
        }
    );

    let seen = c
        .transport_as::<MockTransport>()
        .expect("the mock")
        .requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].uri,
        "https://api.example/v1/a/events"
            .parse::<http::Uri>()
            .unwrap(),
        "a relative SSE URL must resolve against the client's base_url, \
         the same RFC 3986 §5 rule every other Client::request path already follows"
    );
}

/// Review round 1, Minor-6: every response queued anywhere else in this
/// file carries a correct `Content-Type`, so only the `Status` branch of
/// `validate_sse_response` was ever exercised THROUGH the reconnect path —
/// a wrong `Content-Type` was only ever tested on the very first
/// connection (`tests/sse.rs`'s vertical-1 coverage), never on a reconnect
/// specifically. The classification (`Decode`, terminal) is shared code
/// with the already-tested oversized-event case, so the risk was graded
/// low — closing it anyway, since it is cheap and was a genuine, named gap.
#[test]
fn a_wrong_content_type_on_a_reconnect_is_terminal_not_retried() {
    let m = MockTransport::new();
    m.push_response(sse("data: first\n\n")); // clean EOF -> reconnect
    m.push_response(
        http::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body("{}")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(bounded_options())
            .with_timer(TestTimer::new())
            .connect(),
    )
    .unwrap();

    let first = futures_executor::block_on(s.next()).unwrap().unwrap();
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "first".into(),
            id: None
        }
    );

    let second = futures_executor::block_on(s.next())
        .expect("a wrong Content-Type on the reconnect must surface as an item");
    let err = second.expect_err("application/json is not an SSE stream");
    assert_eq!(err.kind(), &ErrorKind::Decode);

    assert!(futures_executor::block_on(s.next()).is_none());
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        2,
        "no further reconnect attempt after the terminal content-type rejection"
    );
}
