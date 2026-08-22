//! IDN conversion — a Unicode domain in, its A-label form out — using the
//! platform's own UTS 46 implementation where the platform has one, and
//! the bundled `idna` crate where it does not.
//!
//! One function's worth of surface: [`domain_to_ascii`]. [`backend`] says
//! which implementation answered, and is resolved by the same cell the
//! conversion uses, so the two cannot drift apart.
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
//! - **a typed error and an honest [`Backend`]**, instead of a `bool`
//!   from a conversion that silently did something else.
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
//! only two of them are ICU *options*. The rest are [`IGNORED_ERRORS`] and
//! [`is_forbidden_domain_byte`] below — safe Rust with tests around it,
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
//!   | input | `uidna_openUTS46(0)` | `uidna_openUTS46(`[`OPTIONS`]`)` | `idna` |
//!   |---|---|---|---|
//!   | `straße.de` | `strasse.de` | `xn--strae-oqa.de` | `xn--strae-oqa.de` |
//!   | `faß.de` | `fass.de` | `xn--fa-hia.de` | `xn--fa-hia.de` |
//!
//!   The function is named for the standard it is being asked *not* to
//!   follow, so nothing about the call site invites suspicion of the flag.
//!   That is why [`OPTIONS`] is a named constant with the bits spelled out
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
//!   bit for bit [`OPTIONS`], arrived at independently, which is the best
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
//!   Not implemented in this task, and one precondition is why it must
//!   not be done casually: `URL` is a *parser*, so `ex@ample.com` comes
//!   back as the host `ample.com` and `ex/ample.com` as `ex` — a
//!   wrong-origin generator if an unvalidated string is handed to it.
//!   Every input in that family carries a byte from the forbidden-domain
//!   set, so [`is_forbidden_domain_byte`] run *before* `new URL()`
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

#![cfg_attr(docsrs, feature(doc_auto_cfg))]
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

#[cfg(icu_backend)]
mod icu;

#[cfg(foundation_backend)]
mod foundation;

/// This crate's own layer, shared by every backend and reached through
/// all of them — see the module docs for what it takes over and why.
///
/// Compiled on every target, including the ones with no platform backend
/// at all, and reachable there through [`testing::policy_over`]: the
/// layer is platform-independent, everything in it is decided against
/// `idna` (a dev-dependency everywhere), and gating it away on Linux
/// would take its tests and its fuzz target with it — on the one runner
/// this project exercises most.
mod policy;

#[cfg(not(any(idna_backend, icu_backend, foundation_backend)))]
compile_error!(
    "hclient-idn needs at least one backend: `bundled` (the `idna` crate, the default) or \
     `system-icu` (the platform's own ICU), or both. With neither, `domain_to_ascii` could only \
     ever return `IdnError::NoImplementation`, which is a build nobody wants by accident."
);

/// What went wrong turning a domain into its A-label form.
///
/// Two variants because `hclient-proto` distinguishes two things a caller
/// can do something about: [`NotAnIdn`](IdnError::NotAnIdn) maps to
/// `UriError::NotAnIdn` ("this name is not usable"), and
/// [`NoImplementation`](IdnError::NoImplementation) maps to
/// `UriError::NonAsciiHost` ("this build cannot convert; send the A-label
/// yourself"). Collapsing them would tell a user to fix their domain when
/// the actual problem is the build they are running.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdnError {
    /// UTS 46 rejected the name: a disallowed code point, a bidi or
    /// joiner-context violation, invalid punycode under an `xn--` label,
    /// or an ASCII character the WHATWG URL Standard forbids in a domain.
    #[error("`{domain}` is not a usable internationalised domain name: UTS 46 rejected it")]
    NotAnIdn {
        /// The domain, as given.
        domain: String,
    },
    /// This build has no IDN implementation that can run here: the
    /// `bundled` feature is off, and no system ICU was found at run time.
    /// The name itself may be perfectly valid.
    #[error(
        "`{domain}` needs IDN conversion and this build has none: it was built with \
         `system-icu` and without `bundled`, and no system ICU library was found at run time. \
         Enable the `bundled` feature, or supply the host in its A-label form — `münchen.de` \
         is written `xn--mnchen-3ya.de`"
    )]
    NoImplementation {
        /// The domain, as given.
        domain: String,
    },
}

