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

    assert_eq!(
        ours, theirs,
        "the policy layer and `idna` disagree about {domain:?}: over a backend that IS `idna`, \
         the layer must be transparent"
    );
});
