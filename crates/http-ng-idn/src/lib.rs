//! IDN conversion — a Unicode domain in, its A-label form out — using the
//! platform's own UTS 46 implementation where the platform has one, and
//! the bundled `idna` crate where it does not.
//!
//! One function's worth of surface: [`domain_to_ascii`]. [`backend`] says
//! which implementation answered, and is resolved by the same cell the
//! conversion uses, so the two cannot drift apart.
//!
//! # Why this crate exists, and why the usual number for it is wrong
//!
//! `http-ng-proto`'s `idn` feature pulls `idna` → `idna_adapter` →
//! `icu_normalizer` + `icu_properties`. The figure this project has
//! repeated for that — "roughly 1.9 MB" — is **vendored source on disk,
//! not bytes in a binary**, and quoting it against a flash budget
//! compares two different things. ICU4X stores its tables as compressed
//! tries and the linker keeps only what is referenced.
//!
//! Measured instead, on a binary that reads a domain from stdin so the
//! call cannot be folded away — `opt-level = "z"`, LTO, `panic = abort`,
//! `strip`, x86-64 Linux. Every row answers `straße.de` with
//! `xn--strae-oqa.de`:
//!
//! | build | binary | `.rodata` | crates |
//! |---|---|---|---|
//! | `idna` called directly — the incumbent, what `http-ng-proto` compiles today | 448,184 B | 128,784 B | 31 |
//! | the same, with `idna_adapter` pinned to 1.1.0 (unicode-rs) | 577,112 B | 257,936 B | **11** |
//! | this crate, default, on a target with a system ICU | **306,568 B** | **23,864 B** | **10** |
//!
//! **The middle row is why this crate justifies itself on data rather
//! than on crate count.** Pinning `idna_adapter` to the unicode-rs
//! backend is one `cargo update`, needs no code, no `unsafe` and no new
//! crate, and it collapses the graph. It is a real answer to "36 crates
//! with `idn`, 10 without", which is how this project has usually stated
//! the problem. What it does not do is remove any Unicode data: it
//! **doubles** it, 129 KiB of `.rodata` to 258 KiB — and it buys those
//! crates with a Unicode version *behind* ICU4X's, which for IDN means a
//! different host, not a cosmetic difference. It was rejected here for
//! exactly that reason.
//!
//! So the two were never competing solutions to one problem. The pin
//! trades bytes, and correctness, for crates. This crate removes the
//! bytes: **102.5 KiB of `.rodata` and 138.3 KiB of binary**, because the
//! tables it uses are already on the machine — and it gets the crate
//! count as well, 31 down to 10. On a 256 KB part that is the difference
//! between half the flash and none of it, which is the claim worth
//! making. "1.9 MB" was not it.
//!
//! Those are the two backends, and a build has exactly one of them —
//! chosen by target, not by a flag (see *Features* below).
//!
//! # The contract: this is `idna::domain_to_ascii_cow(_, AsciiDenyList::URL)`
//!
//! Not "UTS 46" in the abstract — that phrase is not precise enough to
//! implement against, which is the whole trap this crate is built around
//! (below). The exact behaviour reproduced is the one `http-ng-proto`
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
//! Two backends, and they are not the same shape — because the two
//! platforms differ in the one thing that matters, whether there is a
//! stable symbol to link against:
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
//! - **macOS — not attempted, and neither of the two obvious reasons is
//!   the real one.** It is *not* that macOS has no public API: this
//!   crate's first draft said so, following `docs/v02-design.md`, and it
//!   is wrong. Nor is it a second helping of the `IdnToAscii` trap:
//!   `swift-foundation`'s `URLParser+ICU.swift` opens its handle with
//!   `UIDNA_CHECK_BIDI | UIDNA_CHECK_CONTEXTJ |
//!   UIDNA_NONTRANSITIONAL_TO_UNICODE | UIDNA_NONTRANSITIONAL_TO_ASCII`,
//!   which is [`OPTIONS`] exactly, so Foundation agrees with us on
//!   `straße.de`. The reason is cost and ergonomics, and it is worth
//!   naming precisely because it is a decision that could be revisited:
//!   the conversion is reachable only as a **side effect of parsing a
//!   whole URL**, and that costs four things this crate needs — a failed
//!   IDNA returns `nil` for the entire URL rather than for the host; the
//!   `UIDNAInfo.errors` word is consumed and discarded, so there is no
//!   typed reason; `URL.host(percentEncoded: true)` returns the *less*
//!   encoded `m%C3%BCnchen.de` while the plain getter returns the
//!   A-label, which is a wrong-origin bug waiting rather than a compile
//!   error; and the RFC 3986 parser that does IDNA at all is gated on the
//!   SDK the binary was **linked against**, which a `cargo`-built binary
//!   may not satisfy — in which case Foundation silently percent-encodes
//!   the host instead. Reaching it from Rust means `objc2-foundation` and
//!   an Objective-C message send per domain. macOS therefore takes the
//!   bundled path.
//!   (`libicucore.dylib` — Apple's own ICU, which would have none of
//!   these problems — is genuinely out of reach: no headers ship for it,
//!   Apple documents its symbols as not for third-party linking, and
//!   since Big Sur it is not on disk to `dlopen` at all.)
//! - **wasm** — no dynamic loader and no system ICU, so the bundled path
//!   today. But **the browser is a platform IDN implementation**, and on
//!   the evidence it is the best one of the lot: `new URL(…).hostname` in
//!   Chrome 151 agrees with `idna` on all twelve probes tried, including
//!   `straße.de` → `xn--strae-oqa.de`, and it independently pins three
//!   rows of the contract table above (`-lead.de` passes, so
//!   _CheckHyphens=false_; `a..b` passes, so _VerifyDnsLength=false_;
//!   `١٢٣.com` throws, so _CheckBidi=true_). `web-sys` is already in
//!   `http-ng-fetch`'s graph, so it costs no new crate — and it needs no
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
//! | `system-icu` | the platform's ICU explicitly, on a target that has one; a compile error naming the target on one that does not |
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
//!   both answers on all 32 rows. A divergence is a red build on the
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
//! acceptance probe — at which point the probe becomes a version floor
//! and this paragraph can be deleted.

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

