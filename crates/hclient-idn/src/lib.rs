//! IDN conversion — a Unicode domain in, its A-label form out — using the
//! platform's own UTS 46 implementation where the platform has one, and
//! the bundled `idna` crate where it does not.
//!
//! **This crate stands alone.** It depends on nothing from `hclient` — no
//! runtime, no transport, no HTTP types — and it is versioned and
//! released independently of that family, on its own compiler floor. The
//! prefix is where it lives, not what it needs: use it in anything that
//! has to turn a Unicode domain into its A-label form.
//!
//! ```
//! # fn main() -> Result<(), hclient_idn::IdnError> {
//! assert_eq!(hclient_idn::domain_to_ascii("münchen.de")?, "xn--mnchen-3ya.de");
//! # Ok(())
//! # }
//! ```
//!
//! **One function, and that is the whole surface**: [`domain_to_ascii`],
//! plus the [`IdnError`] its `Result` needs.
//!
//! **The reverse direction was here and is not**, and the reason is the
//! rule this crate is built on rather than a gap. Nothing needs it: an
//! HTTP client converts U to A because `http::Uri` refuses a non-ASCII
//! authority — measured, `"https://münchen.de/".parse::<http::Uri>()` is
//! `Err(invalid uri character)` — and never converts back, because
//! nothing downstream takes a U-label. A direction with no caller is a
//! surface that has to be right on four backends for nobody, and one of
//! them could not supply it at all: no JS API performs ToUnicode. So the
//! crate is one direction, every backend is one function, and the
//! acceptance probe asks one question. Which implementation answers is decided by the
//! target and, where the target has a choice, by the one feature this
//! crate has — `idna`, off by default, which forces the bundled tables
//! everywhere.
//!
//! Four backends, one shape: Windows' `icuuc.dll`, Android's
//! `android.icu.text.IDNA` over JNI, the browser's own `new URL()`, and
//! the `idna` crate, which is what the ELF unixes, WASI and Apple get
//! because there is no UTS 46 to ask there. `lib.rs` names the selected one `platform` and
//! nothing past that line names an operating system.
//!
//! # Why this crate exists, and what it is worth
//!
//! One number, measured on the target rather than on disk. The `idna`
//! crate compiles the Unicode tables into your binary; this crate takes
//! UTS 46 from what the platform already carries instead. The same
//! `cdylib` converting one name, `opt-level = "z"`, fat LTO,
//! `panic = "abort"`, stripped:
//!
//! | target | this crate | with `--features idna` | saved |
//! |---|---|---|---|
//! | `aarch64-linux-android` | 304.5 KiB | 443.5 KiB | **139.0 KiB — 31%** |
//! | `x86_64-linux-android` | 334.9 KiB | 478.3 KiB | **143.3 KiB — 30%** |
//!
//! **A third of a small native library, per ABI.** That is the share
//! rather than the absolute, and the share is the honest figure: 139 KiB
//! against a 5 MiB desktop binary is under 3% and no reason to take a
//! dependency, while against 443 KiB of `.so` shipped per ABI it is the
//! largest single item in it. `aarch64` is the ABI almost every device
//! takes.
//!
//! **On Linux and wasm it saves nothing, and that is stated first rather
//! than buried.** There is no system UTS 46 to reach for on those, so the
//! backend *is* the `idna` crate and the only cost is this crate itself —
//! measured at +736 bytes and, in `hclient-proto`'s own graph, one crate.
//! An ELF backend existed and did save it, reaching `libicuuc.so.NN`
//! through `dlopen`; it was removed deliberately, because on Linux the
//! ICU version is a property of the user's machine that nobody chose and
//! nothing reports, and for IDN a Unicode version difference is a
//! different host.
//!
//! **Windows and Apple are unmeasured**, and are recorded as such rather
//! than estimated: no machine that produced this crate has an MSVC linker
//! or an Apple one, and `x86_64-pc-windows-gnu` cannot link `windows-sys`
//! without `x86_64-w64-mingw32-dlltool`. The Android figure is the one
//! that has been taken on the real target.
//!
//! The often-repeated "roughly 1.9 MB" for `idna`'s tables is **vendored
//! source on disk, not bytes in a binary**: ICU4X stores them as
//! compressed tries and the linker keeps only what is referenced. On
//! x86-64 Linux they are 128,784 B of `.rodata` in a 448,184 B binary.
//!
//! One alternative was measured and is worse. Pinning `idna_adapter` to
//! 1.1.0 — the unicode-rs backend — is one `cargo update`, needs no code
//! and collapses the graph to 11 crates, and this workspace recommended
//! it for two verticals on that basis. In a stripped binary it is
//! **126 KiB larger** than the ICU one, and it runs a Unicode version
//! behind, which for IDN is a different host rather than a cosmetic
//! difference. A count of crates is not a count of bytes.
//!
//! # The contract: this is `idna::domain_to_ascii_cow(_, AsciiDenyList::URL)`
//!
//! Not "UTS 46" in the abstract — that phrase is not precise enough to
//! implement against, which is the whole trap this crate is built around
//! (below). The exact behaviour reproduced is the one `hclient-proto`
//! already calls, i.e. the WHATWG URL Standard's *domain to ASCII* with
//! the forbidden-domain-code-point check:
//!
//! | UTS 46 flag | value | where it comes from |
//! |---|---|---|
//! | `Transitional_Processing` | false | `idna` cannot do transitional at all |
//! | `CheckHyphens` | false | `Hyphens::Allow` |
//! | `VerifyDnsLength` | false | `DnsLength::Ignore` |
//! | `CheckBidi` | true | always on in `idna`, not configurable |
//! | `CheckJoiners` (ContextJ) | true | always on in `idna`, not configurable |
//! | `UseSTD3ASCIIRules` | false | plus the WHATWG deny list, which is not the same set |
//! | `IgnoreInvalidPunycode` | false | always off in `idna`, not configurable |
//!
//! Every one of those seven has to be reproduced on the platform side, and
//! only two of them are ICU *options*. The rest are `IGNORED_ERRORS` and
//! `is_forbidden_domain_byte` below — safe Rust with tests around it,
//! deliberately kept out of the files that carry the `unsafe`.
//!
//! # The trap, measured rather than reasoned about
//!
//! Two APIs look right and are not:
//!
//! - **`IdnToAscii` (Windows' `Normaliz.dll`) is IDNA2003**, which
//!   Microsoft documents and a probe on a `windows-latest` runner
//!   confirmed. `straße.de` becomes `strasse.de`, not `xn--strae-oqa.de`
//!   — a different domain, registrable by a different person. For an HTTP
//!   client that is a security difference, not a cosmetic one. This crate
//!   does not use it and neither should anything else here.
//! - **`UIDNA_DEFAULT` is 0, and 0 is *transitional*.** ICU's UTS 46 with
//!   default options agrees with IDNA2003 on exactly the inputs where it
//!   matters. Measured here against system ICU 78.2 on Linux:
//!
//!   | input | `uidna_openUTS46(0)` | `uidna_openUTS46(``OPTIONS``)` | `idna` |
//!   |---|---|---|---|
//!   | `straße.de` | `strasse.de` | `xn--strae-oqa.de` | `xn--strae-oqa.de` |
//!   | `faß.de` | `fass.de` | `xn--fa-hia.de` | `xn--fa-hia.de` |
//!
//!   The function is named for the standard it is being asked *not* to
//!   follow, so nothing about the call site invites suspicion of the flag.
//!   That is why `OPTIONS` is a named constant with the bits spelled out
//!   one per line, and why the corpus in `tests/differential.rs` opens
//!   with those two rows.
//!
//! # Which platform answers
//!
//! Four backends and one module alias. `lib.rs` selects one with
//! `cfg_select!` and names no operating system past that line; each
//! exports `find`, `to_ascii` and a `Handle`.
//!
//! | target | backend | `unsafe` |
//! |---|---|---|
//! | Windows | `icuuc.dll`, linked through `windows-sys` | amendment C9 |
//! | Android | `android.icu.text.IDNA` (ICU4J) over JNI | amendment C19 |
//! | the browser | `new URL()`, through `web-sys` | none |
//! | Apple, Linux, other ELF unixes, WASI | the `idna` crate | none |
//!
//! **Windows.** `windows-sys`' `Win32_Globalization` already declares
//! `uidna_openUTS46`, `uidna_nameToASCII_UTF8`, `uidna_nameToUnicodeUTF8`,
//! `uidna_close`, `UIDNAInfo` and every `UIDNA_*` constant, generated from
//! Microsoft's Win32 metadata, so nothing is transcribed by hand and
//! `src/icu/windows.rs` has no `extern` block at all. This works because
//! Windows' ICU is built with `U_DISABLE_RENAMING` and its exports are
//! unsuffixed. The cost is not hedged: `windows-link` emits a `raw-dylib`
//! **load-time** import, so a Windows without `icuuc.dll` — 10 before
//! 1703, and Server 2016 — does not fall back, the process fails to
//! start. The floor is **Windows 10 1703 / Server 2019**, stated rather
//! than degraded to.
//!
//! **Apple had a backend and does not now, and the measurement is the
//! reason.** Foundation is reached through `NSURL`, which converts an IDN
//! host as a side effect of *parsing a URL* — so it is a URL parser and
//! not a UTS 46 implementation. It does not case-fold ASCII and it does
//! not validate an ACE label: eight rows of the differential corpus came
//! back as themselves, `EXAMPLE.COM` and `xn--zzzz.test` among them.
//! Closing that gap needed a punycode decoder and a conversion sequence
//! written here, which is this crate reimplementing the thing it exists
//! to avoid reimplementing. Apple takes `idna` instead, at a measured
//! cost of 13 crates becoming 50 on `aarch64-apple-darwin` — that is what
//! the tables weigh, and it buys an answer identical to every other
//! target's with nothing of ours in between.
//!
//! One corroboration from that work is worth keeping: `swift-foundation`'s
//! `URLParser+ICU.swift` opens its ICU handle with `UIDNA_CHECK_BIDI |
//! UIDNA_CHECK_CONTEXTJ | UIDNA_NONTRANSITIONAL_TO_UNICODE |
//! UIDNA_NONTRANSITIONAL_TO_ASCII` — bit for bit the option word below,
//! arrived at independently, which is the best evidence that constant
//! will ever get.
//!
//! **The browser, reached the same way and short in the same place.**
//! `NSURL` converts as an undocumented side effect of parsing; the WHATWG
//! URL Standard *defines* host parsing as UTS 46 with named parameters,
//! so an engine gets the half that needs the tables right on purpose.
//! What it leaves out is the ASCII half, and **how much of it is the
//! engine's business rather than the browser's**: measured in headless
//! Firefox, 37 of 38 corpus rows answer what `idna` answers and the one
//! that does not is the empty name; measured in Chrome, six diverge,
//! four because `new URL()` does not validate an ACE label there. So this
//! backend applies `ace.rs` in full, exactly as `apple.rs` does, and the
//! sentence that had it costing one line against Apple's sixty-five went
//! with the number behind it. `tests/web_corpus.rs` asks both engines on
//! every push, which is what turned a Firefox measurement generalised to
//! *the browser* into a red job rather than a wrong host.
//!
//! It is also the target where the tables cost most, because a wasm
//! module has almost nothing else in it: **20.7 KiB against 143.0 KiB**
//! through the full `wasm-pack` pipeline, a saving of 86%. Measure after
//! `wasm-bindgen` and not before — the raw `.wasm` carries a custom
//! section of descriptors that the shim generator consumes and nothing
//! ships, and it made the browser build look 158 KiB *larger* than the
//! one with ICU in it.
//!
//! **One direction, and it is declared rather than discovered.**
//! `URL.hostname` hands back the A-label whatever went in, and no JS API
//! performs ToUnicode. That was this backend's one narrowness while the
//! crate had a reverse direction, and the narrowness outlived it: there
//! is one direction now, so every backend supplies exactly what every
//! other does.
//!
//! **Android.** ICU4J, the same ICU the Windows backend calls, under the
//! same option bits and the same error names — but the NDK exposes no C
//! entry point, so the way in is JNI. It has been executed on a device,
//! API 35, and the first run refused every name: the walk shared by both
//! directions had kept the ASCII direction's closing check. Thirteen
//! cases agree with `idna` there now, including every error name the
//! backend forgives.
//!
//! **Everything else** takes the bundled crate, which is also what
//! `--features idna` forces everywhere.
//!
//! # Features
//!
//! One, and it is off by default.
//!
//! | feature | behaviour |
//! |---|---|
//! | *(none)* | the platform's own UTS 46 where the target has one, the `idna` crate where it does not |
//! | `idna` | **forces** the bundled crate and its Unicode tables on every target |
//!
//! It *forces* rather than selects, which is why there is one switch and
//! not four: on Linux and wasm the answer does not change with it, so a
//! selector would have a setting that buys nothing, and `build.rs` turns
//! (feature, target) into exactly one backend cfg. The four it replaced —
//! `platform`, `bundled`, `system-icu`, `foundation` — had combinations
//! that selected two backends at once or none, and the crate carried a
//! `compile_error!` for the empty case.
//!
//! **Comparing the two happens in the tests, not at run time.** `idna` is
//! a dev-dependency, so `tests/differential.rs` calls it directly as the
//! oracle on exactly the targets where it is *not* a normal dependency —
//! which is the only place the comparison is worth making.
//!
//! # The risk that is left, named rather than buried
//!
//! **A system ICU tracks the operating system's Unicode version; the
//! bundled tables track this crate's.** Where they differ, some names
//! convert differently — and IDN decides *which host is contacted*, so
//! that is a different destination, not a cosmetic difference. It is the
//! same defect class as `IdnToAscii`'s IDNA2003, except that it arrives
//! by upgrading the OS rather than by choosing the wrong API, which makes
//! it harder to see, not easier.
//!
//! **There is no run-time cross-check, and it is worth saying why not**,
//! because comparing the two on every call was the first answer and it is
//! the obvious one. It cannot be built: the two backends are never both
//! compiled in (above), so on a target with a system ICU there is nothing
//! to compare against without putting the tables back — which is the
//! entire cost the crate exists to avoid. Paying it to detect a
//! disagreement would mean never getting the saving that makes the
//! disagreement worth detecting.
//!
//! What guards the gap instead, in order of strength:
//!
//! - **The corpus, per platform, in CI.** `tests/differential.rs` runs
//!   the platform backend against `idna` — a dev-dependency, so it is
//!   there on exactly the targets where the tables are not — and pins
//!   both answers on all 40 rows. A divergence is a red build on the
//!   platform that has it, which is where the answer differs.
//! - **The load-time acceptance probe** in `icu.rs`: a library that does
//!   not answer the transitional pair correctly is not used at all, and
//!   the crate reports `Backend::None` rather than a wrong host. That
//!   catches a badly configured or badly resolved ICU. It is a behaviour
//!   floor, **not a Unicode-version floor**, and this crate does not
//!   claim one.
//!
//! A real version floor is **unverified**: establishing which inputs
//! discriminate ICU 74 from ICU 78 needs several ICU majors to test
//! against, and only 78.2 was available here. What would settle it: run
//! `tests/differential.rs` against a matrix of container images pinned to
//! different `libicu` versions, and promote whatever rows move into the
//! acceptance probe — at which point the probe becomes a version floor.

