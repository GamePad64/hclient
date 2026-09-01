//! IDN conversion — a Unicode domain in, its A-label form out — using the
//! platform's own UTS 46 implementation where the platform has one, and
//! the bundled `idna` crate where it does not.
//!
//! **Two functions, and that is the whole surface**:
//! [`domain_to_ascii`] and [`domain_to_unicode`], plus the [`IdnError`]
//! their `Result` needs. Which implementation answers is decided by the
//! target and, where the target has a choice, by the one feature this
//! crate has — `idna`, off by default, which forces the bundled tables
//! everywhere.
//!
//! Four backends, one shape: Apple's Foundation, Windows' `icuuc.dll`,
//! Android's `android.icu.text.IDNA` over JNI, and the `idna` crate,
//! which is what the ELF unixes and wasm get because there is nothing
//! else there to ask. `lib.rs` names the selected one `platform` and
//! nothing past that line names an operating system.
//!
//! # Why this crate exists — and on Linux it is no longer the size
//!
//! Two corrections to the usual justification, in order of how much they
//! change it.
//!
//! **First, the number.** `hclient-proto`'s `idn` feature pulls `idna` →
//! `idna_adapter` → `icu_normalizer` + `icu_properties`, and the figure
//! this project has repeated for that — "roughly 1.9 MB" — is **vendored
//! source on disk, not bytes in a binary**. ICU4X stores its tables as
//! compressed tries and the linker keeps only what is referenced.
//! Measured on a binary that reads a domain from stdin so the call cannot
//! be folded away (`opt-level = "z"`, LTO, `panic = abort`, `strip`,
//! x86-64 Linux), the tables cost **128,784 B of `.rodata`** in a
//! 448,184 B binary of 31 crates — not 1.9 MB.
//!
//! **Second, and this is the one that changed the crate's purpose: on
//! Linux there is now no saving at all.** An ELF backend existed, reached
//! `libicuuc.so.NN` through `dlopen`, and did save it — 306,568 B and 10
//! crates, `.rodata` down to 23,864 B. It was removed deliberately, and
//! the reason is written out under *Which platform* below: on Linux the
//! ICU version is a property of the user's machine that nobody chose and
//! nothing reports, and for IDN a Unicode version difference is a
//! different host. Measured after the removal, same harness:
//!
//! | build, x86-64 Linux | binary | `.rodata` | crates |
//! |---|---|---|---|
//! | `idna` called directly — what `hclient-proto` compiled before it took this crate | 448,184 B | 128,784 B | 31 |
//! | this crate, default | 448,920 B | 129,144 B | 34 |
//!
//! **+736 bytes and +3 crates, for the same answer.** That is the honest
//! accounting on Linux, and it is stated first rather than buried: there
//! is no size saving on this platform.
//!
//! The +3 is this harness's, not every caller's: it is `hclient-idn`,
//! `thiserror` and `thiserror-impl`, and a graph that already had
//! `thiserror` pays only the first. In `hclient-proto`'s own `cargo tree
//! -e normal` the `idn` feature went from **36 unique crates to 37**.
//!
//! The saving survives only where a platform ICU is linked statically
//! against an OS-versioned ABI, which today means Windows alone. Its
//! magnitude there is **unverified**: measuring it needs a Windows
//! linker, and none produced this crate. What would settle it is the
//! same stdin harness built on a `windows-latest` runner.
//!
//! So what is the crate for, on the two targets where it saves nothing?
//! Three things it does that a direct `idna` call does not:
//!
//! - **one seam and one policy point.** The option word, the error mask
//!   and the deny list are decided once, here, instead of at each call
//!   site — and they are the three things the *Contract* section below
//!   shows are easy to get individually wrong. `policy.rs` is the rest of
//!   it, and it earned its own file the hard way: the two platform
//!   backends turn out to answer a *different question* — Windows' ICU is
//!   a UTS 46 implementation, Apple's Foundation is a URL parser that
//!   calls one, and only for a host that is not ASCII. Everything
//!   decidable from ASCII alone is decided there, once, rather than
//!   repaired per platform.
//! - **the corpus.** `tests/differential.rs` pins both implementations'
//!   answers on 40 rows; that is what makes "the platform agrees" a
//!   measurement rather than a hope, and it is shared by every target.
//! - **a typed error**, instead of a `bool` from a conversion that
//!   silently did something else.
//!
//! One alternative was measured and rejected rather than argued about:
//! pinning `idna_adapter` to 1.1.0 (the unicode-rs backend) is one
//! `cargo update`, needs no code and collapses the graph to 11 crates —
//! but it **doubles** the Unicode data, 128,784 B of `.rodata` to
//! 257,936 B, and runs a Unicode version behind ICU4X, which for IDN is a
//! different host rather than a cosmetic difference.
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
//! # Which platform, and what it costs
//!
//! Two platform backends, and one rule decides who gets one: **static
//! linkage against an ABI the OS versions for us.** Windows and Apple
//! qualify by different routes; Linux does not, and the ELF `dlopen`
//! backend that once lived here was removed for that reason (see
//! `icu/mod.rs`). They are otherwise nothing alike:
//!
//! - **Windows** — `icuuc.dll`, **linked** through `windows-sys`, whose
//!   `Win32_Globalization` already declares `uidna_openUTS46`,
//!   `uidna_nameToASCII_UTF8`, `uidna_close`, `UIDNAInfo` and every
//!   `UIDNA_*` constant, generated from Microsoft's Win32 metadata. So
//!   nothing is transcribed by hand here and `src/icu/windows.rs` has no
//!   `extern` block at all. This works because Windows' ICU is built with
//!   `U_DISABLE_RENAMING` and its exports are *unsuffixed* — a correction
//!   to this project's own design note, which said the opposite; that is
//!   a fact about Linux.
//!
//!   The cost is real and is not hedged: `windows-link` emits a
//!   `raw-dylib` **load-time** import, so a Windows without `icuuc.dll` —
//!   10 before 1703, and Server 2016 — does not fall back, the process
//!   fails to start. The floor is therefore **Windows 10 1703 / Server
//!   2019**, stated rather than degraded to.
//! - **Linux and other ELF unixes** — `libicuuc.so.NN`, **resolved at run
//!   time**, because here the symbols *and* the soname carry the version
//!   (`uidna_openUTS46_78`) and there is nothing stable to link. Having to
//!   do that buys the graceful behaviour the Windows side gives up: a
//!   machine with no ICU gets an ordinary miss. It is also what makes this
//!   crate's central claim *testable on a Linux CI runner* rather than
//!   only on Windows.
//! - **macOS — Foundation, and it is the cheapest backend of the three.**
//!   `URL(string:)` converts a Unicode host to its A-label, and
//!   `swift-foundation`'s `URLParser+ICU.swift` opens its handle with
//!   `UIDNA_CHECK_BIDI | UIDNA_CHECK_CONTEXTJ |
//!   UIDNA_NONTRANSITIONAL_TO_UNICODE | UIDNA_NONTRANSITIONAL_TO_ASCII` —
//!   bit for bit `OPTIONS`, arrived at independently, which is the best
//!   corroboration that constant will ever get.
//!
//!   **It needs no `unsafe` at all**, and so no spec amendment, where the
//!   Windows backend needs C9: every `objc2-foundation` call is a safe
//!   function, measured by a draft that wrapped them and drew ten
//!   `unnecessary unsafe block` warnings. That is the cheapest reason to
//!   keep it.
//!
//!   The catch is that Foundation converts as a **side effect of parsing a
//!   whole URL**, which costs four things — a host that changes where the
//!   URL ends, a scheme the caller could otherwise supply, no error
//!   channel, and two getters that disagree. Each is closed rather than
//!   hoped away; see `foundation.rs`. `libicucore.dylib`, Apple's own ICU,
//!   stays out of reach: no headers, symbols documented as not for
//!   third-party linking, and since Big Sur not on disk to `dlopen`.
//!
//!   **A fifth cost, and it is the one the first live run found: the hook
//!   only runs when the host is not ASCII.** An ASCII host passes RFC 3986
//!   `reg_name` validation and is copied into the URL verbatim, so nothing
//!   lower-cases it, nothing decodes an `xn--` label in it, and an empty
//!   host is a parse failure rather than the empty name. `policy.rs` takes
//!   all of that over, for both backends rather than for this one; see its
//!   module docs for the three corpus rows that measured it.
//! - **wasm** — no dynamic loader and no system ICU, so the bundled path
//!   today. But **the browser is a platform IDN implementation**, and on
//!   the evidence it is the best one of the lot: `new URL(…).hostname` in
//!   Chrome 151 agrees with `idna` on all twelve probes tried, including
//!   `straße.de` → `xn--strae-oqa.de`, and it independently pins three
//!   rows of the contract table above (`-lead.de` passes, so
//!   _CheckHyphens=false_; `a..b` passes, so _VerifyDnsLength=false_;
//!   `١٢٣.com` throws, so _CheckBidi=true_). `web-sys` is already in
//!   `hclient-fetch`'s graph, so it costs no new crate — and it needs no
//!   `unsafe` at all, unlike every other platform path here.
//!
//!   Not implemented, and one precondition is why it must
//!   not be done casually: `URL` is a *parser*, so `ex@ample.com` comes
//!   back as the host `ample.com` and `ex/ample.com` as `ex` — a
//!   wrong-origin generator if an unvalidated string is handed to it.
//!   Every input in that family carries a byte from the forbidden-domain
//!   set, so `is_forbidden_domain_byte` run *before* `new URL()`
//!   removes all of them; the check already exists here, for the other
//!   platform. One divergence would remain and is a decision rather than
//!   a bug to route around: browsers accept invalid punycode (`xn--a.de`)
//!   where `idna` rejects it — _IgnoreInvalidPunycode_, `false` in the
//!   table above and effectively `true` in a browser. Firefox and Safari
//!   are **unverified**; this project already has one Chrome/Safari split
//!   recorded in `Capabilities`, so they need the same probe under
//!   `wasm-pack test --headless` before any of this is believed.
//!
//! # Features
//!
//! | features | behaviour |
//! |---|---|
//! | `platform` (default) | **resolved by target**: the platform's ICU on Windows and ELF unixes, the `idna` crate on macOS, wasm and anything else |
//! | `bundled` | the `idna` crate explicitly, on a target that has no system ICU; a compile error naming the target on one that does |
//! | `system-icu` | Windows' `icuuc.dll` explicitly; a no-op on a target that has no such dependency |
//! | `foundation` | Apple's Foundation explicitly; likewise |
//! | neither | a `compile_error!`, not a silently useless crate |
//!
//! The two backends are **never both compiled in**, and that is a
//! consequence rather than a preference: cargo features are
//! target-independent while dependencies are not, so the target tables in
//! `Cargo.toml` are the only place the choice can vary — and a table
//! either supplies `idna` for this target or does not. Asking for the
//! backend this target has no dependency for is a `cargo::error` from
//! `build.rs` naming the target, not a feature that silently does
//! nothing.
//!
//! **Comparing the two therefore happens in the tests, not at run time.**
//! `idna` is a dev-dependency, so `tests/differential.rs` can call it
//! directly as the oracle on exactly the targets where it is *not* a
//! normal dependency — which is the only place the comparison is worth
//! making.
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
// Unconditional, like `android`: it holds ICU's vocabulary — the option
// bits, the error mask, the acceptance rule — which both ICU backends
// share, and only its Windows binding is gated inside.
mod icu;

