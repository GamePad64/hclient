//! Apple: UTS 46 through Foundation's URL parser.
//!
//! # The only platform backend with no spec amendment
//!
//! **Nothing here is `unsafe`, and that is not an accident of style — it
//! is measured.** Every `objc2-foundation` call this module makes is a
//! safe function; a draft that wrapped them in `unsafe` blocks produced
//! ten `unnecessary unsafe block` warnings from `cargo check --target
//! aarch64-apple-darwin`. So this file needs no `unsafe-code-exception`
//! marker, no entry in `unsafe-code-policy.sh`, and no amendment in the
//! spec, where `icu/windows.rs` needs all three (C9).
//!
//! That is the cheapest reason to keep it. `objc2` has done the work that
//! would otherwise be ours: message sends, retain/release, and the
//! nullability of every return are encoded in its types, so what is left
//! here is ordinary Rust over `Option`.
//!
//! # It is a URL parser that CALLS a UTS 46 implementation — measured
//!
//! This is the sentence the first live `macos-latest` run cost, and it is
//! not the same sentence as "Foundation does UTS 46". Foundation's IDNA
//! hook runs **only when the host is not ASCII**: `URLParser.swift` copies
//! a host that passes RFC 3986 `reg_name` validation straight into the URL
//! (`finalURLString += host`), and only a host that fails it reaches
//! `IDNAEncodeHost`. Three of the corpus's rows failed on that, all of them
//! all-ASCII — `EXAMPLE.COM` kept its case, `xn--zzzz.test` was never
//! decoded and so never found to be bad punycode, and `""` produced no
//! host at all (`URL_Swift.swift` returns nil for `https:///`, with a
//! comment saying apps rely on it).
//!
//! None of that is fixed here. `crate::policy` takes over everything
//! decidable from ASCII alone — the deny list, case folding, ACE decoding,
//! and an all-ASCII name it answers without asking any platform anything —
//! so this module is only ever handed a name containing non-ASCII, which
//! is exactly the input Foundation's hook does run on. The alternative was
//! a macOS-shaped repair beside the Windows one, i.e. two statements of
//! one contract.
//!
//! Where the hook does run, it is the real thing and it agrees with this
//! crate: `UIDNAHookICU` opens its handle with
//! `UIDNA_CHECK_BIDI | UIDNA_CHECK_CONTEXTJ | UIDNA_NONTRANSITIONAL_TO_UNICODE
//! | UIDNA_NONTRANSITIONAL_TO_ASCII`, bit for bit [`crate::OPTIONS`].
//!
//! # Why this is not just "call `URL(string:)`"
//!
//! Foundation converts to A-labels as a **side effect of parsing a whole
//! URL**, and that is a different shape from `uidna_nameToASCII_UTF8`.
//! Four consequences, each handled rather than hoped away:
//!
//! 1. **A host can change where the URL ends.** Hand Foundation
//!    `ex@ample.com` and the authority parses as userinfo `ex` plus host
//!    `ample.com`; `ex/ample.com` gives host `ex` and a path. Either is a
//!    wrong origin returned as a clean success. Every input in that family
//!    carries a byte from the WHATWG forbidden-domain set — `@ / ? # : [
//!    ] \ %`, space and the controls — so [`crate::is_forbidden_domain_byte`]
//!    over the INPUT rejects all of them before Foundation sees anything.
//!
//!    That scan now lives in `crate::policy`, which runs it for both
//!    backends, and the copy below is redundant *from that path*. It is
//!    kept because it is not redundant from the other one: the acceptance
//!    gate in `lib.rs` calls [`convert`] directly, not through the policy,
//!    and this is the only file in the crate that hands a string to a URL
//!    parser. The guard against a wrong origin belongs beside the parser,
//!    not two files away.
//!
//! 2. **The scheme is ours, not the caller's.** The string handed to
//!    Foundation is always `https://{domain}/`, built here. A caller
//!    cannot smuggle `file:` or a relative reference through, because
//!    nothing of theirs reaches the parser except the authority, and that
//!    has already passed the deny list.
//!
//! 3. **A failure carries no reason — and that is the one divergence
//!    from `idna` this crate cannot close.** ICU fills a
//!    `UIDNAInfo.errors` word; Foundation returns nil for the whole URL.
//!    So this backend can only ever produce
//!    [`crate::IdnError::NotAnIdn`], never a more specific one.
//!
//!    It costs more than a vague error message. `URLParser+ICU.swift`'s
//!    `shouldAllow(_:encodeToASCII: true)` sets `allowedErrors = 0`, so
//!    Foundation refuses a name on **any** error bit — including the six
//!    that [`crate::IGNORED_ERRORS`] must mask for this crate to agree with
//!    `idna` at all (_CheckHyphens=false_ and _VerifyDnsLength=false_).
//!    Apple's own masking list exists only for the `nameToUnicode`
//!    direction, which host encoding does not use. So a name with a
//!    non-ASCII label that ALSO has a leading hyphen, an empty label,
//!    hyphens in the third and fourth positions, or a label over 63 bytes
//!    once encoded — `-münchen.de`, `münchen..de`, `ab--cd.münchen` —
//!    comes back nil here and is accepted by `idna` and by Windows' ICU.
//!    The same happens when the A-label keeps an ASCII character RFC 3986
//!    forbids in a `reg_name` (`"`, `` ` ``, `{`, `}`), because Foundation
//!    re-validates its own output.
//!
//!    **Nothing in `policy.rs` can repair this**, and the reason is this
//!    consequence: with no error word there is no way to tell "ICU set a
//!    bit we ignore" from "this name is genuinely not an IDN". Converting
//!    label by label would fix two of the four bits and break CheckBidi,
//!    which is a property of the whole name. So the divergence stands,
//!    and it is bounded rather than merely admitted:
//!    `tests/differential.rs`'s
//!    `where_this_crate_is_stricter_than_idna_it_refuses_rather_than_answering_differently`
//!    asserts, on the runner, that every one of these is a **refusal** —
//!    a name the caller must spell as an A-label — and never a third host.
//!
//! 4. **Which getter returns the A-label was settled by a test, and the
//!    answer is [`NSURL::host`].** `NSURL::host` and
//!    `NSURLComponents::host` / `percentEncodedHost` do not agree — Apple's
//!    documentation for Swift's `URL.host(percentEncoded:)` describes the
//!    `true` case as returning the *less* decoded form, which for an IDN is
//!    the opposite of what the name suggests. Nobody here has a macOS
//!    machine, so this module picked `NSURL::host` on that reading and
//!    `tests/differential.rs::macos_getter_that_returns_the_a_label`
//!    asserted it on `macos-latest`. It passed on the first live run, along
//!    with the acceptance gate — `backend()` reported `SystemFoundation`,
//!    which it only does after `straße.de` and `faß.de` both answer
//!    correctly. The test stays, because a runner image changes and a
//!    one-off probe would not notice.
//!
//! # Never executed here
//!
//! Every line type-checks under `cargo check --target
//! aarch64-apple-darwin`, confirmed with a planted `compile_error!`
//! rather than trusted to a warm cache. Nothing here ran on the machine
//! that wrote it; the workspace `test` job's `macos-latest` leg runs it on
//! every push, and everything above marked *measured* comes from that.

