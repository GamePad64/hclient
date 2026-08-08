//! The one place a string becomes an [`http::Uri`], and the one place a
//! URI reference is resolved against a base — RFC 3986 §5.
//!
//! One implementation for the whole client, because there are exactly two
//! places where a relative reference gets resolved against something, and
//! they must share one rule: the `Location:` from a response
//! (`redirect::decide`), and the request URI against
//! `ClientBuilder::base_url` (`http_ng::Client`). While these were two
//! separate functions, the second one was a silent no-op — and had it been
//! written separately, nothing would have stopped it from resolving
//! differently, leaving the same client understanding `/x` two different
//! ways depending on who sent it.
//!
//! # Why the resolution is written out by hand
//!
//! [`resolve_reference`] used to be `url::Url::parse(base).join(reference)`.
//! That single call put `url` → `idna` → `icu_normalizer` +
//! `icu_properties` into every build of `http-ng-proto`, the sans-io crate
//! every target includes: measured from vendored sources,
//! `icu_properties_data` 1.9 MB, `idna` 1004 KB, `icu_collections` 820 KB,
//! `icu_normalizer_data` 452 KB, almost entirely Unicode tables — for one
//! URI join. On a 256–512 KB microcontroller that is more than the whole
//! flash budget.
//!
//! What replaces it is RFC 3986 §5.2 and nothing else: the reference
//! transform (§5.2.2), the merge (§5.2.3), `remove_dot_segments` (§5.2.4)
//! and the recomposition (§5.3), on the reference exactly as written.
//! `url` implements the WHATWG URL Standard, which is RFC 3986 plus a
//! layer of normalisation and repair; every place the two disagree is
//! enumerated and pinned against `url` itself in
//! `tests/uri_resolution.rs`, where `url` stays on as a **dev-dependency**
//! for exactly that purpose — it is the incumbent, and the only oracle
//! this module has.
//!
//! # IDN, and why it is a feature rather than a casualty
//!
//! The Unicode tables above exist for internationalised domain names, and
//! dropping `url` outright would have dropped IDN with them. It would also
//! have been a smaller loss than it looks, because what this project had
//! before was not IDN support but IDN *inconsistency*:
//!
//! ```text
//! client.get("https://münchen.de/x")
//!   with no base_url  ->  error: invalid uri character
//!   with any base_url ->  ok, https://xn--mnchen-3ya.de/x
//! ```
//!
//! The difference was never a decision. Without a base, `http_ng`'s
//! `effective_uri` handed the string to `http::Uri`, which rejects a
//! non-ASCII authority; with a base, it went through `resolve_reference`
//! and `url`'s IDNA punycoded it. `Location:` on a redirect took the
//! second path for the same reason, and a browser did its own IDNA in its
//! own URL parser regardless.
//!
//! So IDN now lives here, at the single boundary where a string becomes a
//! `Uri` ([`parse`]), which every backend reaches through the same sans-io
//! crate — rather than in the `http-ng` facade, where "the same everywhere"
//! would have rested on everyone going through `Client`. It is behind the
//! `idn` feature, **on by default**; turning it off removes the Unicode
//! tables from the build entirely and turns a non-ASCII host into
//! [`UriError::NonAsciiHost`], which names the cause and says what to send
//! instead.
//!
//! **A host that is already ASCII is never handed to IDNA at all.** That
//! is what makes [`parse`] idempotent: the output of one call is pure
//! ASCII, so a second call leaves it byte for byte alone. This client
//! resolves the same URI twice as a matter of course — `Client::execute`
//! resolves it, and `sse::open` resolves it again — and a conversion that
//! shifted the host on a second pass would corrupt exactly those paths. It
//! also keeps UTS 46's own ASCII lower-casing away from hosts that never
//! asked for it.

use http::Uri;