// Unconditional: only its JNI half is gated, and the half that holds the
// decision — which ICU errors are forgiven — is tested on every host.
mod android;

#[cfg(apple_backend)]
mod apple;

#[cfg(idna_backend)]
mod bundled;

core::cfg_select! {
    idna_backend => { use bundled as platform; }
    apple_backend => { use apple as platform; }
    icu_backend => { use icu as platform; }
    android_backend => { use android as platform; }
}

/// This crate's own layer, shared by every backend and reached through
/// all of them — see the module docs for what it takes over and why.
///
/// Compiled on every target, including the ones with no platform backend
/// at all, and reachable there through [`testing::policy_over`]: the
/// layer is platform-independent, everything in it is decided against
/// `idna` (a dev-dependency everywhere), and gating it away on Linux
/// would take its tests and its fuzz target with it — on the one runner
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
    not(any(apple_backend, android_backend)),
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
/// the other sends Objective-C messages to Foundation — and this is
/// exactly the kind of constant that would drift if each kept its own.
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
    not(any(icu_backend, apple_backend, android_backend)),
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

/// The same probe pair read the other way: an A-label in, the Unicode
/// name out.
///
/// **This is what lets a platform getter be used before anyone has run
/// it.** `apple.rs`'s `to_unicode` reads `NSURLComponents::host` on an
/// argument from Apple's documentation and no machine here can execute
/// it; a build where that getter is not what the argument says fails this
/// and answers `IdnError::NoImplementation`, which is a refusal rather
/// than a wrong name. The forward half has always been gated this way and
/// for the same reason — see `macos_getter_that_returns_the_a_label`.
#[cfg_attr(
    not(any(icu_backend, apple_backend, android_backend)),
    allow(
        dead_code,
        reason = "only a platform backend has anything to gate, but the POLICY is \
                  platform-independent and so are its tests"
    )
)]
pub(crate) fn accepts_back(convert: impl Fn(&str) -> Option<String>) -> bool {
    PROBES
        .iter()
        .all(|(want, input)| convert(input).as_deref() == Some(*want))
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
                // used its answer — which is the case `apple.rs`'s
                // `to_unicode` is written under, since nobody here has a Mac.
                accepts(|input| platform::to_ascii(h, input))
                    && accepts_back(|input| platform::to_unicode(h, input))
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
    over(domain, platform::to_ascii)
}

