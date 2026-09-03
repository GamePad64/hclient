//! The differential probe for `uri::resolve_reference` and `uri::parse`,
//! against the `url` crate they replaced.
//!
//! `url::Url::parse(base).join(reference)` is the one-line alternative.
//! That single call puts `url` → `idna` → `icu_normalizer` +
//! `icu_properties` into every build of `hclient-proto`, the sans-io crate
//! every target includes — megabytes of Unicode tables, for one URI join.
//! It is
//! RFC 3986 §5.2 written out by hand, with IDNA reduced to a feature
//! (`idn`, on by default) applied at one boundary.
//!
//! **`url` is still here, as a dev-dependency, because it is the only
//! oracle these functions have.** `url_resolve` below is the old
//! implementation verbatim. Every row of [`CORPUS`] pins BOTH answers:
//! what the replacement returns, and what `url` returned for the same
//! pair. Rows where the two agree are the safety net for the rewrite; rows
//! where they disagree are the behaviour change, enumerated once in
//! `DIVERGENCES` and asserted to be exactly that set — so a new divergence
//! cannot appear without a test failing, and neither can a `url` upgrade
//! that quietly changes the incumbent's answer.
//!
//! The corpus opens with all 42 reference examples from RFC 3986 §5.4
//! (normal and abnormal), the normative vectors for this exact algorithm,
//! and continues with the forms a client actually meets: scheme-bearing,
//! protocol-relative, root-relative, dot-segment, empty and query-only
//! references, percent-encoding, spaces, doubled slashes, path escapes
//! above the root, userinfo, fragments, IPv6 literals, and bases with and
//! without a trailing slash. It closes with non-ASCII hosts in both forms,
//! U-label and A-label, across every shape the authority can take.
//!
//! **Every row is checked in both settings of the `idn` feature**, which is
//! why the expected answer is an [`Expect`] rather than a plain `Option`:
//! the seven rows carrying a U-label answer with an A-label when the
//! feature is on and with `UriError::NonAsciiHost` when it is off, and
//! nothing else in the corpus moves at all.
//!
//! The corpus is a `const` table walked by a loop rather than an `rstest`
//! case list, and that is deliberate: "the divergences are exactly these"
//! is a property of the table as a whole, and only an enumerable table can
//! carry it. `rstest` is used below for the divergence classes, where one
//! named case per behaviour is what a failure should read like.

use hclient_proto::uri::{self, UriError, resolve_reference};
use http::Uri;
use rstest::rstest;

/// What the replacement must answer for a corpus row.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// The same answer whichever way the `idn` feature is set. `None` is
    /// "some `UriError`" — which one is pinned by the unit tests in
    /// `src/uri.rs`, not here.
    Always(Option<&'static str>),
    /// A non-ASCII host: this A-label with `idn` on, and
    /// `UriError::NonAsciiHost` with it off.
    WithIdn(&'static str),
}

impl Expect {
    fn value(self) -> Option<&'static str> {
        match self {
            Self::Always(v) => v,
            #[cfg(feature = "idn")]
            Self::WithIdn(v) => Some(v),
            #[cfg(not(feature = "idn"))]
            Self::WithIdn(_) => None,
        }
    }
}

use Expect::{Always, WithIdn};

/// One `(base, reference)` pair with both implementations' answers pinned.
#[derive(Debug)]
struct Case {
    base: &'static str,
    reference: &'static str,
    /// What `resolve_reference` must return, as `Uri::to_string`.
    ours: Expect,
    /// What the `url`-based implementation returns for the same pair.
    url_says: Option<&'static str>,
}

/// The implementation that was removed, verbatim, kept as the oracle.
fn url_resolve(base: &Uri, reference: &str) -> Option<Uri> {
    let base = url::Url::parse(&base.to_string()).ok()?;
    let joined = base.join(reference).ok()?;
    joined.as_str().parse::<Uri>().ok()
}

