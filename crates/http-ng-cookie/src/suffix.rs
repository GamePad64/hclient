//! The public-suffix question, and what it cost to answer it.
//!
//! A cookie with `Domain=co.uk` must not be stored. Nothing about the
//! string `co.uk` says so — `co.uk` and `bbc.co.uk` are the same shape, and
//! the only thing that separates "a name a registrant controls" from "a
//! branching point in the registry" is the Mozilla Public Suffix List, a
//! data file of some fifteen thousand rules. So the correct answer costs
//! bytes, and this project measures bytes rather than assuming them.
//!
//! # What each option costs
//!
//! Measured, not quoted: one release binary per candidate, same profile
//! (`opt-level = 3`, `lto = true`, `codegen-units = 1`, `panic = "abort"`,
//! `strip = true`), each calling the crate's suffix lookup on a runtime
//! argument so nothing is constant-folded away, against an identical
//! baseline binary that only splits the argument on `.`. Baseline:
//! **308,384 bytes**.
//!
//! | option | binary delta | crates added | refuses `Domain=co.uk`? |
//! |---|---|---|---|
//! | **`public-suffix` 0.1.3** | **+77,296 B (75 KiB)** | **1, with no dependencies of its own** | yes |
//! | `publicsuffix` 2.3.0 | +186,712 B (182 KiB) of *code*, and the list is **not** included — you ship and parse the ~250 KB `.dat` yourself at run time | 32 (pulls `idna` and the whole ICU set) | yes |
//! | `psl` 2.1.223 | +735,376 B (718 KiB) | 2 | yes |
//! | `adbyss_psl` 0.24.2 | +789,024 B (771 KiB) | 39 (pulls `idna` and the whole ICU set) | yes |
//! | no list, "reject a `Domain` with no embedded dot" | 0 | 0 | **no** — it catches `Domain=com`, and `co.uk` has a dot |
//!
//! The source-versus-binary distinction matters here and this project has
//! been caught by it before: `public-suffix`'s generated table is **253 KB
//! of Rust source** (`src/tld_list.rs`) and **75 KiB in the stripped
//! binary**, because it is a bit-packed trie of `u32`s plus one
//! concatenated text blob, not a `&[&str]`. Quoting the source figure would
//! have made the cheapest option look like the second most expensive.
//!
//! # What is taken, and why
//!
//! `public-suffix` 0.1.3 (1Password, MIT OR Apache-2.0), behind the
//! `public-suffix` feature, on by default. It is a transliteration of Go's
//! `golang.org/x/net/publicsuffix`, which is the same trie Go's own cookie
//! jar uses; it has **no dependencies at all**, which is the property that
//! separates it from every other candidate — `publicsuffix` and
//! `adbyss_psl` both drag `idna` and the entire ICU data set in for a
//! question that never leaves ASCII, and this workspace spent a whole task
//! removing exactly that graph from `http-ng-proto`.
//!
//! Wildcards and exceptions are implemented, checked here rather than
//! assumed: `foo.ck` is a public suffix (`*.ck`), `www.ck` is not
//! (`!www.ck`), and the private section is present (`github.io`,
//! `blogspot.com`, `s3.amazonaws.com` all answer "public suffix"), which is
//! what browsers use for cookie scope.
//!
//! # The gap this choice leaves, named
//!
//! **The list is a snapshot, compiled in, and it goes stale.**
//! `public-suffix` 0.1.3 was published 2025-04-28 and the crate's own
//! documentation says updating it means re-running a Go program by hand
//! ("we intentionally do not try to download the latest version … to keep
//! the build deterministic"). So the list in a binary built today is
//! whatever Mozilla's file said over a year ago, and it can only ever be
//! wrong in one direction: a suffix **added** since the snapshot is not
//! known here.
//!
//! That direction is the harmful one. New entries in the private section
//! are overwhelmingly shared hosting domains — the `pages.dev`,
//! `vercel.app`, `val.run` shape — added precisely so that one tenant's
//! cookies cannot be set for another tenant. Against a host on a suffix
//! added after the snapshot, this jar will accept `Domain=<that suffix>`
//! and then return the cookie to every sibling tenant under it. Nothing
//! here detects that; the only fixes are bumping the dependency or
//! supplying a fresher list through [`PublicSuffixList`], which is why that
//! trait is public and why [`CookieJar`](crate::CookieJar) takes it as a
//! type parameter rather than hard-wiring [`BuiltinList`].
//!
//! Two smaller gaps, for completeness. The crate does not expose the
//! ICANN/private split its table encodes, so a caller cannot ask for
//! "ICANN section only" — this jar uses the full list, matching browsers,
//! and could not offer the alternative even if it wanted to. And lookups
//! are ASCII/A-label only: a Unicode host must already have been
//! punycoded, which in this workspace happens upstream in
//! `http_ng_proto::uri::parse`, not here.