// `deny`, not `forbid`, and only since spec amendment C9: `forbid` cannot
// be relaxed by a scoped `#[allow]` from inside the crate (`E0453`), and
// `icu.rs` needs exactly one such allowance to declare and call the three
// `uidna_*` entry points. Everything else in this crate — including every
// decision about what ICU's answer MEANS — is ordinary safe Rust, and CI's
// `unsafe_code stays forbidden by declaration` job path-scopes the C9
// marker to `src/icu.rs` alone, so an `unsafe` block added to this file
// fails the build exactly as it would in any other crate.
#![deny(unsafe_code)]

use std::borrow::Cow;

// Four backends, one shape: each module exports `find`, `convert` and a
// `Handle` whose `name()` says what answered. Which one is compiled is
// `build.rs`'s single cfg, and the alias below is the only place in this
// crate that names a platform — everything past it says `platform::`.
//
// **`cfg_select!` rather than four `#[cfg]` aliases**, and the arms are
// module selections, which is where this workspace measured the macro to
// pay: a `use` line loses nothing to rustfmt, where a function body inside
// an arm loses all of it. There is no `_` arm because `build.rs` emits
// exactly one of these four and a fifth backend must be a compile error
// here rather than a silent fall-through.

// Unconditional for the reason `icu` and `android` are: every line of it
// is integer arithmetic and string splitting, so compiling it everywhere
// is what puts its tests on a host that can run them. Two backends read
// it — `apple` and `web`, both in full — and both are reached through a
// URL parser rather than a UTS 46 entry point, which is the whole of what
// they have in common and the whole of why this module exists.
mod ace;

