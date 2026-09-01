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
pub(crate) const LABEL_SEPARATORS: [char; 4] = ['.', '\u{3002}', '\u{ff0e}', '\u{ff61}'];

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
/// 1. **The WHATWG deny list — after mapping, and at every point a denied
///    byte can still appear.** For Foundation this is what stops a URL
///    parser reading `ex@ample.com` as the host `ample.com`; for ICU it is
///    redundant and cheap.
///
///    **It ran on the input, before anything else, and that was the wrong
///    order.** UTS 46 §4 maps and then validates, so a forbidden character
///    can stop existing during mapping: `">\u{338}"` composes to `≯`, and
///    a check on the raw input refused a `>` that is not in the result.
///    That is the ordering defect step 3 already carries, met a second
///    time; the fuzzer found both.
///
///    Moving it wholesale to the end was also wrong, and a test said so:
///    punycode preserves the basic code points verbatim, so `xn--%-0fa.de`
///    decodes to `%ä` and a denied byte rides past a check that only looks
///    at the name. So it is **narrowed** rather than moved — on the ASCII
///    fast path in step 4, where mapping is a no-op and the two orders
///    coincide; on each decoded label in step 3, where punycode could have
///    carried one; and on the converted output, which is the string that
///    decides which host is contacted.
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
/// 6. **What this crate emits, this crate accepts.** Steps 1 to 5 confirm
///    an ACE label the caller *gave*; they say nothing about one this
///    crate *produced*, and a label carrying a character that only maps to
///    ASCII is pushed through untouched by design, so the platform can
///    answer with an `xn--` label nothing examined. The result was an
///    accept followed by a refuse — and the second parse is the one a
///    **redirect hop** makes. See [`to_ascii_inner`].
pub(crate) fn to_ascii_over(
    convert: &dyn Fn(&str) -> Option<String>,
    domain: &str,
) -> Option<String> {
    to_ascii_inner(&convert, domain, true)
}