/// Why a string could not be turned into an [`http::Uri`].
///
/// Both host-related variants exist in every build, whichever way the
/// `idn` feature is set: a feature that changes the shape of a public type
/// is not additive, and a caller matching on this enum should not have to
/// be compiled twice to do it. Only which of the two can actually occur
/// changes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UriError {
    /// The base is not usable as a base: RFC 3986 §5.2.1 requires an
    /// absolute URI, and there is nothing to resolve a relative reference
    /// against.
    #[error("`{base}` cannot be used as a base URL: a base needs a scheme and an authority")]
    UnusableBase {
        /// The offending base, as written.
        base: String,
    },
    /// The host is not ASCII and this build has no IDNA to convert it.
    /// Only reachable with the `idn` feature off.
    #[error(
        "`{host}` is not an ASCII host and this build has no IDN support: enable the `idn` \
         feature of `http-ng` (it is on by default), or supply the host in its A-label form — \
         `münchen.de` is written `xn--mnchen-3ya.de`"
    )]
    NonAsciiHost {
        /// The host, as written.
        host: String,
    },
    /// The host is not ASCII and UTS 46 refused to convert it. Only
    /// reachable with the `idn` feature on.
    #[error("`{host}` is not a usable internationalised domain name: UTS 46 rejected it")]
    NotAnIdn {
        /// The host, as written.
        host: String,
    },
    /// Everything `http::Uri` itself rejects, with its own error as the
    /// source.
    #[error("`{uri}` is not a valid URI")]
    NotAUri {
        /// The string that was parsed, after any IDNA conversion.
        uri: String,
        /// What `http::Uri` said about it.
        source: http::uri::InvalidUri,
    },
}

/// Parses `s` as an [`http::Uri`], making it ASCII on the way: a
/// non-ASCII host becomes its A-label (punycode) form, and non-ASCII
/// anywhere else becomes percent-encoded UTF-8.
///
/// **The two conversions are not interchangeable.** `münchen.de` is
/// `xn--mnchen-3ya.de` and `/café` is `/caf%C3%A9`; applying either rule
/// to the other's half produces a URI that points somewhere else. This is
/// also why the function has to find the host rather than work on the
/// string as a whole.
///
/// **This is the boundary.** Every string that becomes a `Uri` in this
/// client should arrive through here or through [`resolve_reference`],
/// which ends in here — that is what makes `https://münchen.de/x` behave
/// the same whether or not a `base_url` is configured, and on native,
/// WASI and in the browser alike. The browser receives an already-ASCII
/// host and applies its own IDNA to it, which is a no-op on A-labels, so
/// the three targets converge rather than each doing their own thing.
///
/// The conversion is UTS 46 through the `idna` crate, called exactly as
/// `url` called it (`AsciiDenyList::URL`, `Hyphens::Allow`,
/// `DnsLength::Ignore`), so a host that resolved before resolves to the
/// same A-label now. Without the `idn` feature there is no `idna` in the
/// build at all and a non-ASCII host is [`UriError::NonAsciiHost`].
///
/// An **ASCII string is passed through untouched**: no IDNA, and no
/// re-encoding of a `%` that is already there. That is what makes the
/// function idempotent (`parse(parse(s)) == parse(s)`) — its output is
/// always pure ASCII, so a second pass takes the untouched path — and the
/// client depends on it: `sse::open` resolves a URL and `Client::execute`
/// resolves the result again, and every redirect hop resolves the previous
/// hop's answer.
///
/// It also means this function does not lower-case an ASCII host the way
/// UTS 46 would, and does not apply UTS 46's validation to an ASCII
/// A-label either, so a nonsense `xn--` label reaches DNS instead of being
/// rejected here. `url` checked those; nothing else in this client did or
/// does.
pub fn parse(s: &str) -> Result<Uri, UriError> {
    match to_ascii(s)? {
        Some(rewritten) => parse_ascii(&rewritten),
        None => parse_ascii(s),
    }
}

fn parse_ascii(s: &str) -> Result<Uri, UriError> {
    s.parse::<Uri>().map_err(|source| UriError::NotAUri {
        uri: s.to_owned(),
        source,
    })
}

