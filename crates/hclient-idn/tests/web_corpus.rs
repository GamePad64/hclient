//! The same corpus, in a browser, against the same oracle.
//!
//! **This is the check the Apple backend never had.** Foundation passed
//! the acceptance probe and failed eight of these rows, and nothing ran
//! them, because no runner in this project is a Mac. The browser backend
//! is reached the same way — build a URL, read the host back — so it is
//! held to the row-by-row comparison from the start rather than to the
//! two-name probe.
//!
//! Run by `just test-idn-browser`, and by the `browser` CI jobs on both
//! engines. `idna` is a dev-dependency here and a normal one nowhere on
//! this target, which is what makes the comparison possible at all.
#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

use std::borrow::Cow;
use wasm_bindgen_test::wasm_bindgen_test;
use wasm_bindgen_test::wasm_bindgen_test_configure;

wasm_bindgen_test_configure!(run_in_browser);

include!("shared/corpus.rs");

/// The oracle, called exactly as the bundled backend calls it.
fn idna_says(domain: &str) -> Option<String> {
    idna::domain_to_ascii_cow(domain.as_bytes(), idna::AsciiDenyList::URL)
        .ok()
        .map(Cow::into_owned)
}

/// Every input on which an engine answers something other than `idna`,
/// with the engine that does it named.
///
/// **It is a union over engines and no single run can exhaust it**, which
/// is the finding this file was written to carry. The backend was
/// measured in Firefox alone — 37 of 38 rows, one divergence — and that
/// number was read as *the browser*. There are two: Chrome answers six of
/// these rows differently, because `new URL()` does not validate an ACE
/// label there and does in Firefox. A measurement taken in one engine is
/// a fact about that engine.
///
/// So the closed-set assertion is gone and two weaker ones stand in its
/// place, and between them they are stronger than it was: nothing outside
/// this list may diverge, and — the half that keeps the list from being a
/// place to park a defect — every divergence a run actually finds must be
/// **repaired**, so that [`hclient_idn::domain_to_ascii`] answers what
/// `idna` answers on it. A row added here that the crate does not repair
/// fails the second assertion.
const DIVERGENCES: &[(&str, &str)] = &[
    // Both engines: `https:///` is not a URL, so an empty host is the
    // one input a URL parser cannot be asked about at all. `crate::ace`
    // answers it before the parser is reached, because an all-ASCII name
    // needs no parser.
    ("", "both engines"),
    // Chrome only, and all four are the same gap: `new URL()` does not
    // validate an ACE label there. Firefox refuses all four, which is
    // what the WHATWG standard requires and is why one engine's
    // agreement is not the browser's. The last of them is the sharpest —
    // its punycode is *valid* and decodes to ASCII alone, which is a
    // rule above the decoder rather than the decoder's own.
    // `crate::ace::decode_ace_labels` refuses all four, for both engines.
    ("xn--zzzz.test", "Chrome: no ACE-label validation"),
    ("xn--a.de", "Chrome: no ACE-label validation"),
    ("xn--.de", "Chrome: no ACE-label validation"),
    ("xn--a-.de", "Chrome: no ACE-label validation"),
    // Chrome only, and a different mechanism: the engine percent-encodes
    // rather than refusing, so the answer carries a `%` — which is in the
    // deny list `crate::ace::to_ascii_over` applies to the conversion's
    // output, and in the one `domain_to_ascii` applies over every backend.
    ("a b.com", "Chrome: percent-encoded, not refused"),
    ("a\u{a0}b.de", "Chrome: NBSP maps to a space, then encoded"),
];

/// The engine alone, reached through the crate rather than bound again
/// here: a second binding would compare a copy of the backend with
/// `idna` and call it a measurement of the browser.
fn browser_says(domain: &str) -> Option<String> {
    hclient_idn::testing::engine(domain)
}