// Unconditional, like `android`: it holds ICU's vocabulary — the option
// bits, the error mask, the acceptance rule — which both ICU backends
// share, and only its Windows binding is gated inside.
mod icu;

// Unconditional: only its JNI half is gated, and the half that holds the
// decision — which ICU errors are forgiven — is tested on every host.
mod android;

#[cfg(idna_backend)]
mod bundled;

// Foundation's `NSURL`. Gated, unlike `icu` and `android`, because every
// line of it is an `objc2-foundation` call: there is no
// platform-independent half to put on a host that can run it, and what
// checks it is the `test (macos-latest)` leg.
#[cfg(apple_backend)]
mod apple;

// The browser's `new URL()`, gated for the same reason: every line is a
// `wasm-bindgen` import, and the corpus that checks it runs in the two
// `browser` jobs — both of them, because the engines do not agree about
// how much of UTS 46 a URL parser owes.
#[cfg(web_backend)]
mod web;

core::cfg_select! {
    idna_backend => { use bundled as platform; }
    icu_backend => { use icu as platform; }
    android_backend => { use android as platform; }
    apple_backend => { use apple as platform; }
    web_backend => { use web as platform; }
}

/// The one error type, in the file this workspace keeps error types in.
///
/// **This comment is the second half of a deletion.** The doc that stood
/// here described the policy layer — *"reachable through
/// `testing::policy_over`"* — and that layer was removed a week before,
/// taking the function it named with it. Nothing broke, because a `///`
/// on a `mod` declaration is prose and the compiler reads none of it, and
/// `just docs` saw no unresolved link because the path was in backticks
/// rather than brackets. It is this file's own recurring finding, wearing
/// the smallest possible clothes: a claim outlives the thing it describes
/// unless something fails when it stops being true.
mod error;