#[cfg(not(any(idna_backend, icu_backend)))]
compile_error!(
    "http-ng-idn needs at least one backend: `bundled` (the `idna` crate, the default) or \
     `system-icu` (the platform's own ICU), or both. With neither, `domain_to_ascii` could only \
     ever return `IdnError::NoImplementation`, which is a build nobody wants by accident."
);

/// What went wrong turning a domain into its A-label form.
///
/// Two variants because `http-ng-proto` distinguishes two things a caller
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
/// [`IGNORED_ERRORS`] is a separate decision and not a footnote: Apple
/// allows none of them, so its `URL` rejects `a..b` and `ab--cd.com`
/// where `idna` — and therefore this crate — accepts both.
pub const OPTIONS: u32 = UIDNA_NONTRANSITIONAL_TO_ASCII
    | UIDNA_NONTRANSITIONAL_TO_UNICODE
    | UIDNA_CHECK_BIDI
    | UIDNA_CHECK_CONTEXTJ;

// ── The `UIDNAInfo.errors` bits this crate ignores ──────────────────────
//
// ICU has no options for CheckHyphens or VerifyDnsLength: it always runs
// both checks and reports them as bits. `idna`, called the way
// `http-ng-proto` calls it, has both OFF. So agreement is not a matter of
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
/// `http-ng-dns-system`: this is a *decision about what the answer means*,
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

/// Converts `domain` to its ASCII (A-label) form.
///
/// ASCII in, ASCII out — but not unchanged: `EXAMPLE.COM` comes back
/// lower-cased, exactly as `idna::domain_to_ascii_cow` returns it. (The
/// caller that cares, `http-ng-proto::uri`, never sends an all-ASCII host
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
    #[cfg(idna_backend)]
    return Backend::Bundled;
    #[cfg(not(idna_backend))]
    Backend::None
}

/// The bundled path: the exact call `http-ng-proto` makes today, so
/// enabling `bundled` alone changes nothing at all.
#[cfg(idna_backend)]
fn bundled_to_ascii(domain: &str) -> Result<Cow<'_, str>, IdnError> {
    idna::domain_to_ascii_cow(domain.as_bytes(), idna::AsciiDenyList::URL).map_err(|_| {
        IdnError::NotAnIdn {
            domain: domain.to_owned(),
        }
    })
}