/// Resolves `reference` against `base` per RFC 3986 §5.
///
/// The base must be usable as one — a scheme and an authority; `http::Uri`
/// cannot even represent a scheme without an authority
/// (`"http:/x".parse::<Uri>()` is `Err`). The result goes through
/// [`parse`], so a reference carrying its own non-ASCII host is punycoded
/// here exactly as it would be anywhere else.
///
/// Three consequences of the rule that most often surprise people — all
/// three are pinned down by the tests below:
/// - a reference with its own scheme (`https://other/x`) is returned as-is,
///   the base doesn't participate (§5.2.2);
/// - a reference starting with `/` REPLACES the base's entire path, rather
///   than being appended to it;
/// - a base without a trailing slash loses its last path segment when
///   resolving a relative reference (merge, §5.3): `https://a/api` + `v1` =
///   `https://a/v1`, whereas `https://a/api/` + `v1` = `https://a/api/v1`.
///
/// # This is RFC 3986, not the WHATWG URL Standard
///
/// Until this was written by hand it was `url::Url::join`, which
/// implements WHATWG. WHATWG normalises and repairs; RFC 3986 §5 only
/// transforms. The differences that survive into the returned `Uri` are
/// listed here because they are a behaviour change to a published API, and
/// every one of them is pinned in `tests/uri_resolution.rs` against `url`
/// itself:
///
/// - **Host case is preserved**, where `url` lower-cased it:
///   `https://EXAMPLE.test/api/` + `v1` is now
///   `https://EXAMPLE.test/api/v1`. Case-insensitivity of the host is a
///   comparison rule, so it lives where the comparison is
///   (`redirect::decide`), not in a syntactic transform. `http::Uri`
///   already lower-cases the scheme by itself, and still preserves host
///   case, so this is the type's own convention.
/// - **A default port is preserved**, where `url` stripped it:
///   `https://a:443/api/` + `v1` is now `https://a:443/api/v1`. This is
///   what already happened to every request made *without* a `base_url`,
///   so the client is now consistent rather than normalising on one path
///   only. `redirect` compares origins through `port_of`, which
///   substitutes the scheme's default, so `:443` is not an origin change.
/// - **Characters that are illegal in a URI are not repaired.** `url`
///   percent-encoded a literal space and deleted tabs and newlines; here
///   the recomposed string goes to `http::Uri`, which rejects it, and the
///   caller gets a typed error. `https://a/api/` + `a b` used to resolve
///   to `https://a/api/a%20b` and is now [`UriError::NotAUri`].
/// - **`%2e` is not a dot segment.** §5.2.4 operates on the path as
///   written, so `%2e%2e/x` stays `%2e%2e/x` instead of collapsing the way
///   `../x` does. Nothing escapes the base's authority either way.
/// - **A reference whose scheme equals the base's is still absolute.**
///   §5.2.2 has a "strict" and a backward-compatible mode; this is the
///   strict one, which is what the second bullet at the top of this
///   comment has always claimed. `http:g` against `http://a/b/c/d;p?q` is
///   `http:g`, not `http://a/b/c/g`.
/// - **The authority is validated by `http::Uri`, which is the more
///   permissive of the two.** A malformed IPv6 literal (`http://[:::1]/`)
///   now survives resolution and fails later, when something tries to
///   connect to it.
///
/// A fragment is parsed and then discarded: `http::Uri` cannot carry one,
/// so it could never have survived this function, and an HTTP request
/// never sends one. `#s/../x` is still a fragment and not a path, so the
/// dot segments in it are not touched before it is dropped.
pub fn resolve_reference(base: &Uri, reference: &str) -> Result<Uri, UriError> {
    let unusable = || UriError::UnusableBase {
        base: base.to_string(),
    };
    // A base needs a scheme and an authority to be a base. `http::Uri`
    // cannot hold a scheme without an authority anyway, so the second
    // check is belt-and-braces for a `Uri` assembled through `Parts`.
    let base_scheme = base.scheme_str().ok_or_else(unusable)?;
    let base_authority = base.authority().ok_or_else(unusable)?.as_str();

    let r = Parts::split(reference);

    // RFC 3986 §5.2.2, strict. `scheme` is always the base's or the
    // reference's, and `fragment` is dropped, so neither appears here.
    let (authority, path, query) = if r.scheme.is_some() {
        // A reference with a scheme resolves to itself; the base takes no
        // part. Recomposed below with the reference's OWN scheme.
        (r.authority, remove_dot_segments(r.path), r.query)
    } else if r.authority.is_some() {
        (r.authority, remove_dot_segments(r.path), r.query)
    } else if r.path.is_empty() {
        // The empty reference (and the query-only one) keeps the base's
        // path verbatim — no dot-segment pass, per §5.2.2.
        let query = if r.query.is_some() {
            r.query
        } else {
            base.query()
        };
        (Some(base_authority), base.path().to_owned(), query)
    } else if r.path.starts_with('/') {
        (Some(base_authority), remove_dot_segments(r.path), r.query)
    } else {
        let merged = merge(base.path(), r.path);
        (Some(base_authority), remove_dot_segments(&merged), r.query)
    };

    // §5.3, recomposition.
    let scheme = r.scheme.unwrap_or(base_scheme);
    let mut out = String::with_capacity(reference.len() + base_authority.len() + 16);
    out.push_str(scheme);
    out.push(':');
    if let Some(authority) = authority {
        out.push_str("//");
        out.push_str(authority);
    }
    out.push_str(&path);
    if let Some(query) = query {
        out.push('?');
        out.push_str(query);
    }
    parse(&out)
}

