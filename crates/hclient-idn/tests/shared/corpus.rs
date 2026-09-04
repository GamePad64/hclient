// The corpus, in one place because two test binaries read it.
//
// `include!`d rather than a module — which is why these are `//` and not
// `//!`, an inner doc comment being an error anywhere but the top of a
// file — and it lives under `tests/shared/` rather than `tests/` so that
// cargo does not build it as a test target of its own. The two readers
// are `differential.rs`, which runs on a host, and `web_corpus.rs`, which
// runs in a browser: a second copy of these rows would be two corpora
// agreeing with each other rather than one corpus judging two backends.

/// One input with both implementations' answers pinned.
///
/// **Each reader uses the columns it needs**, which is why the pinned
/// answers are allowed to go unread. `differential.rs` compares against
/// them, because on Windows the platform is the thing under test and a
/// pinned column is what catches an ICU that changed its mind.
/// `web_corpus.rs` compares against `idna` **live** instead: the browser
/// column would be a fourth thing to keep in sync, and a live oracle is
/// the stronger claim where the oracle is available in the same binary.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "the pinned columns are read by differential.rs; the browser corpus compares \
              against idna live and reads only the input"
)]
struct Case {
    /// What this row is here to catch. One line, and it should say why
    /// deleting the row would lose something.
    what: &'static str,
    input: &'static str,
    /// The bundled `idna` crate: `idna::domain_to_ascii_cow(input,
    /// AsciiDenyList::URL)`. `None` is `Err`.
    idna_says: Option<&'static str>,
    /// The platform's ICU through this crate: `OPTIONS`, the
    /// `IGNORED_ERRORS` mask, and the WHATWG deny list. `None` is `Err`.
    icu_says: Option<&'static str>,
}

/// A 64-byte label — one over the DNS limit, which UTS 46 only enforces
/// under _VerifyDnsLength_, which is off here.
const LABEL_64: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.de";

/// A 255-byte name — over RFC 1035's 253 — made of four legal 63-byte
/// labels, so the only thing wrong with it is its total length.
const NAME_255: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.ccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc.ddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

