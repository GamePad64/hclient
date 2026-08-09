//! What a platform backend is actually asked, and what stays here.
//!
//! A platform backend is asked ONE question: given a name that contains a
//! character outside ASCII, what is its A-label form? Everything a UTS 46
//! implementation does that is decidable from ASCII alone is decided here
//! instead — and that is not a preference, it is what the two backends
//! disagree about.
//!
//! **Measured on `macos-latest`, which is where this section comes from.**
//! Windows' `icuuc.dll` IS a UTS 46 implementation: `uidna_nameToASCII_UTF8`
//! case-folds ASCII through the same mapping table it uses for everything
//! else, answers the empty name with the empty name plus a masked
//! `UIDNA_ERROR_EMPTY_LABEL`, and decodes and validates every `xn--` label,
//! reporting `UIDNA_ERROR_PUNYCODE`. Apple's Foundation is not a UTS 46
//! implementation — it is a URL parser that CALLS one, and only for a host
//! that is not ASCII. So an ASCII host is passed through verbatim and a
//! name with no host at all is a parse failure. The first run of the corpus
//! on a real macOS runner found exactly that, on three rows of 32:
//!
//! | input | `idna` | Foundation | why |
//! |---|---|---|---|
//! | `EXAMPLE.COM` | `example.com` | `EXAMPLE.COM` | ASCII, so the hook never ran |
//! | `""` | `""` | *(nil)* | `https:///` has no host to return |
//! | `xn--zzzz.test` | *(reject)* | `xn--zzzz.test` | ASCII, so nothing decoded the punycode |
//!
//! Fixing those three per-backend would mean a second, macOS-shaped policy
//! beside the ICU one — two places for one contract, which is the defect
//! this crate exists to avoid. They are fixed here instead, in one
//! function both backends go through, and the Windows corpus run is what
//! says the shared policy did not change ICU's answers.

use crate::is_forbidden_domain_byte;

/// The ACE prefix. A label that starts with it is punycode, whatever else
/// it looks like.
///
/// Matched AFTER ASCII case folding, so `XN--MNCHEN-3YA.DE` is an ACE
/// label too — which it has to be, because `idna` answers
/// `xn--mnchen-3ya.de` for it.
const ACE_PREFIX: &str = "xn--";

/// The code points that separate labels — U+002E and the three
/// compatibility full stops UTS 46's mapping table turns into it.
///
/// **Measured, not transcribed.** A list of Unicode code points written
/// out by hand in a crate whose entire purpose is not carrying Unicode
/// data is exactly the thing that would be wrong and look right, so
/// `the_label_separators_are_exactly_these_four` puts all 1 112 064
/// scalar values through `idna` and asserts that no fifth one maps to a
/// dot.
///
/// Splitting on them here rather than only on `.` is required, not
/// tidiness: the platform maps them *for* us, so a name split on `.`
/// alone would come back from the platform with more labels than it went
/// in with, and [`ascii_labels_survived`] would refuse a name `idna`
/// accepts.
const LABEL_SEPARATORS: [char; 4] = ['.', '\u{3002}', '\u{ff0e}', '\u{ff61}'];

fn is_label_separator(c: char) -> bool {
    LABEL_SEPARATORS.contains(&c)
}

// ── RFC 3492, and it needs no Unicode data at all ───────────────────────
//
// The five parameters of the "punycode" instance of Bootstring, plus the
// two initial values, from RFC 3492 §5. They are here rather than in a
// dependency for the reason the whole crate exists: punycode is integer
// arithmetic over `[a-z0-9-]` with nothing to tabulate, so decoding it
// costs no tables — whereas *validating* what comes out needs the whole of
// UTS 46, which is what the platform is for.
const BASE: u32 = 36;
const TMIN: u32 = 1;
const TMAX: u32 = 26;
const SKEW: u32 = 38;
const DAMP: u32 = 700;
const INITIAL_BIAS: u32 = 72;
/// The first non-basic code point. RFC 3492 calls anything below this a
/// *basic* code point, and this crate's guarantee that a decoded label can
/// contain no ASCII the input did not already contain rests on it.
const INITIAL_N: u32 = 128;

/// RFC 3492 §5's digit values — **lower case and digits only**, where the
/// RFC says a general decoder must accept both cases.
///
/// This is not a general decoder. The only caller is [`to_ascii_over`],
/// which has already ASCII case-folded, so an upper-case digit reaching
/// here would mean the case folding did not happen — and accepting it
/// would hide that. A first draft did accept both cases and three of its
/// mutants survived, which is the same statement made by measurement:
/// nothing could reach that arm.
const fn decode_digit(b: u8) -> Option<u32> {
    match b {
        b'a'..=b'z' => Some((b - b'a') as u32),
        b'0'..=b'9' => Some((b - b'0') as u32 + 26),
        _ => None,
    }
}