/// Which implementation [`domain_to_ascii`] actually calls in this
/// process.
///
/// Not "which feature is enabled" — with `bundled` and `system-icu` both
/// on, the answer depends on whether a system ICU was found, which is a
/// property of the machine. A capability that reports a compile-time guess
/// where the truth is a run-time fact is the "capability that lies" defect
/// this project has caught elsewhere; [`backend`] reads the same resolved
/// cell the conversion reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Backend {
    /// The platform's own ICU, found and loaded at run time, and the only
    /// implementation in this build — `system-icu` without `bundled`.
    /// Nothing cross-checks it, which is the price of not carrying the
    /// tables; the load-time acceptance probe in `icu.rs` is the only
    /// guard, and it is a behaviour floor rather than a Unicode-version
    /// floor.
    SystemIcu,
    /// Apple's Foundation, through `NSURL`. Linked, like the ICU
    /// backend, and for the same reason: it arrives with the OS rather
    /// than with whatever the user installed. Unlike the ICU backend it
    /// needs no `unsafe` at all — see `foundation.rs`.
    SystemFoundation,
    /// The bundled `idna` crate and its Unicode tables.
    Bundled,
    /// Neither: `system-icu` without `bundled`, on a machine with no ICU.
    /// Every call returns [`IdnError::NoImplementation`].
    None,
}

// ── The six ICU option bits, from `unicode/uidna.h` ─────────────────────
//
// Spelled out rather than imported, because the crate that would supply
// them does not exist: `rust_icu_sys` 5.7.0's `BINDGEN_SOURCE_MODULES`
// does not include `uidna`, and its function allow-list has no
// `uidna_.*` — checked in the published source, not inferred.

/// `UIDNA_USE_STD3_RULES`. **Deliberately not set**: it restricts ASCII to
/// letters/digits/hyphen, which rejects the underscore that appears in
/// `_dmarc`-style pseudo-hosts, while *allowing* `%` and `<` — so it is
/// neither a superset nor a subset of the WHATWG deny list this crate has
/// to reproduce. [`is_forbidden_domain_byte`] does that job instead.
#[allow(
    dead_code,
    reason = "documented as not set; named so the reader can see it was considered"
)]
pub const UIDNA_USE_STD3_RULES: u32 = 0x0002;

/// `UIDNA_CHECK_BIDI`. `idna` applies CheckBidi unconditionally — it is
/// not one of its configurable flags — so this must be on to match.
pub const UIDNA_CHECK_BIDI: u32 = 0x0004;

/// `UIDNA_CHECK_CONTEXTJ`. Same story: `idna`'s CheckJoiners is always
/// true, so ZWJ/ZWNJ context rules have to be on here too.
pub const UIDNA_CHECK_CONTEXTJ: u32 = 0x0008;

/// `UIDNA_NONTRANSITIONAL_TO_ASCII`. **The bit this whole crate turns on.**
pub const UIDNA_NONTRANSITIONAL_TO_ASCII: u32 = 0x0010;

/// `UIDNA_NONTRANSITIONAL_TO_UNICODE`. Set together with its sibling: this
/// crate only converts to ASCII today, but a handle opened with a
/// half-transitional option set would answer differently the moment
/// anything calls `nameToUnicode` on it, and that is not a difference
/// worth leaving lying around.
pub const UIDNA_NONTRANSITIONAL_TO_UNICODE: u32 = 0x0020;

/// `UIDNA_CHECK_CONTEXTO`. **Deliberately not set**: UTS 46 makes ContextO
/// optional and `idna` does not implement it, so setting it would reject
/// names the bundled path accepts.
#[allow(
    dead_code,
    reason = "documented as not set; named so the reader can see it was considered"
)]
pub const UIDNA_CHECK_CONTEXTO: u32 = 0x0040;

