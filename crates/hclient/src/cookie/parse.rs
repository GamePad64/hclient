//! `Set-Cookie` parsing, RFC 6265bis §5.2 — the header as written, with no
//! reference to the request it arrived on.
//!
//! Splitting this from the storage model in `jar.rs` is not tidiness: §5.2
//! is a pure function of the header bytes, and §5.7 is the part that needs
//! the request URI, the clock and the public suffix list. Keeping them
//! apart is what lets the whole of §5.2 be tested with a table.
//!
//! The rule that catches people is the last one: **an attribute this
//! parser does not recognise is ignored, not an error.** `Set-Cookie:
//! a=1; Priority=High; Partitioned; Domain=example.com` yields a cookie
//! with a domain, not a failure, because a server is entitled to send
//! attributes newer than this crate.

use super::date::parse_cookie_date;
use crate::cookie::error::ParseError;

/// A `Set-Cookie` header as parsed, before any of it has been checked
/// against a request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SetCookie {
    /// The cookie name, already trimmed of surrounding whitespace.
    pub name: String,
    /// The cookie value. Surrounding whitespace is trimmed; surrounding
    /// double quotes are **not**, because §5.2 treats them as part of the
    /// value and every other implementation does the same.
    pub value: String,
    /// The `Domain` attribute, lowercased, with the leading `.` some
    /// servers still send removed (§5.2.3).
    pub domain: Option<String>,
    /// The `Path` attribute, only when it was present and began with `/`.
    /// Anything else is "not specified" per §5.2.4 and the default path
    /// applies.
    pub path: Option<String>,
    /// `Expires`, as seconds since the Unix epoch. `None` covers both "no
    /// `Expires`" and "an `Expires` that did not parse", which §5.2.1 makes
    /// the same thing.
    pub expires: Option<i64>,
    /// `Max-Age`, in seconds, exactly as sent — a value of zero or less is
    /// preserved rather than clamped here, because §5.7 gives it a meaning
    /// (expire immediately) that this layer has no clock to express.
    pub max_age: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    /// `SameSite`, parsed and carried. **Not enforced by this crate** — see
    /// [`SameSite`].
    pub same_site: Option<SameSite>,
}

/// The `SameSite` attribute's value.
///
/// Stored and reported, never acted on. Enforcing `SameSite` needs the
/// "site for cookies" of the *context that initiated the request* — which
/// is a browsing context, a thing a non-browser HTTP client does not have
/// and cannot invent. A jar that guessed would either drop cookies a
/// server expects or claim a protection it is not providing; reporting the
/// attribute and saying plainly that nothing enforces it is the honest
/// third option. In a browser the jar is not ours anyway
/// (`Capabilities::owns_cookie_jar`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

use winnow::combinator::{alt, terminated};
use winnow::token::{rest, take_until};
use winnow::{ModalResult, Parser};

/// One `;`-delimited segment, and the `;` itself where there is one.
///
/// RFC 6265bis §5.2 is written as a sequence of *cuts* rather than as a
/// grammar — "the characters up to the first `;`", then the same again —
/// so this is the whole of its shape, applied once for the name/value pair
/// and then once per attribute. `rest` is the last segment, which has no
/// terminator; that alternative is why an absent trailing `;` is not an
/// error.
fn segment<'a>(t: &mut &'a [u8]) -> ModalResult<&'a [u8]> {
    alt((terminated(take_until(0.., b';'), ";"), rest)).parse_next(t)
}

/// Cuts one segment at its first `=`, if it has one.
///
/// §5.2 splits on the **first** `=` and never on a later one, which is
/// what leaves `a=b=c=d` a cookie named `a` with the value `b=c=d`. A
/// segment with no `=` is a whole key and an empty value — `Secure` and
/// `HttpOnly` are that shape.
fn split_once_on_equals(segment: &[u8]) -> (&[u8], &[u8]) {
    let mut input = segment;
    let key: ModalResult<&[u8]> = terminated(take_until(0.., b'='), "=").parse_next(&mut input);
    match key {
        Ok(key) => (trim(key), trim(input)),
        Err(_) => (trim(segment), &segment[segment.len()..]),
    }
}

