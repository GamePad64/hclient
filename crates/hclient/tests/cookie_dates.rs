//! The `Expires` corpus.
//!
//! `Expires` is where a jar goes wrong quietly. A misparse raises nothing:
//! the attribute is dropped, and the cookie either becomes a session cookie
//! that outlives what the server meant, or — with the sign the other way —
//! vanishes on the next request. Neither shows up as an error anywhere.
//!
//! So the corpus decides this, not any crate's documentation, and it is
//! checked against **two independent oracles**:
//!
//! - `httpdate`, the incumbent HTTP-date parser, on the *format* question.
//!   It is strictly stricter than RFC 6265 §5.1.1, so the property is an
//!   implication rather than an equality: **everything `httpdate` accepts,
//!   we accept, with the same value.** That is asserted over every row
//!   below, so a row added later cannot quietly escape it, and
//!   `httpdate_still_has_an_opinion` keeps the implication from holding
//!   vacuously.
//! - `time`, on the *arithmetic* question, over the whole 1601..=9999 range
//!   the RFC allows — which `httpdate` cannot reach, since its result is a
//!   `SystemTime` built by adding to `UNIX_EPOCH`.
//!
//! Every expected value below was computed independently before the parser
//! was run against it.

// Host-only: this file is the crate's one  consumer, and
//  reaches , which does not build for
// `wasm32-unknown-unknown`. Without this the browser suite could not
// compile the crate's test targets at all.
#![cfg(all(feature = "cookies", not(target_family = "wasm")))]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hclient::cookie::SetCookie;
use proptest::prelude::*;
use rstest::rstest;

/// The parser under test, reached the way a caller reaches it.
fn expires(value: &str) -> Option<i64> {
    SetCookie::parse(format!("a=1; Expires={value}").as_bytes())
        .expect("the pair itself is well formed")
        .expires
}

/// `(the header value, the instant it names)`.
///
/// One table, used by three tests: the values themselves, the `httpdate`
/// implication, and the non-vacuity guard on that implication. Two copies
/// of this list would be two lists within a week.
const CORPUS: &[(&str, Option<i64>)] = &[
    // ── the three shapes RFC 9110 actually defines ──────────────────────
    ("Wed, 21 Oct 2015 07:28:00 GMT", Some(1_445_412_480)),
    ("Sunday, 06-Nov-94 08:49:37 GMT", Some(784_111_777)),
    ("Sun Nov  6 08:49:37 1994", Some(784_111_777)),
    ("Mon, 15 Nov 2094 12:45:26 GMT", Some(3_940_663_526)),
    // ── and the shapes that only §5.1.1 accepts ─────────────────────────
    // The deletion every server on the web sends.
    ("Thu, 01-Jan-1970 00:00:01 GMT", Some(1)),
    ("Wed, 21-Oct-2015 07:28:00 GMT", Some(1_445_412_480)),
    // No weekday, no zone, lower case, odd separators: §5.1.1 has no
    // notion of position, so none of these are required.
    ("21 Oct 2015 07:28:00 GMT", Some(1_445_412_480)),
    ("Wed, 21 Oct 2015 07:28:00", Some(1_445_412_480)),
    ("wed, 21 oct 2015 07:28:00 gmt", Some(1_445_412_480)),
    ("Wed,21-Oct-2015,07:28:00,GMT", Some(1_445_412_480)),
    (
        "Wednesday, 21 October 2015 07:28:00 GMT",
        Some(1_445_412_480),
    ),
    // A weekday that contradicts the date. §5.1.1 never looks at it;
    // `httpdate` refuses the whole string over it. See
    // `we_are_more_permissive_than_httpdate_and_here_is_exactly_where`.
    ("Mon, 06 Nov 1994 08:49:37 GMT", Some(784_111_777)),
    // Single-digit day and hour: the productions are `1*2DIGIT`.
    ("Wed, 9 Jun 2021 10:18:14 GMT", Some(1_623_233_894)),
    (
        "Wed, 09 Jun 2021 1:18:14 GMT",
        Some(1_623_233_894 - 9 * 3600),
    ),
    // ── the two-digit-year window, §5.1.1 step 3 ────────────────────────
    ("Tue, 01 Jan 69 00:00:00 GMT", Some(3_124_224_000)),
    ("Wed, 01 Jan 70 00:00:00 GMT", Some(0)),
    ("Sat, 01 Jan 00 00:00:00 GMT", Some(946_684_800)),
    // A three-digit year is left alone by step 3 and then fails the floor.
    ("Sat, 01 Jan 100 00:00:00 GMT", None),
    // ── the edges of the allowed range ──────────────────────────────────
    ("Mon, 01 Jan 1601 00:00:00 GMT", Some(-11_644_473_600)),
    ("Sun, 01 Jan 1600 00:00:00 GMT", None),
    ("Fri, 31 Dec 9999 23:59:59 GMT", Some(253_402_300_799)),
    ("Thu, 01 Jan 1970 00:00:00 GMT", Some(0)),
    // ── refusals ────────────────────────────────────────────────────────
    ("", None),
    ("0", None),
    ("not a date at all", None),
    // A time with no date, and a date with no time.
    ("07:28:00 GMT", None),
    ("Wed, 21 Oct 2015 GMT", None),
    // Out-of-range components, §5.1.1 step 5.
    ("Wed, 21 Oct 2015 24:00:00 GMT", None),
    ("Wed, 21 Oct 2015 07:60:00 GMT", None),
    ("Wed, 21 Oct 2015 07:28:60 GMT", None),
    ("Wed, 32 Oct 2015 07:28:00 GMT", None),
    ("Wed, 00 Oct 2015 07:28:00 GMT", None),
    // A day the month does not have — see `date.rs` on why this refuses
    // rather than rolling over into the next month. `httpdate` refuses
    // these too, which is the one thing that made the choice easy.
    ("Tue, 31 Apr 2030 07:28:00 GMT", None),
    ("Thu, 29 Feb 2030 07:28:00 GMT", None),
    ("Sun Feb 30 08:49:37 1994", None),
    ("Sun, 29 Feb 2032 00:00:00 GMT", Some(1_961_625_600)),
];

