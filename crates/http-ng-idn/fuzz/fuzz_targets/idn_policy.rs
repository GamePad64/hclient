//! The crate's own safe layer, over arbitrary input.
//!
//! **Not a differential fuzzer, on purpose.** Comparing
//! `domain_to_ascii` against `idna` would be comparing `idna` with itself
//! on every target that has the bundled backend — which is every target a
//! fuzzing runner is likely to be — so it could not fail. That claim is
//! the corpus's, in `tests/differential.rs`, where it is 40 rows measured
//! against a real ICU 78.2 and where it needs a platform backend to mean
//! anything at all.
//!
//! What is fuzzed instead is the layer that is *ours* and that sits in
//! front of every backend: the WHATWG deny list, the `IGNORED_ERRORS`
//! mask, and the ASCII/A-label handling around the conversion. Four
//! properties, each of which would be a real defect in this client:
//!
//! 1. **No panic, on any input.** Hosts arrive from redirect `Location`
//!    headers, which are attacker-controlled; a panic here is a remote
//!    kill switch.
//! 2. **No forbidden byte ever reaches a caller.** This is the wrong-host
//!    property: `%`, `@`, `/`, `:` and the rest are exactly the bytes that
//!    make a URL parser see a different authority, and the crate's whole
//!    job is to refuse them rather than pass them on.
//! 3. **Idempotence** where the conversion succeeds. `http-ng-proto`
//!    depends on it: `uri::parse` runs on its own output at every redirect
//!    hop, and `sse::open` resolves a URL that `Client::execute` resolves
//!    again. A name that moved on the second pass would move the host
//!    under the client.
//! 4. **Output is always ASCII.** It is what makes property 3 possible and
//!    what every caller downstream assumes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // `from_utf8_lossy` rather than a `String` arbitrary impl: it keeps
    // every byte pattern reachable, including lone surrogates' replacement
    // and interior NULs, which are precisely the shapes a hand-written
    // corpus forgets.
    let domain = String::from_utf8_lossy(data);

    let Ok(first) = http_ng_idn::domain_to_ascii(&domain) else {
        // A refusal is always an acceptable answer. The properties below
        // are about what happens when the crate says yes.
        return;
    };

    // Property 4.
    assert!(
        first.is_ascii(),
        "domain_to_ascii returned non-ASCII for {domain:?}: {first:?}"
    );

    // Property 2 — the one that decides which host gets contacted.
    if let Some(bad) = first.bytes().find(|b| http_ng_idn::is_forbidden_domain_byte(*b)) {
        panic!(
            "domain_to_ascii returned a host containing the forbidden domain byte \
             {bad:#04x} for input {domain:?}: {first:?}"
        );
    }

    // Property 3. The output is ASCII, so this second pass is the one a
    // redirect hop would make.
    let second = http_ng_idn::domain_to_ascii(&first)
        .unwrap_or_else(|e| panic!("{first:?} came out of domain_to_ascii and back in as {e}"));
    assert_eq!(
        first, second,
        "domain_to_ascii is not idempotent for {domain:?}"
    );
});
