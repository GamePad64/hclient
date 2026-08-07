//! `Client::sse` — the reconnecting SSE stream.
//!
//! `#![cfg(feature = "test-util")]` — same reason as `tests/sse.rs`: this
//! file pulls in `http_ng::mock::MockTransport`, which only exists behind
//! `test-util`.
//!
//! Every backoff here uses a millisecond-scale `Backoff` (`fast_backoff`
//! below), not the library's 1s/30s production default: the ambient clock
//! (`AmbientClock`, `src/clock.rs`) really sleeps a background thread, and a
//! test suite that waited out real 1s-30s delays per reconnect would be the
//! exact "unbounded wait" class of defect this vertical's own CI discipline
//! polices (`http-ng-native/src/connect.rs`'s `FakeRt` sidesteps this with a
//! virtual clock instead — not available here, since the ambient clock is
//! deliberately NOT injectable from outside the crate, see the report for
//! why). Every test below still finishes in well under a second of real
//! wall-clock time.
#![cfg(feature = "test-util")]

use http_ng::mock::MockTransport;
use http_ng::{Client, ErrorKind, SseEvent, SseOptions};
use http_ng_proto::backoff::Backoff;
use std::time::Duration;

fn sse(body: &'static str) -> http::Response<&'static str> {
    http::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}

/// A backoff that resolves in low single-digit milliseconds even at
/// `max_attempts`, so the reconnect tests below stay fast without needing a
/// fake/injectable clock.
fn fast_backoff() -> Backoff {
    Backoff {
        base: Duration::from_millis(1),
        max: Duration::from_millis(20),
        max_attempts: Some(5),
    }
}

fn fast_options() -> SseOptions {
    SseOptions {
        backoff: fast_backoff(),
        ..Default::default()
    }
}

#[test]
fn reconnects_and_sends_last_event_id() {
    let m = MockTransport::new();
    m.push_response(sse("id: 7\ndata: first\n\n")); // stream breaks on clean EOF
    m.push_response(sse("data: second\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s =
        futures_executor::block_on(c.sse("https://a/stream").options(fast_options()).connect())
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

    let seen = c.transport().requests();
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
    let mut s =
        futures_executor::block_on(c.sse("https://a/stream").options(fast_options()).connect())
            .unwrap();
    while futures_executor::block_on(s.next()).is_some() {}

    assert_eq!(
        c.transport().requests().len(),
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
    let mut s =
        futures_executor::block_on(c.sse("https://a/stream").options(fast_options()).connect())
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
    // The server's `retry: 1` (1ms) replaces the configured `Backoff::base`
    // (which here is deliberately set much larger — 10s — specifically so
    // this test can tell "the server's value was used" apart from "the
    // policy's base was used anyway": if the policy's 10s base leaked
    // through, this test would time out / not finish, rather than silently
    // pass. See the report for why "replaces the base" was chosen over
    // "caps it" or "ignored".
    let m = MockTransport::new();
    m.push_response(sse("retry: 1\ndata: x\n\n"));
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
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(slow_base_but_server_overrides)
            .connect(),
    )
    .unwrap();

    // `next()` also yields the `Retry` directive itself as a real item
    // (first-class, per the design doc — same treatment as `Comment`), so
    // a plain "collect N items" loop would count it as one of the three
    // and never actually reach `data: z` — passing for the wrong reason,
    // without ever exercising the third reconnect. Filter to `Message`
    // events specifically, and bound the iteration count generously (not
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
        "all three messages, across two reconnects, must arrive quickly — \
         a 10s base leaking through would blow the test's real budget \
         and this loop's 8-iteration bound"
    );
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
                ..fast_options()
            })
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
        c.transport().requests().len(),
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
        http_ng::Error::new(ErrorKind::Body, std::io::Error::other("connection reset")),
    );
    m.push_response(sse("data: b\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s =
        futures_executor::block_on(c.sse("https://a/stream").options(fast_options()).connect())
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
    assert_eq!(c.transport().requests().len(), 2, "one reconnect happened");
}

#[test]
fn connect_makes_exactly_one_attempt_regardless_of_error_kind() {
    // The VERY FIRST connection is a decision the caller is actively
    // waiting on, not a background retry loop that hasn't started yet —
    // `SseBuilder::connect` mirrors `SseStream::new`'s own contract (a
    // failed construction is `Err`, full stop). This holds independent of
    // `is_retryable`'s classification: even a kind that would be silently
    // retried mid-stream (`Body`, here) still fails `connect()` outright.
    let m = MockTransport::new();
    m.push_transport_error(http_ng::Error::new(
        ErrorKind::Body,
        std::io::Error::other("down before the first byte"),
    ));

    let c = Client::builder(m).build().unwrap();
    let err =
        futures_executor::block_on(c.sse("https://a/stream").options(fast_options()).connect())
            .expect_err("connect() must fail outright on the very first attempt, no retry");
    assert_eq!(err.kind(), &ErrorKind::Body);
    assert_eq!(
        c.transport().requests().len(),
        1,
        "no retry was attempted for the initial connection"
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
    // (after a successful connect), not at `connect()` itself: `connect()`
    // fails outright on ANY kind regardless of classification (see
    // `connect_makes_exactly_one_attempt_regardless_of_error_kind` above),
    // so only a mid-stream failure actually exercises `is_retryable`.
    let m = MockTransport::new();
    m.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: a\n\n"])
            .unwrap(),
        http_ng::Error::new(
            ErrorKind::Unsupported,
            std::io::Error::other("capability not available"),
        ),
    );
    m.push_response(sse("data: never\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s =
        futures_executor::block_on(c.sse("https://a/stream").options(fast_options()).connect())
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
        c.transport().requests().len(),
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
        http_ng::Error::new(ErrorKind::Body, std::io::Error::other("down")),
    );
    // Only ONE more response queued after the first failure — every
    // reconnect attempt after that hits `MockTransport`'s empty-queue
    // error, `ErrorKind::Other`, which is retryable, so the backoff runs
    // out of `max_attempts` rather than hitting a terminal kind.

    let c = Client::builder(m).build().unwrap();
    let opts = SseOptions {
        backoff: Backoff {
            base: Duration::from_millis(1),
            max: Duration::from_millis(5),
            max_attempts: Some(2),
        },
        ..Default::default()
    };
    let mut s =
        futures_executor::block_on(c.sse("https://a/stream").options(opts).connect()).unwrap();

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
fn reconnect_false_behaves_like_the_plain_non_reconnecting_stream() {
    let m = MockTransport::new();
    m.push_response(sse("data: only\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream")
            .options(SseOptions {
                reconnect: false,
                ..fast_options()
            })
            .connect(),
    )
    .unwrap();

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
        "a clean EOF with reconnect disabled just ends the stream, no error"
    );
    assert_eq!(
        c.transport().requests().len(),
        1,
        "reconnect: false must mean zero reconnect attempts"
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
            .options(fast_options())
            .connect(),
    )
    .unwrap();

    for _ in 0..2 {
        futures_executor::block_on(s.next());
    }

    let seen = c.transport().requests();
    assert_eq!(seen.len(), 2);
    for r in &seen {
        assert_eq!(
            r.headers.get("x-api-key").unwrap(),
            "secret",
            "a caller-supplied header must survive every reconnect, not just the first request"
        );
    }
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
    let mut s =
        futures_executor::block_on(c.sse("https://a/stream").options(fast_options()).connect())
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

    let seen = c.transport().requests();
    assert_eq!(seen.len(), 2, "one reconnect happened");
    assert!(
        seen[1].headers.get("last-event-id").is_none(),
        "the cleared (empty) id must not be sent as Last-Event-ID — an \
         empty header is exactly what WHATWG forbids, and what \
         reqwest-eventsource gets wrong"
    );
}