/// RFC 3492 §6.1's bias adaptation, transcribed.
const fn adapt(delta: u32, numpoints: u32, first: bool) -> u32 {
    let mut delta = if first { delta / DAMP } else { delta / 2 };
    delta += delta / numpoints;
    let mut k = 0;
    while delta > ((BASE - TMIN) * TMAX) / 2 {
        delta /= BASE - TMIN;
        k += BASE;
    }
    k + (((BASE - TMIN + 1) * delta) / (delta + SKEW))
}

/// Decodes the payload of an ACE label — everything after `xn--` — into
/// the label it stands for, or `None` if it is not valid punycode.
///
/// **This is a decoder, not `toUnicode`.** It performs no mapping and no
/// validation; whether the label it returns is a *usable* one is the
/// platform's question, and [`to_ascii_over`] asks it by handing the
/// answer straight back for re-encoding.
///
/// Two properties the rest of this file relies on, both enforced here
/// rather than assumed:
///
/// - **every code point it inserts is non-basic** (`n >= `[`INITIAL_N`]),
///   which is RFC 3492's own "if n is a basic code point then fail". So
///   the only ASCII in the output is ASCII that was already in the input,
///   copied literally out of the basic part — which is why the WHATWG deny
///   list can be applied to the input once and cover the decoded form too,
///   and why a decoded label can never contain a `.`;
/// - **every arithmetic step is checked**, so RFC 3492's overflow
///   conditions are refusals rather than wraps. `char::from_u32` then
///   refuses surrogates and anything above U+10FFFF.
fn decode_punycode(payload: &str) -> Option<String> {
    let payload = payload.as_bytes();
    // A non-ASCII byte after `xn--` is not a digit and not a basic code
    // point; UTS 46 fails the punycode precondition on it, and copying it
    // into the basic part would split a UTF-8 sequence.
    if !payload.is_ascii() {
        return None;
    }
    // RFC 3492 §6.2: the delimiter is the LAST hyphen, and everything
    // before it is copied literally.
    let (basic, mut digits) = match payload.iter().rposition(|b| *b == b'-') {
        Some(at) => (&payload[..at], &payload[at + 1..]),
        None => (&payload[..0], payload),
    };

    let mut out: Vec<char> = basic.iter().map(|b| char::from(*b)).collect();
    let (mut n, mut i, mut bias) = (INITIAL_N, 0u32, INITIAL_BIAS);

    while !digits.is_empty() {
        let old_i = i;
        let (mut w, mut k) = (1u32, BASE);
        loop {
            let (digit, rest) = digits.split_first()?;
            digits = rest;
            let digit = decode_digit(*digit)?;
            i = i.checked_add(digit.checked_mul(w)?)?;
            let t = if k <= bias {
                TMIN
            } else if k >= bias + TMAX {
                TMAX
            } else {
                k - bias
            };
            if digit < t {
                break;
            }
            w = w.checked_mul(BASE - t)?;
            k += BASE;
        }
        let len = u32::try_from(out.len()).ok()?.checked_add(1)?;
        bias = adapt(i - old_i, len, old_i == 0);
        // RFC 3492's "if n is a basic code point then fail" is not written
        // out here, and its absence is deliberate rather than an
        // oversight: `n` starts at INITIAL_N and every step adds an
        // unsigned quantity through `checked_add`, so it can only grow. A
        // test that killed such a check would need `n` to DECREASE, which
        // no input can cause — measured, as two surviving mutants, before
        // the check was removed. The property it would have guarded is
        // pinned instead by `decoding_never_invents_an_ascii_character`.
        n = n.checked_add(i / len)?;
        i %= len;
        out.insert(usize::try_from(i).ok()?, char::from_u32(n)?);
        i = i.checked_add(1)?;
    }
    Some(out.into_iter().collect())
}

