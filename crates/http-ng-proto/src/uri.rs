//! Resolving a URI reference against a base — RFC 3986 §5.
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

use http::Uri;

/// Resolves `reference` against `base` per RFC 3986 §5.
///
/// `None` if the base isn't usable as a base (not absolute: `url::Url` has
/// no notion of a relative URL), if the reference doesn't parse, or if the
/// result can't be expressed as an `http::Uri`. The caller turns this into
/// a typed error, one of its own for each of the two call sites
/// (`RedirectAction::InvalidLocation` / `http_ng::InvalidBaseUrl`).
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
pub fn resolve_reference(base: &Uri, reference: &str) -> Option<Uri> {
    let base = url::Url::parse(&base.to_string()).ok()?;
    let joined = base.join(reference).ok()?;
    joined.as_str().parse::<Uri>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A relative base isn't a base: there's nothing to resolve against,
    /// and silently returning the reference as-is would be exactly the
    /// silent no-op this whole module exists against.
    #[test]
    fn a_relative_base_yields_none_rather_than_pretending_to_resolve() {
        assert!(resolve_reference(&uri("/api/"), "v1").is_none());
    }

    /// A reference that can't be parsed even against a valid base.
    /// `url::Url::join` rejects it, and we don't hand back a broken
    /// result.
    #[test]
    fn an_unparsable_reference_yields_none() {
        assert!(resolve_reference(&uri("https://example.test/"), "http://[:::1]/").is_none());
    }
}