use objc2_foundation::{NSString, NSURL};

/// Nothing to carry: Foundation is part of the OS and is linked, so there
/// is no handle and no library to keep alive. The type exists so the
/// backend has the same shape as the ICU one.
#[derive(Debug)]
pub(crate) struct Foundation;

/// The name every backend module exports, so that `lib.rs` can select one
/// with `cfg_select!` and then name no platform at all.
pub(crate) type Handle = Foundation;

/// Always `Some`: Foundation cannot be absent on an Apple target. The
/// acceptance gate in `lib.rs` is what decides whether it is *usable*.
pub(crate) fn find() -> Option<Foundation> {
    Some(Foundation)
}

/// The A-label form of `domain`.
///
/// **Three steps, and only the middle one is Foundation's.** `NSURL` is a
/// URL parser rather than a UTS 46 implementation, so it does two things
/// ICU does and one it does not: it maps and converts a Unicode host, it
/// does *not* case-fold ASCII, and it does *not* validate an ACE label —
/// `xn--zzzz.test` comes back unchanged where `idna` refuses it. Measured
/// on CI, eight rows of the differential corpus, the day the layer that
/// had been covering for it was deleted.
///
/// So the case folding and the ACE check are done here, out of
/// [`crate::ace`], and the conversion is asked of Foundation. The two
/// ICU backends need neither — they are UTS 46 implementations and
/// answer for themselves — which is why this lives in the backend that
/// falls short rather than in a layer over all of them.
pub(crate) fn to_ascii(_f: &Foundation, domain: &str) -> Option<String> {
    // Consequence 1: before the parser, never after. A denied byte here
    // is one Foundation would consume as a delimiter, silently changing
    // which host comes back. `crate::domain_to_ascii` scans the *answer*
    // for every backend alike; this scans the input, for this parser.
    if domain.bytes().any(crate::is_forbidden_domain_byte) {
        return None;
    }

    // Everything but the conversion is `crate::ace`'s, and the
    // conversion is Foundation's. The sequence lives there so that it can
    // be run on a host with no Foundation — with `idna` as the stand-in —
    // which is the check that would have caught the eight corpus rows
    // this repairs.
    crate::ace::to_ascii_over(
        |unicode| {
            // Consequence 2: the scheme is ours. The trailing slash makes
            // the authority unambiguous even for an empty path.
            let text = NSString::from_str(&format!("https://{unicode}/"));
            let url = NSURL::URLWithString(&text)?;
            Some(url.host()?.to_string())
        },
        domain,
    )
}
