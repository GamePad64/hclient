//! The differential probe: the platform's ICU against the bundled `idna`
//! crate, on one corpus, with **both** answers pinned per row.
//!
//! This is the whole acceptance for this crate. Its claim is "the
//! platform agrees with us", and an untested claim of agreement between
//! two IDNA implementations is false in the tail — measured, not feared:
//! ICU with `UIDNA_DEFAULT` turns `straße.de` into `strasse.de`, a
//! different domain owned by a different person, and every part of that
//! call site looks right.
//!
//! Modelled on `hclient-proto/tests/uri_resolution.rs`, which is the same
//! shape with `url` as the oracle. Here the oracle is `idna`, called
//! exactly as `hclient-proto::uri::host_to_ascii` used to call it
//! directly and now reaches it through this crate.
//!
//! # Which rows actually ran, and where
//!
//! Two columns, and they do not run in the same places:
//!
//! - **`idna_says`** runs everywhere, on every target, because `idna` is
//!   a dev-dependency of this crate rather than a feature of it.
//! - **`icu_says`** runs only where a platform backend was compiled in
//!   AND accepted: `icuuc.dll` on Windows, Foundation on Apple, nowhere
//!   on Linux or wasm (see the crate docs — the ELF `dlopen` backend was
//!   removed on purpose).
//!
//! Its name is older than the second backend and is kept: the column is
//! one column, and it being one column is the point — the same rows are
//! the acceptance for both platforms, and the acceptance probe is what makes
//! that possible, because it takes over everything the two do
//! differently. The one family where they are expected to differ is not
//! in the table at all; see
//! [`where_this_crate_is_stricter_than_idna_it_refuses_rather_than_answering_differently`].
//!
//! So a green run of this file on a machine with no ICU proves nothing at
//! all about the platform column, and saying "the corpus passes" without
//! saying where is the exact defect this file exists to prevent.
//! [`the_platform_column_is_not_silently_empty`] is the guard: set
//! `HCLIENT_IDN_REQUIRE_PLATFORM=1` — CI does, on the one runner that has
//! a platform backend — and a missing library becomes a failure
//! rather than a quiet skip. Every platform-column test also prints the
//! library it measured, by name and version, so a report can say
//! `libicuuc.so.78.2` rather than "an ICU".
//!
//! # Reading a row
//!
//! `None` means "some `IdnError`". Which one is not pinned here — that is
//! the unit tests' job in `src/lib.rs` — because the interesting
//! distinction on this corpus is *accepted with this answer* versus
//! *rejected*, and a row that pinned the error variant would fail for
//! reasons that have nothing to do with IDNA.
//!
//! Rows where the two columns differ are the behaviour difference, listed
//! once in [`DIVERGENCES`] and asserted to be exactly that set, so a new
//! divergence cannot appear without a test failing and a fixed one cannot
//! stay listed.

// The platform column's seam. `#[cfg]`-ed because there is no platform
// backend on Linux, and **its loss is why this file stopped compiling for
// Windows**: a bulk `use`-tidying replaced this line with a
// `Cow` import, which left `testing::` unresolved at five sites and `Cow`
// imported twice. Neither is visible on Linux, where every use of both is
// inside a function this same `cfg` removes.
#[cfg(icu_backend)]
use hclient_idn::testing;
use rstest::rstest;
use std::borrow::Cow;

/// The oracle, called exactly as `hclient-proto::uri::host_to_ascii`
/// reaches it: that function's `idn` feature is now this crate, and on a
/// target with the bundled backend `domain_to_ascii` *is* this call.
///
/// Through the DEV-dependency, so this runs on the targets where `idna`
/// is deliberately absent from the crate's own graph, which is every
/// target that has a system ICU. That is the whole reason it is a
/// dev-dependency: the comparison has to be possible precisely where the
/// shipped build does not contain the thing being compared against.
fn idna_says(domain: &str) -> Option<String> {
    idna::domain_to_ascii_cow(domain.as_bytes(), idna::AsciiDenyList::URL)
        .ok()
        .map(Cow::into_owned)
}

include!("shared/corpus.rs");

/// Every input on which the platform deliberately answers something other
/// than the bundled implementation, in corpus order. The list is closed:
/// the test below derives the same set from [`CORPUS`] and compares.
#[rustfmt::skip]
const DIVERGENCES: &[&str] = &[];

fn label(case: &Case) -> String {
    format!("[{}] input={:?}", case.what, case.input)
}

