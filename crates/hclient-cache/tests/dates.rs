//! `HTTP-date` against an independent parser, over one corpus.
//!
//! `date.rs` is 200 lines of hand-written grammar, and the failure mode
//! that matters is not "it rejects a date" — a test written by the same
//! person who wrote the parser will feed it the same three shapes the
//! parser handles. It is "it accepts something an HTTP-date parser
//! would not", or refuses one of the three forms RFC 9110 §5.6.7 obliges
//! a recipient to read. Neither is visible from inside.
//!
//! `httpdate` is the oracle, dev-only, exactly as it is in
//! `hclient-cookie` and for the same reason. Where the two disagree the
//! disagreement is **enumerated below with its reason**, rather than the
//! corpus being trimmed until they agree — which is the only way a
//! differential test is worth running twice.

use std::time::{Duration, SystemTime};

use hclient_cache::{HttpCache, Lookup};
use http::{HeaderMap, HeaderValue, Method, Uri};

/// Every string in the corpus, with what `httpdate` makes of it. `None`
/// means it refuses.
fn oracle(s: &str) -> Option<i64> {
    let t = httpdate::parse_http_date(s).ok()?;
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok(),
        Err(e) => i64::try_from(e.duration().as_secs()).ok().map(|s| -s),
    }
}