impl SetCookie {
    /// RFC 6265bis §5.2, on the raw header bytes.
    pub fn parse(header: &[u8]) -> Result<Self, ParseError> {
        let mut input = header;
        // Infallible: `segment`'s `rest` arm matches anything, the empty
        // header included.
        let pair = segment.parse_next(&mut input).unwrap_or(header);

        if !pair.contains(&b'=') {
            return Err(ParseError::NoNameValueSeparator);
        }
        let (name, value) = split_once_on_equals(pair);

        if name.is_empty() {
            return Err(ParseError::EmptyName);
        }
        if name.iter().chain(value).copied().any(is_ctl) {
            return Err(ParseError::ControlCharacter);
        }

        let mut out = Self {
            name: to_utf8(name)?,
            value: to_utf8(value)?,
            domain: None,
            path: None,
            expires: None,
            max_age: None,
            secure: false,
            http_only: false,
            same_site: None,
        };

        while !input.is_empty() {
            let Ok(attribute) = segment.parse_next(&mut input) else {
                break;
            };
            let (key, value) = split_once_on_equals(attribute);
            out.apply_attribute(key, value);
        }

        Ok(out)
    }

    fn apply_attribute(&mut self, key: &[u8], value: &[u8]) {
        // §5.2.x compares attribute names case-insensitively; a `String`
        // here rather than a match on bytes because the names are short and
        // this is not a hot path.
        let key = key.to_ascii_lowercase();
        match key.as_slice() {
            b"expires" => {
                // §5.2.1: a value that does not parse means the attribute
                // is ignored — which is precisely the failure that turns a
                // deletion into an immortal session cookie if the date
                // parser is the wrong one. See `date.rs`.
                if let Some(seconds) = parse_cookie_date(value) {
                    self.expires = Some(seconds);
                }
            }
            b"max-age" => {
                // §5.2.2: the first character must be a digit or `-`,
                // otherwise the attribute is ignored. `+30` is therefore
                // **not** a Max-Age, which is easy to get wrong by reaching
                // for `str::parse` alone — that accepts a leading `+`.
                let first = value.first().copied();
                if first.is_some_and(|b| b.is_ascii_digit() || b == b'-')
                    && let Ok(text) = core::str::from_utf8(value)
                    && let Ok(seconds) = text.parse::<i64>()
                {
                    self.max_age = Some(seconds);
                }
            }
            b"domain" => {
                // §5.2.3: an empty value is ignored; a leading `.` is
                // dropped; the rest is lowercased.
                let domain = value.strip_prefix(b".").unwrap_or(value);
                if !domain.is_empty()
                    && let Ok(text) = core::str::from_utf8(domain)
                {
                    self.domain = Some(text.to_ascii_lowercase());
                }
            }
            b"path" => {
                // §5.2.4: a value that is empty or does not start with `/`
                // leaves the path unspecified, so the default path applies.
                if value.starts_with(b"/")
                    && let Ok(text) = core::str::from_utf8(value)
                {
                    self.path = Some(text.to_owned());
                }
            }
            b"secure" => self.secure = true,
            b"httponly" => self.http_only = true,
            b"samesite" => {
                self.same_site = match value.to_ascii_lowercase().as_slice() {
                    b"strict" => Some(SameSite::Strict),
                    b"lax" => Some(SameSite::Lax),
                    b"none" => Some(SameSite::None),
                    // An unrecognised value is not an unrecognised
                    // attribute: 6265bis says to treat it as the default,
                    // which is what `None` means here.
                    _ => None,
                };
            }
            // Everything else — `Priority`, `Partitioned`, whatever ships
            // next — is ignored, not an error.
            _ => {}
        }
    }
}

