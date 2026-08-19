//! `Cache-Control`, RFC 9111 §5.2 — parsed once, into the directives this
//! cache actually reads.
//!
//! # Two structs, not one
//!
//! §5.2.1 and §5.2.2 define two disjoint vocabularies that happen to share
//! a field name and three spellings. `no-cache` on a request means *ask
//! the origin before you use what you have*; on a response it means *never
//! use this without asking*. `max-age` on a request is a bound the client
//! puts on what it will accept; on a response it is the lifetime the
//! origin grants. One struct with a `is_request` flag would let a response
//! directive be read where a request one was meant, which is the shape of
//! bug nothing fails on.
//!
//! # What is deliberately not parsed
//!
//! - **`s-maxage` and `proxy-revalidate`.** Both are addressed to shared
//!   caches (§5.2.2.10, §5.2.2.8) and this is a private one — see
//!   [`crate`]'s own doc for what "private" decides. They are not parsed
//!   rather than parsed-and-ignored, because a field nothing reads is a
//!   field that will eventually be read by accident.
//! - **`public`.** It exists to *widen* a shared cache's permission
//!   (§5.2.2.9) past `Authorization` and past a non-cacheable status. A
//!   private cache never had the restriction, so honouring `public` would
//!   change nothing, and a field that changes nothing is one this project
//!   deletes rather than ships.
//! - **The qualified forms**, `no-cache="field"` and `private="field"`.
//!   Both are §5.2.2's instructions to a *shared* cache about which fields
//!   to withhold. The unqualified reading of `no-cache` is the stricter of
//!   the two (revalidate the whole response rather than one field), so
//!   reading a qualified `no-cache` as an unqualified one fails in the
//!   direction that cannot serve a stale field; `private="Set-Cookie"` on
//!   a private cache is simply inert.
//! - **`no-transform`** (§5.2.2.6). Nothing here transforms: this cache
//!   stores what the transport handed over, byte for byte, `Content-
//!   Encoding` and all, and the client's decompression happens **above**
//!   it on the way out (`http-ng`'s `ClientBody`). A directive with no
//!   subject is not honoured by being parsed.
//! - **`immutable`** (RFC 8246). It is an optimisation over revalidation
//!   for a reload the caller performs; there is no reload here.

use std::time::Duration;

/// The `Cache-Control` directives a **request** carries, RFC 9111 §5.2.1.
///
/// Every field is an `Option`, and the distinction the `Option` carries is
/// the point in two of them: `max-stale: None` is "the caller said
/// nothing" while `max-stale: Some(None)` is `max-stale` with no argument
/// — "any staleness at all" — and `Some(Some(d))` is a bound. A `bool`
/// plus a `Duration` would have collapsed the first two.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RequestDirectives {
    /// §5.2.1.5. Nothing about this request or its response may be stored.
    pub no_store: bool,
    /// §5.2.1.4. A stored response may be used only after successful
    /// validation.
    pub no_cache: bool,
    /// §5.2.1.7. Do not go to the network; a miss is a `504`.
    pub only_if_cached: bool,
    /// §5.2.1.1. The caller will not accept an age above this.
    pub max_age: Option<Duration>,
    /// §5.2.1.2. The caller will accept a stale response — by this much,
    /// or by any amount when the inner `Option` is `None`.
    pub max_stale: Option<Option<Duration>>,
    /// §5.2.1.3. The caller wants the response to stay fresh for at least
    /// this much longer.
    pub min_fresh: Option<Duration>,
}

/// The `Cache-Control` directives a **response** carries, RFC 9111 §5.2.2.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResponseDirectives {
    /// §5.2.2.5. This response may not be stored at all.
    pub no_store: bool,
    /// §5.2.2.4. Stored, but never reused without successful validation.
    pub no_cache: bool,
    /// §5.2.2.7. For a *shared* cache, a refusal. For this one it is
    /// **information**, kept because it is the field the whole
    /// private/shared decision turns on and a reader of the store should
    /// be able to see that the origin marked the response as one user's.
    pub private: bool,
    /// §5.2.2.2. Once stale, this response may not be reused without
    /// successful validation — which is what makes it the one directive
    /// that overrides a request's `max-stale`.
    pub must_revalidate: bool,
    /// §5.2.2.1.
    pub max_age: Option<Duration>,
}

