//! `RetryPolicy::decide` is a pure function, so every rule in it is
//! testable with no socket, no clock and no entropy — which is the whole
//! reason the policy lives in the sans-io crate and the sleeping lives in
//! `hclient::Client`.

use core::time::Duration;

use hclient_proto::backoff::Backoff;
use hclient_proto::retry::{
    Outcome, RetryPolicy, RetryStatuses, Stop, Verdict, retry_after_seconds,
};

fn status(code: u16) -> Outcome {
    Outcome::status(http::StatusCode::from_u16(code).unwrap(), None, false)
}

fn after(code: u16, secs: u64) -> Outcome {
    Outcome::status(
        http::StatusCode::from_u16(code).unwrap(),
        Some(Duration::from_secs(secs)),
        false,
    )
}

/// **The default is the safe retry, and it is safe without asking the
/// caller for a promise.** A failure that never reached a server has
/// nothing at the server to duplicate.
#[test]
fn the_default_retries_what_never_arrived_and_no_status_at_all() {
    let p = RetryPolicy::default();
    assert!(matches!(
        p.decide(Outcome::Unsent, 1, 0.0, true),
        Verdict::After(_)
    ));
    for code in [408, 429, 500, 502, 503, 504] {
        assert_eq!(
            p.decide(status(code), 1, 0.0, true),
            Verdict::Stop(Stop::NotRetryable),
            "{code} is not repeated unless the caller asked"
        );
    }
}

/// **A body that cannot be replayed ends it, before any other rule.** No
/// policy overrides it and no server can ask past it — the same condition
/// the `425` replay is gated on one crate up.
#[test]
fn a_body_that_cannot_be_replayed_stops_every_kind_of_retry() {
    let p = RetryPolicy::transient();
    for outcome in [Outcome::Unsent, status(503), after(503, 1)] {
        assert_eq!(
            p.decide(outcome, 1, 0.0, false),
            Verdict::Stop(Stop::BodyCannotBeReplayed)
        );
    }
}

/// `Transient` is narrower than every 5xx, and the two statuses it leaves
/// out are the ones a repeat cannot change.
#[test]
fn transient_excludes_the_statuses_a_repeat_cannot_fix() {
    let t = RetryStatuses::Transient;
    let any = RetryStatuses::AnyServerError;
    for code in [408, 429, 500, 502, 503, 504] {
        let c = http::StatusCode::from_u16(code).unwrap();
        assert!(t.contains(c), "{code}");
        assert!(any.contains(c), "{code}");
    }
    for code in [501, 505, 507] {
        let c = http::StatusCode::from_u16(code).unwrap();
        assert!(!t.contains(c), "{code} is a statement about the request");
        assert!(any.contains(c), "{code} is still a 5xx");
    }
    for code in [200, 301, 400, 404, 418] {
        let c = http::StatusCode::from_u16(code).unwrap();
        assert!(!t.contains(c), "{code}");
        assert!(!any.contains(c), "{code}");
    }
}

/// **`Retry-After` is a floor, not a replacement.** A server asking for
/// less than our own backoff does not shorten it — the header says *not
/// before*, and nothing obliges a client to come back the instant it may.
#[test]
fn a_short_retry_after_does_not_shorten_the_backoff() {
    let p = RetryPolicy {
        backoff: Backoff {
            base: Duration::from_secs(5),
            max: Duration::from_secs(30),
            max_attempts: Some(3),
        },
        ..RetryPolicy::transient()
    };
    assert_eq!(
        p.decide(after(503, 1), 1, 0.0, true),
        Verdict::After(Duration::from_secs(10)),
        "the backoff at attempt 1 is 10s and the server asked for 1"
    );
    assert_eq!(
        p.decide(after(503, 20), 1, 0.0, true),
        Verdict::After(Duration::from_secs(20)),
        "and a longer ask wins"
    );
}

/// **A wait longer than the ceiling stops the retry rather than being
/// rounded down.** Rounding down retries sooner than the server asked,
/// which is the one behaviour the header exists to prevent — so a client
/// that caps and retries anyway has turned a limit into a violation.
#[test]
fn a_retry_after_beyond_the_ceiling_stops_rather_than_being_clamped() {
    let p = RetryPolicy {
        max_retry_after: Duration::from_secs(60),
        ..RetryPolicy::transient()
    };
    assert_eq!(
        p.decide(after(503, 61), 1, 0.0, true),
        Verdict::Stop(Stop::RetryAfterTooLong)
    );
    assert!(matches!(
        p.decide(after(503, 60), 1, 0.0, true),
        Verdict::After(_)
    ));
}

/// An instruction that exists and cannot be read is refused too, and for
/// the same reason. It is deliberately not the same as the header being
/// absent, which lets the backoff decide.
#[test]
fn an_unreadable_retry_after_stops_where_an_absent_one_does_not() {
    let p = RetryPolicy::transient();
    let unreadable = Outcome::status(http::StatusCode::SERVICE_UNAVAILABLE, None, true);
    assert_eq!(
        p.decide(unreadable, 1, 0.0, true),
        Verdict::Stop(Stop::RetryAfterTooLong)
    );
    assert!(
        matches!(p.decide(status(503), 1, 0.0, true), Verdict::After(_)),
        "silence is not an instruction"
    );
}

/// The attempt count is the backoff's, and running out is its own reason
/// rather than a silent `NotRetryable`.
#[test]
fn running_out_of_attempts_says_so() {
    let p = RetryPolicy::default();
    assert!(matches!(
        p.decide(Outcome::Unsent, 2, 0.0, true),
        Verdict::After(_)
    ));
    assert_eq!(
        p.decide(Outcome::Unsent, 3, 0.0, true),
        Verdict::Stop(Stop::OutOfAttempts),
        "three attempts made, and the default allows three"
    );
}

/// Jitter only ever shortens, which is `Backoff`'s own contract — so a
/// jittered wait can never exceed the unjittered one and a caller's
/// ceiling holds.
#[test]
fn jitter_only_shortens() {
    let p = RetryPolicy::default();
    let Verdict::After(none) = p.decide(Outcome::Unsent, 1, 0.0, true) else {
        panic!("retried")
    };
    let Verdict::After(full) = p.decide(Outcome::Unsent, 1, 1.0, true) else {
        panic!("retried")
    };
    assert!(full <= none, "{full:?} <= {none:?}");
}

/// `Retry-After`'s two forms, and the third thing a server can send.
#[test]
fn retry_after_reads_delta_seconds_and_refuses_the_rest() {
    assert_eq!(retry_after_seconds("120"), Some(Duration::from_secs(120)));
    assert_eq!(retry_after_seconds(" 0 "), Some(Duration::ZERO));
    // The `HTTP-date` form. Refused rather than guessed, and the caller
    // turns that into a stop — see the module doc.
    assert_eq!(retry_after_seconds("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    // A signed or fractional value is not `1*DIGIT` either.
    assert_eq!(retry_after_seconds("-5"), None);
    assert_eq!(retry_after_seconds("1.5"), None);
    assert_eq!(retry_after_seconds(""), None);
    assert_eq!(retry_after_seconds("soon"), None);
}
