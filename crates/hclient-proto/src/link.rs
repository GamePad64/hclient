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
    /// Builds one, applying the same normalisations the parser does.
    ///
    /// **Public because a caller cannot otherwise build a specific link.**
    /// [`Links`] has two constructors and both take a *header*, so testing
    /// code that consumes one link meant hand-assembling a header string
    /// and hoping it parsed — which is `ProposedRedirect::new`'s argument
    /// one module over, where a type with no constructor *"reads as a wall
    /// and sends people to a network they did not need"*. Here the wall
    /// was one step worse: the string a caller assembles is input to this
    /// crate's own parser, so a test of *their* code fails on a quoting
    /// mistake in *ours*.
    ///
    /// # This applies the invariants rather than trusting them
    ///
    /// A `Link` parsed from a header and a `Link` built here must be the
    /// same thing, or the accessors mean two different things depending on
    /// where the value came from — and nothing at the call site would say
    /// which. So all three of the parser's normalisations happen here too,
    /// and none of them is the caller's to skip:
    ///
    /// - **`rels` is lowercased and split on whitespace**, so
    ///   `["Next Last"]` and `["next", "last"]` are the same two
    ///   relations. [`Link::has_rel`] compares case-insensitively anyway,
    ///   but [`Link::rels`] hands the stored form back, and a caller
    ///   comparing those strings would see `Next` where every parsed link
    ///   says `next`.
    /// - **Parameter names are lowercased**, for the same reason one step
    ///   over: [`Link::params`] is the accessor that hands them back.
    /// - **Values are taken as written** — unescaped, because that is what
    ///   the parser hands over ([`Link::param`] promises the text, not the
    ///   header's quoting). A caller writing `say "hello"` gets exactly
    ///   that back; there is no header to escape it into.
    ///
    /// The one invariant that **cannot** be applied here is §3.3's *the
    /// first `rel` wins*, because there is no first: `rels` and `params`
    /// arrive as separate arguments, which is the shape that makes the
    /// rule unstateable rather than violable. A caller who puts a `rel`
    /// into `params` gets an ordinary parameter — which is exactly what a
    /// parsed link does with a *second* `rel`, so the two agree on the one
    /// case they can both reach.
    ///
    /// ```
    /// use hclient_proto::link::Link;
    ///
    /// let link = Link::new(
    ///     "/items?page=2",
    ///     ["Next"],
    ///     [("Title", Some("page two")), ("nofollow", None)],
    /// );
    /// assert_eq!(link.target(), "/items?page=2");
    /// assert!(link.has_rel("next"));
    /// assert_eq!(link.rels().collect::<Vec<_>>(), ["next"]);
    /// assert_eq!(link.param("title"), Some("page two"));
    /// assert!(link.has_param("nofollow"));
    /// ```
    pub fn new<R, P, N, V>(target: impl Into<String>, rels: R, params: P) -> Self
    where
        R: IntoIterator,
        R::Item: AsRef<str>,
        P: IntoIterator<Item = (N, Option<V>)>,
        N: AsRef<str>,
        V: Into<String>,
    {
        Self {
            target: target.into(),
            // Split as well as lowercase: §3.3's `rel` value is a
            // space-separated list, so one argument may carry several
            // relations exactly as one header parameter does. Without the
            // split, `["next last"]` would be a single relation named
            // `next last`, which no header can produce and `has_rel`
            // would never match.
            rels: rels
                .into_iter()
                .flat_map(|r| {
                    r.as_ref()
                        .split_ascii_whitespace()
                        .map(|r| r.to_ascii_lowercase().into_boxed_str())
                        .collect::<Vec<_>>()
                })
                .collect(),
            params: params
                .into_iter()
                .map(|(n, v)| {
                    (
                        n.as_ref().to_ascii_lowercase().into_boxed_str(),
                        v.map(Into::into),
                    )
                })
                .collect(),
        }
    }

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
    ///
    /// # Errors
    ///
    /// Whatever [`resolve_reference`]
    /// returns for this link's target against `base` — see its `# Errors`.
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
    #[must_use]
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

    /// Every link, in header order, whatever its relation.
    pub fn iter(&self) -> std::slice::Iter<'_, Link> {
        self.links.iter()
    }

    /// How many links were parsed — **not** how many distinct relations
    /// there are, since one link may carry several and one relation may
    /// appear on several.
    #[must_use]
    pub fn len(&self) -> usize {
        self.links.len()
    }

    /// Whether the header carried no parseable link at all. True both for
    /// a message with no `Link:` and for one whose value did not parse,
    /// which [`Self::parse_value`] deliberately does not distinguish.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }
}