/// The oracle's own answers are pinned too. Without this, an `idna`
/// upgrade that changed the incumbent's behaviour would silently redefine
/// what "the platform agrees" means, and the divergence list would be
/// measuring the wrong baseline.
#[test]
fn the_bundled_oracle_answers_what_the_corpus_pins_for_it() {
    let mut wrong = Vec::new();
    for case in CORPUS {
        let got = idna_says(case.input);
        if got.as_deref() != case.idna_says {
            wrong.push(format!(
                "  {}: `idna` was pinned at {:?}, now says {:?}",
                label(case),
                case.idna_says,
                got
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} corpus rows changed under the `idna` oracle:\n{}",
        wrong.len(),
        CORPUS.len(),
        wrong.join("\n")
    );
}

/// The claim. Every row's platform answer, measured against what was
/// pinned when the row was written.
#[cfg(icu_backend)]
#[test]
fn the_platform_answers_what_the_corpus_pins_on_every_row() {
    if !testing::has_platform() {
        println!(
            "no platform backend accepted on this machine — the platform column of all {} rows \
             was NOT measured here. See `the_platform_column_is_not_silently_empty`.",
            CORPUS.len()
        );
        return;
    }
    let mut wrong = Vec::new();
    for case in CORPUS {
        let got = testing::platform(case.input)
            .expect("the library was found a line ago; it cannot be gone now")
            .ok();
        // `testing::platform` is the platform backend and nothing else
        // now — the policy layer that used to sit in front of it, and
        // whose rule had to be applied here before the column could be
        // compared, is gone. So the column is the expectation directly.
        let want = case.icu_says;
        if got.as_deref() != want {
            wrong.push(format!(
                "  {}: expected {:?}, the platform said {:?}",
                label(case),
                want,
                got
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} corpus rows answered differently than pinned, against the platform:\n{}",
        wrong.len(),
        CORPUS.len(),
        wrong.join("\n")
    );
    println!("all {} rows measured against the platform", CORPUS.len());
}

/// The behaviour difference, bounded. Everything not on the list must
/// answer identically on both sides.
///
/// This one compares the two pinned columns, so it runs everywhere,
/// including on a machine with no ICU: it is a property of the table, and
/// the table is the thing being reviewed.
#[test]
fn the_divergences_from_idna_are_exactly_the_documented_ones() {
    let found: Vec<&str> = CORPUS
        .iter()
        .filter(|c| c.idna_says != c.icu_says)
        .map(|c| c.input)
        .collect();
    assert_eq!(
        found, DIVERGENCES,
        "the set of inputs where the platform disagrees with `idna` is not the documented one"
    );
    assert_eq!(
        CORPUS.len(),
        40,
        "a corpus row was added or removed without the divergence list being reconsidered"
    );
}

/// The guard against the one way this whole file can pass while proving
/// nothing: no ICU on the machine, so the platform column never ran.
///
/// CI sets `HCLIENT_IDN_REQUIRE_PLATFORM=1` on the runners that are meant
/// to have one. Locally it is unset and this test passes while saying so,
/// which is the honest answer on a machine that genuinely has no ICU.
#[cfg(icu_backend)]
#[test]
fn the_platform_column_is_not_silently_empty() {
    let required = std::env::var_os("HCLIENT_IDN_REQUIRE_PLATFORM").is_some();
    match testing::has_platform() {
        true => println!("platform column measured against this target's own UTS 46"),
        false => assert!(
            !required,
            "HCLIENT_IDN_REQUIRE_PLATFORM is set, so this machine is supposed to have a system \
             ICU — and no library was found. Every platform-column row above passed by not \
             running. Install one (Debian/Ubuntu: libicu-dev or libicu76; Windows 10 1703+ has \
             one), or unset the variable and accept that this run proves nothing about the \
             platform path."
        ),
    }
}

/// **The gate ran and passed**, which is what the removed name test was
/// really asking.
///
/// It used to compare the reported backend's name against the
/// compiled-in one. Two things emptied it: `cfg_select!` selects one
/// backend *module*, so "which backend this build has" and "which
/// answered" became the same fact, and `Handle::name` went with the four
/// strings nothing branched on. What is left is the question that still
/// has two answers — did the acceptance probe accept — and a `None` here
/// means it did not, which is the failure this corpus exists to notice.
#[cfg(icu_backend)]
#[test]
fn the_platform_backend_passed_its_acceptance_probe() {
    assert!(
        testing::has_platform(),
        "the platform backend was compiled in and refused the acceptance probe, so this build \
         answers nothing at all — both directions are gated, so the reverse getter is as \
         likely a cause as the forward one"
    );
}

/// **Where this crate is stricter than `idna`, it must REFUSE — never
/// answer a different host.**
///
/// The corpus above pins inputs whose answer is known on both sides. This
/// pins the family whose answer is not: a name with a non-ASCII label that
/// also trips one of the six `UIDNAInfo.errors` bits `IGNORED_ERRORS`
/// masks. `idna` accepts every one of them, and Apple's Foundation —
/// whose `shouldAllow(_:encodeToASCII: true)` in `URLParser+ICU.swift`
/// allows no error bit at all, `allowedErrors = 0` — is expected to refuse
/// them. Nothing in this crate can repair that: Foundation answers nil
/// rather than a reason, which is `foundation.rs`'s consequence 3, and the
/// six bits are exactly the ones this crate has to ignore to agree with
/// `idna`.
///
/// They are not corpus rows because a corpus row pins ONE answer for a
/// platform column that Windows and Apple share, and here the two
/// platforms are expected to differ. What is shared, and is asserted, is
/// the property that matters: the disagreement is a refusal. A third
/// answer would be a different host — the defect this whole crate exists
/// to prevent — where a refusal is only a name the caller must spell as an
/// A-label itself.
///
/// Every row prints what actually happened, so the Windows run
/// *report* which of the two it was instead of leaving it to be predicted
/// from Apple's source.
#[cfg(icu_backend)]
#[test]
fn where_this_crate_is_stricter_than_idna_it_refuses_rather_than_answering_differently() {
    const STRICTER: &[(&str, &str)] = &[
        ("-münchen.de", "UIDNA_ERROR_LEADING_HYPHEN, masked here"),
        ("münchen-.de", "UIDNA_ERROR_TRAILING_HYPHEN, masked here"),
        ("ab--cd.münchen", "UIDNA_ERROR_HYPHEN_3_4, masked here"),
        ("münchen..de", "UIDNA_ERROR_EMPTY_LABEL, masked here"),
        (".münchen.de", "UIDNA_ERROR_EMPTY_LABEL, leading"),
        ("münchen.de.", "the root label, beside a non-ASCII one"),
        (
            "müncheeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeen.de",
            "UIDNA_ERROR_LABEL_TOO_LONG once encoded, masked here",
        ),
        (
            "a\"ü.de",
            "the A-label keeps the quote, which RFC 3986's reg_name forbids",
        ),
    ];
    if !testing::has_platform() {
        println!("no platform backend accepted here — nothing to compare");
        return;
    }
    let mut wrong = Vec::new();
    for (input, why) in STRICTER {
        let oracle = idna_says(input);
        let ours = testing::platform(input)
            .expect("the backend was found a line ago")
            .ok()
            .map(Cow::into_owned);
        println!("  {input:?} ({why}): `idna` {oracle:?}, the platform {ours:?}");
        if ours.is_some() && ours != oracle {
            wrong.push(format!(
                "  {input:?}: `idna` says {oracle:?} and the platform says {ours:?} — a THIRD host, \
                 not a refusal"
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

/// **The crate's one public function, over the whole corpus.**
///
/// The two columns above are per-implementation; this is the answer a
/// *caller* gets, which is neither column by name but whichever one
/// [`backend`](hclient_idn::backend) selected — so it is chosen by the
/// same cfg the resolution uses rather than assumed to be the same value.
///
/// It needs its own test because nothing else here pins it on a target
/// with no platform backend, which is the one CI exercises most: the
/// platform-column tests are compiled away, the oracle test calls `idna`
/// directly, and the idempotence test below is satisfied by any function
/// that is constant. Measured rather than suspected — `cargo mutants`
/// replaced `domain_to_ascii`'s whole body with `Ok(Cow::Borrowed(""))`
/// and the file stayed green.
#[test]
fn the_public_entry_point_answers_the_corpus() {
    if !hclient_idn::testing::has_platform() {
        println!("no implementation in this build on this machine — nothing to answer with");
        return;
    }
    let mut wrong = Vec::new();
    for case in CORPUS {
        // **No layer's rule comes first any more.** The backend's answer
        // is the crate's answer, so what a row expects is what that
        // target's implementation says — `icu_says` where the platform
        // answers, `idna_says` where the bundled crate does. The rows
        // themselves never moved; it was this crate that used to differ
        // from them.
        let want = if cfg!(icu_backend) {
            case.icu_says
        } else {
            case.idna_says
        };
        let got = hclient_idn::domain_to_ascii(case.input)
            .ok()
            .map(Cow::into_owned);
        if got.as_deref() != want {
            wrong.push(format!(
                "  {}: expected {:?}, `domain_to_ascii` said {:?}",
                label(case),
                want,
                got
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

/// **The seam the fuzzer goes through is the layer itself, not a
/// lookalike.**
///
/// `testing::policy_over` exists so that `fuzz/fuzz_targets/
/// idn_policy_vs_idna.rs` can hand the layer a backend and compare the
/// result with `idna` — and `cargo nextest` does not run fuzz targets, so
/// without this test replacing that function's body with `None` would be
/// invisible to every test in the workspace, and the fuzzer would be
/// fuzzing nothing while still passing. (Three surviving mutants said so.)
///
/// What it asserts is the same claim `src/policy.rs` makes on its own
/// names — over `idna` as the backend the layer must be transparent —
/// made once more through the public seam rather than the private
/// function.
/// The one place the layer is **not** transparent, by rule.
///
/// **This crate answers what `idna` answers, with no layer in between**,
/// which is the whole of its contract: the same conversion, from
/// whatever UTS 46 the platform already carries, for a smaller binary.
///
/// The test that used to sit here asserted the opposite — that a layer of
/// this crate's own refused six of the corpus's inputs that `idna`
/// accepts, `a..b` and `ä..de` among them. That layer is gone. It was
/// URL validation, and answering *may this host be contacted* is not
/// this crate's question: neither `url::Url` nor `http::Uri` refuses
/// those names either, and both are conformant in accepting them.
#[test]
fn the_public_entry_point_answers_what_idna_answers() {
    let mut wrong = Vec::new();
    for case in CORPUS {
        let got = hclient_idn::domain_to_ascii(case.input)
            .map(std::borrow::Cow::into_owned)
            .ok();
        if got.as_deref() != case.idna_says {
            wrong.push(format!(
                "  {}: this crate said {:?}, `idna` said {:?}",
                label(case),
                got,
                case.idna_says
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} corpus rows differ from `idna`, and this crate exists to differ from it in \
         binary size and in nothing else:\n{}",
        wrong.len(),
        CORPUS.len(),
        wrong.join("\n")
    );
}

/// Idempotence, the property `hclient-proto` depends on: `uri::parse` is
/// applied again to its own output on every redirect hop, and `sse::open`
/// resolves a URL that `Client::execute` then resolves again. An A-label
/// that changed on a second pass would move the host under the client.
#[rstest]
fn converting_an_already_converted_name_changes_nothing(
    #[values("münchen.de", "straße.de", "例え.テスト", "EXAMPLE.COM", "مثال.إختبار")] input: &str,
) {
    if !hclient_idn::testing::has_platform() {
        println!("no implementation in this build on this machine — nothing to be idempotent");
        return;
    }
    let once = hclient_idn::domain_to_ascii(input).expect("corpus name must convert");
    let twice = hclient_idn::domain_to_ascii(&once).expect("an A-label must convert");
    assert_eq!(once, twice, "{input:?} is not a fixed point");
}

/// **Windows, on a thread where COM was never initialised.**
///
/// Two live-machine assumptions this crate makes about Windows, neither of
/// which anyone here could check — no Windows machine produced it — and
/// both of which the `test (windows-latest)` matrix job answers on every
/// push, because it runs `--workspace --all-features` on a real runner.
/// Written as a test rather than as a one-off CI probe deliberately: a
/// probe answers once and dies with the job, a test answers again when the
/// runner image changes.
///
/// 1. **`icuuc.dll` loads at all.** `windows-sys` binds it through
///    `windows-link`, which emits a `raw-dylib` *load-time* import: if the
///    DLL were absent this test binary would not start, and every test in
///    it would fail together. So the assertion for that is simply that
///    this test runs — worth naming anyway, because it is exactly the
///    failure mode a Windows older than 1703 produces, and someone reading
///    a wall of unrelated failures should be able to find this comment.
/// 2. **No `CoInitializeEx` is needed.** Microsoft documents COM
///    initialisation as a prerequisite for Win32 apps using the split
///    `icuuc.dll`/`icuin.dll`, waived on 1903+ with the combined
///    `icu.dll`. This crate never calls `CoInitializeEx` — grep it — and
///    the conversion below runs on a freshly spawned thread, which
///    therefore has no COM apartment, apartment state being per-thread. If
///    the assumption is wrong, `uidna_openUTS46` fails, the acceptance
///    probe rejects the library, `backend()` reports `None`, and this test
///    goes red on the runner. That is the only honest way to learn it.
#[cfg(all(windows, icu_backend))]
#[test]
fn windows_icu_answers_on_a_thread_with_no_com_apartment() {
    let answer = std::thread::spawn(|| {
        (
            hclient_idn::testing::has_platform(),
            hclient_idn::domain_to_ascii("straße.de").map(Cow::into_owned),
        )
    })
    .join()
    .expect("the conversion thread must not panic");

    assert!(
        answer.0,
        "on Windows the platform backend is a load-time import, so `None` here means \
         `uidna_openUTS46` failed on a thread with no COM apartment — i.e. `CoInitializeEx` IS \
         required after all, and `icu/windows.rs` has to call it or this crate has to stop \
         claiming otherwise"
    );
    assert_eq!(
        answer.1.as_deref(),
        Ok("xn--strae-oqa.de"),
        "the corpus row that decides which host is contacted, answered by icuuc.dll"
    );
}
