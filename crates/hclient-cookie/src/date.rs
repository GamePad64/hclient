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

use jiff::civil::{Date, DateTime, Time};
use winnow::combinator::{not, preceded, terminated};
use winnow::token::{one_of, take_while};
use winnow::{ModalResult, Parser};

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
    //
    // This loop is deliberately *not* a winnow parser. §5.1.1 is an
    // assignment algorithm rather than a grammar: there is no order to
    // match against, so what a combinator would express is the productions
    // — which is exactly what `match_time`, `match_number` and
    // `match_month` are below — and not the search that applies them.
    for token in input.split(|b| is_delimiter(*b)).filter(|t| !t.is_empty()) {
        if time.is_none()
            && let Some(t) = run(match_time, token)
        {
            time = Some(t);
            continue;
        }
        if day.is_none()
            && let Some(d) = run(number(1, 2), token)
        {
            day = Some(d);
            continue;
        }
        if month.is_none()
            && let Some(m) = run(match_month, token)
        {
            month = Some(m);
            continue;
        }
        if year.is_none()
            && let Some(y) = run(number(2, 4), token)
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

    // §5.1.1's own floor, which no calendar enforces for us.
    if year < 1601 {
        return None;
    }

    // §5.1.1 step 5 and step 6 in one call, which is the shape of the
    // decision rather than a shortcut: step 5 checks `1 <= d <= 31` and
    // step 6 then asks for a date that may not exist, so the day/month
    // agreement has to be settled by whatever builds the date. Refusing —
    // rather than normalising `31 Feb 2030` into 2 Mar — is what Chromium
    // does through `base::Time::FromUTCExploded`, and `Date::new` is the
    // same answer. The hour, minute and second bounds are `Time::new`'s,
    // including the refusal of `:60`: unlike an `HTTP-date`, a
    // `cookie-date` has no leap second to accept.
    //
    // `new` rather than jiff's `civil::date(..)`/`Date::at(..)`, which
    // **panic** on a value the calendar does not have — every one of these
    // fields came off a header. The epoch below is the one place a
    // panicking constructor is used, and it is a `const`, so an impossible
    // date there is a compile error.
    //
    // The civil types rather than `jiff::Timestamp` for the reason
    // `hclient-cache`'s `civil` records: `Timestamp::MAX` is
    // `9999-12-30T22:00Z`, which is inside the range §5.1.1 allows.
    const EPOCH: DateTime = jiff::civil::datetime(1970, 1, 1, 0, 0, 0, 0);

    let date = Date::new(
        i16::try_from(year).ok()?,
        i8::try_from(month).ok()?,
        i8::try_from(day).ok()?,
    )
    .ok()?;
    let time = Time::new(
        i8::try_from(hour).ok()?,
        i8::try_from(minute).ok()?,
        i8::try_from(second).ok()?,
        0,
    )
    .ok()?;
    Some(
        DateTime::from_parts(date, time)
            .duration_since(EPOCH)
            .as_secs(),
    )
}

/// Runs one production over a token, allowing whatever follows it.
///
/// `Parser::parse` is deliberately not used: §5.1.1's productions end at
/// "end of token **or a non-digit**", so `06th` is a day-of-month and
/// unconsumed input is not an error. What must still be refused is a
/// *digit* after the field, which each production checks for itself.
fn run<T>(mut p: impl FnMut(&mut &[u8]) -> ModalResult<T>, token: &[u8]) -> Option<T> {
    let mut input = token;
    p.parse_next(&mut input).ok()
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
fn match_time(t: &mut &[u8]) -> ModalResult<(u32, u32, u32)> {
    let hour = number(1, 2).parse_next(t)?;
    let minute = preceded(":", number(1, 2)).parse_next(t)?;
    let second = preceded(":", number(1, 2)).parse_next(t)?;
    Ok((hour, minute, second))
}

/// The `day-of-month` (`min`/`max` = 1/2) and `year` (2/4) productions:
/// digits, then either the end of the token or a non-digit. The trailing
/// non-digit is *allowed*, not required — `06` and `06th` are both a
/// day-of-month 6, and `1234567` is neither a day nor a year.
///
/// `not` is what states that second half. Without it `take_while(..=2)`
/// would read `123` as `12`, and a seven-digit run would become a
/// plausible year.
fn number(min: usize, max: usize) -> impl FnMut(&mut &[u8]) -> ModalResult<u32> {
    move |t: &mut &[u8]| {
        terminated(
            take_while(min..=max, |b: u8| b.is_ascii_digit())
                .map(|d: &[u8]| d.iter().fold(0u32, |v, b| v * 10 + u32::from(b - b'0'))),
            not(one_of(|b: u8| b.is_ascii_digit())),
        )
        .parse_next(t)
    }
}

const MONTHS: [[u8; 3]; 12] = [
    *b"jan", *b"feb", *b"mar", *b"apr", *b"may", *b"jun", *b"jul", *b"aug", *b"sep", *b"oct",
    *b"nov", *b"dec",
];

/// The `month` production: a case-insensitive match on the **first three
/// characters** of the token, so `Jan`, `january` and `JANUARY-ish` all
/// name month 1.
fn match_month(t: &mut &[u8]) -> ModalResult<u32> {
    take_while(3..=3, |b: u8| b.is_ascii_alphabetic())
        .verify_map(|head: &[u8]| {
            let head: [u8; 3] = head.try_into().ok()?;
            let head = head.map(|b| b.to_ascii_lowercase());
            let i = MONTHS.iter().position(|m| *m == head)?;
            u32::try_from(i).ok().map(|i| i + 1)
        })
        .parse_next(t)
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