fn trim(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if *first == b' ' || *first == b'\t' {
            s = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = s {
        if *last == b' ' || *last == b'\t' {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// 6265bis forbids CTLs in the name and value, **excluding HTAB** — a tab
/// inside a value is legal (the surrounding ones have already been
/// trimmed), a `\n` is not.
pub(super) fn is_ctl(b: u8) -> bool {
    (b <= 0x08) || (0x0A..=0x1F).contains(&b) || b == 0x7F
}

fn to_utf8(bytes: &[u8]) -> Result<String, ParseError> {
    core::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| ParseError::NonUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    fn parse(s: &str) -> SetCookie {
        SetCookie::parse(s.as_bytes()).expect("should parse")
    }

    #[test]
    fn the_pair_is_split_on_the_first_equals_only() {
        let c = parse("a=b=c=d");
        assert_eq!(c.name, "a");
        assert_eq!(c.value, "b=c=d");
    }

    #[test]
    fn surrounding_whitespace_goes_and_quotes_stay() {
        let c = parse("  a  =  \"b\"  ; Path=/x");
        assert_eq!(c.name, "a");
        assert_eq!(c.value, "\"b\"");
        assert_eq!(c.path.as_deref(), Some("/x"));
    }

    #[test]
    fn an_unknown_attribute_is_ignored_rather_than_fatal() {
        let c = parse("a=1; Priority=High; Partitioned; Domain=example.com");
        assert_eq!(c.domain.as_deref(), Some("example.com"));
        assert_eq!(c.value, "1");
    }

    #[test]
    fn a_leading_dot_on_domain_is_dropped_and_the_rest_lowercased() {
        assert_eq!(
            parse("a=1; Domain=.EXAMPLE.com").domain.as_deref(),
            Some("example.com")
        );
        assert_eq!(parse("a=1; Domain=").domain, None);
        assert_eq!(parse("a=1; Domain=.").domain, None);
    }

    #[test]
    fn a_path_that_does_not_start_with_a_slash_is_no_path_at_all() {
        assert_eq!(parse("a=1; Path=foo").path, None);
        assert_eq!(parse("a=1; Path=").path, None);
        assert_eq!(parse("a=1; Path=/foo").path.as_deref(), Some("/foo"));
    }

    #[test]
    fn max_age_takes_a_sign_but_not_a_plus() {
        assert_eq!(parse("a=1; Max-Age=30").max_age, Some(30));
        assert_eq!(parse("a=1; Max-Age=-1").max_age, Some(-1));
        assert_eq!(parse("a=1; Max-Age=0").max_age, Some(0));
        // §5.2.2's first-character rule, which `str::parse` alone would
        // not enforce.
        assert_eq!(parse("a=1; Max-Age=+30").max_age, None);
        assert_eq!(parse("a=1; Max-Age= 30").max_age, Some(30));
        assert_eq!(parse("a=1; Max-Age=abc").max_age, None);
    }

    #[test]
    fn flags_are_case_insensitive_and_need_no_value() {
        let c = parse("a=1; SECURE; httponly; SameSite=STRICT");
        assert!(c.secure);
        assert!(c.http_only);
        assert_eq!(c.same_site, Some(SameSite::Strict));
    }

    #[test]
    fn an_unrecognised_samesite_value_is_the_default_not_an_error() {
        assert_eq!(parse("a=1; SameSite=Nonsense").same_site, None);
    }

    #[test]
    fn the_refusals_of_section_5_2() {
        assert_matches!(
            SetCookie::parse(b"novaluehere"),
            Err(ParseError::NoNameValueSeparator)
        );
        assert_matches!(SetCookie::parse(b"=1"), Err(ParseError::EmptyName));
        assert_matches!(SetCookie::parse(b"   =1"), Err(ParseError::EmptyName));
        assert_matches!(
            SetCookie::parse(b"a=1\n2"),
            Err(ParseError::ControlCharacter)
        );
        assert_matches!(
            SetCookie::parse(&[b'a', b'=', 0xFF]),
            Err(ParseError::NonUtf8)
        );
    }

    #[test]
    fn a_tab_inside_a_value_survives() {
        // CTLs are forbidden "excluding HTAB"; the trim only takes the
        // outer ones.
        assert_eq!(parse("a=\tb\tc\t").value, "b\tc");
    }

    #[test]
    fn a_cookie_may_have_an_empty_value() {
        let c = parse("sid=; Expires=Thu, 01-Jan-1970 00:00:01 GMT");
        assert_eq!(c.name, "sid");
        assert_eq!(c.value, "");
        assert_eq!(c.expires, Some(1));
    }
}