/// The all-ASCII form of `s`, or `None` when it is already ASCII and can
/// be parsed as it stands.
///
/// Two conversions, and they are not interchangeable: the host becomes an
/// A-label through IDNA, everything else has its non-ASCII bytes
/// percent-encoded as UTF-8. Punycoding a path or percent-encoding a host
/// would both produce a URI that resolves somewhere else.
///
/// Returning `None` rather than a copy is what keeps the ASCII path free
/// of an allocation, of UTS 46, and of any re-encoding of a `%` that is
/// already there — which is what makes the whole function idempotent.
fn to_ascii(s: &str) -> Result<Option<String>, UriError> {
    if s.is_ascii() {
        return Ok(None);
    }
    let mut out = String::with_capacity(s.len() * 2);
    match authority_range(s).map(|a| host_range(s, a)) {
        // A non-ASCII host, the only part IDNA applies to. Non-ASCII
        // elsewhere in the authority (userinfo) is percent-encoded with
        // the rest, exactly as `url` did.
        Some(host) if !s[host.clone()].is_ascii() => {
            percent_encode_into(&s[..host.start], &mut out);
            out.push_str(&host_to_ascii(&s[host.clone()])?);
            percent_encode_into(&s[host.end..], &mut out);
        }
        _ => percent_encode_into(s, &mut out),
    }
    Ok(Some(out))
}

/// RFC 3986 §2.5: a URI is ASCII, and a non-ASCII character is carried as
/// the percent-encoded octets of its UTF-8 encoding.
///
/// Only bytes >= 0x80 are touched. An ASCII byte — including a `%` that
/// is already part of an escape, and including a space, which `http::Uri`
/// will reject — is passed through, so this neither double-encodes nor
/// repairs. `url` percent-encoded a wider set than this; the difference is
/// the "no repair of illegal characters" divergence documented on
/// [`resolve_reference`], and it is only about ASCII.
fn percent_encode_into(s: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in s.bytes() {
        if byte.is_ascii() {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(byte >> 4)] as char);
            out.push(HEX[usize::from(byte & 0x0f)] as char);
        }
    }
}

/// UTS 46, called exactly as `url` called it, so a host that resolved
/// before resolves to the same A-label now.
#[cfg(feature = "idn")]
fn host_to_ascii(host: &str) -> Result<String, UriError> {
    idna::domain_to_ascii_cow(host.as_bytes(), idna::AsciiDenyList::URL)
        .map(std::borrow::Cow::into_owned)
        .map_err(|_| UriError::NotAnIdn {
            host: host.to_owned(),
        })
}

/// Without the `idn` feature there is no `idna` in the build — this is the
/// whole point of the feature — so the only honest answer is an error that
/// says so and says what to send instead.
#[cfg(not(feature = "idn"))]
fn host_to_ascii(host: &str) -> Result<String, UriError> {
    Err(UriError::NonAsciiHost {
        host: host.to_owned(),
    })
}

/// Byte range of the authority within a URI string, or `None` if it has
/// none (a relative reference, or a scheme with an opaque path such as
/// `mailto:`).
fn authority_range(s: &str) -> Option<core::ops::Range<usize>> {
    let after_scheme = scheme_end(s).map_or(0, |i| i + 1);
    let rest = s[after_scheme..].strip_prefix("//")?;
    let start = after_scheme + 2;
    let end = rest.find(['/', '?', '#']).map_or(s.len(), |i| start + i);
    Some(start..end)
}

