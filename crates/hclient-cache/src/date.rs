//! `HTTP-date`, RFC 9110 §5.6.7 — and, unlike its cousin in
//! `hclient-cookie`, the strict thing.
//!
//! # Why this is not `hclient-cookie`'s parser, and not `httpdate` either
//!
//! Two different grammars sit one crate apart, and confusing them is a
//! defect in whichever direction it is made. `Expires` **on a cookie** is
//! RFC 6265 §5.1.1's deliberately position-free algorithm; `Expires` **on a
//! response** is `HTTP-date`, which has exactly three accepted forms and a
//! fixed layout. A cache that used the cookie parser would accept
//! `1970 GMT 00:00:00 Jan 01 Thu` as a freshness bound; a jar that used
//! this one would read `Expires=Thu, 01-Jan-1970 00:00:01 GMT` — the most
//! common deletion on the web — as no date at all.
//!
//! `httpdate` is a leaf crate that reads all three forms correctly, and it
//! **is** used here — as the oracle in `tests/dates.rs`, exactly as `url`
//! is the oracle for `uri.rs` and as it is in `hclient-cookie`. Two reasons
//! keep it out of the implementation, and neither is leniency: it parses
//! RFC 850 and asctime just as this does, which the differential corpus
//! proves by agreeing on them.
//!
//! The first is what a refusal has to *mean*. §5.3 turns an unparsable
//! `Expires` into *already stale*, which is the opposite of ignoring the
//! header, and that distinction is made at the call site from a `None` this
//! module owns rather than inferred from an `Err` somebody else chose. The
//! second is the return type, below. This parser also deliberately accepts
//! **more** than `httpdate` does — the weekday is not checked against the
//! date — for a reason recorded beside the test that pins it.
//!
//! # What this returns, and why it is not a `SystemTime`
//!
//! Seconds since the Unix epoch, as an `i64`, negative before 1970. A
//! `SystemTime` cannot represent a date before `UNIX_EPOCH` on every
//! platform without arithmetic that can fail, and `Expires: Thu, 01 Jan
//! 1970 00:00:00 GMT` — a server saying *already stale* in the only way
//! HTTP has ever had — is precisely a date at the boundary. The conversion
//! to `SystemTime` happens once, in [`crate::policy`], where it can be
//! saturated against the epoch in one place.

use jiff::civil::{Date, DateTime, Time};
use winnow::combinator::{alt, preceded, terminated};
use winnow::token::{literal, take_while};
use winnow::{ModalResult, Parser};

/// The three forms of `HTTP-date` a recipient must accept.
///
/// `None` means "not a date". Every caller turns that into a *specific*
/// thing rather than into a shrug — see [`crate::policy::expires_at`],
/// where an unparsable `Expires` becomes a date in the past, which is what
/// RFC 9111 §5.3 requires and is the opposite of ignoring the header.
pub(crate) fn parse_http_date(input: &[u8]) -> Option<i64> {
    // RFC 9110 §5.5: a field value does not include leading or trailing
    // whitespace. A recipient is supposed to have removed it, and one
    // that has not is the ordinary case — this was found by the
    // differential in `tests/dates.rs`, where `httpdate` accepted
    // `"… GMT "` and this parser did not. Without the trim the failure is
    // a *cache miss on every request* for one server's `Date` field,
    // which nothing announces.
    let input = trim_ows(input);
    // `Parser::parse` requires the whole input to be consumed, which is
    // what makes each form's trailing literal load-bearing: `"… GMT x"` is
    // refused by the parser rather than by a separate emptiness check.
    alt((imf_fixdate, rfc850, asctime)).parse(input).ok()
}

fn trim_ows(v: &[u8]) -> &[u8] {
    let Some(start) = v.iter().position(|b| !b.is_ascii_whitespace()) else {
        return &[];
    };
    let end = v
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .unwrap_or(start);
    &v[start..=end]
}