/// The crate's whole policy, over any platform conversion — the shape
/// [`accepts`] already uses, and for the same reason: a function that only
/// takes a real backend can only be tested by owning the platform, and
/// nobody here owns two.
///
/// `convert` is asked exactly one kind of question: **a name containing
/// non-ASCII, in its Unicode form**. Everything else is settled here.
///
/// 1. **The WHATWG deny list, on the input, before anything else.** For
///    Foundation this is what stops a URL parser reading `ex@ample.com` as
///    the host `ample.com`; for ICU it is redundant and cheap. It runs on
///    the input rather than only on the output because the input is the
///    only place a denied byte that a parser would *consume as a
///    delimiter* is still visible.
/// 2. **ASCII case folding.** The only ASCII mapping in UTS 46 is
///    upper-case to lower-case, and no code point outside ASCII maps to an
///    ASCII upper-case letter, so doing it here is doing it exactly once.
/// 3. **ACE labels are decoded here** — see [`decode_punycode`] — and a
///    label that decodes to nothing, or to ASCII alone, is refused:
///    UTS 46 calls that `UIDNA_ERROR_INVALID_ACE_LABEL`, and `idna`
///    refuses `xn--`, `xn---`, `xn----` and `xn--a-` for exactly this.
/// 4. **An all-ASCII name never reaches the platform at all.** With
///    _UseSTD3ASCIIRules=false_, _CheckHyphens=false_ and
///    _VerifyDnsLength=false_ — the three columns of this crate's contract
///    table — UTS 46 does nothing to an ASCII name but lower-case it. That
///    is a claim about `idna`, so it is measured against `idna` rather
///    than argued: `every_ascii_name_is_its_own_answer_unless_it_has_an_ace_label`
///    checks all 16 384
///    two-byte ASCII strings and 537 824 more from an alphabet chosen to
///    be all hyphens, dots, `xn`-prefixes and case.
/// 5. **Every ASCII label must come back exactly as it went in.** The
///    platform is asked about the labels this crate cannot decide, and
///    nothing else; a backend that answered `xn--strae-oqa.de` with
///    `strasse.de`, or that renamed an ASCII label on the way past, is
///    refused rather than believed. This is what makes an ACE label safe
///    to accept: it is emitted only after the platform has re-encoded its
///    decoded form to the very same bytes.
pub(crate) fn to_ascii_over(
    convert: impl Fn(&str) -> Option<String>,
    domain: &str,
) -> Option<String> {
    if domain.bytes().any(is_forbidden_domain_byte) {
        return None;
    }
    let lower = domain.to_ascii_lowercase();

    let mut unicode = String::with_capacity(lower.len());
    for (nth, label) in lower.split(is_label_separator).enumerate() {
        if nth > 0 {
            unicode.push('.');
        }
        match label.strip_prefix(ACE_PREFIX) {
            Some(payload) => {
                let decoded = decode_punycode(payload)?;
                // `is_ascii` is true of the empty string too, which is the
                // `xn--` and `xn---` case. A separator inside a decoded
                // label is UTS 46's `UIDNA_ERROR_LABEL_HAS_DOT`: it cannot
                // be `.`, which is a basic code point and so unreachable
                // here, but it can be one of the other three.
                if decoded.is_ascii() || decoded.contains(is_label_separator) {
                    return None;
                }
                unicode.push_str(&decoded);
            }
            None => unicode.push_str(label),
        }
    }

    // Reachable only when no label was an ACE label, because a decoded one
    // is non-ASCII by the check above.
    if unicode.is_ascii() {
        return Some(unicode);
    }
    // `decode_punycode` guarantees it introduced no ASCII, so this cannot
    // fire — checked rather than trusted, because what it guards is a URL
    // parser being handed a delimiter.
    if unicode.bytes().any(is_forbidden_domain_byte) {
        return None;
    }

    let ascii = convert(&unicode)?;
    if !ascii.is_ascii() || ascii.bytes().any(is_forbidden_domain_byte) {
        return None;
    }
    if !ascii_labels_survived(&lower, &ascii) {
        return None;
    }
    Some(ascii)
}

