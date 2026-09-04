//! Punycode, and the two ACE-label rules a backend may have to apply
//! itself.
//!
//! **This is UTS 46 conformance, not URL policy**, which is the line the
//! layer this comes from failed to draw. That layer answered questions
//! about whether a host was *usable* — empty labels, DNS length, what a
//! `Host` header may carry — and those are the caller's and left with it.
//! What is here is narrower and is this crate's own contract: *the same
//! answers `idna` gives*.
//!
//! **It exists because one backend is a URL parser.** ICU and ICU4J are
//! UTS 46 implementations and validate an ACE label themselves;
//! Foundation is reached through `NSURL`, which parses a URL and hands
//! back its authority — so `xn--zzzz.test` comes back unchanged where
//! `idna` refuses it, and `EXAMPLE.COM` comes back unfolded. Measured on
//! CI, eight rows of the differential corpus, after the layer was
//! deleted. See [`crate::apple`], the only caller.
//!
//! **Unconditional on purpose, like `icu` and `android` beside it.** Every
//! line here is integer arithmetic over `[a-z0-9-]` and string splitting —
//! no platform, no Unicode table — so compiling it everywhere is what puts
//! its tests on a host that can run them. The alternative is a hundred
//! lines of RFC 3492 inside `#[cfg(apple_backend)]`, where no runner in
//! this project would ever execute them.
//!
//! What is deliberately NOT here: the empty-label rule, `VerifyDnsLength`,
//! and the deny list. The first two are the URL validation that left with
//! the policy layer and must not come back — `ä..de` converts under `idna`
//! and so must convert here. The third is applied once, on the converted
//! name, in [`crate::domain_to_ascii`].

// **Compiled everywhere, read by one backend**, which is `icu/mod.rs`'s
// shape and the same trade. The point of compiling it on every target is
// that its tests run on a host that has one — `to_ascii_over` with `idna`
// as the conversion is the Apple path minus Apple — and the cost is that
// a genuinely unused item here is caught on macOS alone. That cost is
// paid knowingly: this module went in because eight rows were only
// measurable on a runner nobody here has, and a test that runs is worth
// more than a lint that fires.
#![cfg_attr(
    not(apple_backend),
    allow(
        dead_code,
        reason = "the sequence is compiled everywhere so its tests run everywhere; only the \
                  backend that is a URL parser rather than a UTS 46 implementation reads it"
    )
)]
use crate::is_forbidden_domain_byte;

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
pub(crate) const LABEL_SEPARATORS: [char; 4] = ['.', '\u{3002}', '\u{ff0e}', '\u{ff61}'];

fn is_label_separator(c: char) -> bool {
    LABEL_SEPARATORS.contains(&c)
}

/// The ACE prefix. A label that starts with it is punycode, whatever else
/// it looks like.
///
/// Matched AFTER ASCII case folding, so `XN--MNCHEN-3YA.DE` is an ACE
/// label too — which it has to be, because `idna` answers
/// `xn--mnchen-3ya.de` for it.
const ACE_PREFIX: &str = "xn--";

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
/// This is not a general decoder. The only caller is [`decode_punycode`],
/// reached from [`decode_ace_labels`], which has already ASCII case-folded, so an upper-case digit reaching
/// here would mean the case folding did not happen — and accepting it
/// would hide that. Accepting both cases leaves three mutants alive here,
/// which is the same statement made by measurement: nothing reaches that
/// arm.
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
/// platform's question, and [`decode_ace_labels`] asks it by handing the
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