/// The option word passed to `uidna_openUTS46`. `0x3c`.
///
/// **`UIDNA_DEFAULT` is 0 and 0 is transitional** — see the crate docs for
/// the measurement. Four bits, each justified on its own constant above.
///
/// **Independently arrived at by Apple, which is the best corroboration
/// available for a number like this.** `swift-foundation`'s
/// `Sources/FoundationInternationalization/URLParser+ICU.swift` (`struct
/// UIDNAHookICU`) opens its own handle with
/// `UIDNA_CHECK_BIDI | UIDNA_CHECK_CONTEXTJ |
/// UIDNA_NONTRANSITIONAL_TO_UNICODE | UIDNA_NONTRANSITIONAL_TO_ASCII` —
/// the same four bits, in a URL parser solving the same problem, written
/// by people with no connection to this one.
///
/// They then diverge from us on the *errors*, which is exactly why
/// [`IGNORED_ERRORS`] is a separate decision and not a footnote:
/// `shouldAllow(_:encodeToASCII: true)` sets `allowedErrors = 0`, so
/// Apple's `URL` refuses a name on any bit at all, the six this crate
/// masks included.
///
/// **`a..b` and `ab--cd.com` are NOT examples of this**, and
/// `macos-latest` measures both as accepted. The reason is one level up
/// and is the whole of `foundation.rs`'s first
/// section: those two are all-ASCII, so Foundation's IDNA hook never runs
/// on them and there is no `errors` word to be strict about. The
/// divergence is real, but its inputs are names with a non-ASCII label
/// *and* one of those defects — `-münchen.de`, `münchen..de`,
/// `ab--cd.münchen`.
pub const OPTIONS: u32 = UIDNA_NONTRANSITIONAL_TO_ASCII
    | UIDNA_NONTRANSITIONAL_TO_UNICODE
    | UIDNA_CHECK_BIDI
    | UIDNA_CHECK_CONTEXTJ;

// ── The `UIDNAInfo.errors` bits this crate ignores ──────────────────────
//
// ICU has no options for CheckHyphens or VerifyDnsLength: it always runs
// both checks and reports them as bits. `idna`, called the way
// `hclient-proto` calls it, has both OFF. So agreement is not a matter of
// options alone — the six bits below have to be masked out of ICU's
// answer, or every one of `a..b`, `-lead.de`, `ab--cd.com`, a 64-byte
// label and a 254-byte name would be rejected by the platform path and
// accepted by the bundled one.

const UIDNA_ERROR_EMPTY_LABEL: u32 = 0x0001;
const UIDNA_ERROR_LABEL_TOO_LONG: u32 = 0x0002;
const UIDNA_ERROR_DOMAIN_NAME_TOO_LONG: u32 = 0x0004;
const UIDNA_ERROR_LEADING_HYPHEN: u32 = 0x0008;
const UIDNA_ERROR_TRAILING_HYPHEN: u32 = 0x0010;
const UIDNA_ERROR_HYPHEN_3_4: u32 = 0x0020;

/// The `UIDNAInfo.errors` bits that do **not** make a name unusable here.
///
/// The first three are _VerifyDnsLength=false_, the last three are
/// _CheckHyphens=false_. Nothing else is masked: a disallowed code point,
/// a leading combining mark, bad punycode, a dot inside a decoded label,
/// an invalid ACE label, a bidi violation or a ContextJ violation all
/// stand, because `idna` rejects all of those too.
pub const IGNORED_ERRORS: u32 = UIDNA_ERROR_EMPTY_LABEL
    | UIDNA_ERROR_LABEL_TOO_LONG
    | UIDNA_ERROR_DOMAIN_NAME_TOO_LONG
    | UIDNA_ERROR_LEADING_HYPHEN
    | UIDNA_ERROR_TRAILING_HYPHEN
    | UIDNA_ERROR_HYPHEN_3_4;

/// Whether ICU's `UIDNAInfo.errors` word means "reject this name".
///
/// Lives here, in safe tested code, and not in `icu.rs`, for the same
/// reason `sys::classify_written` lives outside the FFI file in
/// `hclient-dns-system`: this is a *decision about what the answer means*,
/// and it is worth more as ordinary Rust with tests around it than as one
/// more line inside an `unsafe` block.
#[must_use]
pub const fn is_fatal(errors: u32) -> bool {
    errors & !IGNORED_ERRORS != 0
}

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
pub const fn is_forbidden_domain_byte(b: u8) -> bool {
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
    not(any(icu_backend, foundation_backend)),
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
/// [`IdnError::NoImplementation`] if this build has no implementation that
/// can run here — see [`Backend`].
pub fn domain_to_ascii(domain: &str) -> Result<Cow<'_, str>, IdnError> {
    match backend() {
        #[cfg(icu_backend)]
        Backend::SystemIcu => system_icu_to_ascii(domain),
        #[cfg(foundation_backend)]
        Backend::SystemFoundation => foundation_to_ascii(domain),
        #[cfg(idna_backend)]
        Backend::Bundled => bundled_to_ascii(domain),
        Backend::None => Err(IdnError::NoImplementation {
            domain: domain.to_owned(),
        }),
        #[allow(
            unreachable_patterns,
            reason = "the arms above are feature-gated, so which variants are constructible \
                      changes with the feature set; this arm is dead in some of them"
        )]
        other => unreachable!("{other:?} is not reachable in this feature set"),
    }
}

