//! RFC 8288 `Link:`, parsed from header values.
//!
//! The case this exists for is a paginated API: a server answers with
//! `Link: <https://api/items?page=2>; rel="next"` and the caller wants the
//! next URL without writing a splitter of their own. That is the whole of
//! it — no link relation is *acted* on here, and nothing dereferences
//! anything.
//!
//! # A grammar, not a cut — measured against the rule this crate already has
//!
//! This workspace's rule is that a parser combinator library pays where
//! there is a grammar and charges where there is a *cut* (`charset` grew,
//! `Cache-Control` shrank, `WWW-Authenticate` shrank and lost a defect
//! with it). `Link` is a grammar, and for `WWW-Authenticate`'s exact
//! reason: **the comma that separates two link-values is the same comma
//! that a quoted parameter value may contain**, and so is the semicolon
//! that separates a link-value's own parameters. Nothing local tells them
//! apart, so a hand-written splitter has to track quote state and then
//! look ahead; a combinator does not need either — the parameter list
//! stops where `link-param` fails to match, and the outer list takes the
//! comma.
//!
//! The `<`…`>` around the target is the one cut in the value, and it is
//! one `take_till`, because a URI-Reference cannot contain `>`. That it
//! *can* contain a comma is the second place a naive `split(',')` breaks,
//! and it is pinned by a test rather than argued.
//!
//! # The relation is not a key, and this type is not a map
//!
//! RFC 8288 §3.3 lets one header carry the same relation twice — two
//! `rel="item"` links to two different items is the ordinary case — and it
//! lets *one* link-value carry several relations at once
//! (`rel="next last"`). A `HashMap<String, Link>` silently drops one of
//! the first and cannot express the second, so [`Links`] is an **ordered
//! list** with map-shaped accessors over it: [`Links::get`] answers the
//! first link carrying a relation and [`Links::get_all`] answers every
//! one. That is `http::HeaderMap`'s own answer to the same question, which
//! is the precedent worth following over inventing a second shape for it.
//!
//! What it costs is that `get` picks by position rather than by anything
//! about the link, so a caller who cares which of two `rel="item"` links
//! they get must use `get_all` and choose. Order is the header's own,
//! across every copy of it, which is the only order there is to offer.
//!
//! # What it does not do
//!
//! **`anchor` is parsed and not interpreted.** §3.2 makes it re-target the
//! link's *context*, so a link with one describes a relation from
//! somewhere other than the response — acting on that needs a notion of
//! context this crate does not have, and inventing one would make
//! `links()` answer questions about a document nobody fetched.
//!
//! **`title*` is not decoded.** RFC 8187's encoded form carries a charset
//! label, and decoding one means a charset decoder — which in this
//! workspace is `hclient`'s `charset` feature and a megabyte of tables,
//! where this is the sans-io leaf whose dependency count is guarded. The
//! parameter is handed over as written, which is what
//! [`Link::param`] promises for every parameter.

use std::ops::Index;

use http::{HeaderMap, Uri};
use winnow::combinator::{alt, delimited, opt, preceded, repeat, separated};
use winnow::token::take_till;
use winnow::{ModalResult, Parser};

// RFC 9110 §5.6's three productions, shared rather than written a fourth
// time — see `crate::field` for the count and for why there are two
// `quoted-string`s.
use crate::field::{ows, quoted_string, token};
use crate::uri::{UriError, resolve_reference};

/// One `link-value`: a target and the parameters that came with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    target: String,
    /// The relation types of the **first** `rel` parameter, lowercased.
    ///
    /// First rather than all of them because §3.3 says so in as many
    /// words: `rel` must not appear more than once in a link-value, and
    /// occurrences after the first must be ignored. The later ones are
    /// still in `params`, so nothing is lost — they are simply not
    /// relations.
    rels: Vec<Box<str>>,
    /// Every parameter, in the order it was written, name lowercased and
    /// value unescaped. Kept as a list rather than a map for the reason
    /// [`Links`] is one: a repeat is a fact about the header, and a map
    /// would drop it on the floor.
    params: Vec<(Box<str>, Option<String>)>,
}