fn decode_ace_labels(lower: &str) -> Option<String> {
    let mut unicode = String::with_capacity(lower.len());
    for (nth, label) in lower.split(is_label_separator).enumerate() {
        if nth > 0 {
            unicode.push('.');
        }
        // **`is_ascii` first, and the order is UTS 46's rather than a
        // guard.** §4 maps before it looks for an ACE label, so `xn--` is
        // only meaningful on a label that mapping has already made ASCII.
        // A label carrying a character that *maps* to ASCII — a fullwidth
        // `ｗ`, say — is a valid ACE label after mapping and nonsense
        // before it, and punycode is defined over ASCII, so decoding it
        // here could only fail. This crate has no mapper of its own; the
        // backend is the mapper, so such a label is pushed through
        // untouched and the backend does the whole of §4 on it.
        //
        // Found by the fuzzer, on
        // `xn--qqqqqqqqqqHJJJJJJ'ｗJJJJJJJJJJJi-0dJd`: this layer answered
        // `None` where `idna` answered the ACE form. Substituting the
        // mapped `w` by hand makes both answer the same string, which is
        // what identified the ordering rather than the characters.
        match label.strip_prefix(ACE_PREFIX).filter(|_| label.is_ascii()) {
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
                // **A denied byte can be smuggled through punycode**, and
                // the check that used to catch this sat on the whole
                // assembled name with a comment saying it "cannot fire".
                // It could: `xn--%-0fa.de` decodes to `%ä`, because a
                // literal `%` rides in the basic-code-point half that
                // punycode preserves verbatim. So the check belongs here,
                // on the decoded label, and not on the name — on the name
                // it also refused a forbidden character that **mapping
                // removes**, which is the `">\u{338}"` case the fuzzer
                // found. Narrowing it is what lets both be right.
                if decoded.bytes().any(is_forbidden_domain_byte) {
                    return None;
                }
                unicode.push_str(&decoded);
            }
            None => unicode.push_str(label),
        }
    }
    Some(unicode)
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

/// The ASCII direction for a backend that converts but does not validate.
///
/// **`convert` is the platform's UTS 46 and everything else here is the
/// part it does not do.** `NSURL` maps and converts a Unicode host and
/// then stops: it does not case-fold ASCII and it does not validate an
/// ACE label, because it is a URL parser. ICU and ICU4J do both and call
/// none of this.
///
/// The order is UTS 46's rather than convenient. Case folding first,
/// because §4 maps before it looks at anything; then the ACE labels,
/// because `xn--` is only meaningful on a label mapping has already made
/// ASCII; then the platform, on the Unicode form; then the checks that
/// say the platform answered about the name it was given.
///
/// **An all-ASCII name never reaches `convert`**, and that is not an
/// optimisation: for such a name UTS 46 under this crate's options does
/// nothing but the fold above, so there is no conversion to ask for — and
/// asking would hand a URL parser names that are legal hosts and not
/// legal URLs. `""` and `a"b.com` are the corpus rows that measures.
///
/// Tested on any host by passing `idna` as `convert`, which is what makes
/// this module unconditional: the sequence is the part of the Apple
/// backend that has nothing to do with Apple.
pub(crate) fn to_ascii_over<F: Fn(&str) -> Option<String>>(
    convert: F,
    domain: &str,
) -> Option<String> {
    let lower = domain.to_ascii_lowercase();
    let unicode = decode_ace_labels(&lower)?;
    if unicode.is_ascii() {
        return Some(unicode);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    /// The oracle, called exactly as [`crate::bundled`] calls it.
    fn idna_says(domain: &str) -> Option<String> {
        idna::domain_to_ascii_cow(domain.as_bytes(), idna::AsciiDenyList::URL)
            .ok()
            .map(Cow::into_owned)
    }

    /// [`to_ascii_over`] with `idna` standing in for Foundation — the
    /// real sequence, on a host that can run it. Any difference from
    /// [`idna_says`] is this module's own.
    fn over_idna(domain: &str) -> Option<String> {
        to_ascii_over(idna_says, domain)
    }

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
    /// so the answer is pure ASCII — and it is [`decode_ace_labels`], not
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

    /// **The eight rows the Apple backend answered differently**, run
    /// through the same sequence with `idna` as the conversion.
    ///
    /// They are the measurement that sent this module back into the
    /// crate: after the policy layer was deleted, `NSURL` answered these
    /// eight as itself rather than as UTS 46 — the case folding it does
    /// not do, and the ACE labels it does not validate. Each is pinned at
    /// `idna`'s answer, which is the crate's whole contract.
    ///
    /// Nothing here needs Foundation. What it cannot prove is that the
    /// *real* backend now agrees — only the macOS job can — but it does
    /// prove that everything around the conversion is right, which is
    /// where all eight went wrong.
    #[test]
    fn the_rows_foundation_answered_as_itself_now_answer_as_uts46() {
        for input in [
            "EXAMPLE.COM",
            "XN--MNCHEN-3YA.DE",
            "",
            "a\"b.com",
            "xn--zzzz.test",
            "xn--a.de",
            "xn--.de",
            "xn--a-.de",
        ] {
            assert_eq!(
                over_idna(input),
                idna_says(input),
                "{input:?} is one of the rows the Apple backend got wrong"
            );
        }
    }
}