pub use error::IdnError;

/// The WHATWG URL Standard's [forbidden domain code point] set, as `idna`
/// spells it: `AsciiDenyList::new(true, "%#/:<>?@[\\]^|")` — the explicit
/// list plus, from `deny_glyphless`, U+0020 SPACE and below and U+007F
/// DELETE.
///
/// ICU has no option for this. `UIDNA_USE_STD3_RULES` is a *different*
/// set (see its constant), so the check is done here.
///
/// **Bytes, not chars, and that is correct**: every denied code point is
/// ASCII, and no byte of a multi-byte UTF-8 sequence is ever an ASCII
/// byte, so scanning bytes cannot produce a false positive on a non-ASCII
/// scalar.
///
/// [forbidden domain code point]: https://url.spec.whatwg.org/#forbidden-domain-code-point
#[must_use]
#[cfg_attr(
    not(android_backend),
    allow(
        dead_code,
        reason = "only the backends that take no deny list of their own apply this — `idna` \
                  and ICU have one — but the constant and its test are platform-independent"
    )
)]
pub(crate) const fn is_forbidden_domain_byte(b: u8) -> bool {
    matches!(
        b,
        0x00..=0x20
            | 0x7f
            | b'%'
            | b'#'
            | b'/'
            | b':'
            | b'<'
            | b'>'
            | b'?'
            | b'@'
            | b'['
            | b'\\'
            | b']'
            | b'^'
            | b'|'
    )
}