/// Whether every ASCII label of the input came back unchanged, and the
/// name still has the same number of labels.
///
/// The ACE labels are the reason this exists: `idna` does not re-encode a
/// valid `xn--` label, it emits the one it was given, so this crate must
/// emit that one too — and may only do so once the platform has agreed
/// that re-encoding the decoded form produces it. A label the platform
/// canonicalised differently is one this crate could not confirm, and an
/// A-label nobody confirmed is a host nobody checked.
///
/// The label count is part of it: a mapping that turns a code point into
/// `.` (U+3002 does) would otherwise slide the labels past each other.
fn ascii_labels_survived(lower: &str, ascii: &str) -> bool {
    let mut theirs = ascii.split('.');
    for ours in lower.split(is_label_separator) {
        let Some(theirs) = theirs.next() else {
            return false;
        };
        if ours.is_ascii() && ours != theirs {
            return false;
        }
    }
    theirs.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The oracle, called exactly as `http-ng-proto::uri::host_to_ascii`
    /// reaches it through this crate — and, in [`over_idna`], standing in
    /// for a platform that IS a full UTS 46 implementation, which is what
    /// Windows' ICU is.
    fn idna_says(domain: &str) -> Option<String> {
        idna::domain_to_ascii_cow(domain.as_bytes(), idna::AsciiDenyList::URL)
            .ok()
            .map(std::borrow::Cow::into_owned)
    }

    /// The policy over a backend that answers everything correctly. Any
    /// difference from [`idna_says`] is the policy's own.
    fn over_idna(domain: &str) -> Option<String> {
        to_ascii_over(idna_says, domain)
    }

    /// Apple's Foundation, modelled from what `macos-latest` measured and
    /// what `swiftlang/swift-foundation` says, so that the three rows this
    /// module exists to fix can be checked on a Linux runner.
    ///
    /// Two behaviours, and they are the *reason* those rows failed:
    ///
    /// 1. **An empty host is nil.** `URL_Swift.swift`'s `host()` returns
    ///    nil for `https:///`, with a comment saying it is deliberate —
    ///    "apps rely on this behavior, so keep it for bincompat".
    /// 2. **An ASCII host never reaches the IDNA hook.**
    ///    `URLParser.swift` copies a host that passes RFC 3986 `reg_name`
    ///    validation straight into the URL (`finalURLString += host`), and
    ///    only a host that fails it goes to `IDNAEncodeHost`. So
    ///    `EXAMPLE.COM` keeps its case and `xn--zzzz.test` keeps its
    ///    unexamined punycode — and an ASCII byte outside `unreserved` /
    ///    `sub-delims`, which the WHATWG deny list permits, makes the whole
    ///    URL nil.
    ///
    /// **Deliberately NOT modelled: `shouldAllow(_:encodeToASCII: true)`,
    /// which allows no `UIDNAInfo.errors` bit at all.** Where a name has a
    /// non-ASCII label AND trips one of the six bits
    /// [`crate::IGNORED_ERRORS`] masks, real Foundation answers nil and
    /// this model answers what `idna` does. That residue is written down
    /// in `foundation.rs`; it is not fixable from here, and modelling it
    /// would mean guessing which bits ICU sets on which input, which is
    /// the kind of unmeasured claim this crate does not make.
    fn foundation_model(domain: &str) -> Option<String> {
        const fn is_reg_name_byte(b: u8) -> bool {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'-' | b'.'
                        | b'_'
                        | b'~'
                        | b'!'
                        | b'$'
                        | b'&'
                        | b'\''
                        | b'('
                        | b')'
                        | b'*'
                        | b'+'
                        | b','
                        | b';'
                        | b'='
                        | b'%'
                )
        }
        if domain.is_empty() {
            return None;
        }
        if domain.is_ascii() {
            return domain
                .bytes()
                .all(is_reg_name_byte)
                .then(|| domain.to_owned());
        }
        let out = idna_says(domain)?;
        out.bytes().all(is_reg_name_byte).then_some(out)
    }

    fn over_foundation(domain: &str) -> Option<String> {
        to_ascii_over(foundation_model, domain)
    }

    /// The inputs `tests/differential.rs` pins, plus the ones this module
    /// added. Kept here as well because the corpus can only measure the
    /// platform column where there IS a platform, and these tests have to
    /// run on the runner that runs everything.
    const NAMES: &[&str] = &[
        "straße.de",
        "faß.de",
        "ςoly.de",
        "a\u{200d}b.de",
        "a\u{200c}b.de",
        "münchen.de",
        "MÜNCHEN.de",
        "xn--mnchen-3ya.de",
        "XN--MNCHEN-3YA.DE",
        "xn--strae-oqa.de",
        "xn--mnchen-3ya.münchen.de",
        "example.com",
        "EXAMPLE.COM",
        "例え.テスト",
        "example.com.",
        "-lead.de",
        "trail-.de",
        "ab--cd.com",
        "a..b",
        "",
        ".",
        "a<b.com",
        "a%b.com",
        "a b.com",
        "a\u{0}b.com",
        "a_b.com",
        "a\"b.com",
        "a`b.com",
        "a{b}.com",
        "a\u{ff0f}b.de",
        "xn--%-0fa.de",
        "xn--zzzz.test",
        "xn--a.de",
        "xn--.de",
        "xn---.de",
        "xn----.de",
        "xn--a-.de",
        "xn--ü.de",
        "\u{301}abc.de",
        "☕.example",
        "a\u{a0}b.de",
        "مثال.إختبار",
        "اa.de",
    ];

    /// **The policy adds nothing and takes nothing away.** Over a backend
    /// that is already a full UTS 46 implementation — which is what
    /// `icuuc.dll` is — every answer must be the one `idna` gives.
    ///
    /// This is the test that says the shared policy is safe to put in
    /// front of the Windows backend, written on a machine that has no
    /// Windows.
    #[test]
    fn the_policy_is_transparent_over_a_real_uts46() {
        let mut wrong = Vec::new();
        for name in NAMES {
            let (got, want) = (over_idna(name), idna_says(name));
            if got != want {
                wrong.push(format!("  {name:?}: policy {got:?}, `idna` {want:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of {} names changed when the policy was put in front of a correct backend:\n{}",
            wrong.len(),
            NAMES.len(),
            wrong.join("\n")
        );
    }

    /// **The three rows `macos-latest` failed, and why they now pass.**
    /// Over a Foundation-shaped backend the policy must still answer what
    /// `idna` answers — which it can only do by case-folding, answering
    /// the empty name and decoding punycode itself.
    #[test]
    fn the_policy_repairs_what_a_url_parser_does_not_do() {
        let mut wrong = Vec::new();
        for name in NAMES {
            let (got, want) = (over_foundation(name), idna_says(name));
            if got != want {
                wrong.push(format!("  {name:?}: policy {got:?}, `idna` {want:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of {} names still differ from `idna` over a Foundation-shaped backend:\n{}",
            wrong.len(),
            NAMES.len(),
            wrong.join("\n")
        );
    }

    /// The three, one at a time and by name, so a failure says which of
    /// them regressed rather than "one of 43 names".
    #[test]
    fn the_three_rows_macos_failed_on() {
        assert_eq!(
            over_foundation("EXAMPLE.COM").as_deref(),
            Some("example.com"),
            "upper-case ASCII: Foundation passes an ASCII host through verbatim, so the case \
             folding has to be this layer's"
        );
        assert_eq!(
            over_foundation("").as_deref(),
            Some(""),
            "the empty name: `https:///` has no host for Foundation to return, and `idna` \
             answers the empty name with itself"
        );
        assert_eq!(
            over_foundation("xn--zzzz.test"),
            None,
            "invalid punycode: Foundation never decodes an ASCII host, so nothing there ever \
             discovers that `zzzz` is not punycode"
        );
    }

    /// The ASCII short-circuit, stated as the property it is: a name that
    /// is ASCII and has no ACE label is answered without the platform
    /// being asked anything at all.
    ///
    /// That is what puts the three rows above out of a URL parser's reach
    /// — and it is why `a"b.com`, which RFC 3986 forbids in a `reg_name`
    /// and the WHATWG deny list allows, is answered here rather than
    /// refused by Foundation.
    #[test]
    fn an_all_ascii_name_never_reaches_the_platform() {
        let plain = NAMES.iter().filter(|n| {
            n.is_ascii()
                && !n
                    .to_ascii_lowercase()
                    .split('.')
                    .any(|l| l.starts_with(ACE_PREFIX))
        });
        let mut checked = 0usize;
        for name in plain {
            let got = to_ascii_over(
                |asked| {
                    panic!("the platform was asked about {asked:?} for the ASCII name {name:?}")
                },
                name,
            );
            assert_eq!(got, idna_says(name), "{name:?}");
            checked += 1;
        }
        assert!(checked >= 15, "the filter left almost nothing: {checked}");
    }

    /// The measurement behind step 4 of [`to_ascii_over`], and the one
    /// claim in this module that is not a transcription of a
    /// specification: with _UseSTD3ASCIIRules_, _CheckHyphens_ and
    /// _VerifyDnsLength_ all false, UTS 46 does nothing to an ASCII name
    /// but lower-case it — unless a label is an ACE label.
    ///
    /// Exhaustive over every two-byte ASCII string, and over five-byte
    /// strings from an alphabet that is nothing but the characters which
    /// could break it: the ACE prefix's letters, hyphens, dots, digits,
    /// case, and the deny list's edges.
    #[test]
    fn every_ascii_name_is_its_own_answer_unless_it_has_an_ace_label() {
        let expected = |s: &str| {
            if s.bytes().any(is_forbidden_domain_byte) {
                None
            } else {
                Some(s.to_ascii_lowercase())
            }
        };
        for a in 0..=0x7fu8 {
            for b in 0..=0x7fu8 {
                let s = String::from_utf8(vec![a, b]).expect("two ASCII bytes are UTF-8");
                assert_eq!(idna_says(&s), expected(&s), "{s:?}");
            }
        }
        const ALPHABET: [u8; 14] = *b"ab-.xn09_AZ~!*";
        let mut checked = 0usize;
        for a in ALPHABET {
            for b in ALPHABET {
                for c in ALPHABET {
                    for d in ALPHABET {
                        for e in ALPHABET {
                            let s = String::from_utf8(vec![a, b, c, d, e])
                                .expect("five ASCII bytes are UTF-8");
                            if s.split('.').any(|l| l.starts_with(ACE_PREFIX)) {
                                continue;
                            }
                            checked += 1;
                            assert_eq!(idna_says(&s), expected(&s), "{s:?}");
                        }
                    }
                }
            }
        }
        assert!(
            checked > 500_000,
            "the five-byte sweep skipped almost everything: {checked} names"
        );
    }

    // ── The punycode decoder ────────────────────────────────────────────

    /// Round-trip against the oracle: `idna` encodes, this decodes, and
    /// the label that comes back must be the one that went in.
    ///
    /// Generated from the oracle rather than transcribed from RFC 3492's
    /// examples on purpose — a transcription error would be a wrong host
    /// behind a green test.
    #[test]
    fn what_idna_encodes_this_decodes_back() {
        for label in [
            "münchen",
            "straße",
            "faß",
            "bücher",
            "mañana",
            "例え",
            "テスト",
            "☕",
            "ü",
            "مثال",
            "إختبار",
            "ςoly",
            "日本",
            "☃",
            "a☕b",
            "-ü",
            "ü-",
            "aü9",
        ] {
            let ascii = idna_says(&format!("{label}.test")).expect("the oracle must convert");
            let ace = ascii.split('.').next().expect("a first label");
            let payload = ace
                .strip_prefix(ACE_PREFIX)
                .unwrap_or_else(|| panic!("{label:?} did not encode to an ACE label: {ace:?}"));
            assert_eq!(
                decode_punycode(payload).as_deref(),
                Some(label),
                "{ace:?} did not decode back to {label:?}"
            );
        }
    }

    /// RFC 3492 §6.1's loop guard is `>`, not `>=`, and one step of that
    /// loop divides `delta` by 35 and adds 36 to the answer — so the
    /// difference between the two is a different bias, a different `t` on
    /// the next digit, and eventually a different label.
    ///
    /// No name in this file's corpora lands `delta` on the boundary
    /// exactly (`((base - tmin) * tmax) / 2`, which is 455), so the
    /// boundary is pinned here directly. `adapt(910, 456, false)` halves
    /// 910 to 455 and then adds `455 / 456`, i.e. nothing — landing on it.
    #[test]
    fn the_bias_adaptations_loop_boundary_is_exclusive() {
        assert_eq!(((BASE - TMIN) * TMAX) / 2, 455);
        let on_it = adapt(910, 456, false);
        let just_past = adapt(912, 456, false);
        assert!(
            on_it < BASE,
            "at exactly 455 the loop must not run, so the answer is below one `k` step: {on_it}"
        );
        assert!(
            just_past >= BASE,
            "one past it the loop must run once, so the answer is at least one `k` step: \
             {just_past}"
        );
        // And the two halves of `first`, which pick DAMP over 2.
        assert_eq!(adapt(700, 1, true), adapt(2, 1, false));
    }

    /// The measured payloads as literals, so the decoder is pinned by more
    /// than agreement with the crate it exists to do without.
    #[test]
    fn the_measured_payloads_decode_to_the_measured_labels() {
        for (payload, label) in [
            ("mnchen-3ya", "münchen"),
            ("strae-oqa", "straße"),
            ("fa-hia", "faß"),
            ("r8jz45g", "例え"),
            ("zckzah", "テスト"),
            ("53h", "☕"),
            ("tda", "ü"),
            ("mgbh0fb", "مثال"),
            ("kgbechtv", "إختبار"),
            ("oly-lzc", "ςoly"),
        ] {
            assert_eq!(
                decode_punycode(payload).as_deref(),
                Some(label),
                "{payload:?}"
            );
        }
    }

    /// Everything the decoder must refuse, with the reason each one is
    /// here. That `idna` refuses all of them too is checked rather than
    /// asserted, by [`the_policy_is_transparent_over_a_real_uts46`] over
    /// the `xn--` rows of [`NAMES`].
    #[test]
    fn invalid_punycode_is_refused_rather_than_guessed_at() {
        for (payload, why) in [
            ("zzzz", "digits that run past the last scalar value"),
            ("0", "a digit that decodes to no valid scalar"),
            (
                "ü",
                "a non-ASCII byte after `xn--` is neither a digit nor basic",
            ),
            ("mnchen-3y", "a truncated payload from a real name"),
            ("mnchen-3ya!", "a byte that is not a punycode digit"),
            ("999999999999999", "an overflow of the delta accumulator"),
            (
                "zzzzzzzzzzzzzzzzzzzzzzzz",
                "the same overflow, the long way",
            ),
        ] {
            assert_eq!(decode_punycode(payload), None, "{payload:?}: {why}");
        }
    }

    /// **The decoder is a decoder, and stops there.** These payloads are
    /// valid punycode *and* unusable labels — nothing after the delimiter,
    /// so the answer is pure ASCII — and it is [`to_ascii_over`], not
    /// [`decode_punycode`], that refuses them, because the rule is UTS 46's
    /// `UIDNA_ERROR_INVALID_ACE_LABEL` rather than RFC 3492's.
    ///
    /// Worth a test of its own: if the decoder ever started refusing them
    /// itself, the layering would be wrong in a way that still passed
    /// every other test here.
    #[test]
    fn a_payload_with_no_digits_decodes_and_is_refused_one_layer_up() {
        for (payload, decoded) in [
            ("", ""),
            ("-", ""),
            ("--", "-"),
            ("a-", "a"),
            ("mnchen-3ya-", "mnchen-3ya"),
        ] {
            assert_eq!(
                decode_punycode(payload).as_deref(),
                Some(decoded),
                "{payload:?} is valid punycode for {decoded:?}"
            );
            assert_eq!(
                over_idna(&format!("xn--{payload}.de")),
                None,
                "xn--{payload}.de decodes to ASCII and must be refused"
            );
        }
    }

    /// The list of label separators, measured rather than believed: every
    /// Unicode scalar value is put through the oracle, and the ones that
    /// come back as a dot are exactly [`LABEL_SEPARATORS`].
    ///
    /// A fifth one appearing in a future Unicode version would otherwise
    /// be silent — the policy would hand the platform a name split one way
    /// and get it back split another, and refuse a name `idna` accepts.
    #[test]
    fn the_label_separators_are_exactly_these_four() {
        let mut found: Vec<char> = Vec::new();
        for c in '\0'..=char::MAX {
            if idna_says(&format!("a{c}b")).as_deref() == Some("a.b") {
                found.push(c);
            }
        }
        assert_eq!(
            found,
            LABEL_SEPARATORS.to_vec(),
            "the set of code points UTS 46 turns into a label separator is not the one \
             `LABEL_SEPARATORS` lists"
        );
    }

    /// A decoded label can only contain ASCII the encoded one already
    /// contained, because RFC 3492 forbids inserting a basic code point.
    /// The rest of this file leans on it: it is why the deny list runs
    /// once, on the input, and still covers the decoded form, and why a
    /// decoded label can never carry a `.`.
    #[test]
    fn decoding_never_invents_an_ascii_character() {
        let mut decoded_any = 0usize;
        for payload in [
            "mnchen-3ya",
            "strae-oqa",
            "r8jz45g",
            "53h",
            "tda",
            "oly-lzc",
            "%-0fa",
            "caf-dma",
            "e-9fa",
            "9dbq2a",
        ] {
            let Some(decoded) = decode_punycode(payload) else {
                continue;
            };
            decoded_any += 1;
            let basic = payload.rsplit_once('-').map_or("", |(head, _)| head);
            for c in decoded.chars().filter(char::is_ascii) {
                assert!(
                    basic.contains(c),
                    "{payload:?} decoded to {decoded:?}, whose ASCII {c:?} is not in the basic \
                     part {basic:?} — RFC 3492's \"if n is a basic code point then fail\" is not \
                     being enforced"
                );
            }
            assert!(
                !decoded.contains('.'),
                "{payload:?} decoded to a label with a dot in it"
            );
        }
        assert!(decoded_any >= 8, "almost nothing decoded: {decoded_any}");
    }

    /// An ACE label that decodes to nothing, or to ASCII alone, is not a
    /// usable label — UTS 46 reports `UIDNA_ERROR_INVALID_ACE_LABEL`, and
    /// `idna` refuses all four of these.
    #[test]
    fn an_ace_label_that_decodes_to_ascii_is_refused() {
        for input in ["xn--.de", "xn---.de", "xn----.de", "xn--a-.de"] {
            assert_eq!(over_idna(input), None, "{input:?}");
            assert_eq!(idna_says(input), None, "the oracle must agree: {input:?}");
        }
    }

    // ── The round-trip that makes an A-label safe to emit ───────────────

    /// **A transitional backend cannot smuggle an A-label past this.**
    /// `xn--strae-oqa.de` decodes to `straße.de`, and a backend that
    /// answers `strasse.de` for that has re-encoded it to a different
    /// origin — the label it produced is not the label that arrived.
    ///
    /// The acceptance gate in `lib.rs` catches the same backend on the
    /// Unicode form. This catches it on the A-label form, which the gate
    /// never sees.
    #[test]
    fn a_transitional_backend_cannot_answer_an_ace_label() {
        let transitional = |domain: &str| match domain {
            "straße.de" => Some("strasse.de".to_owned()),
            other => idna_says(other),
        };
        assert_eq!(
            to_ascii_over(transitional, "xn--strae-oqa.de"),
            None,
            "the platform re-encoded the decoded label to a different A-label and was believed"
        );
        assert_eq!(
            over_idna("xn--strae-oqa.de").as_deref(),
            Some("xn--strae-oqa.de"),
            "a correct backend must still answer it"
        );
    }

    /// The same guard on an ASCII label that is not an ACE label: a URL
    /// parser that rewrote `de` into something else would be refused too.
    #[test]
    fn a_backend_that_rewrites_an_ascii_label_is_refused() {
        let liar = |_: &str| Some("xn--mnchen-3ya.com".to_owned());
        assert_eq!(to_ascii_over(liar, "münchen.de"), None);
    }

    /// And a backend that changed how many labels there are. A code point
    /// that maps to `.` — U+3002 does — is how that happens by accident
    /// rather than by malice.
    #[test]
    fn a_backend_that_changes_the_label_count_is_refused() {
        let splitter = |_: &str| Some("xn--mnchen-3ya.de.extra".to_owned());
        assert_eq!(to_ascii_over(splitter, "münchen.de"), None);
        let joiner = |_: &str| Some("xn--mnchen-3ya".to_owned());
        assert_eq!(to_ascii_over(joiner, "münchen.de"), None);
    }

    #[test]
    fn the_survival_check_is_about_ascii_labels_and_label_counts() {
        assert!(ascii_labels_survived("münchen.de", "xn--mnchen-3ya.de"));
        assert!(ascii_labels_survived("a..b", "a..b"));
        assert!(!ascii_labels_survived("münchen.de", "xn--mnchen-3ya.com"));
        assert!(!ascii_labels_survived("münchen.de", "xn--mnchen-3ya"));
        assert!(!ascii_labels_survived("münchen.de", "xn--mnchen-3ya.de.x"));
        // A non-ASCII label is the platform's business, and is not compared.
        assert!(ascii_labels_survived("münchen.de", "anything.de"));
    }

    // ── The deny list, at both ends ─────────────────────────────────────

    /// A denied byte on the way IN is refused before the platform sees it,
    /// which for a URL parser is the difference between a host and a
    /// delimiter.
    #[test]
    fn a_denied_byte_never_reaches_the_platform() {
        for input in [
            "ex@ample.com",
            "ex/ample.com",
            "a%b.com",
            "a b.com",
            "a\u{0}b.de",
            "xn--%-0fa.de",
        ] {
            assert_eq!(
                to_ascii_over(
                    |asked| panic!("the platform was asked about {asked:?}"),
                    input
                ),
                None,
                "{input:?}"
            );
        }
    }

    /// And a denied byte the platform *produced* is refused on the way
    /// out. U+FF0F FULLWIDTH SOLIDUS maps to `/`, and is not an ASCII byte
    /// on the way in.
    #[test]
    fn a_denied_byte_the_platform_produced_is_refused() {
        assert_eq!(over_idna("a\u{ff0f}b.de"), None);
        let sloppy = |_: &str| Some("a/b.de".to_owned());
        assert_eq!(to_ascii_over(sloppy, "aüb.de"), None);
        let not_ascii = |_: &str| Some("aüb.de".to_owned());
        assert_eq!(to_ascii_over(not_ascii, "aüb.de"), None);
    }

    // ── The differential, over generated input ──────────────────────────

    /// Pseudo-random names from an alphabet of exactly the pieces that
    /// reach the interesting branches — ACE prefixes, punycode digits,
    /// hyphens, dots, the deviation characters, a combining mark, an RTL
    /// letter and a mapped one.
    ///
    /// `xorshift64` rather than a dev-dependency, and a fixed seed, so the
    /// same names are checked on every machine and a failure names an
    /// input that reproduces.
    fn generated(seed: u64, mut check: impl FnMut(&str)) {
        const ALPHABET: &[&str] = &[
            "xn--", "x", "n", "-", ".", "a", "b", "0", "9", "3ya", "mnchen", "zzzz", "ß", "ς", "ü",
            "例", "\u{200d}", "\u{301}", "ا", "\u{ff0f}", "\u{3002}", "A", "_",
        ];
        let mut state = seed;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..100_000 {
            let pieces = 1 + (next() % 6) as usize;
            let mut name = String::new();
            for _ in 0..pieces {
                let nth = usize::try_from(next() % ALPHABET.len() as u64).expect("fits");
                name.push_str(ALPHABET[nth]);
            }
            check(&name);
        }
    }

    /// The same claim as [`the_policy_is_transparent_over_a_real_uts46`],
    /// over input nobody chose. A hand-written punycode decoder in the
    /// path that decides which host is contacted is the riskiest code in
    /// this crate, and a corpus of names somebody thought of is the
    /// weakest thing that could stand behind it.
    #[test]
    fn the_policy_agrees_with_idna_on_generated_names() {
        let mut disagreed = Vec::new();
        generated(0x2545_f491_4f6c_dd1d, |name| {
            let (got, want) = (over_idna(name), idna_says(name));
            if got != want && disagreed.len() < 20 {
                disagreed.push(format!("  {name:?}: policy {got:?}, `idna` {want:?}"));
            }
        });
        assert!(
            disagreed.is_empty(),
            "the policy disagreed with `idna` on generated names:\n{}",
            disagreed.join("\n")
        );
    }

    /// The same generated names over the Foundation model: everything the
    /// model can answer at all, the policy must turn into `idna`'s answer.
    #[test]
    fn the_policy_agrees_with_idna_on_generated_names_over_foundation() {
        let mut disagreed = Vec::new();
        generated(0x9e37_79b9_7f4a_7c15, |name| {
            let (got, want) = (over_foundation(name), idna_says(name));
            if got != want && disagreed.len() < 20 {
                disagreed.push(format!("  {name:?}: policy {got:?}, `idna` {want:?}"));
            }
        });
        assert!(
            disagreed.is_empty(),
            "the policy over a Foundation-shaped backend disagreed with `idna`:\n{}",
            disagreed.join("\n")
        );
    }
}
