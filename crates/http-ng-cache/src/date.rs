//! `HTTP-date`, RFC 9110 §5.6.7 — and, unlike its cousin in
//! `http-ng-cookie`, the strict thing.
//!
//! # Why this is not `http-ng-cookie`'s parser, and not `httpdate` either
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
//! `httpdate` would do this correctly and is a leaf crate. It is a
//! dev-dependency instead, exactly as it is in `http-ng-cookie`: this crate
//! also has to parse in the **lenient** direction the RFC requires of
//! recipients (RFC 850 and asctime, which senders must not produce and
//! recipients must accept), and it has to answer `None` for an unparsable
//! `Expires` in a way §5.3 turns into *already stale* rather than into
//! *ignore the header*. Owning the parser is what lets that distinction be
//! made here rather than inferred from an `Err` somebody else chose.
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
    imf_fixdate(input)
        .or_else(|| rfc850(input))
        .or_else(|| asctime(input))
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
fn imf_fixdate(t: &[u8]) -> Option<i64> {
    let t = strip_day_name(t, false)?;
    let t = t.strip_prefix(b", ")?;
    let (day, t) = fixed_digits(t, 2)?;
    let t = t.strip_prefix(b" ")?;
    let (month, t) = month(t)?;
    let t = t.strip_prefix(b" ")?;
    let (year, t) = fixed_digits(t, 4)?;
    let t = t.strip_prefix(b" ")?;
    let (hms, t) = time_of_day(t)?;
    if t != b" GMT" {
        return None;
    }
    civil(year, month, day, hms)
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
/// [`strip_day_name`].
fn rfc850(t: &[u8]) -> Option<i64> {
    let t = strip_day_name(t, true)?;
    let t = t.strip_prefix(b", ")?;
    let (day, t) = fixed_digits(t, 2)?;
    let t = t.strip_prefix(b"-")?;
    let (month, t) = month(t)?;
    let t = t.strip_prefix(b"-")?;
    let (yy, t) = fixed_digits(t, 2)?;
    let t = t.strip_prefix(b" ")?;
    let (hms, t) = time_of_day(t)?;
    if t != b" GMT" {
        return None;
    }
    let year = if yy >= 70 { 1900 + yy } else { 2000 + yy };
    civil(year, month, day, hms)
}

/// `Sun Nov  6 08:49:37 1994` — C's `asctime`, whose day-of-month is
/// space-padded rather than zero-padded.
fn asctime(t: &[u8]) -> Option<i64> {
    let t = strip_day_name(t, false)?;
    let t = t.strip_prefix(b" ")?;
    let (month, t) = month(t)?;
    let t = t.strip_prefix(b" ")?;
    // `%2d`: either two digits or a space and one.
    let (day, t) = match t.strip_prefix(b" ") {
        Some(rest) => fixed_digits(rest, 1)?,
        None => fixed_digits(t, 2)?,
    };
    let t = t.strip_prefix(b" ")?;
    let (hms, t) = time_of_day(t)?;
    let t = t.strip_prefix(b" ")?;
    let (year, t) = fixed_digits(t, 4)?;
    if !t.is_empty() {
        return None;
    }
    civil(year, month, day, hms)
}

const DAYS: [&[u8]; 7] = [b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat", b"Sun"];
const LONG_DAYS: [&[u8]; 7] = [
    b"Monday",
    b"Tuesday",
    b"Wednesday",
    b"Thursday",
    b"Friday",
    b"Saturday",
    b"Sunday",
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
fn strip_day_name(t: &[u8], long: bool) -> Option<&[u8]> {
    let names = if long { LONG_DAYS } else { DAYS };
    names.iter().find_map(|n| t.strip_prefix(*n))
}

fn month(t: &[u8]) -> Option<(i64, &[u8])> {
    const MONTHS: [&[u8]; 12] = [
        b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov",
        b"Dec",
    ];
    let head = t.get(..3)?;
    let i = MONTHS.iter().position(|m| *m == head)?;
    Some((i as i64 + 1, &t[3..]))
}

/// Exactly `n` ASCII digits, no more and no fewer.
///
/// "No more" is what keeps `19944` from parsing as the year 1994 with a
/// stray digit, which is the failure a `take_while` would have.
fn fixed_digits(t: &[u8], n: usize) -> Option<(i64, &[u8])> {
    let head = t.get(..n)?;
    if !head.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut v: i64 = 0;
    for b in head {
        v = v * 10 + i64::from(b - b'0');
    }
    Some((v, &t[n..]))
}

/// Hour, minute, second.
type Hms = (i64, i64, i64);

fn time_of_day(t: &[u8]) -> Option<(Hms, &[u8])> {
    let (h, t) = fixed_digits(t, 2)?;
    let t = t.strip_prefix(b":")?;
    let (m, t) = fixed_digits(t, 2)?;
    let t = t.strip_prefix(b":")?;
    let (s, t) = fixed_digits(t, 2)?;
    // 60 is a leap second and is a legal `second` in the grammar; it is
    // accepted and folded onto :59 rather than refused, because refusing
    // would make a response stale for the one second a year it can happen.
    if h > 23 || m > 59 || s > 60 {
        return None;
    }
    Some(((h, m, s.min(59)), t))
}

fn civil(year: i64, month: i64, day: i64, (h, m, s): Hms) -> Option<i64> {
    if day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + h * 3_600 + m * 60 + s)
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date, by Howard
/// Hinnant's `days_from_civil` — the same function `http-ng-cookie`'s
/// `date.rs` carries, and deliberately copied rather than shared: making
/// one of those two crates depend on the other to save nine lines would
/// tie a jar's release to a cache's.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