#[test]
fn the_corpus_parses_to_the_instants_it_names() {
    for (value, expected) in CORPUS {
        assert_eq!(expires(value), *expected, "Expires={value:?}");
    }
}

/// `httpdate` is strictly stricter than §5.1.1, so this is the property:
/// whatever it accepts, we accept, and to the same second.
///
/// Asserted over the whole table rather than a hand-picked subset, so that
/// a row added later is covered by it automatically.
#[test]
fn nothing_httpdate_accepts_is_refused_here() {
    for (value, expected) in CORPUS {
        let Ok(theirs) = httpdate::parse_http_date(value) else {
            continue;
        };
        let seconds = i64::try_from(
            theirs
                .duration_since(UNIX_EPOCH)
                .expect("httpdate cannot represent anything before the epoch")
                .as_secs(),
        )
        .expect("within i64");
        assert_eq!(
            *expected,
            Some(seconds),
            "httpdate accepts {value:?} and reads it as {seconds}"
        );
        assert_eq!(expires(value), Some(seconds), "{value:?}");
    }
}

/// The guard that keeps the implication above from being vacuous.
///
/// If a future `httpdate` refused everything in the table, that test would
/// go green while checking nothing at all. **Seven** is the count measured
/// while writing (`httpdate` 1.0); the assertion is a floor rather than an
/// equality so that `httpdate` becoming *more* permissive is not a failure,
/// while it becoming stricter — and quietly emptying the oracle — is.
#[test]
fn httpdate_still_has_an_opinion() {
    let accepted = CORPUS
        .iter()
        .filter(|(value, _)| httpdate::parse_http_date(value).is_ok())
        .count();
    assert!(
        accepted >= 7,
        "httpdate accepted only {accepted} of {} rows, so the implication \
         test has stopped proving anything",
        CORPUS.len()
    );
}