impl Link {
    /// The target, **as the header wrote it** — which may be a relative
    /// reference.
    ///
    /// A resolved target is what a caller usually wants, and resolving
    /// needs a base this crate cannot have: a sans-io parser is handed a
    /// string, not a response. So the resolution lives one layer up, at
    /// `hclient::Response::links`, which knows the URL that answered; the
    /// mechanism it uses is [`Links::resolved_against`] below, and it is
    /// public so that a caller parsing a header by hand can reach the same
    /// answer.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The relation types this link carries, lowercased.
    ///
    /// Plural because §3.3's `rel` value is a space-separated list:
    /// `rel="next last"` is one link that answers to both names.
    pub fn rels(&self) -> impl ExactSizeIterator<Item = &str> {
        self.rels.iter().map(|r| &**r)
    }

    /// Whether this link carries `rel`, compared case-insensitively per
    /// §3.3.
    pub fn has_rel(&self, rel: &str) -> bool {
        self.rels.iter().any(|r| r.eq_ignore_ascii_case(rel))
    }

    /// The value of the first parameter called `name`, compared
    /// case-insensitively.
    ///
    /// **`None` is two facts, and [`Self::has_param`] is what separates
    /// them.** A parameter may be written with no value at all — RFC
    /// 8288's `link-param = token BWS [ "=" BWS ( token / quoted-string )
    /// ]` makes the `=` optional — so `; nofollow` is a parameter that is
    /// present and has nothing to hand back. This answers `None` for that
    /// and for a parameter that is not there; `has_param` answers `true`
    /// for the first and `false` for the second. The same shape
    /// `hclient`'s `(timing.tls, tls_version)` pair already uses to
    /// separate *there was no handshake* from *this backend does not
    /// describe one*.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .and_then(|(_, v)| v.as_deref())
    }

    /// Whether a parameter called `name` is present at all, with or
    /// without a value.
    pub fn has_param(&self, name: &str) -> bool {
        self.params
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(name))
    }

    /// Every parameter, in the order written: name lowercased, value
    /// unescaped, `None` where the parameter had no `=`.
    pub fn params(&self) -> impl ExactSizeIterator<Item = (&str, Option<&str>)> {
        self.params.iter().map(|(n, v)| (&**n, v.as_deref()))
    }

    /// This link's target resolved against `base`, RFC 3986 §5.2.
    pub fn resolve(&self, base: &Uri) -> Result<Uri, UriError> {
        resolve_reference(base, &self.target)
    }
}

/// Every link a response carried, in the order the header wrote them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    links: Vec<Link>,
}

impl Links {
    /// Parses one `Link` header value.
    ///
    /// **Nothing here is an error.** RFC 8288 §3 tells a recipient to
    /// ignore a link-value it cannot parse, so a malformed element ends
    /// the list rather than poisoning the ones before it — which is
    /// `Cache-Control`'s rule one crate over, and for the same reason: a
    /// header with one bad element still carries the good ones, and a
    /// caller who lost them all to a stray quote would have no way to
    /// notice.
    pub fn parse_value(value: &str) -> Self {
        let mut input = value;
        // Not `Parser::parse`: the tail that does not parse is discarded
        // and the elements before it are kept. `opt` is what lets an empty
        // element — RFC 9110 §5.6.1's legacy `<a>,,<b>` — be skipped
        // rather than end the list.
        let list: Vec<Option<Link>> =
            preceded(ows, separated(0.., opt(link_value), (ows, ',', ows)))
                .parse_next(&mut input)
                .unwrap_or_default();
        Self {
            links: list.into_iter().flatten().collect(),
        }
    }

