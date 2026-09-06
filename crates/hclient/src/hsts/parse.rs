//! RFC 6797 §6.1's `Strict-Transport-Security` grammar.
//!
//! The productions are [`hclient_proto::field`]'s — §6.1 names `token`
//! and `quoted-string` from RFC 2616 §2.2, which are the same two that
//! module already carries for `Cache-Control`, `Link` and `charset`. This
//! is its third consumer, which is what that module's own doc says it
//! exists for.
//!
//! # The grammar parses, and the rules refuse
//!
//! `directive` below is deliberately total: it reads a name and an
//! optional value and judges neither. Every refusal §6.1 lists —
//! a missing `max-age`, a repeated directive, a `max-age` that is not
//! `1*DIGIT`, a value on `includeSubDomains` — is applied in [`parse`]
//! over the parsed list.
//!
//! That is not a stylistic split. §6.1 requirement 4 refuses **the whole
//! field value**, so a rule applied inside the parser would have to
//! abort a `separated` mid-list and could not distinguish *"this
//! directive is malformed"* from *"the list ended here"* — which is
//! exactly the trailing-rubbish case requirement 4 also covers. Parsing
//! the shape first and judging the collection second makes both one
//! answer.

use winnow::combinator::{alt, opt, preceded, separated};
use winnow::{ModalResult, Parser};

use hclient_proto::field::{ows, quoted_string, token};

/// What one well-formed `Strict-Transport-Security` field value said.
///
/// **There is no `Clear` arm, unlike `Alt-Svc`'s `FieldValue`, and the
/// difference is which layer the instruction belongs to.** RFC 7838 has a
/// literal `clear` token, so the *parser* has to report it. §6.1.1 gives
/// deletion no syntax of its own: `max-age=0` is an ordinary directive
/// with an ordinary value, and it is §8.1 — the rules — that reads a zero
/// as *"cease regarding the host as a Known HSTS Host"*. So a
/// `Directives` with `max_age: 0` is what this type hands back, and
/// [`Hsts::note`](super::Hsts::note) is where the deletion happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Directives {
    /// §6.1.1's REQUIRED `max-age`, in seconds.
    ///
    /// `u64`, saturating on overflow rather than failing to parse: the
    /// production is `1*DIGIT` with no stated ceiling, so
    /// `max-age=99999999999999999999` is *syntactically valid* and
    /// refusing it would be this parser inventing a rule §6.1 does not
    /// have. What an absurd value means is the rules layer's question,
    /// and [`MAX_AGE_CAP`](super::MAX_AGE_CAP) is its answer.
    pub max_age: u64,
    /// §6.1.2's OPTIONAL, valueless `includeSubDomains`.
    pub include_subdomains: bool,
}

impl Directives {
    /// Parse one field value — `None` where §6.1 requirement 4 says to ignore
    /// it.
    ///
    /// # What makes a value malformed, and why each is refused
    ///
    /// §6.1 requirement 4 is *"UAs MUST ignore any STS header field
    /// containing directives, or other header field value data, that does
    /// not conform to the syntax defined in this specification"* — one
    /// refusal for the whole value rather than a best-effort salvage, which
    /// is why every branch here answers `None` rather than a partial answer.
    ///
    /// - **No `max-age`.** §6.1.1 makes it REQUIRED, and a value carrying
    ///   only `includeSubDomains` states a scope for a lifetime nobody gave.
    /// - **A repeated directive.** §6.1 requirement 2: *"All directives MUST
    ///   appear only once in an STS header field."* Both copies are refused
    ///   rather than one being preferred, because the RFC gives no rule for
    ///   choosing and a client taking the first would disagree with one
    ///   taking the last.
    /// - **A `max-age` that is not `1*DIGIT`.** §6.1.1's `delta-seconds`.
    ///   `max-age=abc`, `max-age=` and a bare `max-age` are all this.
    /// - **A value on `includeSubDomains`.** §6.1.2 calls it *"a valueless
    ///   directive"*, so `includeSubDomains=1` does not conform.
    /// - **Trailing rubbish** — the *"or other header field value data"*
    ///   half of requirement 4.
    ///
    /// An **unrecognised** directive is none of those: §6.1 requirement 5
    /// says to ignore it and process the rest, so `max-age=1; preload` is a
    /// well-formed value here and `preload` is dropped. That is the one place
    /// this parser is deliberately permissive, and it is the RFC's own
    /// instruction rather than leniency — `preload` is not in RFC 6797 at
    /// all, and a client refusing it would refuse most of the real
    /// deployment of HSTS on the web.
    pub fn parse(value: &str) -> Option<Self> {
        let mut input = value;
        let list: Vec<Raw<'_>> = separated(0.., raw_directive, (ows, ';', ows))
            .parse_next(&mut input)
            .ok()?;
        let _ = ows(&mut input);
        // Requirement 4's "or other header field value data": anything left
        // over means the value did not conform, whatever the prefix looked
        // like.
        if !input.is_empty() {
            return None;
        }

        let mut max_age: Option<u64> = None;
        let mut include_subdomains: Option<bool> = None;
        for Raw { name, value } in list {
            // §6.1 requirement 3: "Directive names are case-insensitive."
            if name.eq_ignore_ascii_case("max-age") {
                // §6.1.1's `delta-seconds`, read "after quoted-string
                // unescaping, if necessary" — which is why the value arrives
                // through the unescaping `quoted_string` rather than the
                // borrowing one: `max-age="31536000"` is a form §6.1's
                // grammar permits.
                let v = value?;
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                // Saturating rather than refusing — see `Directives::max_age`.
                if max_age
                    .replace(v.parse::<u64>().unwrap_or(u64::MAX))
                    .is_some()
                {
                    return None; // requirement 2
                }
            } else if name.eq_ignore_ascii_case("includesubdomains") {
                // §6.1.2: "a valueless directive".
                if value.is_some() {
                    return None;
                }
                if include_subdomains.replace(true).is_some() {
                    return None; // requirement 2
                }
            }
            // else: requirement 5 — ignore what we do not recognise, and go
            // on to process the rest.
        }

        Some(Directives {
            // §6.1.1: REQUIRED.
            max_age: max_age?,
            include_subdomains: include_subdomains.unwrap_or(false),
        })
    }
}

/// One `directive` as written: a name, and the value it carried if any.
///
/// Empty is representable — §6.1's grammar is
/// `[ directive ] *( ";" [ directive ] )`, so `max-age=1;` and
/// `;max-age=1` both conform, the brackets making each element optional —
/// and an empty name matches neither directive below, so it falls
/// through requirement 5's arm and is ignored.
struct Raw<'a> {
    name: &'a str,
    value: Option<String>,
}

fn raw_directive<'a>(i: &mut &'a str) -> ModalResult<Raw<'a>> {
    let name = opt(token).parse_next(i)?.unwrap_or("");
    let value = opt(preceded(
        (ows, '=', ows),
        alt((quoted_string, token.map(str::to_owned))),
    ))
    .parse_next(i)?;
    Ok(Raw { name, value })
}