/// What the engine itself answers, row by row, with the divergences named
/// — and every one of them repaired by the crate around it.
///
/// The second half is what makes the first honest. Listing a row is not
/// permission to be wrong about it: a divergence this run finds must come
/// back from [`hclient_idn::domain_to_ascii`] as `idna`'s own answer, so
/// the list can only ever record *where the engine falls short*, never
/// *where the crate does*.
#[wasm_bindgen_test]
fn the_engine_answers_what_idna_answers_except_where_it_is_listed() {
    let listed = |input: &str| DIVERGENCES.iter().any(|(row, _)| *row == input);
    let mut wrong = Vec::new();
    let mut unrepaired = Vec::new();
    for case in CORPUS {
        let ours = idna_says(case.input);
        let theirs = browser_says(case.input);
        if ours == theirs {
            continue;
        }
        if !listed(case.input) {
            wrong.push(format!(
                "  [{}] {:?}: idna {:?}, the engine {:?}",
                case.what, case.input, ours, theirs
            ));
            continue;
        }
        let repaired = hclient_idn::domain_to_ascii(case.input)
            .ok()
            .map(Cow::into_owned);
        if repaired != ours {
            unrepaired.push(format!(
                "  [{}] {:?}: idna {:?}, the engine {:?}, the crate {:?}",
                case.what, case.input, ours, theirs, repaired
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} rows diverge and are not on the list:\n{}",
        wrong.len(),
        CORPUS.len(),
        wrong.join("\n")
    );
    assert!(
        unrepaired.is_empty(),
        "{} listed divergence(s) are not repaired by this crate:\n{}",
        unrepaired.len(),
        unrepaired.join("\n")
    );
}

/// What a caller gets, which is the engine plus `src/web.rs`'s one line.
///
/// **No divergence is allowed here at all**, and that is the difference
/// between a backend and the API over it: the crate promises `idna`'s
/// answers, so a row the engine cannot express has to be answered before
/// the engine is asked or the promise is broken.
#[wasm_bindgen_test]
fn the_public_entry_point_answers_what_idna_answers_on_every_row() {
    let mut wrong = Vec::new();
    for case in CORPUS {
        let want = idna_says(case.input);
        let got = hclient_idn::domain_to_ascii(case.input)
            .ok()
            .map(Cow::into_owned);
        if got != want {
            wrong.push(format!(
                "  [{}] {:?}: expected {:?}, `domain_to_ascii` said {:?}",
                case.what, case.input, want, got
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} corpus rows came back differently from the public entry point:\n{}",
        wrong.len(),
        CORPUS.len(),
        wrong.join("\n")
    );
}

/// Every input where this crate is **stricter** than `idna` in a browser,
/// and the only property that makes that acceptable.
///
/// **The browser has this and had no check for it.** `web.rs` scans the
/// input for the WHATWG forbidden-domain set before handing it to
/// `new URL()`, exactly as `apple.rs` does and for the same reason: a
/// denied byte is one the parser consumes as a delimiter, so
/// `ex@ämple.com` comes back as `xn--mple-hva.com` — a different origin
/// returned as a clean success, which `ace::ascii_labels_survived` cannot
/// catch because the label that lost the `ex@` is the non-ASCII one it
/// does not compare.
///
/// The cost is that a forbidden byte UTS 46's mapping *removes* is
/// refused here where `idna` answers: `>` followed by U+0338 composes to
/// `≯`, so `idna` says `xn--hdh` and this crate says nothing. That is the
/// safe direction — a name the caller must spell as an A-label — and what
/// must never happen is the third answer, a host that is neither.
///
/// `tests/differential.rs` asserts the same property for Apple, which has
/// the same scan. It was `#[cfg(icu_backend)]` and so ran only on Windows,
/// which has no scan at all; the browser half did not exist.
#[wasm_bindgen_test]
fn where_this_crate_is_stricter_than_idna_it_refuses_rather_than_answering_differently() {
    const STRICTER: &[(&str, &str)] = &[
        (
            ">\u{338}",
            "a forbidden byte that mapping removes — the input scan cannot see that far",
        ),
        (
            "a\u{ff0f}b.de",
            "fullwidth solidus, which MAPS to `/` — refused on the answer rather than the input",
        ),
    ];
    let mut wrong = Vec::new();
    for (input, why) in STRICTER {
        let oracle = idna_says(input);
        let ours = hclient_idn::domain_to_ascii(input)
            .ok()
            .map(Cow::into_owned);
        if ours.is_some() && ours != oracle {
            wrong.push(format!(
                "  {input:?} ({why}): `idna` says {oracle:?} and this crate says {ours:?} — a \
                 THIRD host, not a refusal"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} inputs came back as a host that is neither `idna`'s answer nor an error:\n{}",
        wrong.len(),
        STRICTER.len(),
        wrong.join("\n")
    );
}
