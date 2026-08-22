//! The crate's own layer against `idna`, over arbitrary input — and this
//! one **is** a differential fuzzer, where its sibling deliberately is
//! not.
//!
//! The sibling's reason for refusing to be one still stands:
//! `domain_to_ascii` on a target with the bundled backend *is*
//! `idna::domain_to_ascii_cow`, so comparing the two would be comparing
//! `idna` with itself, and a fuzzer that cannot fail is worse than none.
//!
//! What changed is that there is now a layer in front of every backend
//! with something of its own to get wrong, and
//! `hclient_idn::testing::policy_over` lets the backend be *supplied*.
//! Hand it `idna` and `idna` cancels out of both sides of the comparison:
//! whatever is left is this crate's code — the WHATWG deny list, ASCII
//! case folding, the label separators, the ASCII short-circuit, and a
//! hand-written RFC 3492 punycode decoder.
//!
//! **The decoder is why this target exists.** It is around sixty lines of
//! integer arithmetic in the path that decides which host a request goes
//! to, it was written here rather than taken from a crate (punycode needs
//! no Unicode tables, which is the whole saving), and a wrong answer from
//! it is not a crash — it is a different domain name, reached through a
//! green build. The unit tests in `src/policy.rs` check it against 43
//! named inputs and 2 x 100 000 generated ones from a fixed alphabet;
//! this checks it against input nobody chose, guided by coverage.
//!
//! The property is equality, in both directions, and there is deliberately
//! no tolerance in it:
//!
//! - the policy accepting where `idna` rejects would be a host that was
//!   never validated — `xn--zzzz.test` is exactly that shape;
//! - the policy rejecting where `idna` accepts would be a name the client
//!   refuses to reach, which is a bug even though it fails closed;
//! - a different answer is a different host, which is the defect the whole
//!   crate exists to prevent.

#![no_main]

use libfuzzer_sys::fuzz_target;

/// The oracle, and the backend, and the same call `hclient-proto` makes.
fn idna_says(domain: &str) -> Option<String> {
    idna::domain_to_ascii_cow(domain.as_bytes(), idna::AsciiDenyList::URL)
        .ok()
        .map(std::borrow::Cow::into_owned)
}

fuzz_target!(|data: &[u8]| {
    // `from_utf8_lossy` rather than a `String` arbitrary impl: it keeps
    // every byte pattern reachable, including lone surrogates' replacement
    // and interior NULs, which are precisely the shapes a hand-written
    // corpus forgets.
    let domain = String::from_utf8_lossy(data);

    let ours = hclient_idn::testing::policy_over(idna_says, &domain);
    let theirs = idna_says(&domain);

    if ours == theirs {
        return;
    }

    // **The layer is not transparent, and saying it was cost a real bug and
    // a false alarm.** It verifies: it decodes an ACE label itself, hands
    // the decoded form to the backend, and emits the backend's answer only
    // once that answer re-encodes to the label it was given. So it may
    // refuse where the backend does not — but only for a reason the backend
    // itself supplies, and never by inventing a *different* answer.
    //
    // That is the contract asserted here, and it is narrower than
    // `assert_eq!` on purpose while still failing on what `assert_eq!`
    // caught. The bug it caught, `xn--…'ｗ…-0dJd`, was the layer decoding
    // punycode *before* UTS 46 mapping; `idna` round-trips that input
    // perfectly, so the first arm below still fires on it. The false alarm,
    // `xn--xn--aaaaaaax*-nlw`, is `idna` accepting an ACE label whose own
    // `domain_to_unicode` it then refuses to re-encode — a backend that
    // will not confirm its own output, which this layer is right to
    // decline.
    assert!(
        ours.is_none(),
        "the layer answered {ours:?} where `idna` answered {theirs:?} for {domain:?}: \
         it may refuse, and it may never invent a different answer"
    );

    // The layer's one **policy** refusal, named rather than folded into
    // the condition below. An empty label that is not the single trailing
    // root is refused whatever a backend says, because `idna` and Windows's
    // ICU convert `ä..de` and Apple's Foundation refuses it — a name
    // reachable on two of this project's three platforms and not the third.
    // That refusal is deliberately *not* "for a reason the backend
    // supplies", which is what the assertion below is about, so it is
    // excluded here and asserted directly by `policy.rs`'s
    // `refuses_an_empty_label_but_not_the_trailing_root`.
    // All four separators, not just `'.'`: splitting on the ASCII dot
    // alone is answered by the fuzzer with `"．"` in seconds — a fullwidth
    // full stop, which the policy treats as a separator.
    let sep = hclient_idn::testing::LABEL_SEPARATORS;
    let empty_label = |name: &str| {
        let n = name.split(sep).count();
        name.split(sep)
            .enumerate()
            .any(|(i, l)| l.is_empty() && i + 1 < n)
    };
    // The input **and** what `idna` made of it: UTS 46 maps some code
    // points to nothing, so two non-empty labels can become none — and the
    // rule is applied to the answer as well, or this crate would not be
    // idempotent over its own output.
    if empty_label(&domain) || theirs.as_deref().is_some_and(empty_label) {
        return;
    }

    let (unicode, malformed) = idna::domain_to_unicode(&domain);
    assert!(
        malformed.is_err() || idna_says(&unicode).is_none(),
        "the layer refused {domain:?} where `idna` answered {theirs:?}, and `idna` \
         round-trips it — so the refusal is the layer's own and is a defect"
    );
});