/// Which implementation answers in this process — see [`Backend`].
///
/// Cheap after the first call: the library search behind `system-icu`
/// happens once, in a `OnceLock`.
#[must_use]
pub fn backend() -> Backend {
    #[cfg(icu_backend)]
    if icu::library().is_some() {
        return Backend::SystemIcu;
    }
    #[cfg(foundation_backend)]
    if foundation_backend().is_some() {
        return Backend::SystemFoundation;
    }
    #[cfg(idna_backend)]
    return Backend::Bundled;
    #[cfg(not(idna_backend))]
    Backend::None
}

/// The bundled path: the exact call `hclient-proto` used to make itself,
/// so a target that resolves to this backend answers what that crate
/// answered before it took this one — byte for byte, and measured that
/// way rather than argued.
#[cfg(idna_backend)]
fn bundled_to_ascii(domain: &str) -> Result<Cow<'_, str>, IdnError> {
    // **Through the policy, like the other two**, and it was the one that
    // was not. It called `idna::domain_to_ascii_cow` directly, so on Linux
    // and wasm the shared policy never ran — which is the ICU path's own
    // argument left unapplied: *"the alternative is two statements of one
    // contract and the newer one is always the one that rots."*
    //
    // What it cost was the divergence this crate exists to prevent.
    // `ä..de` converted here and was refused by Foundation, so the same
    // host was reachable on Linux and Windows and not on macOS — and the
    // empty-label rule added to `policy::to_ascii_over` fixed nothing
    // until this line, because the rule was in a layer this path skipped.
    // Measured, not assumed: the `uri_resolution` corpus stayed green on
    // Linux across that change, which is what said the layer was being
    // bypassed.
    policy::to_ascii_over(
        |unicode| {
            idna::domain_to_ascii_cow(unicode.as_bytes(), idna::AsciiDenyList::URL)
                .ok()
                .map(std::borrow::Cow::into_owned)
        },
        domain,
    )
    .map(Cow::Owned)
    .ok_or_else(|| IdnError::NotAnIdn {
        domain: domain.to_owned(),
    })
}

/// The ICU path: the shared policy, with ICU as its conversion.
///
/// ICU would answer every one of [`policy::to_ascii_over`]'s six steps correctly
/// on its own — it is a UTS 46 implementation, and the corpus measured
/// that on a real `windows-latest` runner before this policy existed. It
/// goes through the policy anyway, because the alternative is two
/// statements of one contract and the newer one is always the one that
/// rots. What the Windows corpus run now checks is that the shared policy
/// left ICU's answers alone.
///
/// Two consequences worth naming. An all-ASCII name no longer reaches
/// `uidna_nameToASCII_UTF8` at all — step 4 answers it — so the `unsafe`
/// boundary is crossed for fewer inputs than before. And the input deny
/// scan is back after having been measured redundant and deleted: it is
/// redundant *for ICU*, and it is not redundant for a policy that hands a
/// decoded label onward.
#[cfg(icu_backend)]
fn system_icu_to_ascii(domain: &str) -> Result<Cow<'_, str>, IdnError> {
    policy::to_ascii_over(icu::name_to_ascii, domain)
        .map(Cow::Owned)
        .ok_or_else(|| IdnError::NotAnIdn {
            domain: domain.to_owned(),
        })
}