    /// Every `Link` header on a message, in order.
    ///
    /// A value that is not valid UTF-8 is skipped rather than lossily
    /// converted: a URL made of replacement characters is a URL that
    /// points somewhere else.
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let mut links = Vec::new();
        for value in headers.get_all(http::header::LINK) {
            let Ok(value) = value.to_str() else { continue };
            links.extend(Self::parse_value(value).links);
        }
        Self { links }
    }

    /// The same links with every target that resolves against `base`
    /// replaced by its resolved form, RFC 3986 §5.2.
    ///
    /// **A target that does not resolve is left exactly as written**, and
    /// that is the choice worth knowing about. The alternative is dropping
    /// it, and a link dropped for being unresolvable is a fact the server
    /// stated disappearing with nothing said — which is the silent-loss
    /// shape this workspace refuses everywhere else. What survives instead
    /// is a `target()` that is still the header's own text, so a caller
    /// who hands it to something expecting an absolute URL gets a typed
    /// error at the point of use rather than a wrong request.
    ///
    /// Resolution can only fail for a reference that is not a URI at all
    /// (a raw space, a control character): `base` here is a URL that
    /// already answered a request.
    pub fn resolved_against(mut self, base: &Uri) -> Self {
        for link in &mut self.links {
            if let Ok(resolved) = resolve_reference(base, &link.target) {
                link.target = resolved.to_string();
            }
        }
        self
    }

    /// The first link carrying `rel`, compared case-insensitively.
    pub fn get(&self, rel: &str) -> Option<&Link> {
        self.links.iter().find(|l| l.has_rel(rel))
    }

    /// Every link carrying `rel`, in header order — the accessor for the
    /// case §3.3 allows and [`Self::get`] cannot express.
    pub fn get_all<'a>(&'a self, rel: &'a str) -> impl Iterator<Item = &'a Link> {
        self.links.iter().filter(move |l| l.has_rel(rel))
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Link> {
        self.links.iter()
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }
}

impl<'a> IntoIterator for &'a Links {
    type Item = &'a Link;
    type IntoIter = std::slice::Iter<'a, Link>;

    fn into_iter(self) -> Self::IntoIter {
        self.links.iter()
    }
}

impl IntoIterator for Links {
    type Item = Link;
    type IntoIter = std::vec::IntoIter<Link>;

    fn into_iter(self) -> Self::IntoIter {
        self.links.into_iter()
    }
}

/// `links["next"]`, which **panics** when there is no such relation.
///
/// Exactly `http::HeaderMap`'s bargain, and taken because that is the type
/// a reader of this crate already knows: indexing is for the call site
/// that knows the link is there, and [`Links::get`] is for every other.
impl Index<&str> for Links {
    type Output = Link;

    fn index(&self, rel: &str) -> &Link {
        self.get(rel)
            .unwrap_or_else(|| panic!("no link with rel=`{rel}`"))
    }
}

/// `link-value = "<" URI-Reference ">" *( OWS ";" OWS link-param )`.
fn link_value(i: &mut &str) -> ModalResult<Link> {
    // The one cut in the grammar, and it is one `take_till`: a
    // URI-Reference cannot contain `>` (RFC 3986 §2 makes it excluded),
    // so nothing has to be tracked to find the end. It *can* contain a
    // comma, which is why the target is consumed before the outer list
    // ever looks for its separator.
    let target = delimited('<', take_till(0.., '>'), '>').parse_next(i)?;
    let params: Vec<(String, Option<String>)> =
        repeat(0.., preceded((ows, ';', ows), link_param)).parse_next(i)?;

    // §3.3: `rel` must not appear more than once, and occurrences after
    // the first are ignored. Splitting on ASCII whitespace is the same
    // section's list form.
    let rels = params
        .iter()
        .find(|(n, _)| n == "rel")
        .and_then(|(_, v)| v.as_deref())
        .map(|v| {
            v.split_ascii_whitespace()
                .map(|r| r.to_ascii_lowercase().into_boxed_str())
                .collect()
        })
        .unwrap_or_default();

    Ok(Link {
        target: target.to_owned(),
        rels,
        params: params
            .into_iter()
            .map(|(n, v)| (n.into_boxed_str(), v))
            .collect(),
    })
}