/// Converts `domain` to its Unicode (U-label) form — UTS 46 ToUnicode.
///
/// `xn--mnchen-3ya.de` comes back as `münchen.de`. A name with no ACE
/// label comes back lower-cased and otherwise unchanged, which is what
/// ToUnicode does rather than a shortcut.
///
/// **It is the platform's own reverse direction**, not a punycode
/// decoder of this crate's: `uidna_nameToUnicodeUTF8` on Windows,
/// `IDNA.nameToUnicode` on Android, `idna::domain_to_unicode` in the
/// bundled build, `NSURLComponents::host` on Apple. So it answers what
/// `idna` answers, which is the whole of what this crate promises.
///
/// # Errors
///
/// As [`domain_to_ascii`].
pub fn domain_to_unicode(domain: &str) -> Result<Cow<'_, str>, IdnError> {
    over(domain, platform::to_unicode)
}

/// One of the two entry points in [`policy`], as a value the dispatch can
/// take: [`policy::to_ascii_over`] or `policy::to_unicode_over`.
///
/// A named type because clippy asks for one, and it earns the name — the
/// pair of directions is the thing being abstracted over, and `PolicyFn`
/// says that where the written-out signature says only that something
/// takes a closure.
type Direction = fn(&platform::Handle, &str) -> Option<String>;