/// The two inputs a platform backend has to get right, and what "right"
/// is.
///
/// They are the pair the entire crate turns on: a transitional
/// implementation answers `strasse.de` and `fass.de` here, which are
/// different origins, registrable by different people.
///
/// Shared by every platform backend rather than duplicated per platform.
/// The two backends have nothing else in common — one links `icuuc.dll`,
/// the other makes JNI calls into ICU4J — and this is exactly the kind of
/// constant that would drift if each kept its own.
pub(crate) const PROBES: [(&str, &str); 2] = [
    ("straße.de", "xn--strae-oqa.de"),
    ("faß.de", "xn--fa-hia.de"),
];

/// The gate's *policy*, over any conversion function.
///
/// Split out from each backend's gate for one reason: a function that
/// only takes a real handle can only be tested by owning a broken
/// platform, and nobody here does. Over a closure it is four exhaustively
/// testable cases — the same move `hclient-dns-system` makes with
/// `sys::classify_written`.
///
/// **Every probe must pass, not any**: an implementation that gets
/// `straße.de` right and `faß.de` wrong is not one to trust with the
/// rest.
#[cfg_attr(
    not(any(icu_backend, android_backend)),
    allow(
        dead_code,
        reason = "only a platform backend has anything to gate, but the POLICY is                   platform-independent and so are its tests — gating it away here would stop                   them running on the one target CI exercises most"
    )
)]
pub(crate) fn accepts(convert: impl Fn(&str) -> Option<String>) -> bool {
    PROBES
        .iter()
        .all(|(input, want)| convert(input).as_deref() == Some(*want))
}