#[rustfmt::skip]
const CORPUS: &[Case] = &[
    // ── RFC 3986 §5.4.1, the normal examples ────────────────
    // `#s` and every other fragment below: RFC 3986 says the result
    // carries it, `http::Uri` has nowhere to put it, so both
    // implementations end at the same fragment-less URI.
    Case { base: "http://a/b/c/d;p?q", reference: "g:h", ours: Always(Some("g:h")), url_says: Some("g:h") },
    Case { base: "http://a/b/c/d;p?q", reference: "g", ours: Always(Some("http://a/b/c/g")), url_says: Some("http://a/b/c/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "./g", ours: Always(Some("http://a/b/c/g")), url_says: Some("http://a/b/c/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "g/", ours: Always(Some("http://a/b/c/g/")), url_says: Some("http://a/b/c/g/") },
    Case { base: "http://a/b/c/d;p?q", reference: "/g", ours: Always(Some("http://a/g")), url_says: Some("http://a/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "//g", ours: Always(Some("http://g/")), url_says: Some("http://g/") },
    Case { base: "http://a/b/c/d;p?q", reference: "?y", ours: Always(Some("http://a/b/c/d;p?y")), url_says: Some("http://a/b/c/d;p?y") },
    Case { base: "http://a/b/c/d;p?q", reference: "g?y", ours: Always(Some("http://a/b/c/g?y")), url_says: Some("http://a/b/c/g?y") },
    Case { base: "http://a/b/c/d;p?q", reference: "#s", ours: Always(Some("http://a/b/c/d;p?q")), url_says: Some("http://a/b/c/d;p?q") },
    Case { base: "http://a/b/c/d;p?q", reference: "g#s", ours: Always(Some("http://a/b/c/g")), url_says: Some("http://a/b/c/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "g?y#s", ours: Always(Some("http://a/b/c/g?y")), url_says: Some("http://a/b/c/g?y") },
    Case { base: "http://a/b/c/d;p?q", reference: ";x", ours: Always(Some("http://a/b/c/;x")), url_says: Some("http://a/b/c/;x") },
    Case { base: "http://a/b/c/d;p?q", reference: "g;x", ours: Always(Some("http://a/b/c/g;x")), url_says: Some("http://a/b/c/g;x") },
    Case { base: "http://a/b/c/d;p?q", reference: "g;x?y#s", ours: Always(Some("http://a/b/c/g;x?y")), url_says: Some("http://a/b/c/g;x?y") },
    Case { base: "http://a/b/c/d;p?q", reference: "", ours: Always(Some("http://a/b/c/d;p?q")), url_says: Some("http://a/b/c/d;p?q") },
    Case { base: "http://a/b/c/d;p?q", reference: ".", ours: Always(Some("http://a/b/c/")), url_says: Some("http://a/b/c/") },
    Case { base: "http://a/b/c/d;p?q", reference: "./", ours: Always(Some("http://a/b/c/")), url_says: Some("http://a/b/c/") },
    Case { base: "http://a/b/c/d;p?q", reference: "..", ours: Always(Some("http://a/b/")), url_says: Some("http://a/b/") },
    Case { base: "http://a/b/c/d;p?q", reference: "../", ours: Always(Some("http://a/b/")), url_says: Some("http://a/b/") },
    Case { base: "http://a/b/c/d;p?q", reference: "../g", ours: Always(Some("http://a/b/g")), url_says: Some("http://a/b/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "../..", ours: Always(Some("http://a/")), url_says: Some("http://a/") },
    Case { base: "http://a/b/c/d;p?q", reference: "../../", ours: Always(Some("http://a/")), url_says: Some("http://a/") },
    Case { base: "http://a/b/c/d;p?q", reference: "../../g", ours: Always(Some("http://a/g")), url_says: Some("http://a/g") },
    // ── RFC 3986 §5.4.2, the abnormal examples ──────────────
    // The last row is the RFC's own strict/backward-compatible split
    // (§5.2.2). Dot segments inside a query or a fragment are text, not
    // path operations, and the four rows before it say so.
    Case { base: "http://a/b/c/d;p?q", reference: "../../../g", ours: Always(Some("http://a/g")), url_says: Some("http://a/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "../../../../g", ours: Always(Some("http://a/g")), url_says: Some("http://a/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "/./g", ours: Always(Some("http://a/g")), url_says: Some("http://a/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "/../g", ours: Always(Some("http://a/g")), url_says: Some("http://a/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "g.", ours: Always(Some("http://a/b/c/g.")), url_says: Some("http://a/b/c/g.") },
    Case { base: "http://a/b/c/d;p?q", reference: ".g", ours: Always(Some("http://a/b/c/.g")), url_says: Some("http://a/b/c/.g") },
    Case { base: "http://a/b/c/d;p?q", reference: "g..", ours: Always(Some("http://a/b/c/g..")), url_says: Some("http://a/b/c/g..") },
    Case { base: "http://a/b/c/d;p?q", reference: "..g", ours: Always(Some("http://a/b/c/..g")), url_says: Some("http://a/b/c/..g") },
    Case { base: "http://a/b/c/d;p?q", reference: "./../g", ours: Always(Some("http://a/b/g")), url_says: Some("http://a/b/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "./g/.", ours: Always(Some("http://a/b/c/g/")), url_says: Some("http://a/b/c/g/") },
    Case { base: "http://a/b/c/d;p?q", reference: "g/./h", ours: Always(Some("http://a/b/c/g/h")), url_says: Some("http://a/b/c/g/h") },
    Case { base: "http://a/b/c/d;p?q", reference: "g/../h", ours: Always(Some("http://a/b/c/h")), url_says: Some("http://a/b/c/h") },
    Case { base: "http://a/b/c/d;p?q", reference: "g;x=1/./y", ours: Always(Some("http://a/b/c/g;x=1/y")), url_says: Some("http://a/b/c/g;x=1/y") },
    Case { base: "http://a/b/c/d;p?q", reference: "g;x=1/../y", ours: Always(Some("http://a/b/c/y")), url_says: Some("http://a/b/c/y") },
    Case { base: "http://a/b/c/d;p?q", reference: "g?y/./x", ours: Always(Some("http://a/b/c/g?y/./x")), url_says: Some("http://a/b/c/g?y/./x") },
    Case { base: "http://a/b/c/d;p?q", reference: "g?y/../x", ours: Always(Some("http://a/b/c/g?y/../x")), url_says: Some("http://a/b/c/g?y/../x") },
    Case { base: "http://a/b/c/d;p?q", reference: "g#s/./x", ours: Always(Some("http://a/b/c/g")), url_says: Some("http://a/b/c/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "g#s/../x", ours: Always(Some("http://a/b/c/g")), url_says: Some("http://a/b/c/g") },
    Case { base: "http://a/b/c/d;p?q", reference: "http:g", ours: Always(Some("http:g")), url_says: Some("http://a/b/c/g") },
    // ── The forms a client actually meets ───────────────────
    Case { base: "https://example.test/api/", reference: "http://other.test/x", ours: Always(Some("http://other.test/x")), url_says: Some("http://other.test/x") },
    Case { base: "https://example.test/api/", reference: "//other.test/x", ours: Always(Some("https://other.test/x")), url_says: Some("https://other.test/x") },
    Case { base: "https://example.test/api/", reference: "//other.test", ours: Always(Some("https://other.test/")), url_says: Some("https://other.test/") },
    Case { base: "https://example.test/api/", reference: "///x", ours: Always(None), url_says: None },
    Case { base: "https://example.test/api/v1/", reference: "/other", ours: Always(Some("https://example.test/other")), url_says: Some("https://example.test/other") },
    Case { base: "https://example.test/api/", reference: "v1/things", ours: Always(Some("https://example.test/api/v1/things")), url_says: Some("https://example.test/api/v1/things") },
    Case { base: "https://example.test/api", reference: "v1/things", ours: Always(Some("https://example.test/v1/things")), url_says: Some("https://example.test/v1/things") },
    Case { base: "https://example.test/api/things", reference: "", ours: Always(Some("https://example.test/api/things")), url_says: Some("https://example.test/api/things") },
    Case { base: "https://example.test/api/things", reference: "?q=1", ours: Always(Some("https://example.test/api/things?q=1")), url_says: Some("https://example.test/api/things?q=1") },
    Case { base: "https://example.test/api/things", reference: "?", ours: Always(Some("https://example.test/api/things?")), url_says: Some("https://example.test/api/things?") },
    Case { base: "http://example.test/a?x=1", reference: "", ours: Always(Some("http://example.test/a?x=1")), url_says: Some("http://example.test/a?x=1") },
    Case { base: "http://example.test/a?x=1", reference: "b", ours: Always(Some("http://example.test/b")), url_says: Some("http://example.test/b") },
    Case { base: "https://example.test/a/b/c", reference: "../d", ours: Always(Some("https://example.test/a/d")), url_says: Some("https://example.test/a/d") },
    Case { base: "https://example.test/a/b/", reference: "../../../g", ours: Always(Some("https://example.test/g")), url_says: Some("https://example.test/g") },
    Case { base: "https://example.test/api/", reference: "a%20b", ours: Always(Some("https://example.test/api/a%20b")), url_says: Some("https://example.test/api/a%20b") },
    Case { base: "https://example.test/api/", reference: "a b", ours: Always(None), url_says: Some("https://example.test/api/a%20b") },
    Case { base: "https://example.test/api/", reference: "?q=a b", ours: Always(None), url_says: Some("https://example.test/api/?q=a%20b") },
    Case { base: "https://example.test/api//", reference: "x", ours: Always(Some("https://example.test/api//x")), url_says: Some("https://example.test/api//x") },
    Case { base: "https://example.test/api/", reference: "x//y", ours: Always(Some("https://example.test/api/x//y")), url_says: Some("https://example.test/api/x//y") },
    Case { base: "https://user:pass@example.test/api/", reference: "v1", ours: Always(Some("https://user:pass@example.test/api/v1")), url_says: Some("https://user:pass@example.test/api/v1") },
    Case { base: "https://example.test/api/", reference: "https://user@other.test/y", ours: Always(Some("https://user@other.test/y")), url_says: Some("https://user@other.test/y") },
    Case { base: "https://example.test/api/", reference: "v1#f", ours: Always(Some("https://example.test/api/v1")), url_says: Some("https://example.test/api/v1") },
    Case { base: "https://example.test:8443/api/", reference: "v1", ours: Always(Some("https://example.test:8443/api/v1")), url_says: Some("https://example.test:8443/api/v1") },
    Case { base: "https://example.test:443/api/", reference: "v1", ours: Always(Some("https://example.test:443/api/v1")), url_says: Some("https://example.test/api/v1") },
    Case { base: "https://example.test", reference: "v1", ours: Always(Some("https://example.test/v1")), url_says: Some("https://example.test/v1") },
    Case { base: "https://EXAMPLE.test/API/", reference: "v1", ours: Always(Some("https://EXAMPLE.test/API/v1")), url_says: Some("https://example.test/API/v1") },
    Case { base: "https://example.test/", reference: "http://[::1]:8080/x", ours: Always(Some("http://[::1]:8080/x")), url_says: Some("http://[::1]:8080/x") },
    Case { base: "https://[::1]/api/", reference: "v1", ours: Always(Some("https://[::1]/api/v1")), url_says: Some("https://[::1]/api/v1") },
    Case { base: "https://example.test/", reference: "http://[:::1]/", ours: Always(Some("http://[:::1]/")), url_says: None },
    Case { base: "https://example.test/api/", reference: "%2e%2e/x", ours: Always(Some("https://example.test/api/%2e%2e/x")), url_says: Some("https://example.test/x") },
    Case { base: "https://example.test/api/", reference: "..%2Fx", ours: Always(Some("https://example.test/api/..%2Fx")), url_says: Some("https://example.test/api/..%2Fx") },
    Case { base: "https://example.test/api/", reference: "a\\b", ours: Always(Some("https://example.test/api/a\\b")), url_says: Some("https://example.test/api/a/b") },
    Case { base: "https://example.test/api/", reference: "a\tb", ours: Always(None), url_says: Some("https://example.test/api/ab") },
    Case { base: "https://example.test/api/", reference: "https:v1", ours: Always(Some("https:v1")), url_says: Some("https://example.test/api/v1") },
    Case { base: "https://example.test/api/", reference: "HTTPS://Other.TEST/x", ours: Always(Some("https://Other.TEST/x")), url_says: Some("https://other.test/x") },
    Case { base: "https://example.test/", reference: "mailto:a@b.test", ours: Always(Some("mailto:a@b.test")), url_says: Some("mailto:a@b.test") },
    Case { base: "https://example.test/api/", reference: "http://a<b/", ours: Always(None), url_says: None },
    Case { base: "https://example.test/api/", reference: "a/b:c", ours: Always(Some("https://example.test/api/a/b:c")), url_says: Some("https://example.test/api/a/b:c") },
    Case { base: "https://example.test/api/", reference: "1a:b", ours: Always(Some("https://example.test/api/1a:b")), url_says: Some("https://example.test/api/1a:b") },
    Case { base: "/api/", reference: "v1", ours: Always(None), url_says: None },
    // ── Non-ASCII hosts, in both forms ──────────────────────
    // A U-label (`münchen.de`) is what IDNA converts; its A-label
    // (`xn--mnchen-3ya.de`) is already ASCII and must come back
    // untouched, which is what makes the conversion idempotent. Both
    // forms are here for every shape the authority can take.
    Case { base: "https://example.test/", reference: "http://例え.テスト/x", ours: WithIdn("http://xn--r8jz45g.xn--zckzah/x"), url_says: Some("http://xn--r8jz45g.xn--zckzah/x") },
    Case { base: "https://example.test/", reference: "http://xn--r8jz45g.xn--zckzah/x", ours: Always(Some("http://xn--r8jz45g.xn--zckzah/x")), url_says: Some("http://xn--r8jz45g.xn--zckzah/x") },
    Case { base: "https://example.test/", reference: "https://münchen.de/x", ours: WithIdn("https://xn--mnchen-3ya.de/x"), url_says: Some("https://xn--mnchen-3ya.de/x") },
    Case { base: "https://example.test/", reference: "https://xn--mnchen-3ya.de/x", ours: Always(Some("https://xn--mnchen-3ya.de/x")), url_says: Some("https://xn--mnchen-3ya.de/x") },
    Case { base: "https://example.test/", reference: "https://MÜNCHEN.de:8443/x", ours: WithIdn("https://xn--mnchen-3ya.de:8443/x"), url_says: Some("https://xn--mnchen-3ya.de:8443/x") },
    Case { base: "https://example.test/", reference: "https://u:p@münchen.de/x", ours: WithIdn("https://u:p@xn--mnchen-3ya.de/x"), url_says: Some("https://u:p@xn--mnchen-3ya.de/x") },
    Case { base: "https://example.test/", reference: "//münchen.de/x", ours: WithIdn("https://xn--mnchen-3ya.de/x"), url_says: Some("https://xn--mnchen-3ya.de/x") },
    Case { base: "https://example.test/", reference: "https://xn--zzzz.test/x", ours: Always(Some("https://xn--zzzz.test/x")), url_says: None },
    // **This row moved onto the oracle**, which is the direction a
    // divergence list should shrink in. It used to be `Always(None)`,
    // because `hclient-idn` refused an empty label on every backend — a
    // rule of that crate's own, not `idna`'s. That crate is a
    // smaller-binary `idna` and nothing more now, so the answer is
    // `idna`'s, which is `url`'s, which is this row.
    Case { base: "https://example.test/", reference: "https://ä..de/x", ours: WithIdn("https://xn--4ca..de/x"), url_says: Some("https://xn--4ca..de/x") },
    Case { base: "https://example.test/", reference: "https://münchen.de/ä", ours: WithIdn("https://xn--mnchen-3ya.de/%C3%A4"), url_says: Some("https://xn--mnchen-3ya.de/%C3%A4") },
    // Non-ASCII outside the host: percent-encoded UTF-8, never punycode.
    // These need no `idn` feature — nothing here is a domain name.
    Case { base: "https://example.test/api/", reference: "café", ours: Always(Some("https://example.test/api/caf%C3%A9")), url_says: Some("https://example.test/api/caf%C3%A9") },
    Case { base: "https://example.test/api/", reference: "?q=ä", ours: Always(Some("https://example.test/api/?q=%C3%A4")), url_says: Some("https://example.test/api/?q=%C3%A4") },
    Case { base: "https://example.test/api/", reference: "/café", ours: Always(Some("https://example.test/caf%C3%A9")), url_says: Some("https://example.test/caf%C3%A9") },
    // A mixed-case ASCII host WITH non-ASCII elsewhere. The only row that
    // reaches the host lookup with an ASCII host, and so the only one that
    // notices if IDNA stops being restricted to non-ASCII hosts: measured,
    // removing that restriction lower-cases the host here and nothing else
    // in the suite changes at all.
    Case { base: "https://example.test/api/", reference: "https://EXAMPLE.test/café", ours: Always(Some("https://EXAMPLE.test/caf%C3%A9")), url_says: Some("https://example.test/caf%C3%A9") },
];

/// Every `(base, reference)` on which the replacement deliberately answers
/// something other than `url` did, in corpus order. The list is closed: the
/// test below derives the same set from [`CORPUS`] and compares, so a
/// divergence nobody decided on cannot slip in, and one that gets fixed
/// cannot stay listed.
///
/// With `idn` on this is the RFC-versus-WHATWG list and **nothing else**.
/// It carried one more entry until `hclient-idn` stopped answering
/// questions about URLs: a host with an empty label, `ä..de`, which that
/// crate refused on every backend by a rule of its own. It is a
/// smaller-binary `idna` and nothing more now, so it converts what `idna`
/// converts, which is what `url` converts — and the row moved onto the
/// oracle rather than beside it.
///
/// The divergence that removing it *could* have introduced is Apple's:
/// Foundation refuses `ä..de` where `idna` and Windows' ICU convert it,
/// so the same name is answered on two of this project's platforms and
/// not the third. That is a fact about Foundation, measured and recorded
/// in `hclient-idn`'s differential corpus, and it is that crate's to
/// report rather than this one's to paper over — which is the whole of
/// what changed.
#[cfg(feature = "idn")]
#[rustfmt::skip]
const DIVERGENCES: &[(&str, &str)] = &[
    ("http://a/b/c/d;p?q", "http:g"),
    ("https://example.test/api/", "a b"),
    ("https://example.test/api/", "?q=a b"),
    ("https://example.test:443/api/", "v1"),
    ("https://EXAMPLE.test/API/", "v1"),
    ("https://example.test/", "http://[:::1]/"),
    ("https://example.test/api/", "%2e%2e/x"),
    ("https://example.test/api/", "a\\b"),
    ("https://example.test/api/", "a\tb"),
    ("https://example.test/api/", "https:v1"),
    ("https://example.test/api/", "HTTPS://Other.TEST/x"),
    ("https://example.test/", "https://xn--zzzz.test/x"),
    ("https://example.test/api/", "https://EXAMPLE.test/café"),
];

/// The same list with the `idn` feature off, where every U-label becomes a
/// `UriError::NonAsciiHost` instead. The seven extra entries ARE the
/// feature: they are what a build without the Unicode tables gives up,
/// named one by one rather than described.
///
/// `ä..de` is one of them again. It left the list above when
/// `hclient-idn` stopped refusing an empty label — with the feature on it
/// converts what `idna` converts and agrees with the oracle — and it
/// never left this one, because a build with no IDN at all still answers
/// `NonAsciiHost` where `url` answers a host. Removing it from both lists
/// was one edit too many, and only the feature-off build could say so.
#[cfg(not(feature = "idn"))]
#[rustfmt::skip]
const DIVERGENCES: &[(&str, &str)] = &[
    ("http://a/b/c/d;p?q", "http:g"),
    ("https://example.test/api/", "a b"),
    ("https://example.test/api/", "?q=a b"),
    ("https://example.test:443/api/", "v1"),
    ("https://EXAMPLE.test/API/", "v1"),
    ("https://example.test/", "http://[:::1]/"),
    ("https://example.test/api/", "%2e%2e/x"),
    ("https://example.test/api/", "a\\b"),
    ("https://example.test/api/", "a\tb"),
    ("https://example.test/api/", "https:v1"),
    ("https://example.test/api/", "HTTPS://Other.TEST/x"),
    ("https://example.test/", "http://例え.テスト/x"),
    ("https://example.test/", "https://münchen.de/x"),
    ("https://example.test/", "https://MÜNCHEN.de:8443/x"),
    ("https://example.test/", "https://u:p@münchen.de/x"),
    ("https://example.test/", "//münchen.de/x"),
    ("https://example.test/", "https://xn--zzzz.test/x"),
    ("https://example.test/", "https://ä..de/x"),
    ("https://example.test/", "https://münchen.de/ä"),
    ("https://example.test/api/", "https://EXAMPLE.test/café"),
];

fn base_of(case: &Case) -> Uri {
    case.base
        .parse()
        .expect("every corpus base must be a parsable `http::Uri`")
}

fn as_string(u: Result<Uri, UriError>) -> Option<String> {
    u.ok().map(|u| u.to_string())
}

/// The rewrite's acceptance: every pair with its answer written down.
/// Nothing here is derived from the implementation — the RFC §5.4 block is
/// copied from the specification, and the rest was measured against `url`
/// before the replacement was written.
#[test]
fn resolve_reference_answers_what_the_corpus_pins_on_every_row() {
    let mut wrong = Vec::new();
    for case in CORPUS {
        let got = as_string(resolve_reference(&base_of(case), case.reference));
        if got.as_deref() != case.ours.value() {
            wrong.push(format!(
                "  base={:?} ref={:?}: expected {:?}, got {:?}",
                case.base,
                case.reference,
                case.ours.value(),
                got
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} corpus rows resolved differently than pinned:\n{}",
        wrong.len(),
        CORPUS.len(),
        wrong.join("\n")
    );
}

/// The other half of "differential": the oracle's answers are pinned too.
/// Without this, a `url` upgrade that changed the incumbent's behaviour
/// would silently redefine what "the same as before" means, and the
/// divergence list would be measuring the wrong baseline.
#[test]
fn the_url_oracle_still_answers_what_the_corpus_pins_for_it() {
    let mut wrong = Vec::new();
    for case in CORPUS {
        let got = url_resolve(&base_of(case), case.reference).map(|u| u.to_string());
        if got.as_deref() != case.url_says {
            wrong.push(format!(
                "  base={:?} ref={:?}: url was pinned at {:?}, now says {:?}",
                case.base, case.reference, case.url_says, got
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} corpus rows changed under the `url` oracle:\n{}",
        wrong.len(),
        CORPUS.len(),
        wrong.join("\n")
    );
}

/// The behaviour change, bounded. Everything not on the list must still
/// produce, byte for byte, the URI the `url`-based implementation produced.
#[test]
fn the_divergences_from_url_are_exactly_the_documented_ones() {
    let found: Vec<(&str, &str)> = CORPUS
        .iter()
        .filter(|c| c.ours.value() != c.url_says)
        .map(|c| (c.base, c.reference))
        .collect();
    assert_eq!(
        found, DIVERGENCES,
        "the set of pairs where the replacement disagrees with `url` is not the documented one"
    );
    assert_eq!(
        CORPUS.len(),
        96,
        "a corpus row was added or removed without the divergence list being reconsidered"
    );
}

/// Resolving a result against the same base again must be a no-op: every
/// answer is absolute, an absolute reference wins over the base (§5.2.2),
/// and a host that came back as an A-label is ASCII and so never reaches
/// IDNA a second time. A second pass that moved the URI would mean the
/// output of one call is not a valid input to the next — and this client
/// does exactly that: `sse::open` resolves, `Client::execute` resolves the
/// result again, and every redirect hop resolves the previous hop's answer.
#[test]
fn resolving_an_already_resolved_reference_changes_nothing() {
    let mut wrong = Vec::new();
    for case in CORPUS {
        let Some(first) = case.ours.value() else {
            continue;
        };
        let again = as_string(resolve_reference(&base_of(case), first));
        if again.as_deref() != Some(first) {
            wrong.push(format!(
                "  base={:?} ref={:?}: {:?} resolved again became {:?}",
                case.base, case.reference, first, again
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} corpus results are not fixed points:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// `parse` is the boundary and `resolve_reference` ends in it, so an
/// absolute reference must come out the same whether or not a base is
/// involved. This is the property the client did NOT have before: with no
/// base the string went straight to `http::Uri`, which rejects a non-ASCII
/// authority, and with a base it went through `url`, which punycoded it —
/// so `https://münchen.de/x` worked or failed depending on an unrelated
/// setting.
#[test]
fn a_base_makes_no_difference_to_an_absolute_reference() {
    let base: Uri = "https://example.test/api/".parse().unwrap();
    let mut wrong = Vec::new();
    let mut checked = 0;
    for case in CORPUS {
        // Only references that carry their own scheme AND authority: the
        // rest legitimately depend on the base.
        if !case.reference.contains("://") {
            continue;
        }
        checked += 1;
        let with_base = as_string(resolve_reference(&base, case.reference));
        let without = as_string(uri::parse(case.reference));
        if with_base != without {
            wrong.push(format!(
                "  {:?}: with a base {:?}, without one {:?}",
                case.reference, with_base, without
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} absolute references answer differently depending on whether a base is set:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    assert!(
        checked >= 15,
        "only {checked} absolute references in the corpus — this test would pass vacuously; \
         15 is the count measured when it was written, IDN rows included"
    );
}

/// The divergence classes that do not depend on the `idn` feature, one
/// named case each — this is the list that belongs in a changelog, and
/// each case says what changed and in which direction.
///
/// `expected` is the new answer and `was` is the old one, both spelled out,
/// so a case cannot pass by asserting that something merely "differs".
#[rstest]
// Host case is left alone: it is a comparison rule, not a syntactic
// transform, and `redirect::decide` is where the comparison lives.
#[case::the_hosts_case_survives(
    "https://EXAMPLE.test/API/",
    "v1",
    Some("https://EXAMPLE.test/API/v1"),
    Some("https://example.test/API/v1")
)]
// The scheme is still lower-cased, because `http::Uri` does that itself —
// so "no normalisation" is a claim about what this function adds, not
// about what the type does.
#[case::the_schemes_case_does_not(
    "https://example.test/api/",
    "HTTPS://Other.TEST/x",
    Some("https://Other.TEST/x"),
    Some("https://other.test/x")
)]
// A default port is kept, which is what already happened to every request
// made without a `base_url`. `redirect::port_of` substitutes the scheme
// default before comparing origins, so `:443` is not an origin change.
#[case::an_explicit_default_port_survives(
    "https://example.test:443/api/",
    "v1",
    Some("https://example.test:443/api/v1"),
    Some("https://example.test/api/v1")
)]
// No repair of illegal characters: a literal space is rejected instead of
// being percent-encoded, and the caller gets a typed error.
#[case::a_literal_space_is_rejected_instead_of_encoded(
    "https://example.test/api/",
    "a b",
    None,
    Some("https://example.test/api/a%20b")
)]
// Same class, deletion rather than encoding: `url` strips tab, CR and LF
// anywhere in the input.
#[case::a_tab_is_rejected_instead_of_deleted(
    "https://example.test/api/",
    "a\tb",
    None,
    Some("https://example.test/api/ab")
)]
// §5.2.4 operates on the path as written; `%2e` is a percent-encoded
// octet, not a dot segment. Neither answer leaves the base's authority.
#[case::percent_encoded_dots_are_not_dot_segments(
    "https://example.test/api/",
    "%2e%2e/x",
    Some("https://example.test/api/%2e%2e/x"),
    Some("https://example.test/x")
)]
// A backslash is an ordinary path character in RFC 3986; WHATWG rewrites
// it to `/` for "special" schemes.
#[case::a_backslash_stays_a_backslash(
    "https://example.test/api/",
    "a\\b",
    Some("https://example.test/api/a\\b"),
    Some("https://example.test/api/a/b")
)]
// §5.2.2 strict: a reference with a scheme is absolute, even when the
// scheme is the base's. The RFC offers the other reading explicitly, for
// backward compatibility with RFC 1630; this is not that reading.
#[case::a_reference_repeating_the_bases_scheme_is_still_absolute(
    "http://a/b/c/d;p?q",
    "http:g",
    Some("http:g"),
    Some("http://a/b/c/g")
)]
#[case::and_so_is_one_repeating_it_with_a_relative_path(
    "https://example.test/api/",
    "https:v1",
    Some("https:v1"),
    Some("https://example.test/api/v1")
)]
// The authority is now validated by `http::Uri` rather than by `url`, and
// `http::Uri` is the more permissive of the two. A malformed IPv6 literal
// gets through resolution and is rejected later, when something tries to
// connect to it.
#[case::a_malformed_ipv6_literal_is_no_longer_caught_here(
    "https://example.test/",
    "http://[:::1]/",
    Some("http://[:::1]/"),
    None
)]
// An ASCII host is never handed to UTS 46 — that is what makes the
// conversion idempotent — so a nonsense A-label is no longer rejected
// here. It reaches DNS instead, which has an opinion about it.
#[case::a_nonsense_a_label_is_no_longer_validated(
    "https://example.test/",
    "https://xn--zzzz.test/x",
    Some("https://xn--zzzz.test/x"),
    None
)]
fn a_documented_divergence_from_url(
    #[case] base: &str,
    #[case] reference: &str,
    #[case] expected: Option<&str>,
    #[case] was: Option<&str>,
) {
    let base: Uri = base.parse().unwrap();
    assert_ne!(expected, was, "this case is not a divergence at all");
    assert_eq!(
        as_string(resolve_reference(&base, reference)).as_deref(),
        expected,
        "the replacement no longer produces the answer this divergence was decided on"
    );
    assert_eq!(
        url_resolve(&base, reference)
            .map(|u| u.to_string())
            .as_deref(),
        was,
        "`url` no longer produces the answer this divergence was measured against"
    );
}

/// The property the whole design rests on: IDNA runs on U-labels and never
/// on anything else, so applying `parse` to its own output is a no-op.
/// `sse::open` resolves a URL and `Client::execute` resolves the result
/// again; every redirect hop resolves the previous hop's answer. A
/// conversion that shifted the host on a second pass would corrupt exactly
/// those paths.
///
/// Checked to FOUR passes, not two: UTS 46 lower-cases and normalises, so
/// a bug here would more likely be a slow drift than a single jump.
#[cfg(feature = "idn")]
#[rstest]
#[case("https://münchen.de/x")]
#[case("https://MÜNCHEN.de:8443/x")]
#[case("https://u:p@münchen.de/x")]
#[case("http://例え.テスト/x")]
#[case("https://xn--mnchen-3ya.de/x")]
#[case("https://xn--r8jz45g.xn--zckzah/x")]
// An ASCII host that UTS 46 WOULD have lower-cased. It must not be
// touched: this is the case that breaks if `parse` ever runs IDNA
// unconditionally instead of only on a non-ASCII host.
#[case("https://EXAMPLE.test/API/x")]
#[case("https://a.test/x")]
fn punycoding_a_host_is_idempotent(#[case] input: &str) {
    let first = uri::parse(input).expect("must parse").to_string();
    let mut current = first.clone();
    for pass in 2..=4 {
        let next = uri::parse(&current)
            .unwrap_or_else(|e| panic!("pass {pass} of {input:?} failed: {e}"))
            .to_string();
        assert_eq!(
            next, current,
            "pass {pass} of {input:?} moved the URI: {current:?} -> {next:?}"
        );
        current = next;
    }
    assert!(
        first.is_ascii(),
        "the boundary must produce a pure-ASCII host for the browser's own IDNA to be a \
         no-op on it, got {first:?}"
    );
}

/// An ASCII host reaches the `Uri` byte for byte, case and all. UTS 46
/// would have lower-cased it; it is never asked.
///
/// The two inputs are not the same test twice. The first never reaches the
/// host lookup at all — `parse` returns early on an all-ASCII string — so
/// on its own it proves only that the early return exists. The second has
/// non-ASCII in its PATH, which forces the conversion path and puts an
/// ASCII host in front of the check that decides whether to call IDNA.
/// Measured: with that check removed, UTS 46 lower-cases the host in the
/// second line and nothing else in the whole suite moves.
#[cfg(feature = "idn")]
#[test]
fn an_ascii_host_is_never_handed_to_uts46() {
    assert_eq!(
        uri::parse("https://EXAMPLE.test/API").unwrap().to_string(),
        "https://EXAMPLE.test/API"
    );
    assert_eq!(
        uri::parse("https://EXAMPLE.test/café").unwrap().to_string(),
        "https://EXAMPLE.test/caf%C3%A9"
    );
}

/// With the feature off, the answer must NAME the problem. `http::Uri`'s
/// own message for the same input is "invalid uri character", which does
/// not tell a caller that an A-label would work — and this error is the
/// only place they can learn it.
#[cfg(not(feature = "idn"))]
#[rstest]
#[case("https://münchen.de/x", "münchen.de")]
#[case("http://例え.テスト/x", "例え.テスト")]
#[case("https://MÜNCHEN.de:8443/x", "MÜNCHEN.de")]
#[case("https://u:p@münchen.de/x", "münchen.de")]
fn without_the_feature_a_non_ascii_host_is_named_not_merely_refused(
    #[case] input: &str,
    #[case] host: &str,
) {
    let err = uri::parse(input).expect_err("no `idn` feature, so this cannot resolve");
    assert!(
        matches!(&err, UriError::NonAsciiHost { host: h } if h == host),
        "the error must name the host, and only the host: {err:?}"
    );
    let message = err.to_string();
    assert!(
        message.contains("xn--"),
        "the error must tell the caller what to send instead: {message}"
    );
    assert!(
        message.contains("idn"),
        "the error must name the feature that would have handled it: {message}"
    );
}

/// The A-label form still works with the feature off — which is what makes
/// the error's advice true rather than merely polite.
///
/// Checked against EVERY U-label row in the corpus, not one example: each
/// of those rows pins the exact A-label the `idn` build produces, so this
/// asserts that what a caller is told to send is the same string the
/// feature would have produced for them, in every shape the authority
/// takes.
#[cfg(not(feature = "idn"))]
#[test]
fn without_the_feature_the_a_label_it_asks_for_resolves_instead() {
    let mut checked = 0;
    for case in CORPUS {
        let Expect::WithIdn(a_label) = case.ours else {
            continue;
        };
        checked += 1;
        assert!(
            a_label.is_ascii(),
            "{a_label:?} is pinned as the A-label answer but is not ASCII"
        );
        assert_eq!(
            as_string(uri::parse(a_label)).as_deref(),
            Some(a_label),
            "the A-label this build asks for must itself resolve, unchanged"
        );
    }
    // **Seven, and it read six for one commit.** `ä..de` was taken out of
    // this count on the reasoning that `hclient-idn` refuses an empty
    // label on every backend — which had just stopped being true, since
    // that refusal was the policy layer being deleted in the same change.
    // It converts what `idna` converts now, so it is a `WithIdn` row like
    // the other six and there is an A-label for a caller to be told to
    // send. The number is a floor against the corpus quietly losing its
    // U-labels, so it moves with a reason rather than being relaxed.
    assert_eq!(
        checked, 7,
        "the corpus must still carry every U-label shape, or this proves nothing"
    );
}
