//! RFC 9110 §5.6's field-value primitives, as winnow parsers.
//!
//! `token`, `OWS` and `quoted-string` are the three productions every
//! comma-and-semicolon header in HTTP is built out of, and this workspace
//! had **four** hand-written copies of them by the time `Link:` arrived —
//! in `hclient`'s `digest.rs`, its `cache::directives`, its
//! `response::charset_param`, and this crate's new `link`. They were
//! written independently and they agree, which is why this is tidy-up
//! rather than a defect; what it buys is that the next header does not
//! make it five.
//!
//! Two of the four are converted: [`crate::link`], and `hclient`'s
//! `response::charset_param`, whose 30 existing tests pass unaltered —
//! which is the evidence that this changed nothing. The other two are
//! named below with what each would take.
//!
//! # Two `quoted-string`s, and the split is a decision rather than an
//! oversight
//!
//! [`quoted_string`] unescapes and allocates; [`quoted_string_raw`]
//! borrows the bytes between the quotes and leaves a `quoted-pair` as it
//! was written. Both *consume* an escape correctly, so a `\"` can never
//! end the string early in either.
//!
//! Which one a header wants is decided by what its values are. A `realm`
//! or a `title` is free text a deployment chooses, so a caller wants the
//! quote rather than the backslash. A `Cache-Control` argument is a
//! field-name list and a `charset` is an encoding label, neither of which
//! can contain a quote at all — so unescaping there would allocate on
//! every value to change nothing. That split already existed between those
//! modules; it is now stated here once instead of being re-derived at each
//! of them.
//!
//! # What is deliberately not here
//!
//! **A `&[u8]` variant.** `cache::directives` parses over bytes because a
//! `HeaderValue` is bytes and it never needs a `str`; a second copy of
//! each production for that one caller would be two implementations of one
//! grammar, which is the shape this module exists against. It keeps its
//! own until something else wants bytes too.
//!
//! **`token68`, `parameter`, `auth-param`.** Those are one header's
//! grammar each, not primitives — `token68` appears in
//! `WWW-Authenticate` and nowhere else here.
//!
//! **`digest.rs`'s copy, which is `&str` for `&str` and would take one
//! import.** It is left for whoever touches that file next rather than
//! reached into from here: a conversion that changes no behaviour is
//! worth exactly as much whenever it happens, and doing it from a
//! neighbouring change costs a reviewer a diff they did not ask for.

use winnow::ascii::space0;
use winnow::combinator::{alt, delimited, repeat};
use winnow::token::{any, none_of, take_while};
use winnow::{ModalResult, Parser};

/// RFC 9110 §5.6.2 `token`.
pub fn token<'a>(i: &mut &'a str) -> ModalResult<&'a str> {
    take_while(1.., |c: char| {
        c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
    })
    .parse_next(i)
}

/// RFC 9110 §5.6.3 `OWS`.
///
/// It also stands in for `BWS`: the two are the same production and
/// differ only in what a *sender* may write.
pub fn ows(i: &mut &str) -> ModalResult<()> {
    space0.void().parse_next(i)
}

/// RFC 9110 §5.6.4 `quoted-string`, **unescaped** — for a value that is
/// free text, where a `\"` is a quote the caller should see.
pub fn quoted_string(i: &mut &str) -> ModalResult<String> {
    delimited(
        '"',
        repeat(
            0..,
            alt((winnow::combinator::preceded('\\', any), none_of(['"']))),
        ),
        '"',
    )
    .parse_next(i)
}

/// RFC 9110 §5.6.4 `quoted-string`, **not** unescaped — the bytes between
/// the quotes, borrowed.
///
/// A `quoted-pair` is still consumed, so this ends where the string ends;
/// what it does not do is spend an allocation removing backslashes from
/// values whose grammar cannot contain one.
pub fn quoted_string_raw<'a>(i: &mut &'a str) -> ModalResult<&'a str> {
    delimited(
        '"',
        repeat(0.., alt((('\\', any).void(), none_of(['"']).void())))
            .map(|(): ()| ())
            .take(),
        '"',
    )
    .parse_next(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_quoted_strings_differ_only_in_the_escape() {
        let mut a = r#""say \"hi\"" rest"#;
        assert_eq!(quoted_string(&mut a).unwrap(), r#"say "hi""#);
        assert_eq!(a, " rest", "both stop at the same byte");

        let mut b = r#""say \"hi\"" rest"#;
        assert_eq!(quoted_string_raw(&mut b).unwrap(), r#"say \"hi\""#);
        assert_eq!(b, " rest");
    }

    #[test]
    fn an_escaped_quote_cannot_end_either_string_early() {
        // The property that makes the borrowing form safe rather than
        // merely cheap.
        let mut a = r#""a\"b";x"#;
        assert_eq!(quoted_string(&mut a).unwrap(), r#"a"b"#);
        assert_eq!(a, ";x");
        let mut b = r#""a\"b";x"#;
        assert_eq!(quoted_string_raw(&mut b).unwrap(), r#"a\"b"#);
        assert_eq!(b, ";x");
    }

    #[test]
    fn a_token_stops_at_the_first_separator() {
        let mut i = "max-age=5";
        assert_eq!(token(&mut i).unwrap(), "max-age");
        assert_eq!(i, "=5");
    }

    #[test]
    fn ows_is_optional_and_never_fails() {
        let mut i = "   x";
        ows(&mut i).unwrap();
        assert_eq!(i, "x");
        let mut j = "x";
        ows(&mut j).unwrap();
        assert_eq!(j, "x");
    }
}