/// Where a domain sits relative to the registry's branching points.
///
/// A seam rather than a fixed table so that the staleness named in this
/// module's documentation is something a caller can actually fix — supply
/// a list that is newer than the snapshot this crate was built with, or a
/// list of your own for a private naming scheme, without waiting for a
/// release here.
pub trait PublicSuffixList {
    /// Is `domain` — already lowercased, with no leading or trailing dot —
    /// **exactly** a public suffix?
    ///
    /// `example.com` is not (it is a registrable name); `com` and `co.uk`
    /// are; `www.example.com` is not.
    fn is_public_suffix(&self, domain: &str) -> bool;

    /// Whether this implementation actually consults a list.
    ///
    /// Exists only so that a refusal can name its real cause. An
    /// implementation that answers `true` from
    /// [`is_public_suffix`](PublicSuffixList::is_public_suffix) for
    /// everything — [`NoList`], and [`BuiltinList`] in a build without the
    /// `public-suffix` feature — refuses `Domain=example.com` for a
    /// completely different reason than a real list refusing
    /// `Domain=co.uk`, and a caller staring at
    /// [`Rejected`](crate::Rejected) deserves to be told which.
    fn has_list(&self) -> bool {
        true
    }
}

/// The list compiled into this crate.
///
/// With the `public-suffix` feature — the default — this is the Mozilla
/// Public Suffix List snapshot described in this module's documentation.
///
/// **Without the feature it answers `true` for every domain**, which makes
/// the jar host-only: no `Domain` attribute is ever honoured beyond the
/// exact host that sent it, and every one of them is refused with
/// [`Rejected::NoPublicSuffixList`](crate::Rejected::NoPublicSuffixList).
/// That is the same behaviour as [`NoList`], and it is deliberately the
/// *narrow* direction — a build with no list cannot be talked into sending
/// a cookie anywhere the list build would not.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltinList;

impl PublicSuffixList for BuiltinList {
    #[cfg(feature = "public-suffix")]
    fn is_public_suffix(&self, domain: &str) -> bool {
        // `public_suffix` answers "what is the suffix of this name", and a
        // name *is* a public suffix exactly when it is its own suffix.
        //
        // The obvious alternative — `effective_tld_plus_one(domain)
        // .is_err()` — is wrong rather than merely indirect: that call also
        // errors on an empty label (`a..b`, a leading or trailing dot),
        // which would report a malformed input as a public suffix and
        // refuse the cookie for a reason that has nothing to do with the
        // registry.
        public_suffix::DEFAULT_PROVIDER.public_suffix(domain) == domain
    }

    #[cfg(not(feature = "public-suffix"))]
    fn is_public_suffix(&self, _domain: &str) -> bool {
        true
    }

    fn has_list(&self) -> bool {
        cfg!(feature = "public-suffix")
    }
}

/// No list at all: every domain is treated as a public suffix.
///
/// The name says what it is; the behaviour says what that costs. Since
/// [`CookieJar`](crate::CookieJar) only rescues a public-suffix `Domain`
/// attribute when it is identical to the request host — RFC 6265bis
/// §5.7's own rule — a jar built on this stores **host-only cookies and
/// nothing else**. Useful for a build that cannot spend the 75 KiB, and
/// for tests that want the no-list branch without a second cargo
/// invocation.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoList;

impl PublicSuffixList for NoList {
    fn is_public_suffix(&self, _domain: &str) -> bool {
        true
    }

    fn has_list(&self) -> bool {
        false
    }
}

impl<T: PublicSuffixList + ?Sized> PublicSuffixList for &T {
    fn is_public_suffix(&self, domain: &str) -> bool {
        (**self).is_public_suffix(domain)
    }

    fn has_list(&self) -> bool {
        (**self).has_list()
    }
}

#[cfg(all(test, feature = "public-suffix"))]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // The case the whole dependency exists for.
    #[case("co.uk", true)]
    #[case("com", true)]
    #[case("uk", true)]
    // …and the names under it, which must stay registrable.
    #[case("bbc.co.uk", false)]
    #[case("example.com", false)]
    #[case("www.example.com", false)]
    // Wildcard and exception rules, checked rather than assumed.
    #[case("ck", true)]
    #[case("foo.ck", true)]
    #[case("www.ck", false)]
    // The private section, which is what browsers use for cookie scope.
    #[case("github.io", true)]
    #[case("blogspot.com", true)]
    #[case("s3.amazonaws.com", true)]
    #[case("someuser.github.io", false)]
    fn the_list_answers(#[case] domain: &str, #[case] expected: bool) {
        assert_eq!(BuiltinList.is_public_suffix(domain), expected, "{domain}");
    }

    #[test]
    fn an_unknown_tld_is_a_public_suffix() {
        // The PSL's prevailing rule when nothing matches is `*`, so a name
        // under a TLD the list has never heard of still has a suffix.
        // `localhost` lands here, which is why `Domain=localhost` on host
        // `localhost` has to be rescued by the "identical to the request
        // host" branch rather than by the list.
        assert!(BuiltinList.is_public_suffix("localhost"));
        assert!(BuiltinList.is_public_suffix("nonesuch"));
        assert!(!BuiltinList.is_public_suffix("a.nonesuch"));
    }

    #[test]
    fn the_builtin_list_says_it_has_one() {
        assert!(BuiltinList.has_list());
        assert!(!NoList.has_list());
    }
}