/// Every ACE label in `lower` decoded to the label it stands for, with
/// everything else passed through — UTS 46 ToUnicode, over a name that
/// mapping has already made ASCII.
///
/// **Shared by both directions**, and that is what makes them agree by
/// construction rather than by two implementations being kept in step:
/// [`to_ascii_inner`] runs this to get the Unicode form it hands the
/// backend, and [`to_unicode_over`] runs it on the answer that came back.
/// A label this refuses is a label neither direction will emit.
/// UTS 46 **ToUnicode**, over the same backend and the same rules as
/// [`to_ascii_over`].
///
/// **It is the ASCII direction with one more step, deliberately.** The
/// name is converted to its A-label form first — which runs every check
/// this crate has, including the backend's own mapping and step 6's
/// round-trip — and only then are the ACE labels decoded. Two things fall
/// out of that order and neither is free any other way:
///
/// - **the two directions accept exactly the same names**, so a caller
///   cannot be handed a Unicode form for a host `domain_to_ascii` would
///   refuse to contact;
/// - **the answer is the platform's**, not this layer's. Punycode is
///   reversible arithmetic and this crate can do it anywhere, but *which*
///   name is legal is UTS 46's question and the backend's answer.
///
/// The cost is one extra backend call for a name that has an ACE label
/// and no other, which is a conversion nobody's DNS lookup will notice.
///
/// **No platform ToUnicode entry point is used, and that is the point.**
/// ICU has `uidna_nameToUnicodeUTF8` and ICU4J has `nameToUnicode`, but
/// Apple's Foundation exposes no way back at all — `NSURL` hands out the
/// A-label and nothing decodes it. A per-backend implementation would
/// therefore have had three implementations and one hole, where this has
/// one implementation and no hole.
pub(crate) fn to_unicode_over(
    convert: &dyn Fn(&str) -> Option<String>,
    domain: &str,
) -> Option<String> {
    let ascii = to_ascii_inner(&convert, domain, true)?;
    decode_ace_labels(&ascii)
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

/// [`to_ascii_over`], with step 6 switchable.
///
/// **Step 6 is "what this crate emits, this crate would accept".** Steps 1
/// to 5 confirm an ACE label the caller *gave* — decoded, handed to the
/// platform, and emitted only once the platform re-encodes it to the same
/// bytes. They said nothing about an ACE label this crate *produced*, and
/// the two are not the same input: a label carrying a character that only
/// **maps** to ASCII is pushed through untouched by design (§4 maps before
/// it looks for an ACE label — the ordering this module already fixed
/// once), so the platform can hand back an `xn--` label that steps 1 to 5
/// never examined.
///
/// The result was an accept followed by a refuse: the name resolved, and
/// re-parsing the answer did not. That is not a formality — **the second
/// parse is the one a redirect hop makes**, so a host reached once became
/// unreachable the next time it was seen. Found by
/// `fuzz_targets/idn_policy.rs`, whose whole subject is that property, on
/// `xn--xn--kd--kd-xn--kd--kdijaakkkx`.
///
/// The confirmation is the second parse itself, run once, which is why
/// `confirm` exists rather than a recursive call: the inner pass must not
/// confirm its own answer, and one level is all it takes — the answer is
/// ASCII, so the pass below either takes the fast path or decodes and
/// re-encodes, and there is nothing left to differ about.
///
/// Refusing rather than reconciling is the safe direction, and the same
/// one the empty-label rule takes: a name this crate cannot vouch for
/// twice is one it should not hand out once.
fn to_ascii_inner<F: Fn(&str) -> Option<String>>(
    convert: &F,
    domain: &str,
    confirm: bool,
) -> Option<String> {
    let lower = domain.to_ascii_lowercase();

    // **An empty label is refused, and the platforms disagreed about it.**
    // `ä..de` converts to `xn--4ca..de` under `idna` and under Windows's
    // ICU, and Apple's Foundation refuses it — so before this line the
    // same host was contactable on two of this project's three platforms
    // and not the third, which is the one thing this crate exists to
    // prevent. Refusing is the direction available (nothing here can make
    // Foundation accept) and the direction that is safe: an empty label is
    // not a legal DNS label, so no reachable host is lost, where the other
    // resolution would be a name that resolves differently per platform.
    //
    // A **single trailing** empty label is the root and stays legal:
    // `example.com.` is an ordinary fully-qualified name. `a..` is not,
    // and is refused with the rest.
    if has_empty_label(&lower) {
        return None;
    }

    let unicode = decode_ace_labels(&lower)?;

    // Reachable only when no label was an ACE label, because a decoded one
    // is non-ASCII by the check above.
    //
    // **The deny list is applied here rather than to the input, and moving
    // it is UTS 46's order.** §4 maps before it validates, so a forbidden
    // ASCII character can stop existing during mapping: `">\u{338}"` is
    // `>` followed by a combining long solidus overlay, which composes to
    // `≯` — and `idna` answers `xn--hdh` for it where a check on the raw
    // input refuses a `>` that is not in the result. That is the ordering
    // defect already recorded one field over, about ACE labels, met again
    // on the deny list; the fuzzer found this one too.
    //
    // Applying it here costs nothing, because UTS 46 does nothing to an
    // ASCII name under this crate's settings — `every_ascii_name_is_its_
    // own_answer_unless_it_has_an_ace_label` is that measurement — so for
    // an ASCII name "before mapping" and "after mapping" are the same
    // string. For anything else the check that decides which host is
    // contacted is the one on `ascii` below, after `convert`.
    if unicode.is_ascii() {
        if unicode.bytes().any(is_forbidden_domain_byte) {
            return None;
        }
        return Some(unicode);
    }

    let ascii = convert(&unicode)?;
    if !ascii.is_ascii() || ascii.bytes().any(is_forbidden_domain_byte) {
        return None;
    }
    if !ascii_labels_survived(&lower, &ascii) {
        return None;
    }
    // **The rule again, on the answer, and the input check does not imply
    // it.** UTS 46 maps some code points to nothing — a soft hyphen, a
    // zero-width space — so `"\u{ad}.\u{ad}"` arrives with two non-empty
    // labels and leaves as `"."`. Emitting that would break the property
    // `fuzz_targets/idn_policy.rs` calls idempotence, which is not a
    // formality: it is the second pass a **redirect hop** makes, so a name
    // this crate accepted once would be refused the next time it was seen.
    // Found by the fuzzer, in under a minute, on exactly that input.
    if has_empty_label(&ascii) {
        return None;
    }
    // Step 6 — see this function's doc comment.
    if confirm && ascii != lower && to_ascii_inner(convert, &ascii, false)? != ascii {
        return None;
    }
    Some(ascii)
}

/// A label that is empty and is not the single trailing root.
///
/// **Refused, because the platforms disagreed about it.** `idna` and
/// Windows's ICU convert `ä..de`; Apple's Foundation refuses it — a host
/// reachable on two of this project's three platforms and not the third,
/// which is the one thing this crate exists to prevent. Refusing is the
/// direction available, since nothing here can make Foundation accept, and
/// the safe one: an empty label is not a legal DNS label, so no reachable
/// host is lost.
///
/// A **single trailing** empty label is the root and stays legal:
/// `example.com.` is an ordinary fully-qualified name.
fn has_empty_label(name: &str) -> bool {
    let n = name.split(is_label_separator).count();
    name.split(is_label_separator)
        .enumerate()
        .any(|(i, l)| l.is_empty() && i + 1 < n)
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
    use std::borrow::Cow;

    /// The oracle, called exactly as `hclient-proto::uri::host_to_ascii`
    /// reaches it through this crate — and, in [`over_idna`], standing in
    /// for a platform that IS a full UTS 46 implementation, which is what
    /// Windows' ICU is.
    fn idna_says(domain: &str) -> Option<String> {
        idna::domain_to_ascii_cow(domain.as_bytes(), idna::AsciiDenyList::URL)
            .ok()
            .map(Cow::into_owned)
    }

    /// The policy over a backend that answers everything correctly. Any
    /// difference from [`idna_says`] is the policy's own.
    fn over_idna(domain: &str) -> Option<String> {
        to_ascii_over(&idna_says, domain)
    }

    /// **UTS 46 maps before it looks for an ACE label, and this layer used
    /// to do it the other way round.**
    ///
    /// Found by `fuzz/fuzz_targets/idn_policy_vs_idna.rs`. The label
    /// carries a fullwidth `ｗ`, which §4's mapping turns into an ASCII
    /// `w`; only then is `xn--` meaningful and the payload decodable.
    /// Decoding first meant handing punycode — which is defined over ASCII
    /// — a non-ASCII payload, where it could only fail, so the layer
    /// answered `None` for a name `idna` accepts.
    ///
    /// **It was invisible on Linux and would not have been on Windows.**
    /// There `domain_to_ascii` *is* `idna`, so the shipped path agreed with
    /// the oracle; the layer is what runs over ICU and Foundation. A host
    /// contacted on one platform and refused on another is the one thing
    /// this crate exists to prevent.
    ///
    /// The second row is the same name with the mapping done by hand, and
    /// it is what identified the ordering rather than the characters: both
    /// sides already agreed on it before the fix.
    #[test]
    fn an_ace_label_is_decoded_after_mapping_and_not_before() {
        let mapped = "xn--qqqqqqqqqqHJJJJJJ'wJJJJJJJJJJJi-0dJd";
        let raw = "xn--qqqqqqqqqqHJJJJJJ'\u{ff57}JJJJJJJJJJJi-0dJd";

        assert_eq!(
            over_idna(mapped),
            idna_says(mapped),
            "the hand-mapped form always agreed, which is why the characters were not the cause"
        );
        assert_eq!(
            over_idna(raw),
            idna_says(raw),
            "and the unmapped form must now agree too — decoded after mapping, not before"
        );
        assert_eq!(
            over_idna(raw),
            over_idna(mapped),
            "mapping is the backend's, so the two spellings must reach the same answer"
        );
        assert!(
            over_idna(raw).is_some(),
            "a check that both sides answer `None` would pass for the defect this pins"
        );
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
        to_ascii_over(&foundation_model, domain)
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

    /// Names this crate refuses **by its own rule**, whatever a backend
    /// says: a label that is empty and is not the single trailing root.
    ///
    /// The two transparency tests below exclude these rather than assert
    /// them, and the exclusion is the point: transparency is the property
    /// everywhere the policy has no opinion, and there is now exactly one
    /// place it has one. `refuses_an_empty_label` is what pins the opinion
    /// itself, so nothing here is merely being stepped around.
    fn refused_by_policy(name: &str) -> bool {
        let n = name.split(is_label_separator).count();
        name.split(is_label_separator)
            .enumerate()
            .any(|(i, l)| l.is_empty() && i + 1 < n)
    }

    /// The opinion the two tests below step around, asserted directly.
    ///
    /// **`ä..de` was contactable on Linux and Windows and refused on
    /// macOS** — `idna` and ICU convert it, Foundation does not — which is
    /// a name resolving differently per platform, the one thing this crate
    /// exists to prevent. Refusing is the direction available, since
    /// nothing here can make Foundation accept, and the safe one: an empty
    /// label is not a legal DNS label, so no reachable host is lost.
    ///
    /// The trailing root is the control: `example.com.` is an ordinary
    /// fully-qualified name and must survive, or this rule would be a
    /// refusal of correct input rather than of nonsense.
    #[test]
    fn refuses_an_empty_label_but_not_the_trailing_root() {
        let pass = |n: &str| over_idna(n).is_some();
        assert!(!pass("a..b"), "an empty label in the middle");
        assert!(!pass("ä..de"), "the name the platforms disagreed about");
        assert!(!pass("a.."), "two trailing dots are not the root label");
        assert!(!pass("."), "the bare root is not a host");
        assert!(pass("example.com."), "the trailing root must survive");
        assert!(pass("example.com"), "and so must the ordinary form");

        // **The label can also become empty during mapping**, which the
        // input check cannot see: UTS 46 maps a soft hyphen to nothing, so
        // this arrives with two non-empty labels and would leave as `"."`.
        // Emitting it would make the crate non-idempotent — and the second
        // pass is the one a redirect hop makes, so a host accepted once
        // would be refused the next time it was seen. The fuzzer found it.
        assert!(!pass("\u{ad}.\u{ad}"), "both labels map away to nothing");
        assert!(!pass("\u{ad}.a"), "the leading label maps away");
        assert!(
            pass("a.\u{ad}"),
            "a trailing label that maps away is the root"
        );
    }

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
        for name in NAMES.iter().filter(|n| !refused_by_policy(n)) {
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
        for name in NAMES.iter().filter(|n| !refused_by_policy(n)) {
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
                &|asked| {
                    panic!("the platform was asked about {asked:?} for the ASCII name {name:?}")
                },
                name,
            );
            // A name the policy refuses by rule never reaches a backend
            // either — which is this test's claim in its strongest form,
            // so it is checked here rather than filtered out.
            let want = if refused_by_policy(name) {
                None
            } else {
                idna_says(name)
            };
            assert_eq!(got, want, "{name:?}");
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
            to_ascii_over(&transitional, "xn--strae-oqa.de"),
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
        assert_eq!(to_ascii_over(&liar, "münchen.de"), None);
    }

    /// And a backend that changed how many labels there are. A code point
    /// that maps to `.` — U+3002 does — is how that happens by accident
    /// rather than by malice.
    #[test]
    fn a_backend_that_changes_the_label_count_is_refused() {
        let splitter = |_: &str| Some("xn--mnchen-3ya.de.extra".to_owned());
        assert_eq!(to_ascii_over(&splitter, "münchen.de"), None);
        let joiner = |_: &str| Some("xn--mnchen-3ya".to_owned());
        assert_eq!(to_ascii_over(&joiner, "münchen.de"), None);
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

    /// **What this crate emits, this crate accepts** — step 6, and the
    /// property `fuzz_targets/idn_policy.rs` exists for.
    ///
    /// It is not a formality: the second parse is the one a **redirect
    /// hop** makes, so a name that resolved once and is refused the next
    /// time is a host that becomes unreachable in the middle of a chain.
    ///
    /// The hole steps 1-5 left is that they confirm an ACE label the
    /// caller **gave**, and say nothing about one this crate **produced**.
    /// A label carrying a character that only maps to ASCII is pushed
    /// through untouched by design, so the platform can answer with an
    /// `xn--` label nothing examined — and re-parsing that label decodes
    /// it, asks for confirmation, and rightly does not get it.
    #[test]
    fn what_the_layer_emits_the_layer_accepts() {
        for name in [
            "münchen.de",
            "例え.テスト",
            "example.com",
            "example.com.",
            "EXAMPLE.COM",
            "xn--mnchen-3ya.de",
            ">\u{338}",
        ] {
            let once = over_idna(name).unwrap_or_else(|| panic!("{name:?} must resolve"));
            assert_eq!(
                over_idna(&once).as_deref(),
                Some(once.as_str()),
                "{name:?} resolved to {once:?}, which does not resolve to itself"
            );
        }
        // The case the fuzzer found, refused rather than reconciled: the
        // platform's own answer for it is an ACE label the platform will
        // not confirm on the way back, so there is nothing to emit that
        // would survive a second parse.
        assert_eq!(over_idna("xn--xn--kd--kd-xn--kd--kdijaakkkx"), None);
    }

    /// **The deny list is applied after mapping, and the pair is the
    /// decision.** Either half alone reads as the wrong fix.
    ///
    /// `">\u{338}"` is `>` followed by a combining long solidus overlay,
    /// which composes to `≯` — so the forbidden character is *not in the
    /// name* by the time UTS 46 validates, and `idna` answers `xn--hdh`.
    /// Refusing it was this layer applying §4's steps out of order, the
    /// same defect already recorded about ACE labels. The fuzzer found it.
    ///
    /// `xn--%-0fa.de` is the other direction and is why the check could
    /// not simply move to the end: punycode preserves the basic code
    /// points verbatim, so a literal `%` rides through and the label
    /// decodes to `%ä`. The check that caught this carried a comment
    /// saying it "cannot fire" — it could, and removing it on the strength
    /// of that comment is what this test would have caught.
    #[test]
    fn a_denied_byte_is_judged_after_mapping_and_after_decoding() {
        let pass = |n: &str| over_idna(n);
        assert_eq!(
            pass(">\u{338}").as_deref(),
            Some("xn--hdh"),
            "the `>` composes away, so there is no forbidden character to refuse"
        );
        assert_eq!(
            pass("xn--%-0fa.de"),
            None,
            "punycode carried a literal `%` into the decoded label"
        );
        // The control, and the property that must never move: a forbidden
        // character that survives mapping is still refused.
        for still_denied in ["a>b.de", "a<b.de", "a b.de", "a%b.de", "ex@ample.com"] {
            assert_eq!(pass(still_denied), None, "{still_denied:?}");
        }
    }

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
                    &|asked| panic!("the platform was asked about {asked:?}"),
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
        assert_eq!(to_ascii_over(&sloppy, "aüb.de"), None);
        let not_ascii = |_: &str| Some("aüb.de".to_owned());
        assert_eq!(to_ascii_over(&not_ascii, "aüb.de"), None);
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
            // The policy's one opinion is excluded here and asserted by
            // `refuses_an_empty_label_but_not_the_trailing_root`. A
            // generator that emits `.` and `。` freely produces a great
            // many of these, and none of them is about agreement with
            // `idna` on names both are willing to convert.
            if refused_by_policy(name) {
                return;
            }
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
            // The policy's one opinion is excluded here and asserted by
            // `refuses_an_empty_label_but_not_the_trailing_root`. A
            // generator that emits `.` and `。` freely produces a great
            // many of these, and none of them is about agreement with
            // `idna` on names both are willing to convert.
            if refused_by_policy(name) {
                return;
            }
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