/// The rows where §5.1.1 is more permissive, each with the reason.
///
/// **This list is why `date.rs` exists.** Every entry is a real `Expires`
/// shape; a jar that reached for an HTTP-date parser would drop the
/// attribute on each of them. The first row is how essentially every server
/// on the web deletes a cookie, which would have turned every deletion into
/// a session cookie that never expires.
#[rstest]
#[case(
    "Thu, 01-Jan-1970 00:00:01 GMT",
    "hyphens with a four-digit year: neither IMF-fixdate nor RFC 850"
)]
#[case("Wed, 21-Oct-2015 07:28:00 GMT", "the same, in the future")]
#[case("21 Oct 2015 07:28:00 GMT", "no weekday")]
#[case("Wed, 21 Oct 2015 07:28:00", "no zone")]
#[case("wed, 21 oct 2015 07:28:00 gmt", "lower case")]
#[case("Wed, 9 Jun 2021 10:18:14 GMT", "a single-digit day")]
#[case("Wednesday, 21 October 2015 07:28:00 GMT", "the full month name")]
#[case(
    "Mon, 06 Nov 1994 08:49:37 GMT",
    "a weekday that contradicts the date — httpdate checks it, §5.1.1 \
     never reads it, and a server getting it wrong is a real and common bug"
)]
fn we_are_more_permissive_than_httpdate_and_here_is_exactly_where(
    #[case] value: &str,
    #[case] why: &str,
) {
    assert!(
        httpdate::parse_http_date(value).is_err(),
        "httpdate was expected to refuse {value:?} ({why}); if it no longer \
         does, this row has stopped proving anything"
    );
    assert!(expires(value).is_some(), "{value:?} ({why})");
}

/// The `Expires` attribute failing to parse is not an error, and does not
/// take the cookie with it — §5.2.1.
#[test]
fn an_unparsable_expires_leaves_a_session_cookie_rather_than_a_failure() {
    let parsed = SetCookie::parse(b"a=1; Expires=nonsense; Path=/x").expect("still a cookie");
    assert_eq!(parsed.expires, None);
    assert_eq!(parsed.value, "1");
    assert_eq!(parsed.path.as_deref(), Some("/x"));
}

proptest! {
    /// The calendar arithmetic, against `time`, over the whole range the
    /// RFC allows — including the years before 1970 that `httpdate` cannot
    /// represent at all, and every leap-year boundary in between.
    #[test]
    fn agrees_with_time_over_the_whole_range(
        year in 1601i32..=9999,
        month in 1u8..=12,
        day in 1u8..=31,
        hour in 0u8..=23,
        minute in 0u8..=59,
        second in 0u8..=59,
    ) {
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun",
            "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let text = format!(
            "{day:02} {} {year:04} {hour:02}:{minute:02}:{second:02} GMT",
            months[usize::from(month) - 1]
        );

        // `None` here covers the invalid day-of-month combinations the
        // strategy generates on purpose (31 April, 29 February in a common
        // year), so the two implementations are compared on refusals as
        // well as on values.
        let oracle = time::Month::try_from(month)
            .ok()
            .and_then(|m| time::Date::from_calendar_date(year, m, day).ok())
            .and_then(|d| d.with_hms(hour, minute, second).ok())
            .map(|dt| dt.assume_utc().unix_timestamp());

        prop_assert_eq!(expires(&text), oracle, "{}", text);
    }
}

/// The whole point of getting the date right: an `Expires` in the past
/// deletes, one in the future does not.
#[test]
fn the_parsed_date_actually_reaches_the_jar() {
    use hclient::cookie::CookieJar;
    use http::{HeaderValue, Uri};

    let uri: Uri = "https://example.com/".parse().expect("uri");
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut jar = CookieJar::new();

    jar.store(
        &uri,
        &HeaderValue::from_static("a=1; Expires=Mon, 15 Nov 2094 12:45:26 GMT"),
        now,
    )
    .expect("stored");
    assert_eq!(
        jar.iter().next().expect("one").expires(),
        // Capped at 400 days by §5.5, which is a separate rule from the
        // parse and is asserted here so that "the date parsed" and "the
        // date survived the cap" cannot be confused for one another.
        Some(now + Duration::from_secs(400 * 24 * 60 * 60))
    );

    jar.store(
        &uri,
        &HeaderValue::from_static("a=1; Expires=Thu, 01-Jan-1970 00:00:01 GMT"),
        now,
    )
    .expect("accepted");
    assert!(jar.is_empty(), "a past Expires deletes");
}

/// A guard on the corpus itself: `SystemTime` on this platform must be able
/// to represent the range the corpus asserts about, or the two edge rows in
/// it are testing the platform rather than the parser.
#[test]
fn the_platform_can_hold_the_range_the_corpus_names() {
    assert!(
        UNIX_EPOCH
            .checked_add(Duration::from_secs(253_402_300_799))
            .is_some()
    );
    assert!(
        UNIX_EPOCH
            .checked_sub(Duration::from_secs(11_644_473_600))
            .is_some()
    );
    assert!(SystemTime::now() > UNIX_EPOCH);
}