impl RequestDirectives {
    pub(crate) fn parse(headers: &http::HeaderMap) -> Self {
        let mut out = Self::default();
        for_each_directive(headers, |name, arg| match name.as_slice() {
            b"no-store" => out.no_store = true,
            b"no-cache" => out.no_cache = true,
            b"only-if-cached" => out.only_if_cached = true,
            b"max-age" => out.max_age = seconds(arg),
            b"min-fresh" => out.min_fresh = seconds(arg),
            // The nested `Option` the struct's doc comment explains: the
            // directive with no argument is not the same statement as the
            // directive with an unparsable one, and it is certainly not
            // the same as its absence.
            b"max-stale" => out.max_stale = Some(arg.and_then(seconds_of)),
            _ => {}
        });
        out
    }
}

impl ResponseDirectives {
    pub(crate) fn parse(headers: &http::HeaderMap) -> Self {
        let mut out = Self::default();
        for_each_directive(headers, |name, arg| match name.as_slice() {
            b"no-store" => out.no_store = true,
            b"no-cache" => out.no_cache = true,
            b"private" => out.private = true,
            b"must-revalidate" => out.must_revalidate = true,
            b"max-age" => out.max_age = seconds(arg),
            _ => {}
        });
        out
    }
}

/// Walks every `Cache-Control` directive across every copy of the header,
/// lowercasing the name and handing over the argument unquoted.
///
/// **Splitting on `,` alone is wrong and this does not do it.** A quoted
/// argument may contain a comma — `private="X-A, X-B"` is one directive,
/// not two — and a parser that split first would see a directive called
/// `x-b"` and, worse, would end the `private` argument early. The quote
/// state is tracked while scanning, which costs four lines and is the
/// difference between reading a header and guessing at one.
fn for_each_directive(headers: &http::HeaderMap, mut f: impl FnMut(Vec<u8>, Option<&[u8]>)) {
    for value in headers.get_all(http::header::CACHE_CONTROL) {
        for part in split_outside_quotes(value.as_bytes()) {
            let part = trim(part);
            if part.is_empty() {
                continue;
            }
            let (name, arg) = match part.iter().position(|b| *b == b'=') {
                Some(i) => (&part[..i], Some(unquote(trim(&part[i + 1..])))),
                None => (part, None),
            };
            let name: Vec<u8> = trim(name).iter().map(u8::to_ascii_lowercase).collect();
            f(name, arg);
        }
    }
}