/// The dispatch both directions share.
///
/// `direction` is the selected backend's own conversion — `to_ascii` or
/// `to_unicode` — and there is nothing between it and the caller. That is
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
/// the target has neither ICU nor Foundation, so on every target where such
/// a function would compile it *is* the backend [`domain_to_ascii`] already
/// calls — a differential probe comparing `idna` with itself. Measured
/// rather than reasoned: `cargo check -p hclient-idn --all-features`, i.e.
/// every backend feature requested at once, emits exactly one
/// `rustc-cfg` per target — `idna_backend` on `x86_64-unknown-linux-gnu`,
/// `icu_backend` on `x86_64-pc-windows-msvc`, `apple_backend` on
/// `aarch64-apple-darwin`, and never two. Making `idna` available beside a
/// platform backend would buy the comparison back at the price of the ICU
/// tables on Windows and macOS, which is the saving this crate exists for.
/// [`testing::policy_over`] is the differential seam that does work, and
/// `fuzz/fuzz_targets/idn_policy_vs_idna.rs` is what uses it.
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

    /// **The two directions are one another's inverse on a name that has
    /// an ACE label**, which is the property `domain_to_unicode` exists
    /// for and the one a caller will assume.
    #[test]
    fn to_unicode_undoes_to_ascii() {
        assert_eq!(
            domain_to_unicode("xn--mnchen-3ya.de").unwrap(),
            "münchen.de"
        );
        assert_eq!(domain_to_unicode("münchen.de").unwrap(), "münchen.de");
        assert_eq!(
            domain_to_ascii(&domain_to_unicode("xn--strae-oqa.de").unwrap()).unwrap(),
            "xn--strae-oqa.de"
        );
    }

    /// A name with no ACE label comes back lower-cased and otherwise
    /// untouched — ToUnicode's answer rather than a shortcut, and the
    /// control that says the decoder is not being reached for every name.
    #[test]
    fn an_ascii_name_is_its_own_unicode_form_lower_cased() {
        assert_eq!(domain_to_unicode("EXAMPLE.COM").unwrap(), "example.com");
    }

    /// **The two directions do not accept the same names, and that is
    /// `idna`'s property rather than a defect of this crate's.**
    ///
    /// `domain_to_ascii` takes `AsciiDenyList::URL` and refuses
    /// `a<b.com`; `idna::domain_to_unicode` takes no deny list at all and
    /// answers it. A test here asserted they agree, and it passed only
    /// while a layer of this crate's own forced both through one path —
    /// which is the URL validation that no longer belongs here.
    ///
    /// So the assertion is the difference rather than the agreement: this
    /// crate is a smaller-binary `idna` and inherits its shape, including
    /// the parts a caller might not expect.
    #[test]
    fn the_deny_list_is_the_ascii_direction_s_alone_as_it_is_in_idna() {
        assert!(domain_to_ascii("a<b.com").is_err());
        assert!(
            domain_to_unicode("a<b.com").is_ok(),
            "`idna::domain_to_unicode` takes no deny list, so neither does this — a caller who \
             needs one applies it where the host is used, which is what `hclient-proto::uri` does"
        );
    }
}
