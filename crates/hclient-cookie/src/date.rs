//! The `Expires` attribute's date, per RFC 6265 §5.1.1 — and nothing else.
//!
//! # Why this is not `httpdate::parse_http_date`
//!
//! `Expires` looks like an HTTP date and is not one. RFC 6265 §5.1.1
//! defines its own algorithm, and the algorithm is deliberately *lenient*
//! where RFC 9110's `HTTP-date` is strict: it splits the string on a
//! delimiter set, then assigns each token to whichever of
//! time / day-of-month / month / year it can still be, **in that order and
//! regardless of position**. There is no fixed layout to match against, no
//! required weekday, and no required `GMT`.
//!
//! That difference is not academic. The single most common `Expires` value
//! on the web is a deletion:
//!
//! ```text
//! Set-Cookie: sid=; Expires=Thu, 01-Jan-1970 00:00:01 GMT
//! ```
//!
//! `httpdate::parse_http_date` **rejects** that string — it is neither
//! IMF-fixdate (which wants `01 Jan 1970`, spaces not hyphens) nor RFC 850
//! (which wants the full weekday name and a two-digit year). A jar that
//! parsed `Expires` with an HTTP-date parser would silently drop the
//! attribute and turn every such deletion into a **session cookie that
//! never expires**. `tests/dates.rs` pins exactly this: the corpus runs the
//! same strings through both parsers and records every string where they
//! disagree, with the reason.
//!
//! # The one place this is stricter than the RFC as written
//!
//! §5.1.1 step 5 validates the day-of-month only as `1 <= d <= 31`, so
//! `31 Feb 2030` passes its checks and step 6 then asks for "the date whose
//! day-of-month … is 31 and whose month is February", which does not
//! exist. Something has to give, and the two ways out differ by a whole
//! month: normalise (2 Mar 2030) or refuse (attribute ignored → session
//! cookie).
//!
//! This module refuses, which is what Chromium does — its
//! `ParseCookieExpirationTime` hands the exploded fields to
//! `base::Time::FromUTCExploded`, which fails for a day the month does not
//! have. Refusing is also the safer of the two for a jar: the failure mode
//! is a cookie that lives for the session instead of one that outlives the
//! date the server actually meant.

/// The RFC 6265 §5.1.1 `cookie-date` algorithm.
///
/// `None` means "not a date" — every caller of this turns that into
/// *ignore the `Expires` attribute*, never into an error, because §5.2.1
/// says an unparsable attribute value is ignored rather than fatal.
/// The result is seconds since the Unix epoch, negative before 1970 —
/// which `Expires` reaches every time a server deletes a cookie, and which
/// the RFC allows down to year 1601.
pub(crate) fn parse_cookie_date(input: &[u8]) -> Option<i64> {
    let mut time: Option<(u32, u32, u32)> = None;
    let mut day: Option<u32> = None;
    let mut month: Option<u32> = None;
    let mut year: Option<u32> = None;

    // §5.1.1 step 2: the first production that both matches and has not
    // been filled in yet wins. The order is fixed by the RFC and is load
    // bearing — `1*2DIGIT` (day-of-month) is tried before `2*4DIGIT`
    // (year), which is why `Sun, 06 Nov 94 08:49:37 GMT` reads `06` as the
    // day and `94` as the year rather than the other way round.
    for token in input.split(|b| is_delimiter(*b)).filter(|t| !t.is_empty()) {
        if time.is_none()
            && let Some(t) = match_time(token)
        {
            time = Some(t);
            continue;
        }
        if day.is_none()
            && let Some(d) = match_number(token, 1, 2)
        {
            day = Some(d);
            continue;
        }
        if month.is_none()
            && let Some(m) = match_month(token)
        {
            month = Some(m);
            continue;
        }
        if year.is_none()
            && let Some(y) = match_number(token, 2, 4)
        {
            year = Some(y);
            continue;
        }
    }

    let (hour, minute, second) = time?;
    let day = day?;
    let month = month?;
    let mut year = year?;

    // §5.1.1 step 3. Written as the RFC writes it: 70..=99 is the 1900s,
    // 0..=69 the 2000s, and anything else is left alone — a three-digit
    // year stays three digits and is then rejected by the 1601 floor.
    if (70..=99).contains(&year) {
        year += 1900;
    } else if year <= 69 {
        year += 2000;
    }

    // §5.1.1 step 5, plus the day/month agreement discussed in the module
    // documentation.
    if day < 1 || day > days_in_month(year, month) {
        return None;
    }
    if year < 1601 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let days = days_from_civil(i64::from(year), i64::from(month), i64::from(day));
    Some(days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))
}