/// Foundation, gated once per process on the same probe pair the ICU
/// backend uses.
///
/// The gate matters more here than it looks. A wrong getter (see
/// `foundation.rs`, consequence 4) produces a *plausible* answer — a
/// percent-encoded host rather than an A-label — so the failure would not
/// announce itself. `straße.de` does.
#[cfg(foundation_backend)]
fn foundation_backend() -> Option<&'static foundation::Foundation> {
    use std::sync::OnceLock;
    static F: OnceLock<Option<foundation::Foundation>> = OnceLock::new();
    F.get_or_init(|| foundation::find().filter(|f| accepts(|input| foundation::convert(f, input))))
        .as_ref()
}

/// The Foundation path: the same shared policy, with Foundation as its
/// conversion.
///
/// Foundation reaches a UTS 46 implementation only for a host that is not
/// ASCII — which is the one question [`policy::to_ascii_over`] asks a backend, and
/// the reason the three rows that failed on `macos-latest` were all
/// all-ASCII ones.
#[cfg(foundation_backend)]
fn foundation_to_ascii(domain: &str) -> Result<Cow<'_, str>, IdnError> {
    let f = foundation_backend().ok_or_else(|| IdnError::NoImplementation {
        domain: domain.to_owned(),
    })?;
    policy::to_ascii_over(|unicode| foundation::convert(f, unicode), domain)
        .map(Cow::Owned)
        .ok_or_else(|| IdnError::NotAnIdn {
            domain: domain.to_owned(),
        })
}