/// The one gate every backend passes, and the handle it hands back.
///
/// **One `OnceLock` and one probe for all four**, where the ICU module
/// kept its own and the two platform backends had theirs written out in
/// `lib.rs`. Three copies of a gate is three chances for one of them to
/// stop asking; the modules are interchangeable now, so the gate can be
/// too.
///
/// `None` means this build has an implementation it will not trust — a
/// Windows with no `icuuc.dll`, an Android process with no JVM, an ICU
/// that answered the transitional pair wrongly. Every caller turns that
/// into [`IdnError::NoImplementation`], which is a refusal rather than a
/// wrong host.
fn selected() -> Option<&'static platform::Handle> {
    use std::sync::OnceLock;
    static SELECTED: OnceLock<Option<platform::Handle>> = OnceLock::new();
    SELECTED
        .get_or_init(|| {
            platform::find().filter(|h| {
                // **Both directions, and the reverse half is what makes an
                // unverified platform getter safe.** A backend that converts
                // out correctly and hands back something else on the way in
                // is refused here rather than at the call that would have
                // used its answer — which is the case the Android
                // backend was written under, since its device run came
                // months after its code.
                accepts(|input| platform::to_ascii(h, input))
            })
        })
        .as_ref()
}

/// Converts `domain` to its ASCII (A-label) form.
///
/// ASCII in, ASCII out — but not unchanged: `EXAMPLE.COM` comes back
/// lower-cased, exactly as `idna::domain_to_ascii_cow` returns it. (The
/// caller that cares, `hclient-proto::uri`, never sends an all-ASCII host
/// here at all; that is its own documented behaviour, not this
/// function's.)
///
/// # Errors
///
/// [`IdnError::NotAnIdn`] if UTS 46 rejects the name, and
/// [`IdnError::NoImplementation`] if this build has an implementation it
/// will not trust — see `selected`.
pub fn domain_to_ascii(domain: &str) -> Result<Cow<'_, str>, IdnError> {
    let ascii = over(domain, platform::to_ascii)?;
    // **The deny list is applied here and in no backend**, because the
    // platform ones take none: `uidna_*` and ICU4J convert without one, so
    // a name `idna` refuses would have been answered on Windows and
    // refused on Linux — the same host reachable on one of this project's
    // platforms and not another, which is the one thing this crate exists
    // to prevent. It went missing when the policy layer was
    // deleted: that layer was URL validation and had to go, and this one
    // line of it was not — it is part of *being* `idna`, which takes
    // `AsciiDenyList::URL`.
    //
    // **On the converted name, not on the input**, which is the ordering
    // the fuzzer already established once: `">\u{338}"` composes to `≯`,
    // so a check before mapping refuses a character UTS 46 removes, where
    // `idna` answers `xn--hdh`. Punycode copies basic code points
    // verbatim, so a forbidden byte that survives mapping survives into
    // the A-label — and one that mapping removed is not in it. An ACE
    // label the caller supplied is covered by the same line, because
    // `xn--%-0fa.de` is ASCII and passes through unchanged.
    if ascii.bytes().any(is_forbidden_domain_byte) {
        return Err(IdnError::NotAnIdn {
            domain: domain.to_owned(),
        });
    }
    Ok(ascii)
}

/// One of the two entry points in [`policy`], as a value the dispatch can
/// take. One direction, so one value — kept as a type because the
/// dispatch is written over it and a second direction would be a second
/// value rather than a second function.
///
/// A named type because clippy asks for one, and it earns the name — the
/// pair of directions is the thing being abstracted over, and `PolicyFn`
/// says that where the written-out signature says only that something
/// takes a closure.
type Direction = fn(&platform::Handle, &str) -> Option<String>;