/// Byte range of the host within an authority: userinfo and port removed.
///
/// The port is found from the right, and only outside a bracketed IPv6
/// literal — `[::1]:8443` has three colons that are not the port's.
fn host_range(s: &str, authority: core::ops::Range<usize>) -> core::ops::Range<usize> {
    let auth = &s[authority.clone()];
    let start = match auth.rfind('@') {
        Some(i) => authority.start + i + 1,
        None => authority.start,
    };
    let hostport = &s[start..authority.end];
    let end = match hostport.rfind(']') {
        // A bracketed literal: the port, if any, follows the `]`.
        Some(bracket) => hostport[bracket..]
            .find(':')
            .map_or(authority.end, |i| start + bracket + i),
        None => hostport.rfind(':').map_or(authority.end, |i| start + i),
    };
    start..end
}

/// The five components of a URI reference, RFC 3986 Appendix B, sliced out
/// of the reference rather than copied.
struct Parts<'a> {
    scheme: Option<&'a str>,
    authority: Option<&'a str>,
    path: &'a str,
    query: Option<&'a str>,
}

impl<'a> Parts<'a> {
    fn split(s: &'a str) -> Self {
        // Fragment first: everything after the FIRST `#` is fragment, so a
        // `?` inside it is not a query and a `/../` inside it is not a
        // path. Parsed and dropped — see `resolve_reference`'s doc.
        let s = match s.find('#') {
            Some(i) => &s[..i],
            None => s,
        };
        let (s, query) = match s.find('?') {
            Some(i) => (&s[..i], Some(&s[i + 1..])),
            None => (s, None),
        };
        let (scheme, s) = match scheme_end(s) {
            Some(i) => (Some(&s[..i]), &s[i + 1..]),
            None => (None, s),
        };
        let (authority, path) = match s.strip_prefix("//") {
            Some(rest) => {
                let end = rest.find('/').unwrap_or(rest.len());
                (Some(&rest[..end]), &rest[end..])
            }
            None => (None, s),
        };
        Self {
            scheme,
            authority,
            path,
            query,
        }
    }
}

/// Index of the `:` that terminates a scheme, or `None` if the reference
/// has no scheme.
///
/// RFC 3986 §3.1: `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`. The
/// alphabetic first character is what keeps a port from being read as a
/// scheme (`//a:8080/x` starts with `/`), and the check that the whole
/// prefix is scheme-legal is what keeps a colon inside a path segment from
/// being read as one (`a/b:c` — the prefix contains `/`).
fn scheme_end(s: &str) -> Option<usize> {
    let i = s.find(':')?;
    let mut chars = s[..i].chars();
    if !chars.next()?.is_ascii_alphabetic() {
        return None;
    }
    chars
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        .then_some(i)
}

/// RFC 3986 §5.2.3. The base's last segment is not a directory unless it
/// is followed by a slash, so it is dropped: this is the "`/api` + `v1` =
/// `/v1`" surprise, and it is the RFC's rule, not ours.
fn merge(base_path: &str, reference_path: &str) -> String {
    if base_path.is_empty() {
        // The base has an authority (checked by the caller) and no path.
        return format!("/{reference_path}");
    }
    match base_path.rfind('/') {
        Some(i) => format!("{}{reference_path}", &base_path[..=i]),
        None => reference_path.to_owned(),
    }
}

/// RFC 3986 §5.2.4, transcribed. The output buffer is what keeps `..`
/// from escaping above the root: popping an empty buffer is a no-op, so
/// `/a/../../../g` lands on `/g` rather than reaching outside the
/// authority.
fn remove_dot_segments(path: &str) -> String {
    let mut input = path;
    let mut out = String::with_capacity(path.len());
    while !input.is_empty() {
        // A: leading `../` or `./` on a relative path — drop it, it has
        // no segment in the output to act on.
        if let Some(rest) = input.strip_prefix("../") {
            input = rest;
        } else if let Some(rest) = input.strip_prefix("./") {
            input = rest;
        }
        // B: `/./` becomes `/`. Slicing from index 2 leaves the `/` that
        // the RFC says to substitute, rather than re-allocating it.
        else if input.starts_with("/./") {
            input = &input[2..];
        } else if input == "/." {
            input = "/";
        }
        // C: `/../` becomes `/`, and takes the previous segment with it.
        else if input.starts_with("/../") {
            input = &input[3..];
            pop_segment(&mut out);
        } else if input == "/.." {
            input = "/";
            pop_segment(&mut out);
        }
        // D: a bare `.` or `..` is a complete path and contributes
        // nothing.
        else if input == "." || input == ".." {
            input = "";
        }
        // E: move one segment, with its leading `/` if it has one.
        else {
            let start = usize::from(input.starts_with('/'));
            let end = input[start..].find('/').map_or(input.len(), |i| start + i);
            out.push_str(&input[..end]);
            input = &input[end..];
        }
    }
    out
}