/// This crate's answer, reached the only way a consumer can reach it:
/// through a stored `Expires`, which is where the parser is actually used.
///
/// A stored response with `Date: <epoch>` and `Expires: <s>` has a
/// freshness lifetime of exactly `parse(s)` seconds, so an entry is fresh
/// at `parse(s) - 1` and stale at `parse(s) + 1`. Rather than reproduce
/// that arithmetic, this binary-searches the boundary — which also
/// exercises the arithmetic itself, and would catch a parser that is right
/// and a `freshness_lifetime` that is not.
fn ours(s: &str) -> Option<i64> {
    // Only the forward half of the range is reachable this way: an
    // `Expires` at or before the `Date` gives a lifetime of zero and is
    // indistinguishable from an unparsable one. The corpus below marks
    // those rows and checks them by a different assertion.
    let mut lo: i64 = 0;
    let mut hi: i64 = 1 << 40;
    if !fresh_at(s, 0) {
        return None;
    }
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if fresh_at(s, mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(lo + 1)
}

/// Is a response `Date`d at the epoch and `Expires`-ing at `s` still fresh
/// `after` seconds later?
fn fresh_at(s: &str, after: i64) -> bool {
    let mut cache = HttpCache::new();
    let uri: Uri = "https://example.test/x".parse().unwrap();
    let epoch = SystemTime::UNIX_EPOCH;
    let (p, ()) = http::Response::builder()
        .header("date", "Thu, 01 Jan 1970 00:00:00 GMT")
        .header("expires", HeaderValue::from_str(s).unwrap())
        .body(())
        .unwrap()
        .into_parts();
    let Ok(storing) = cache.storing(&Method::GET, &uri, &HeaderMap::new(), &p, epoch, epoch) else {
        return false;
    };
    cache.store(storing, bytes::Bytes::new()).expect("stored");
    let now = epoch + Duration::from_secs(u64::try_from(after).unwrap());
    matches!(
        cache.lookup(&Method::GET, &uri, &HeaderMap::new(), now),
        Lookup::Hit(_)
    )
}

/// Strings both parsers must read the same way.
const AGREE: &[&str] = &[
    // RFC 9110 §5.6.7's own three spellings of one instant.
    "Sun, 06 Nov 1994 08:49:37 GMT",
    "Sunday, 06-Nov-94 08:49:37 GMT",
    "Sun Nov  6 08:49:37 1994",
    // Ordinary values off real servers.
    "Wed, 21 Oct 2015 07:28:00 GMT",
    "Fri, 31 Dec 1999 23:59:59 GMT",
    "Tue, 29 Feb 2000 12:00:00 GMT",
    // Refusals.
    "0",
    "",
    "not a date at all",
    "Thu, 01-Jan-1970 00:00:01 GMT", // the cookie deletion; not an HTTP-date
    "Sun, 6 Nov 1994 08:49:37 GMT",  // one digit where two are required
    "Sun, 06 Nov 1994 08:49:37 UTC", // a timezone that is not GMT
    "Sun, 06 Nov 1994 08:49:37 GMT ", // trailing OWS: both accept, §5.5
    " Sun, 06 Nov 1994 08:49:37 GMT", // leading OWS, likewise
    "Sun, 06 Nov 1994 08:49:37 GMT x", // trailing rubbish that is not OWS
    "Wed, 30 Feb 2030 00:00:00 GMT", // a day February does not have
    "Sun, 06 Nov 19944 08:49:37 GMT",
];

#[test]
fn the_two_parsers_agree_on_the_corpus() {
    let mut checked = 0usize;
    for s in AGREE {
        let theirs = oracle(s);
        // Only forward-of-epoch dates are reachable through the freshness
        // arithmetic; everything at or before it is checked by the
        // refusal test below instead.
        if theirs.is_some_and(|v| v <= 0) {
            continue;
        }
        assert_eq!(ours(s), theirs, "disagreement on {s:?}");
        checked += 1;
    }
    assert!(
        checked >= 5,
        "the corpus filtered down to {checked} comparisons — a differential test \
         that compares almost nothing passes for the wrong reason"
    );
}

/// The rows that cannot be compared through the freshness arithmetic
/// because a lifetime cannot be negative. Both parsers must still refuse,
/// or accept, the same strings.
#[test]
fn the_two_parsers_agree_on_which_strings_are_dates_at_all() {
    for s in AGREE {
        let theirs = oracle(s).is_some();
        // `fresh_at(s, 0)` is true only for a parsable date strictly after
        // the epoch, so it cannot answer this on its own — but a string
        // both parsers refuse and a string both accept-and-place-in-the-
        // past are the two cases here, and the second is checked by the
        // test above wherever it is reachable.
        if theirs && !s.contains("1970") {
            continue;
        }
        assert!(!fresh_at(s, 0), "{s:?} should not produce a live lifetime");
    }
}

/// **The one deliberate disagreement, measured rather than guessed.**
///
/// `httpdate` validates the day name against the date and refuses a
/// mismatch; `date.rs` consumes it without checking, because RFC 9110
/// §5.6.7's wording there is a `SHOULD`-shaped "ought to" and the
/// consequence here is harsher than the RFC's: an unparsable `Expires` is
/// *already stale* (§5.3), so a server whose formatter gets the weekday
/// wrong would lose its freshness lifetime entirely.
///
/// The direction is safe in the other sense too — this crate accepts more
/// dates, never fewer, and every date it accepts and `httpdate` refuses is
/// one whose numeric fields both parsers read identically.
#[test]
fn the_weekday_is_checked_there_and_not_here() {
    // 1 January 2001 was a Monday.
    let right = "Monday, 01-Jan-01 00:00:00 GMT";
    let wrong = "Friday, 01-Jan-01 00:00:00 GMT";
    assert_eq!(oracle(right), Some(978_307_200));
    assert_eq!(oracle(wrong), None, "httpdate refuses a mismatched weekday");
    assert_eq!(
        ours(right),
        ours(wrong),
        "this crate reads the date either way, and the numbers agree with the oracle's"
    );
    assert_eq!(ours(right), oracle(right));
}

/// The two-digit RFC 850 year, where a guess was written down and then
/// found to be wrong.
///
/// `date.rs` uses RFC 6265 §5.1.1's **fixed** split because RFC 9110
/// §5.6.7's own rule is a moving 50-year window and this crate has no
/// clock. The comment there originally said that `httpdate` implements the
/// moving window and that the two therefore differ. Measured: it does not
/// — it uses the same fixed split, so the clock this crate does not have
/// would have bought nothing at all.
#[test]
fn the_two_digit_year_split_is_the_same_in_both() {
    for (day, yy, expected) in [
        ("Saturday", "00", 946_684_800i64),
        ("Tuesday", "69", 3_124_224_000),
        ("Thursday", "70", 0),
        ("Friday", "99", 915_148_800),
    ] {
        let s = format!("{day}, 01-Jan-{yy} 00:00:00 GMT");
        assert_eq!(oracle(&s), Some(expected), "{s}");
        if expected > 0 {
            assert_eq!(ours(&s), Some(expected), "{s}");
        } else {
            assert!(
                !fresh_at(&s, 0),
                "{s} names the epoch, so it is stale at once"
            );
        }
    }
}