/// RFC 6265 §5.1.1's `delimiter`: `%x09 / %x20-2F / %x3B-40 / %x5B-60 /
/// %x7B-7E`.
///
/// Note what is *not* here: `:` (0x3A) and the digits and letters. That is
/// what keeps `00:00:01` one token while splitting `01-Jan-1970` into
/// three.
fn is_delimiter(b: u8) -> bool {
    b == 0x09
        || (0x20..=0x2F).contains(&b)
        || (0x3B..=0x40).contains(&b)
        || (0x5B..=0x60).contains(&b)
        || (0x7B..=0x7E).contains(&b)
}

/// `1*2DIGIT ":" 1*2DIGIT ":" 1*2DIGIT` followed by end-of-token or a
/// non-digit.
fn match_time(t: &[u8]) -> Option<(u32, u32, u32)> {
    let (hour, rest) = take_digits(t, 1, 2)?;
    let rest = rest.strip_prefix(b":")?;
    let (minute, rest) = take_digits(rest, 1, 2)?;
    let rest = rest.strip_prefix(b":")?;
    let (second, rest) = take_digits(rest, 1, 2)?;
    if rest.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some((hour, minute, second))
}

/// The `day-of-month` (`min`/`max` = 1/2) and `year` (2/4) productions:
/// digits, then either the end of the token or a non-digit. The trailing
/// non-digit is *allowed*, not required — `06` and `06th` are both a
/// day-of-month 6, and `1234567` is neither a day nor a year.
fn match_number(t: &[u8], min: usize, max: usize) -> Option<u32> {
    let (value, rest) = take_digits(t, min, max)?;
    if rest.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some(value)
}

fn take_digits(t: &[u8], min: usize, max: usize) -> Option<(u32, &[u8])> {
    let n = t
        .iter()
        .take(max)
        .take_while(|b| b.is_ascii_digit())
        .count();
    if n < min {
        return None;
    }
    let mut value = 0u32;
    for b in &t[..n] {
        value = value * 10 + u32::from(b - b'0');
    }
    Some((value, &t[n..]))
}

const MONTHS: [[u8; 3]; 12] = [
    *b"jan", *b"feb", *b"mar", *b"apr", *b"may", *b"jun", *b"jul", *b"aug", *b"sep", *b"oct",
    *b"nov", *b"dec",
];

/// The `month` production: a case-insensitive match on the **first three
/// characters** of the token, so `Jan`, `january` and `JANUARY-ish` all
/// name month 1.
fn match_month(t: &[u8]) -> Option<u32> {
    let head: [u8; 3] = t.get(..3)?.try_into().ok()?;
    let head = head.map(|b| b.to_ascii_lowercase());
    MONTHS
        .iter()
        .position(|m| *m == head)
        .map(|i| u32::try_from(i).unwrap_or(0) + 1)
}

fn is_leap(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date, by Howard
/// Hinnant's `days_from_civil`. Correct for any year the RFC allows
/// (1601..=9999) and for dates before the epoch, which `Expires` reaches
/// every time a server deletes a cookie with a 1601 or 1970 date.
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

    #[test]
    fn the_epoch_itself() {
        assert_eq!(parse_cookie_date(b"Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
    }

    #[test]
    fn tokens_may_arrive_in_any_order() {
        // Not a legal HTTP-date in any of the three forms; §5.1.1 has no
        // notion of position, so it parses.
        assert_eq!(
            parse_cookie_date(b"1970 GMT 00:00:00 Jan 01 Thu"),
            parse_cookie_date(b"Thu, 01 Jan 1970 00:00:00 GMT")
        );
    }

    #[test]
    fn a_month_the_day_does_not_exist_in_is_not_a_date() {
        assert_eq!(parse_cookie_date(b"Sat, 31 Apr 2030 00:00:00 GMT"), None);
        assert_eq!(parse_cookie_date(b"Sat, 30 Feb 2030 00:00:00 GMT"), None);
        assert_eq!(parse_cookie_date(b"Sat, 29 Feb 2030 00:00:00 GMT"), None);
        // 2032 is a leap year, so the same day-of-month is fine there.
        assert!(parse_cookie_date(b"Sun, 29 Feb 2032 00:00:00 GMT").is_some());
    }

    #[test]
    fn the_1601_floor_is_enforced() {
        assert!(parse_cookie_date(b"Mon, 01 Jan 1601 00:00:00 GMT").is_some());
        assert_eq!(parse_cookie_date(b"Sun, 01 Jan 1600 00:00:00 GMT"), None);
    }

    #[test]
    fn a_seven_digit_run_is_neither_a_day_nor_a_year() {
        assert_eq!(parse_cookie_date(b"Thu, 1234567 Jan 00:00:00 GMT"), None);
    }
}