/// `Sun, 06 Nov 1994 08:49:37 GMT` — the only form a sender may produce.
fn imf_fixdate(t: &mut &[u8]) -> ModalResult<i64> {
    day_name(false).parse_next(t)?;
    let day = preceded(", ", digits(2)).parse_next(t)?;
    let month = preceded(" ", month).parse_next(t)?;
    let year = preceded(" ", digits(4)).parse_next(t)?;
    let hms = terminated(preceded(" ", time_of_day), " GMT").parse_next(t)?;
    civil(year, month, day, hms).ok_or_else(fail)
}

/// `Sunday, 06-Nov-94 08:49:37 GMT` — RFC 850, with a two-digit year.
///
/// The two-digit year is the reason this form is obsolete and the reason
/// it still has to be read. RFC 9110 §5.6.7 makes the window a **moving**
/// one — a date more than 50 years in the future is read as the most
/// recent past year with the same last two digits — and a moving window
/// needs the `now` this clockless module deliberately does not take. So
/// the fixed 1970/2000 split of RFC 6265 §5.1.1 is used instead: `70` and
/// above is the 1900s, below it the 2000s.
///
/// The cost was **measured against `httpdate` rather than assumed**, and
/// the assumption was wrong in the interesting direction: `httpdate`
/// implements the same fixed split (`00`→2000, `69`→2069, `70`→1970,
/// `99`→1999 — `tests/dates.rs`), so the two agree everywhere and the
/// clock this module does not have would have bought nothing. The
/// disagreement that does exist is one field to the left, in
/// [`day_name`].
///
/// chrono's `%y` implements the same split, and it is still not used —
/// reaching for it would mean handing chrono the weekday too, which it
/// would then check.
fn rfc850(t: &mut &[u8]) -> ModalResult<i64> {
    day_name(true).parse_next(t)?;
    let day = preceded(", ", digits(2)).parse_next(t)?;
    let month = preceded("-", month).parse_next(t)?;
    let yy = preceded("-", digits(2)).parse_next(t)?;
    let hms = terminated(preceded(" ", time_of_day), " GMT").parse_next(t)?;
    let year = if yy >= 70 { 1900 + yy } else { 2000 + yy };
    civil(year, month, day, hms).ok_or_else(fail)
}

/// `Sun Nov  6 08:49:37 1994` — C's `asctime`, whose day-of-month is
/// space-padded rather than zero-padded.
fn asctime(t: &mut &[u8]) -> ModalResult<i64> {
    day_name(false).parse_next(t)?;
    let month = preceded(" ", month).parse_next(t)?;
    // `%2d`: either two digits or a space and one.
    let day = preceded(" ", alt((digits(2), preceded(" ", digits(1))))).parse_next(t)?;
    let hms = preceded(" ", time_of_day).parse_next(t)?;
    let year = preceded(" ", digits(4)).parse_next(t)?;
    civil(year, month, day, hms).ok_or_else(fail)
}

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const LONG_DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// Consumes the day name, which is **not checked against the date**.
///
/// RFC 9110 §5.6.7 says a recipient that finds a day name disagreeing with
/// the date "ought to" ignore the date. Ignoring it here would mean a
/// response with an off-by-one weekday is read as *already stale* (see the
/// module doc for what `None` becomes), which is a harsher answer than the
/// RFC's `SHOULD`-shaped wording, for a mismatch that is nearly always a
/// server's formatting bug rather than a lie about the date.
///
/// **This is the one place this parser and `httpdate` disagree**, measured
/// rather than argued: `httpdate` refuses `Monday, 01-Jan-70 …` where this
/// accepts it. `tests/dates.rs` pins the difference in both directions, so
/// it stays a decision rather than becoming a surprise.
///
/// It is also why the name is consumed here rather than by a chrono
/// `%a`/`%A`: chrono cross-checks the weekday against the parsed date and
/// refuses a mismatch, so delegating this field would silently adopt
/// `httpdate`'s answer and lose the decision.
fn day_name(long: bool) -> impl FnMut(&mut &[u8]) -> ModalResult<()> {
    move |t: &mut &[u8]| {
        let names = if long { LONG_DAYS } else { DAYS };
        // Longest-first is not needed: no short name is a prefix of a
        // different day's long name, and within one list every name has
        // the same length.
        alt([
            literal(names[0]),
            literal(names[1]),
            literal(names[2]),
            literal(names[3]),
            literal(names[4]),
            literal(names[5]),
            literal(names[6]),
        ])
        .void()
        .parse_next(t)
    }
}