#[rustfmt::skip]
const CORPUS: &[Case] = &[
    // ── The trap, first, because it is the reason the crate exists ─────
    // With `UIDNA_DEFAULT` (0) ICU answers `strasse.de` and `fass.de` for
    // these two — IDNA2003's answer, a different origin. If `OPTIONS`
    // loses UIDNA_NONTRANSITIONAL_TO_ASCII, these two rows are what says
    // so.
    Case { what: "sharp s: THE transitional/non-transitional divergence", input: "straße.de", idna_says: Some("xn--strae-oqa.de"), icu_says: Some("xn--strae-oqa.de") },
    Case { what: "sharp s again, shorter label, same bit", input: "faß.de", idna_says: Some("xn--fa-hia.de"), icu_says: Some("xn--fa-hia.de") },
    Case { what: "final sigma: the second deviation character", input: "ςoly.de", idna_says: Some("xn--oly-lzc.de"), icu_says: Some("xn--oly-lzc.de") },
    Case { what: "ZWJ: deviation character and a ContextJ rule at once", input: "a\u{200d}b.de", idna_says: None, icu_says: None },
    Case { what: "ZWNJ: the fourth deviation character", input: "a\u{200c}b.de", idna_says: None, icu_says: None },

    // ── The ordinary cases, which must not move ───────────────────────
    Case { what: "the plain IDN case", input: "münchen.de", idna_says: Some("xn--mnchen-3ya.de"), icu_says: Some("xn--mnchen-3ya.de") },
    Case { what: "upper-case non-ASCII: case folding happens before punycode", input: "MÜNCHEN.de", idna_says: Some("xn--mnchen-3ya.de"), icu_says: Some("xn--mnchen-3ya.de") },
    Case { what: "an input that is already an A-label: must be a no-op", input: "xn--mnchen-3ya.de", idna_says: Some("xn--mnchen-3ya.de"), icu_says: Some("xn--mnchen-3ya.de") },
    Case { what: "upper-case ACE prefix: case folding runs BEFORE `xn--` is recognised, or this is not an A-label at all", input: "XN--MNCHEN-3YA.DE", idna_says: Some("xn--mnchen-3ya.de"), icu_says: Some("xn--mnchen-3ya.de") },
    Case { what: "the A-label of a DEVIATION name: a transitional platform re-encodes it to `strasse.de`, i.e. another origin", input: "xn--strae-oqa.de", idna_says: Some("xn--strae-oqa.de"), icu_says: Some("xn--strae-oqa.de") },
    Case { what: "an A-label beside a U-label: both come back as A-labels, and the first must come back byte for byte", input: "xn--mnchen-3ya.münchen.de", idna_says: Some("xn--mnchen-3ya.xn--mnchen-3ya.de"), icu_says: Some("xn--mnchen-3ya.xn--mnchen-3ya.de") },
    Case { what: "IDEOGRAPHIC FULL STOP: a label separator that is not `.`, and the row that fails if only `.` is split on", input: "a\u{3002}b", idna_says: Some("a.b"), icu_says: Some("a.b") },
    Case { what: "all-ASCII", input: "example.com", idna_says: Some("example.com"), icu_says: Some("example.com") },
    Case { what: "upper-case ASCII: lower-cased, NOT passed through", input: "EXAMPLE.COM", idna_says: Some("example.com"), icu_says: Some("example.com") },
    Case { what: "two non-ASCII labels, non-Latin script", input: "例え.テスト", idna_says: Some("xn--r8jz45g.xn--zckzah"), icu_says: Some("xn--r8jz45g.xn--zckzah") },
    Case { what: "the root label: a trailing dot is a legal name, not an empty label to reject", input: "example.com.", idna_says: Some("example.com."), icu_says: Some("example.com.") },

    // ── CheckHyphens=false — ICU has no option for this and reports it
    //    as errors 0x08/0x10/0x20, which IGNORED_ERRORS masks off ──────
    Case { what: "leading hyphen: UIDNA_ERROR_LEADING_HYPHEN, masked", input: "-lead.de", idna_says: Some("-lead.de"), icu_says: Some("-lead.de") },
    Case { what: "trailing hyphen: UIDNA_ERROR_TRAILING_HYPHEN, masked", input: "trail-.de", idna_says: Some("trail-.de"), icu_says: Some("trail-.de") },
    Case { what: "hyphens in 3rd and 4th position: UIDNA_ERROR_HYPHEN_3_4, masked", input: "ab--cd.com", idna_says: Some("ab--cd.com"), icu_says: Some("ab--cd.com") },

    // ── VerifyDnsLength=false — same story, errors 0x01/0x02/0x04 ─────
    Case { what: "empty label", input: "a..b", idna_says: Some("a..b"), icu_says: Some("a..b") },
    Case { what: "the empty name", input: "", idna_says: Some(""), icu_says: Some("") },
    Case { what: "a 64-byte label: over the DNS limit, under UTS 46 with VerifyDnsLength off", input: LABEL_64, idna_says: Some(LABEL_64), icu_says: Some(LABEL_64) },
    Case { what: "a 255-byte name: over RFC 1035's 253, same reason", input: NAME_255, idna_says: Some(NAME_255), icu_says: Some(NAME_255) },

    // ── The WHATWG deny list, which ICU has no option for either ──────
    Case { what: "a forbidden domain code point", input: "a<b.com", idna_says: None, icu_says: None },
    Case { what: "percent: forbidden by the URL standard, VALID under UTS 46 with STD3 off", input: "a%b.com", idna_says: None, icu_says: None },
    Case { what: "space: the glyphless half of the deny list", input: "a b.com", idna_says: None, icu_says: None },
    Case { what: "NUL: the other end of the glyphless range, and no NUL terminator is involved", input: "a\u{0}b.com", idna_says: None, icu_says: None },
    Case { what: "underscore: allowed by the URL list, DENIED by STD3 — the row that fails if UIDNA_USE_STD3_RULES is ever set", input: "a_b.com", idna_says: Some("a_b.com"), icu_says: Some("a_b.com") },
    Case { what: "a quote: allowed by the WHATWG deny list, forbidden in an RFC 3986 `reg_name` — so a URL parser refuses it and only the ASCII short-circuit can answer it", input: "a\"b.com", idna_says: Some("a\"b.com"), icu_says: Some("a\"b.com") },
    Case { what: "fullwidth solidus: MAPS to `/`, so only a scan of the OUTPUT can catch it", input: "a\u{ff0f}b.de", idna_says: None, icu_says: None },
    Case { what: "a denied byte inside an A-label: punycode copies basic code points literally", input: "xn--%-0fa.de", idna_says: None, icu_says: None },

    // ── Rejections that must stay rejections ──────────────────────────
    Case { what: "an `xn--` label that is not valid punycode", input: "xn--zzzz.test", idna_says: None, icu_says: None },
    Case { what: "the shortest invalid `xn--` label, and the one browsers accept", input: "xn--a.de", idna_says: None, icu_says: None },
    Case { what: "`xn--` with nothing after it at all", input: "xn--.de", idna_says: None, icu_says: None },
    Case { what: "an ACE label whose punycode is valid and decodes to ASCII alone: UIDNA_ERROR_INVALID_ACE_LABEL, which is NOT the decoder's refusal but the layer above it", input: "xn--a-.de", idna_says: None, icu_says: None },
    Case { what: "a leading combining mark", input: "\u{301}abc.de", idna_says: None, icu_says: None },
    // Written into the corpus as `None`/`None` on the assumption that
    // emoji are disallowed — IDNA2008 does disallow them. Both
    // implementations said otherwise, in agreement, and the row is pinned
    // at what they said: UTS 46's mapping table marks U+2615 *valid*, so
    // an emoji domain converts. Kept because it is the only row where the
    // guess was wrong, and a corpus whose rows were all guessed correctly
    // is a corpus that was written from the implementation.
    Case { what: "an emoji: valid under UTS 46, whatever IDNA2008 says", input: "☕.example", idna_says: Some("xn--53h.example"), icu_says: Some("xn--53h.example") },
    Case { what: "a genuinely disallowed code point (NBSP)", input: "a\u{a0}b.de", idna_says: None, icu_says: None },

    // ── CheckBidi, which is always on in `idna` and an OPTION in ICU ──
    Case { what: "a valid RTL name: must survive", input: "مثال.إختبار", idna_says: Some("xn--mgbh0fb.xn--kgbechtv"), icu_says: Some("xn--mgbh0fb.xn--kgbechtv") },
    Case { what: "an RTL label ending in a Latin letter: the row that fails if UIDNA_CHECK_BIDI is dropped", input: "اa.de", idna_says: None, icu_says: None },
];