/// Collects links into a set, for the caller who built one with
/// [`Link::new`] and needs to hand it to something taking a [`Links`].
///
/// **`FromIterator` rather than a named constructor, and the reason is
/// that this is not a second way to parse.** [`Links::parse_value`] is the
/// constructor a caller reaches for, and it stays the one that answers
/// *what did this header say*; a `Links::new(Vec<Link>)` beside it would
/// read as an alternative to it rather than as the plumbing it is.
///
/// It is needed at all because `parse_value` **cannot express every
/// [`Link`]**, which is a fact about the grammar rather than about
/// convenience: a target containing `>` has no header form (the parser
/// ends the target there, RFC 3986 §2 excluding it from a URI-Reference),
/// and neither does a parameter name outside RFC 9110 §5.6.2's `token`.
///
/// Measured rather than assumed, and the answer is worse than
/// *truncated*: `</a?q=<x>>; rel=next` parses to a link whose target is
/// `/a?q=<x` and which carries **no relation at all**, because everything
/// after the first `>` is a malformed tail and this module discards one
/// by design. So round-tripping such a link through a header string does
/// not cost a character, it costs the link's identity — silently, which
/// is the shape this workspace refuses.
///
/// Order is kept exactly as iterated, because order is the whole of what
/// [`Links::get`] and [`Links::iter`] promise.
///
/// ```
/// use hclient_proto::link::{Link, Links};
///
/// let links: Links = [
///     Link::new("/p2", ["next"], [] as [(&str, Option<&str>); 0]),
///     Link::new("/p9", ["last"], [] as [(&str, Option<&str>); 0]),
/// ]
/// .into_iter()
/// .collect();
/// assert_eq!(links["next"].target(), "/p2");
/// assert_eq!(links.len(), 2);
/// ```
impl FromIterator<Link> for Links {
    fn from_iter<T: IntoIterator<Item = Link>>(iter: T) -> Self {
        Self {
            links: iter.into_iter().collect(),
        }
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

    /// **The thing a consumer wants to do and could not**: build one
    /// specific link and read every accessor back off it, with no header
    /// string and no response.
    #[test]
    fn a_built_link_answers_every_accessor() {
        let link = Link::new(
            "/items?page=2",
            ["next"],
            [("title", Some("page two")), ("nofollow", None)],
        );

        assert_eq!(link.target(), "/items?page=2");
        assert_eq!(link.rels().collect::<Vec<_>>(), vec!["next"]);
        assert!(link.has_rel("next"));
        assert!(!link.has_rel("prev"));
        assert_eq!(link.param("title"), Some("page two"));
        // The valueless-parameter pair, which is the one place two
        // accessors are needed to say one thing — see `Link::param`.
        assert_eq!(link.param("nofollow"), None);
        assert!(link.has_param("nofollow"));
        assert!(!link.has_param("type"));
        assert_eq!(link.params().count(), 2);
        assert_eq!(
            link.resolve(&base()).unwrap().to_string(),
            "https://api.example.com/items?page=2"
        );
    }

    /// **A built link and a parsed one are the same value**, which is what
    /// makes the constructor safe to have: if they were not, every
    /// accessor would mean two things depending on where the link came
    /// from, and nothing at the call site would say which.
    ///
    /// `assert_eq!` on the whole `Link` rather than field by field,
    /// because the fields are private and the derived `PartialEq` is what
    /// a caller comparing two links would get. The corners are chosen to
    /// be the three normalisations: a shouted relation, a shouted
    /// parameter name, and an escaped value that the header quotes and the
    /// constructor does not.
    #[test]
    fn a_built_link_equals_the_same_link_parsed_from_a_header() {
        let parsed = Links::parse_value(r#"</a>; REL="Next Last"; Title="say \"hi\""; nofollow"#);
        let built = Link::new(
            "/a",
            ["Next Last"],
            [
                ("REL", Some("Next Last")),
                ("Title", Some(r#"say "hi""#)),
                ("nofollow", None),
            ],
        );
        assert_eq!(parsed.iter().next().unwrap(), &built);
    }

    /// §3.3's `rel` is a space-separated list, so one argument may carry
    /// several relations exactly as one header parameter does.
    ///
    /// Without the split, `["next last"]` would be a single relation of
    /// that name — which no header can produce, and which `has_rel` would
    /// never match for either half.
    #[test]
    fn a_relation_argument_is_split_on_whitespace_like_the_header_form() {
        let built = Link::new("/p9", ["next last"], [] as [(&str, Option<&str>); 0]);
        assert_eq!(built.rels().collect::<Vec<_>>(), vec!["next", "last"]);
        assert!(built.has_rel("next") && built.has_rel("last"));
        // And the same two written as two arguments, which is the form a
        // caller is likelier to reach for, agree exactly.
        assert_eq!(
            Link::new("/p9", ["next", "last"], [] as [(&str, Option<&str>); 0]),
            built
        );
    }

    /// The constructor **lowercases** a relation and a parameter name, and
    /// leaves a value alone. The value row is the control: a constructor
    /// that lowercased everything would pass the first two assertions.
    #[test]
    fn the_constructor_lowercases_names_and_leaves_values_alone() {
        let built = Link::new("/a", ["NEXT"], [("Title", Some("Keep Me"))]);
        assert_eq!(built.rels().collect::<Vec<_>>(), vec!["next"]);
        assert_eq!(
            built.params().collect::<Vec<_>>(),
            vec![("title", Some("Keep Me"))]
        );
    }

    /// **A `Links` can be assembled from links a caller built**, which is
    /// what a test of code taking a `&Links` needs.
    ///
    /// The targets here are the reason this is not *"just parse a header"*:
    /// a `>` inside a target has no header form at all, so the round trip
    /// through a string would silently truncate it.
    #[test]
    fn links_collects_from_built_links_including_ones_no_header_can_write() {
        let links: Links = [
            Link::new("/a?q=<x>", ["next"], [] as [(&str, Option<&str>); 0]),
            Link::new("/b", ["prev"], [] as [(&str, Option<&str>); 0]),
        ]
        .into_iter()
        .collect();

        assert_eq!(links.len(), 2);
        assert_eq!(links["next"].target(), "/a?q=<x>");
        assert_eq!(
            links.iter().map(Link::target).collect::<Vec<_>>(),
            vec!["/a?q=<x>", "/b"],
            "order is the iterator's, which is what `get` and `iter` promise"
        );

        // The control for the claim in the doc, and it is worse than
        // "truncated": the target ends at the first `>`, so the rest of
        // the value — `>; rel=next` — is a malformed tail, which
        // `parse_value` discards by its own rule. The link survives with
        // a shortened target and **no relation at all**, so a caller
        // round-tripping through a header does not merely lose a
        // character, they lose the link's identity with nothing said.
        let via_header = Links::parse_value("</a?q=<x>>; rel=next");
        assert_eq!(via_header.len(), 1);
        let only = via_header.iter().next().unwrap();
        assert_eq!(only.target(), "/a?q=<x");
        assert_eq!(only.rels().count(), 0, "`rel=next` went with the tail");
    }

    /// An empty `Links` collected from nothing is the empty one, so the
    /// two ways of having no links agree.
    #[test]
    fn collecting_no_links_is_the_default() {
        assert_eq!(
            std::iter::empty::<Link>().collect::<Links>(),
            Links::default()
        );
    }
}