/// `link-param = token BWS [ "=" BWS ( token / quoted-string ) ]`.
///
/// The value is **unescaped**, which is `digest.rs`'s answer rather than
/// `directives.rs`'s: a `title` is free text a deployment writes, so a
/// `\"` inside one is a quote the caller should see, where a
/// `Cache-Control` argument is a field-name list that can never contain
/// one.
fn link_param(i: &mut &str) -> ModalResult<(String, Option<String>)> {
    let name = token.parse_next(i)?;
    let value = opt(preceded(
        (ows, '=', ows),
        alt((quoted_string, token.map(str::to_owned))),
    ))
    .parse_next(i)?;
    Ok((name.to_ascii_lowercase(), value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &str) -> Links {
        Links::parse_value(v)
    }

    fn base() -> Uri {
        "https://api.example.com/items?page=1".parse().unwrap()
    }

    #[test]
    fn the_paginated_case_reads_as_one_line() {
        let links = parse(
            r#"<https://api.example.com/items?page=2>; rel="next", \
               <https://api.example.com/items?page=9>; rel="last""#
                .replace("\\\n               ", "")
                .as_str(),
        );
        assert_eq!(
            links["next"].target(),
            "https://api.example.com/items?page=2"
        );
        assert_eq!(
            links["last"].target(),
            "https://api.example.com/items?page=9"
        );
        assert!(links.get("prev").is_none());
    }

    #[test]
    fn a_relation_may_appear_twice_and_neither_is_dropped() {
        // The whole reason this is a list with map-shaped accessors and
        // not a map: a `HashMap<String, Link>` answers one of these two
        // and loses the other with nothing said.
        let links = parse(r#"</a>; rel="item", </b>; rel="item""#);
        let all: Vec<_> = links.get_all("item").map(Link::target).collect();
        assert_eq!(all, vec!["/a", "/b"]);
        assert_eq!(
            links["item"].target(),
            "/a",
            "`get` is the first, in header order"
        );
    }

    #[test]
    fn one_link_value_may_carry_several_relations() {
        let links = parse(r#"</p9>; rel="next last""#);
        assert_eq!(links["next"].target(), "/p9");
        assert_eq!(links["last"].target(), "/p9");
        assert_eq!(links.len(), 1, "one link, reachable under two names");
        let rels: Vec<_> = links.iter().next().unwrap().rels().collect();
        assert_eq!(rels, vec!["next", "last"]);
    }

    #[test]
    fn relations_are_case_insensitive_both_ways() {
        // §3.3. Both directions matter: a server shouting `NEXT` and a
        // caller shouting `Next` must meet in the middle.
        let links = parse(r#"</p2>; rel="NEXT""#);
        assert_eq!(links["next"].target(), "/p2");
        assert_eq!(links["Next"].target(), "/p2");
        assert!(links.iter().next().unwrap().has_rel("nExT"));
        // Stored lowercased, which is what makes `rels()` comparable
        // without every caller writing `eq_ignore_ascii_case`.
        assert_eq!(
            links.iter().next().unwrap().rels().collect::<Vec<_>>(),
            vec!["next"]
        );
    }

    #[test]
    fn a_comma_inside_a_quoted_parameter_does_not_split_the_list() {
        // The defect a hand-written `split(',')` ships with, and the
        // reason this is a combinator: nothing local tells this comma from
        // the one separating two link-values.
        let links = parse(r#"</a>; rel="next"; title="one, two", </b>; rel="prev""#);
        assert_eq!(links.len(), 2);
        assert_eq!(links["next"].param("title"), Some("one, two"));
        assert_eq!(links["prev"].target(), "/b");
    }

    #[test]
    fn a_comma_inside_the_target_does_not_split_the_list() {
        // A URI-Reference may contain a comma — RFC 3986 §2.2 makes it a
        // sub-delim — and the target is consumed by its own `<`…`>` cut
        // before the outer list looks for a separator.
        let links = parse(r#"</items?ids=1,2,3>; rel="next", </b>; rel="prev""#);
        assert_eq!(links.len(), 2);
        assert_eq!(links["next"].target(), "/items?ids=1,2,3");
    }

    #[test]
    fn a_semicolon_inside_a_quoted_parameter_does_not_start_a_parameter() {
        let links = parse(r#"</a>; rel="next"; title="a;b"; type="text/plain""#);
        assert_eq!(links["next"].param("title"), Some("a;b"));
        assert_eq!(links["next"].param("type"), Some("text/plain"));
    }

    #[test]
    fn a_parameter_with_no_value_is_present_and_valueless() {
        // The pair is the answer: one method cannot separate *absent* from
        // *present with nothing to say*.
        let links = parse(r#"</a>; rel="next"; nofollow"#);
        let l = &links["next"];
        assert_eq!(l.param("nofollow"), None);
        assert!(l.has_param("nofollow"));
        assert_eq!(l.param("title"), None);
        assert!(!l.has_param("title"));
    }

    #[test]
    fn an_escaped_quote_in_a_parameter_is_unescaped() {
        // `digest.rs`'s answer rather than `directives.rs`'s: a title is
        // free text, so the caller wants the quote rather than the
        // backslash.
        let links = parse(r#"</a>; rel="next"; title="say \"hello\"""#);
        assert_eq!(links["next"].param("title"), Some(r#"say "hello""#));
    }

    #[test]
    fn a_parameter_name_is_case_insensitive_and_the_value_is_not() {
        let links = parse(r#"</a>; REL="next"; Title="Keep Me""#);
        assert_eq!(links["next"].param("title"), Some("Keep Me"));
        assert_eq!(links["next"].param("TITLE"), Some("Keep Me"));
    }

    #[test]
    fn an_unquoted_parameter_value_is_a_token() {
        let links = parse("</a>; rel=next");
        assert_eq!(links["next"].target(), "/a");
    }

    #[test]
    fn a_repeated_rel_is_the_first_one_and_the_rest_are_ordinary_parameters() {
        // §3.3 in as many words: occurrences after the first MUST be
        // ignored. Ignored as *relations* — they are still parameters, so
        // nothing is thrown away.
        let links = parse(r#"</a>; rel="next"; rel="prev""#);
        assert_eq!(links["next"].target(), "/a");
        assert!(links.get("prev").is_none());
        assert_eq!(links.iter().next().unwrap().params().count(), 2);
    }

    #[test]
    fn an_empty_element_is_skipped_rather_than_ending_the_list() {
        // RFC 9110 §5.6.1's legacy allowance for `<a>,,<b>`.
        let links = parse(r#"</a>; rel="next",, </b>; rel="prev""#);
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn a_malformed_tail_keeps_the_elements_before_it() {
        // The rule `Cache-Control` already follows here: one bad element
        // must not take the good ones with it.
        let links = parse(r#"</a>; rel="next", this is not a link-value"#);
        assert_eq!(links.len(), 1);
        assert_eq!(links["next"].target(), "/a");
    }

    #[test]
    fn a_value_with_no_link_at_all_is_empty_rather_than_an_error() {
        assert!(parse("").is_empty());
        assert!(parse("nonsense").is_empty());
        assert!(parse("<unterminated; rel=next").is_empty());
    }

    #[test]
    fn a_relative_target_is_handed_back_as_written_until_it_is_resolved() {
        // The parser has no base and invents none; `resolved_against` is
        // where a base arrives, and `hclient::Response::links` is the
        // caller that has one.
        let links = parse(r#"</items?page=2>; rel="next""#);
        assert_eq!(links["next"].target(), "/items?page=2");

        let resolved = links.resolved_against(&base());
        assert_eq!(
            resolved["next"].target(),
            "https://api.example.com/items?page=2"
        );
    }

    #[test]
    fn resolution_leaves_an_absolute_target_alone() {
        let links = parse(r#"<https://other.example/x>; rel="next""#).resolved_against(&base());
        assert_eq!(links["next"].target(), "https://other.example/x");
    }

    #[test]
    fn a_target_that_cannot_be_resolved_survives_as_written() {
        // A raw space is not a URI and RFC 3986 §5.2 has nothing to say
        // about it. Dropping the link would lose a fact the server stated
        // with nothing said; leaving it means `target()` is still the
        // header's own text.
        let links = parse("</a b>; rel=next").resolved_against(&base());
        assert_eq!(links["next"].target(), "/a b");
    }

    #[test]
    fn resolve_is_reachable_per_link_for_a_caller_holding_a_base() {
        let links = parse(r#"</items?page=2>; rel="next""#);
        assert_eq!(
            links["next"].resolve(&base()).unwrap().to_string(),
            "https://api.example.com/items?page=2"
        );
    }

    #[test]
    fn every_copy_of_the_header_contributes_in_order() {
        let mut headers = HeaderMap::new();
        headers.append(http::header::LINK, r#"</a>; rel="next""#.parse().unwrap());
        headers.append(http::header::LINK, r#"</b>; rel="prev""#.parse().unwrap());
        let links = Links::from_headers(&headers);
        assert_eq!(
            links.iter().map(Link::target).collect::<Vec<_>>(),
            vec!["/a", "/b"]
        );
    }

    #[test]
    fn no_link_header_is_an_empty_set_rather_than_anything_to_unwrap() {
        assert!(Links::from_headers(&HeaderMap::new()).is_empty());
    }

    #[test]
    #[should_panic(expected = "no link with rel=`next`")]
    fn indexing_a_relation_that_is_not_there_panics_like_a_header_map() {
        let _ = &Links::default()["next"];
    }
}
