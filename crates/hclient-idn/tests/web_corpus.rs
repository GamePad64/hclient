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

/// Every input on which the browser deliberately answers something other
/// than `idna`, in corpus order. The list is closed: the test below
/// derives the same set and compares, so a new divergence cannot appear
/// without a failure and a fixed one cannot stay listed.
///
/// **One row, and it is the shape of the seam rather than a defect.**
/// `https:///` is not a URL any engine will parse, so the empty name
/// cannot be asked of a URL parser; `src/web.rs` answers it in one line
/// before the parser is reached, which is why the entry-point test below
/// finds no divergence at all while this one does. Kept separate on
/// purpose: what this pins is the *browser's* answer, and what that pins
/// is the crate's.
const DIVERGENCES: &[&str] = &[""];

/// The engine alone, reached through the crate rather than bound again
/// here: a second binding would compare a copy of the backend with
/// `idna` and call it a measurement of the browser.
fn browser_says(domain: &str) -> Option<String> {
    hclient_idn::testing::engine(domain)
}

/// What the engine itself answers, row by row, with the divergences named.
#[wasm_bindgen_test]
fn the_engine_answers_what_idna_answers_except_where_it_is_listed() {
    let mut found = Vec::new();
    let mut wrong = Vec::new();
    for case in CORPUS {
        let ours = idna_says(case.input);
        let theirs = browser_says(case.input);
        if ours != theirs {
            found.push(case.input);
            if !DIVERGENCES.contains(&case.input) {
                wrong.push(format!(
                    "  [{}] {:?}: idna {:?}, the engine {:?}",
                    case.what, case.input, ours, theirs
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} rows diverge and are not on the list:\n{}",
        wrong.len(),
        CORPUS.len(),
        wrong.join("\n")
    );
    assert_eq!(
        found, DIVERGENCES,
        "the set of inputs where the engine disagrees with `idna` is not the documented one"
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