fn split_outside_quotes(v: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    let mut i = 0usize;
    while i < v.len() {
        match v[i] {
            b'"' => quoted = !quoted,
            // A quoted-pair: the escaped byte cannot close the string and
            // cannot be a separator.
            b'\\' if quoted => i += 1,
            b',' if !quoted => {
                out.push(&v[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&v[start.min(v.len())..]);
    out
}

fn trim(v: &[u8]) -> &[u8] {
    let start = v.iter().position(|b| !b.is_ascii_whitespace());
    let Some(start) = start else { return &[] };
    let end = v
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .unwrap_or(start);
    &v[start..=end]
}

fn unquote(v: &[u8]) -> &[u8] {
    match (v.first(), v.last()) {
        (Some(b'"'), Some(b'"')) if v.len() >= 2 => &v[1..v.len() - 1],
        _ => v,
    }
}

fn seconds(arg: Option<&[u8]>) -> Option<Duration> {
    seconds_of(arg?)
}

/// A `delta-seconds` (§1.2.2). An argument that is not a number at all is
/// `None` — **not zero** — because §5.2's own error handling is that an
/// unrecognised or malformed directive is ignored, and a `max-age` of zero
/// would be an instruction the server never gave.
///
/// A value too large for a `u64` is saturated rather than refused, which
/// §1.2.2 requires in as many words: *"a recipient that receives a value
/// larger than the largest it can represent... must consider the value to
/// be 2147483648 or the greatest positive integer it can represent"*.
fn seconds_of(arg: &[u8]) -> Option<Duration> {
    if arg.is_empty() || !arg.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut v: u64 = 0;
    for b in arg {
        v = match v
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(b - b'0')))
        {
            Some(v) => v,
            None => return Some(Duration::from_secs(u64::MAX)),
        };
    }
    Some(Duration::from_secs(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn h(v: &str) -> http::HeaderMap {
        let mut m = http::HeaderMap::new();
        m.insert(
            http::header::CACHE_CONTROL,
            HeaderValue::from_str(v).unwrap(),
        );
        m
    }

    #[test]
    fn a_response_max_age_is_read_case_insensitively_and_with_spaces() {
        let d = ResponseDirectives::parse(&h(" Max-Age = 60 , must-revalidate"));
        assert_eq!(d.max_age, Some(Duration::from_secs(60)));
        assert!(d.must_revalidate);
    }

    #[test]
    fn a_comma_inside_a_quoted_argument_does_not_split_a_directive() {
        // Without the quote tracking this reads as three directives, the
        // second of which is `x-b"` and the third `max-age=60` — which is
        // the one that would still work, hiding the defect.
        let d = ResponseDirectives::parse(&h(r#"private="X-A, X-B", max-age=60"#));
        assert!(d.private);
        assert_eq!(d.max_age, Some(Duration::from_secs(60)));
    }

    #[test]
    fn the_three_states_of_max_stale() {
        assert_eq!(RequestDirectives::parse(&h("no-cache")).max_stale, None);
        assert_eq!(
            RequestDirectives::parse(&h("max-stale")).max_stale,
            Some(None)
        );
        assert_eq!(
            RequestDirectives::parse(&h("max-stale=30")).max_stale,
            Some(Some(Duration::from_secs(30)))
        );
    }

    #[test]
    fn a_malformed_delta_seconds_is_absent_rather_than_zero() {
        assert_eq!(ResponseDirectives::parse(&h("max-age=abc")).max_age, None);
        assert_eq!(ResponseDirectives::parse(&h("max-age=")).max_age, None);
        assert_eq!(ResponseDirectives::parse(&h("max-age=-1")).max_age, None);
        assert_eq!(
            ResponseDirectives::parse(&h("max-age=0")).max_age,
            Some(Duration::ZERO),
            "zero is a real instruction and must not be confused with the absent case"
        );
    }

    #[test]
    fn an_overlong_delta_seconds_saturates_rather_than_vanishing() {
        assert_eq!(
            ResponseDirectives::parse(&h("max-age=99999999999999999999999")).max_age,
            Some(Duration::from_secs(u64::MAX))
        );
    }

    #[test]
    fn directives_are_read_across_every_copy_of_the_header() {
        let mut m = http::HeaderMap::new();
        m.append(
            http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        );
        m.append(
            http::header::CACHE_CONTROL,
            HeaderValue::from_static("max-age=5"),
        );
        let d = ResponseDirectives::parse(&m);
        assert!(d.no_cache);
        assert_eq!(d.max_age, Some(Duration::from_secs(5)));
    }

    /// The two vocabularies share spellings and must not share readers:
    /// `only-if-cached` is a request directive and has no response
    /// meaning, `must-revalidate` the reverse.
    #[test]
    fn the_two_vocabularies_do_not_read_each_other() {
        let req = RequestDirectives::parse(&h("must-revalidate, private"));
        assert_eq!(req, RequestDirectives::default());
        let resp = ResponseDirectives::parse(&h("only-if-cached, min-fresh=5"));
        assert_eq!(resp, ResponseDirectives::default());
    }
}