fn month(t: &mut &[u8]) -> ModalResult<i64> {
    alt([
        literal("Jan").value(1i64),
        literal("Feb").value(2),
        literal("Mar").value(3),
        literal("Apr").value(4),
        literal("May").value(5),
        literal("Jun").value(6),
        literal("Jul").value(7),
        literal("Aug").value(8),
        literal("Sep").value(9),
        literal("Oct").value(10),
        literal("Nov").value(11),
        literal("Dec").value(12),
    ])
    .parse_next(t)
}

/// Exactly `n` ASCII digits, no more and no fewer.
///
/// "No more" is what keeps `19944` from parsing as the year 1994 with a
/// stray digit, and "no fewer" is what keeps `6 Nov` out of a grammar that
/// spells it `06`. Both are why chrono's `%d`/`%Y` are not used: they
/// accept one digit where two are written, which would quietly widen what
/// this crate calls an `HTTP-date`.
fn digits(n: usize) -> impl FnMut(&mut &[u8]) -> ModalResult<i64> {
    move |t: &mut &[u8]| {
        take_while(n..=n, |b: u8| b.is_ascii_digit())
            .map(|d: &[u8]| d.iter().fold(0i64, |v, b| v * 10 + i64::from(b - b'0')))
            .parse_next(t)
    }
}

/// Hour, minute, second.
type Hms = (i8, i8, i8);

fn time_of_day(t: &mut &[u8]) -> ModalResult<Hms> {
    let h = digits(2).parse_next(t)?;
    let m = preceded(":", digits(2)).parse_next(t)?;
    let s = preceded(":", digits(2)).parse_next(t)?;
    // 60 is a leap second and is a legal `second` in the grammar; it is
    // accepted and folded onto :59 rather than refused, because refusing
    // would make a response stale for the one second a year it can happen.
    // chrono would fold it the same way, and is not asked to: it never
    // sees the field, because it never sees the time as text.
    if h > 23 || m > 59 || s > 60 {
        return Err(fail());
    }
    // The cast cannot lose anything: two digits, bounded above.
    Ok((h as i8, m as i8, s.min(59) as i8))
}

/// The calendar, which is jiff's half of this file.
///
/// This is what the two date parsers in this workspace genuinely had in
/// common — `days_from_civil`, `is_leap` and `days_in_month`, copied byte
/// for byte between here and `hclient-cookie`. Delegating it removes the
/// copy from both rather than moving it to a third place; a proleptic
/// Gregorian calendar has one correct answer and no versions, so nothing
/// about the delegation can drift.
///
/// **The civil types are used rather than `jiff::Timestamp`, and that is
/// load bearing.** `Timestamp::MAX` is `9999-12-30T22:00Z`, so a route
/// through `to_zoned(..).timestamp()` cannot represent `Expires: Fri, 31
/// Dec 9999 23:59:59 GMT` — a real "never expires" idiom, which this
/// parser has always read as `253402300799`. `civil::Date::MAX` is
/// `9999-12-31`, and `DateTime::duration_since` against a civil epoch
/// answers `253402300799` exactly.
///
/// `Date::new` and `Time::new` are the fallible constructors on purpose:
/// `civil::date(..)` and `Date::at(..)` **panic** on a value the calendar
/// does not have, and both would be reachable from a header. The one
/// panicking constructor here is in a `const`, where an impossible date
/// is a compile error rather than anything a server could trigger.
fn civil(year: i64, month: i64, day: i64, (h, m, s): Hms) -> Option<i64> {
    const EPOCH: DateTime = jiff::civil::datetime(1970, 1, 1, 0, 0, 0, 0);

    let date = Date::new(
        i16::try_from(year).ok()?,
        i8::try_from(month).ok()?,
        i8::try_from(day).ok()?,
    )
    .ok()?;
    let time = Time::new(h, m, s, 0).ok()?;
    Some(
        DateTime::from_parts(date, time)
            .duration_since(EPOCH)
            .as_secs(),
    )
}