/// The dispatch both directions share.
///
/// `direction` is the selected backend's own conversion — `to_ascii` or
/// alone — and there is nothing between it and the caller. That is
/// the crate's whole contract: **the same answer `idna` gives, from
/// whatever UTS 46 the platform already carries**, and the acceptance
/// probe in [`selected`] is what enforces it.
fn over(domain: &str, direction: Direction) -> Result<Cow<'_, str>, IdnError> {
    let handle = selected().ok_or_else(|| IdnError::NoImplementation {
        domain: domain.to_owned(),
    })?;
    direction(handle, domain)
        .map(Cow::Owned)
        .ok_or_else(|| IdnError::NotAnIdn {
            domain: domain.to_owned(),
        })
}

/// What the differential corpus in `tests/differential.rs` needs and
/// [`domain_to_ascii`] deliberately will not give it: the platform backend
/// by name, and this crate's own policy layer
/// over a conversion the caller supplies.
///
/// **There is no `bundled` here, and there cannot usefully be one.** It
/// existed until it was checked: `build.rs` sets `idna_backend` only where
/// the target has no ICU to ask, so on every target where such
/// a function would compile it *is* the backend [`domain_to_ascii`] already
/// calls — a differential probe comparing `idna` with itself. Measured
/// rather than reasoned: `cargo check -p hclient-idn --all-features`, i.e.
/// every backend feature requested at once, emits exactly one
/// `rustc-cfg` per target — `idna_backend` on `x86_64-unknown-linux-gnu`
/// and on `aarch64-apple-darwin`, `icu_backend` on
/// `x86_64-pc-windows-msvc`, `android_backend` on
/// `aarch64-linux-android`, and never two. Making `idna` available beside
/// a platform backend would buy the comparison back at the price of the
/// ICU tables on the two platforms this crate exists to keep them off.
///
/// So the comparison lives where the two implementations are both
/// reachable: `tests/differential.rs` calls `idna` as a dev-dependency
/// and the platform through `testing::platform`, on the targets that have
/// one.
///
/// Not a public API: no stability promise, and nothing in the client calls
/// it.
#[doc(hidden)]
pub mod testing {
    use super::IdnError;
    use std::borrow::Cow;

    /// Whether a backend was selected at all — the difference between
    /// "the corpus checked the platform column" and "the corpus silently
    /// checked nothing".
    ///
    /// **It used to hand back the implementation's name and no longer
    /// does.** `Handle::name` was four strings nothing branched on: the
    /// crate reports which backend answered nowhere else, and a test that
    /// wanted the name was really asking whether the gate had passed. One
    /// `bool` says that; four `impl` blocks said it four times.
    #[must_use]
    pub fn has_platform() -> bool {
        super::selected().is_some()
    }