/// Remove the last segment and its preceding `/` from the output buffer.
/// An empty buffer stays empty — that is the clamp at the root.
fn pop_segment(out: &mut String) {
    match out.rfind('/') {
        Some(i) => out.truncate(i),
        None => out.clear(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    fn uri(s: &str) -> Uri {
        s.parse().unwrap()
    }

    fn resolved(base: &str, reference: &str) -> String {
        resolve_reference(&uri(base), reference)
            .expect("must resolve")
            .to_string()
    }

    #[test]
    fn a_reference_with_its_own_scheme_wins_over_the_base() {
        assert_eq!(
            resolved("https://example.test/api/", "http://other.test/x"),
            "http://other.test/x"
        );
    }

    #[test]
    fn a_root_relative_reference_replaces_the_whole_path_of_the_base() {
        assert_eq!(
            resolved("https://example.test/api/v1/", "/other"),
            "https://example.test/other"
        );
    }

    #[test]
    fn a_path_relative_reference_extends_a_base_that_ends_in_a_slash() {
        assert_eq!(
            resolved("https://example.test/api/", "v1/things"),
            "https://example.test/api/v1/things"
        );
    }

    /// The merge from §5.3, the one genuinely non-obvious part of the
    /// rule: the base's last segment, without a slash, isn't a directory,
    /// and gets dropped.
    #[test]
    fn a_base_without_a_trailing_slash_loses_its_last_segment() {
        assert_eq!(
            resolved("https://example.test/api", "v1/things"),
            "https://example.test/v1/things"
        );
    }

    #[test]
    fn an_empty_reference_is_the_base_without_its_fragment() {
        assert_eq!(
            resolved("https://example.test/api/things", ""),
            "https://example.test/api/things"
        );
    }

    /// The empty reference keeps the base's QUERY too (§5.2.2), which is
    /// the one place the base's query survives at all — any non-empty
    /// reference replaces it.
    #[test]
    fn an_empty_reference_keeps_the_bases_query_while_any_other_drops_it() {
        assert_eq!(
            resolved("https://example.test/api?x=1", ""),
            "https://example.test/api?x=1"
        );
        assert_eq!(
            resolved("https://example.test/api?x=1", "other"),
            "https://example.test/other"
        );
    }

    /// `/a/b/c` — `c` isn't a directory, the merge gives `/a/b/`, then
    /// `../` strips `b`: what's left is `/a/d`. The expectation of "/d" in
    /// this test's first version was a bug in the test, not the code —
    /// merge and remove_dot_segments apply in sequence, not in place of
    /// each other.
    #[test]
    fn dot_segments_are_removed_after_the_merge_not_instead_of_it() {
        assert_eq!(
            resolved("https://example.test/a/b/c", "../d"),
            "https://example.test/a/d"
        );
    }

    /// The second half of `resolve_reference`'s base check — "and an
    /// authority" — cannot be reached by any `Uri` that can exist today:
    /// `http::Uri` refuses to hold a scheme without an authority, through
    /// `Parts` no less than through parsing. The check stays because it is
    /// a `?` and not a panic, and this test is what fails first if `http`
    /// ever relaxes the rule — which is the moment the check starts
    /// mattering.
    ///
    /// Written down after a mutation run, not before: replacing that `?`
    /// with a default of `""` leaves the whole suite green, and it is
    /// better for that to be a stated invariant than to look like a hole
    /// in the tests.
    #[test]
    fn http_uri_cannot_hold_a_scheme_without_an_authority() {
        let mut parts = http::uri::Parts::default();
        parts.scheme = Some(http::uri::Scheme::HTTPS);
        parts.path_and_query = Some("/x".parse().unwrap());
        assert!(
            Uri::from_parts(parts).is_err(),
            "a `Uri` with a scheme and no authority would reach `resolve_reference` and \
             recompose to `https:/x`"
        );
        assert!(
            "http:/x".parse::<Uri>().is_err(),
            "and the same holds for the parsing route"
        );
    }

    /// A relative base isn't a base: there's nothing to resolve against,
    /// and silently returning the reference as-is would be exactly the
    /// silent no-op this whole module exists against.
    #[test]
    fn a_relative_base_is_named_as_unusable_rather_than_pretending_to_resolve() {
        assert_matches!(
            resolve_reference(&uri("/api/"), "v1"),
            Err(UriError::UnusableBase { base }) if base == "/api/"
        );
    }

    /// A reference that can't be parsed even against a valid base.
    /// Recomposition is string work and cannot fail; `http::Uri` is what
    /// rejects it, and we don't hand back a broken result.
    ///
    /// `<` is the rejected character on purpose: it is illegal in an
    /// authority for BOTH `http::Uri` and `url`, so this test says
    /// "unparsable" and not "unparsable by the one of them we happen to
    /// use". The previous version used `http://[:::1]/`, which `url`
    /// rejected as a malformed IPv6 literal and `http::Uri` accepts — that
    /// pair is now pinned as a divergence in `tests/uri_resolution.rs`
    /// instead of masquerading as a property.
    #[test]
    fn an_unparsable_reference_is_named_as_such() {
        assert_matches!(
            resolve_reference(&uri("https://example.test/"), "http://a<b/"),
            Err(UriError::NotAUri { .. })
        );
    }

    /// `..` cannot climb above the authority: the RFC's output buffer has
    /// nothing left to pop, and the extra `..`s are absorbed. Without the
    /// clamp in `pop_segment` this would produce a path outside the base.
    #[test]
    fn dot_dot_cannot_escape_above_the_root() {
        assert_eq!(
            resolved("https://example.test/a/b/", "../../../../g"),
            "https://example.test/g"
        );
    }

    /// A colon inside a path segment is not a scheme (§3.1 requires an
    /// alphabetic first character and no `/` before the colon), and a port
    /// in a protocol-relative reference is not one either. Both would be
    /// misparsed by a naive "split at the first `:`".
    #[test]
    fn a_colon_in_a_path_or_a_port_is_not_a_scheme() {
        assert_eq!(
            resolved("https://example.test/api/", "a/b:c"),
            "https://example.test/api/a/b:c"
        );
        assert_eq!(
            resolved("https://example.test/api/", "//other.test:8443/x"),
            "https://other.test:8443/x"
        );
        assert_eq!(
            resolved("https://example.test/api/", "1a:b"),
            "https://example.test/api/1a:b",
            "a scheme must start with a letter, so `1a:` is a path segment"
        );
    }

    /// The fragment is split off BEFORE dot-segment removal, so `..`
    /// inside it is text and not a path operation — RFC 3986 §5.4.2 pins
    /// this exact case. It is then dropped, because `http::Uri` has no
    /// room for it.
    #[test]
    fn dot_segments_inside_a_fragment_are_not_path_operations() {
        assert_eq!(
            resolved("https://example.test/b/c/d;p?q", "g#s/../x"),
            "https://example.test/b/c/g"
        );
    }

    /// A `?` after a `#` belongs to the fragment, not to the query.
    #[test]
    fn a_question_mark_inside_a_fragment_is_not_a_query() {
        assert_eq!(
            resolved("https://example.test/api/", "g#s?x=1"),
            "https://example.test/api/g"
        );
    }

    /// The host is what gets converted — not the userinfo, not the port,
    /// and not the path. Sliced by index, so an off-by-one would move the
    /// authority's boundaries rather than merely mis-convert.
    #[test]
    fn the_host_is_located_apart_from_userinfo_port_and_path() {
        let cases = [
            ("https://a.test/x", Some("a.test")),
            ("https://u:p@a.test:8443/x?q=1#f", Some("a.test")),
            ("https://[::1]:8443/x", Some("[::1]")),
            ("https://[::1]/x", Some("[::1]")),
            ("//a.test/x", Some("a.test")),
            ("https://a.test", Some("a.test")),
            ("https://a.test?q=1", Some("a.test")),
            ("mailto:a@b.test", None),
            ("/api/v1", None),
            ("v1/things", None),
        ];
        for (input, expected) in cases {
            let got = authority_range(input).map(|a| {
                let h = host_range(input, a);
                &input[h]
            });
            assert_eq!(got, expected, "host of {input:?}");
        }
    }
}