/// A refusal from a check the grammar could not express.
///
/// `civil` rejects `30 Feb`, and `time_of_day` rejects `25:00:00`, after
/// the shape has already matched — so both need to fail a parser that has
/// consumed input. Backtracking is what `alt` in [`parse_http_date`] wants
/// here: an `imf_fixdate` that matched its shape and then found an
/// impossible day should let `rfc850` try, exactly as a shape mismatch
/// does.
fn fail() -> winnow::error::ErrMode<winnow::error::ContextError> {
    winnow::error::ErrMode::Backtrack(winnow::error::ContextError::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 9110 §5.6.7's own three spellings of one instant.
    #[test]
    fn the_three_forms_name_the_same_second() {
        let a = parse_http_date(b"Sun, 06 Nov 1994 08:49:37 GMT");
        let b = parse_http_date(b"Sunday, 06-Nov-94 08:49:37 GMT");
        let c = parse_http_date(b"Sun Nov  6 08:49:37 1994");
        assert_eq!(a, Some(784_111_777));
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn the_epoch_and_before_it() {
        assert_eq!(parse_http_date(b"Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            parse_http_date(b"Wed, 31 Dec 1969 23:59:59 GMT"),
            Some(-1),
            "a date before the epoch is why this returns an i64 and not a SystemTime"
        );
    }

    /// The cookie parser's headline case, which this one must refuse: the
    /// two grammars are one crate apart and must not drift together.
    #[test]
    fn a_cookie_date_is_not_an_http_date() {
        assert_eq!(parse_http_date(b"Thu, 01-Jan-1970 00:00:01 GMT"), None);
        assert_eq!(parse_http_date(b"1970 GMT 00:00:00 Jan 01 Thu"), None);
    }

    #[test]
    fn the_layout_is_fixed_where_the_cookie_grammar_has_none() {
        // One digit where two are required.
        assert_eq!(parse_http_date(b"Sun, 6 Nov 1994 08:49:37 GMT"), None);
        // A timezone that is not GMT.
        assert_eq!(parse_http_date(b"Sun, 06 Nov 1994 08:49:37 UTC"), None);
        // Trailing rubbish that is not whitespace. Whitespace itself is
        // trimmed first — RFC 9110 §5.5 — which the differential against
        // `httpdate` is what found.
        assert_eq!(parse_http_date(b"Sun, 06 Nov 1994 08:49:37 GMT x"), None);
        assert_eq!(
            parse_http_date(b"  Sun, 06 Nov 1994 08:49:37 GMT "),
            Some(784_111_777)
        );
        // A five-digit year, which a `take_while` parser would truncate.
        assert_eq!(parse_http_date(b"Sun, 06 Nov 19944 08:49:37 GMT"), None);
    }

    #[test]
    fn a_month_the_day_does_not_exist_in_is_not_a_date() {
        assert_eq!(parse_http_date(b"Wed, 30 Feb 2030 00:00:00 GMT"), None);
        assert!(parse_http_date(b"Sun, 29 Feb 2032 00:00:00 GMT").is_some());
    }

    #[test]
    fn a_leap_second_is_read_rather_than_refused() {
        assert_eq!(
            parse_http_date(b"Sun, 31 Dec 2016 23:59:60 GMT"),
            parse_http_date(b"Sun, 31 Dec 2016 23:59:59 GMT"),
        );
        assert_eq!(parse_http_date(b"Sun, 31 Dec 2016 23:59:61 GMT"), None);
    }

    #[test]
    fn the_rfc850_year_window_is_the_fixed_one_this_module_documents() {
        assert_eq!(
            parse_http_date(b"Thursday, 01-Jan-70 00:00:00 GMT"),
            Some(0)
        );
        assert_eq!(
            parse_http_date(b"Tuesday, 01-Jan-69 00:00:00 GMT"),
            parse_http_date(b"Tue, 01 Jan 2069 00:00:00 GMT")
        );
    }
}