    /// The platform's answer for `domain`, for the differential corpus.
    ///
    /// The same call [`super::domain_to_ascii`] makes — there is nothing
    /// between them any more — kept as a name of its own so the corpus
    /// can say *the platform column* and mean it.
    ///
    /// # Errors
    /// As [`super::domain_to_ascii`].
    pub fn platform(domain: &str) -> Option<Result<Cow<'_, str>, IdnError>> {
        has_platform().then(|| super::domain_to_ascii(domain))
    }

    /// The browser engine's own answer, with nothing of this crate's
    /// around it.
    ///
    /// `tests/web_corpus.rs` makes two claims — what the engine answers,
    /// and what a caller gets — and the gap between them is [`super::ace`].
    /// Measuring it is the point rather than a diagnostic: the gap is one
    /// row in Firefox and six in Chrome, so a backend written against
    /// either number alone is wrong in the other engine. A second `URL`
    /// binding in the test would have measured a copy of the backend
    /// rather than the backend.
    #[cfg(web_backend)]
    #[must_use]
    pub fn engine(domain: &str) -> Option<String> {
        super::web::engine(domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deny_list_is_the_whatwg_set_and_not_std3() {
        for b in b"%#/:<>?@[\\]^|" {
            assert!(
                is_forbidden_domain_byte(*b),
                "{:?} must be denied",
                *b as char
            );
        }
        for b in 0x00..=0x20u8 {
            assert!(
                is_forbidden_domain_byte(b),
                "0x{b:02x} (glyphless) must be denied"
            );
        }
        assert!(is_forbidden_domain_byte(0x7f), "DELETE must be denied");
        // Not STD3: these are allowed by the WHATWG list, and `idna`
        // accepts them, so denying them here would be stricter than the
        // implementation this crate has to agree with.
        for b in b"_~!$&'()*+,;=\"`{}" {
            assert!(
                !is_forbidden_domain_byte(*b),
                "{:?} is allowed by AsciiDenyList::URL",
                *b as char
            );
        }
        for b in b"abzABZ019-." {
            assert!(!is_forbidden_domain_byte(*b));
        }
        // Nothing above ASCII: a UTF-8 continuation byte must never be
        // mistaken for a denied character.
        for b in 0x80..=0xffu8 {
            assert!(!is_forbidden_domain_byte(b), "0x{b:02x} is not ASCII");
        }
    }

    /// A conversion that answers correctly is accepted.
    #[test]
    fn a_correct_icu_is_accepted() {
        assert!(accepts(|input| PROBES
            .iter()
            .find(|(i, _)| *i == input)
            .map(|(_, want)| (*want).to_owned())));
    }

    /// **The case the gate exists for.** A transitional ICU converts both
    /// probes successfully, to the wrong origin — no error, no null, just
    /// a different host. It must be refused.
    #[test]
    fn a_transitional_icu_is_refused() {
        assert!(!accepts(|input| match input {
            "straße.de" => Some("strasse.de".to_owned()),
            "faß.de" => Some("fass.de".to_owned()),
            _ => None,
        }));
    }

    /// Right on one probe, wrong on the other. `all`, not `any` — if this
    /// passed, one correct answer would vouch for an ICU wrong everywhere
    /// else.
    #[test]
    fn getting_only_one_probe_right_is_not_enough() {
        assert!(!accepts(|input| match input {
            "straße.de" => Some("xn--strae-oqa.de".to_owned()),
            _ => Some("fass.de".to_owned()),
        }));
    }

    /// A conversion that fails outright — `uidna_openUTS46` refusing for
    /// want of `CoInitializeEx` looks exactly like this — is refused too,
    /// rather than read as "nothing to object to".
    #[test]
    fn an_icu_that_cannot_convert_at_all_is_refused() {
        assert!(!accepts(|_| None));
    }

    /// The probes are the transitional pair and nothing else has crept
    /// in. If one were softened to a name both flavours agree on, the
    /// gate would still be consulted, would still pass, and would no
    /// longer check the one thing it is for.
    #[test]
    fn the_probes_are_the_inputs_the_two_flavours_disagree_on() {
        assert_eq!(PROBES.len(), 2);
        assert!(PROBES.iter().all(|(_, want)| want.starts_with("xn--")));
        assert!(PROBES.iter().any(|(i, _)| *i == "straße.de"));
        assert!(PROBES.iter().any(|(i, _)| *i == "faß.de"));
    }

    #[cfg(idna_backend)]
    #[test]
    fn the_bundled_path_is_the_call_hclient_proto_makes_today() {
        assert_eq!(domain_to_ascii("münchen.de").unwrap(), "xn--mnchen-3ya.de");
        assert_eq!(domain_to_ascii("straße.de").unwrap(), "xn--strae-oqa.de");
        assert_eq!(domain_to_ascii("EXAMPLE.COM").unwrap(), "example.com");
        assert!(matches!(
            domain_to_ascii("a<b.com"),
            Err(IdnError::NotAnIdn { .. })
        ));
    }

    /// The deny list is `AsciiDenyList::URL`, applied to the converted
    /// name for every backend alike.
    ///
    /// **It used to be half of a pair.** The other half asserted that
    /// `domain_to_unicode` accepts `a<b.com`, because `idna`'s reverse
    /// direction takes no deny list — a difference between the two
    /// directions that this crate inherited and had to state. There is
    /// one direction now, so the pair has one member and the difference
    /// has no subject.

    #[test]
    fn the_deny_list_is_the_ascii_direction_s_alone_as_it_is_in_idna() {
        assert!(domain_to_ascii("a<b.com").is_err());
    }
}