/// The platform path: ICU, then the deny list on what came back.
///
/// **On the output only, and that was measured rather than argued.** The
/// first draft scanned the input as well, on the reasoning that a denied
/// ASCII character can hide inside an already-A-label input — `xn--%-0fa`
/// decodes to `%ä`, and ICU with STD3 rules off (as it must be here)
/// considers `%` valid. Deleting that scan and running the corpus killed
/// nothing: every row still passed. The reason is a property of punycode
/// rather than an accident of the corpus — **basic code points are copied
/// into the A-label literally**, so a denied ASCII byte in an `xn--`
/// label is still a denied ASCII byte in ICU's output, and the output
/// scan sees it. Nothing in UTS 46 removes an ASCII character either: the
/// only ASCII mapping is upper-case to lower-case, and no code point in
/// the ignored set is ASCII. So the input scan could not catch anything
/// the output scan does not, and it is gone.
///
/// The output scan, by contrast, is load-bearing twice over: it catches
/// the `xn--%…` case above **and** the opposite one, where UTS 46 mapping
/// *produces* a denied character from one that is not — U+FF0F FULLWIDTH
/// SOLIDUS maps to `/`, U+FF03 to `#`, and neither is an ASCII byte on
/// the way in. Removing it kills two corpus rows.
#[cfg(icu_backend)]
fn system_icu_to_ascii(domain: &str) -> Result<Cow<'_, str>, IdnError> {
    let reject = || IdnError::NotAnIdn {
        domain: domain.to_owned(),
    };
    let ascii = icu::name_to_ascii(domain).ok_or_else(reject)?;
    if ascii.bytes().any(is_forbidden_domain_byte) {
        return Err(reject());
    }
    Ok(Cow::Owned(ascii))
}

/// Both implementations, reachable side by side, for the differential
/// corpus in `tests/differential.rs` — which cannot use
/// [`domain_to_ascii`] for this, since that deliberately exposes only one.
///
/// Not a public API: no stability promise, and nothing in the client calls
/// it.
#[doc(hidden)]
pub mod testing {
    use super::{Backend, IdnError};
    use std::borrow::Cow;

    /// The bundled implementation, whether or not it is the one
    /// [`super::backend`] selected.
    ///
    /// # Errors
    /// As [`super::domain_to_ascii`].
    #[cfg(idna_backend)]
    pub fn bundled(domain: &str) -> Result<Cow<'_, str>, IdnError> {
        super::bundled_to_ascii(domain)
    }

    /// The platform implementation, or `None` if no system ICU was found
    /// — which is the difference between "the corpus checked the platform
    /// column" and "the corpus silently checked nothing".
    ///
    /// # Errors
    /// As [`super::domain_to_ascii`].
    #[cfg(icu_backend)]
    pub fn system_icu(domain: &str) -> Option<Result<Cow<'_, str>, IdnError>> {
        super::icu::library()?;
        Some(super::system_icu_to_ascii(domain))
    }

    /// The file name of the library the platform path is using, for a
    /// report that says *which* ICU answered rather than "an ICU".
    #[cfg(icu_backend)]
    #[must_use]
    pub fn system_icu_library() -> Option<&'static str> {
        super::icu::library_name()
    }

    /// Re-exported so a test can assert the selected backend without
    /// duplicating the feature logic.
    #[must_use]
    pub fn selected() -> Backend {
        super::backend()
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

    #[cfg(idna_backend)]
    #[test]
    fn the_bundled_path_is_the_call_http_ng_proto_makes_today() {
        assert_eq!(bundled_to_ascii("münchen.de").unwrap(), "xn--mnchen-3ya.de");
        assert_eq!(bundled_to_ascii("straße.de").unwrap(), "xn--strae-oqa.de");
        assert_eq!(bundled_to_ascii("EXAMPLE.COM").unwrap(), "example.com");
        assert!(matches!(
            bundled_to_ascii("a<b.com"),
            Err(IdnError::NotAnIdn { .. })
        ));
    }
}