/// What the differential corpus in `tests/differential.rs` needs and
/// [`domain_to_ascii`] deliberately will not give it: the platform backend
/// by name, the resolved [`Backend`], and this crate's own policy layer
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
/// `icu_backend` on `x86_64-pc-windows-msvc`, `foundation_backend` on
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
    use super::Backend;

    /// UTS 46's four label separators, for a test or a fuzz target that
    /// has to split a domain the way this crate does.
    ///
    /// **Data, deliberately, and not the rule.** The policy refuses a
    /// domain with an empty label that is not the single trailing root,
    /// and a caller checking that rule writes it out itself — sharing the
    /// rule would make every such check a tautology over the
    /// implementation. Sharing the character set is safe because it is
    /// four codepoints with no logic in them, and `policy.rs`'s
    /// `the_label_separators_are_exactly_these_four` is what pins it.
    ///
    /// The alternative was hard-coding them at each site, and the fuzzer
    /// showed what that costs: a helper splitting on `'.'` alone missed
    /// `"．"` — a fullwidth full stop, which is one of the four.
    pub const LABEL_SEPARATORS: [char; 4] = super::policy::LABEL_SEPARATORS;
    #[cfg(any(icu_backend, foundation_backend))]
    use super::IdnError;
    #[cfg(any(icu_backend, foundation_backend))]
    use std::borrow::Cow;

    /// The platform implementation — whichever this target has — or
    /// `None` if it was not accepted, which is the difference between
    /// "the corpus checked the platform column" and "the corpus silently
    /// checked nothing".
    ///
    /// One name for both backends on purpose: `tests/differential.rs`
    /// then contains no `#[cfg]` choosing between Windows and Apple, and
    /// the same 40 rows are the acceptance for both.
    ///
    /// # Errors
    /// As [`super::domain_to_ascii`].
    #[cfg(any(icu_backend, foundation_backend))]
    pub fn platform(domain: &str) -> Option<Result<Cow<'_, str>, IdnError>> {
        #[cfg(icu_backend)]
        {
            super::icu::library()?;
            Some(super::system_icu_to_ascii(domain))
        }
        #[cfg(foundation_backend)]
        {
            super::foundation_backend()?;
            Some(super::foundation_to_ascii(domain))
        }
    }

    /// What answered, for a report that names it rather than saying "the
    /// platform" — `icuuc.dll (windows-sys, load-time import)`,
    /// `Foundation NSURL (objc2-foundation, safe bindings)`.
    #[cfg(any(icu_backend, foundation_backend))]
    #[must_use]
    pub fn platform_name() -> Option<&'static str> {
        #[cfg(icu_backend)]
        {
            super::icu::library_name()
        }
        #[cfg(foundation_backend)]
        {
            super::foundation_backend().map(super::foundation::Foundation::name)
        }
    }

    /// Re-exported so a test can assert the selected backend without
    /// duplicating the feature logic.
    #[must_use]
    pub fn selected() -> Backend {
        super::backend()
    }

    /// The crate's own layer, over a conversion the caller supplies.
    ///
    /// This is what makes the layer fuzzable **differentially**, which
    /// `domain_to_ascii` is not: on a target with the bundled backend that
    /// function *is* `idna::domain_to_ascii_cow`, so comparing it with
    /// `idna` compares `idna` with itself. Hand `idna` in here as the
    /// backend instead and the only thing left in the comparison is this
    /// crate's own code — the deny list, the case folding, the ASCII
    /// short-circuit and a hand-written RFC 3492 decoder, which is the
    /// riskiest thing in the crate because it sits in the path that
    /// decides which host is contacted.
    ///
    /// See `fuzz/fuzz_targets/idn_policy_vs_idna.rs`.
    #[must_use]
    pub fn policy_over(convert: impl Fn(&str) -> Option<String>, domain: &str) -> Option<String> {
        super::policy::to_ascii_over(convert, domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_option_word_is_non_transitional_and_nothing_is_transitional_about_it() {
        assert_eq!(
            OPTIONS, 0x3c,
            "OPTIONS changed; the bits are 0x10|0x20|0x04|0x08"
        );
        assert_ne!(
            OPTIONS, 0,
            "UIDNA_DEFAULT is 0 and 0 is transitional — see the crate docs"
        );
        assert_eq!(
            OPTIONS & UIDNA_NONTRANSITIONAL_TO_ASCII,
            UIDNA_NONTRANSITIONAL_TO_ASCII,
            "the one bit that decides whether `straße.de` becomes `strasse.de` or `xn--strae-oqa.de`"
        );
        assert_eq!(
            OPTIONS & UIDNA_NONTRANSITIONAL_TO_UNICODE,
            UIDNA_NONTRANSITIONAL_TO_UNICODE
        );
        assert_eq!(OPTIONS & UIDNA_CHECK_BIDI, UIDNA_CHECK_BIDI);
        assert_eq!(OPTIONS & UIDNA_CHECK_CONTEXTJ, UIDNA_CHECK_CONTEXTJ);
        assert_eq!(
            OPTIONS & UIDNA_USE_STD3_RULES,
            0,
            "STD3 is a different set from the WHATWG deny list, not a stricter one"
        );
        assert_eq!(
            OPTIONS & UIDNA_CHECK_CONTEXTO,
            0,
            "`idna` does not implement ContextO, so neither may this"
        );
    }

    #[test]
    fn the_ignored_error_bits_are_exactly_check_hyphens_and_verify_dns_length() {
        assert_eq!(IGNORED_ERRORS, 0x3f);
        // VerifyDnsLength=false.
        assert!(!is_fatal(UIDNA_ERROR_EMPTY_LABEL));
        assert!(!is_fatal(UIDNA_ERROR_LABEL_TOO_LONG));
        assert!(!is_fatal(UIDNA_ERROR_DOMAIN_NAME_TOO_LONG));
        // CheckHyphens=false.
        assert!(!is_fatal(UIDNA_ERROR_LEADING_HYPHEN));
        assert!(!is_fatal(UIDNA_ERROR_TRAILING_HYPHEN));
        assert!(!is_fatal(UIDNA_ERROR_HYPHEN_3_4));
        // Everything else stands. 0x0040 LEADING_COMBINING_MARK, 0x0080
        // DISALLOWED, 0x0100 PUNYCODE, 0x0200 LABEL_HAS_DOT, 0x0400
        // INVALID_ACE_LABEL, 0x0800 BIDI, 0x1000 CONTEXTJ, 0x2000/0x4000
        // CONTEXTO.
        for bit in [
            0x0040, 0x0080, 0x0100, 0x0200, 0x0400, 0x0800, 0x1000, 0x2000, 0x4000,
        ] {
            assert!(is_fatal(bit), "0x{bit:04x} must not be masked away");
        }
        assert!(!is_fatal(0), "a clean answer is a clean answer");
    }

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
        assert_eq!(bundled_to_ascii("münchen.de").unwrap(), "xn--mnchen-3ya.de");
        assert_eq!(bundled_to_ascii("straße.de").unwrap(), "xn--strae-oqa.de");
        assert_eq!(bundled_to_ascii("EXAMPLE.COM").unwrap(), "example.com");
        assert!(matches!(
            bundled_to_ascii("a<b.com"),
            Err(IdnError::NotAnIdn { .. })
        ));
    }
}
