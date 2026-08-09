> **Re-measured against this workspace before merging.** The
> `idna_adapter` pin below was checked here against `http-ng-proto`
> specifically, and the numbers differ from the survey's because the survey
> measured a standalone probe crate:
>
> - `cargo tree -p http-ng-proto -e normal`: **35 crates -> 20** with
>   `idna_adapter` pinned to 1.1.0 (not 30 -> 10).
> - The five ICU4X derive macros do go (`yoke-derive`, `zerofrom-derive`,
>   `zerovec-derive`, `displaydoc`, `synstructure`), but **`syn`, `quote`
>   and `proc-macro2` stay either way** — `thiserror-impl` needs them, and
>   `thiserror` is approved for use throughout this workspace. "Zero
>   proc-macros" does not hold here.
> - `cargo nextest run -p http-ng-proto --all-features`: 130/130 green on
>   the pinned graph, so the URI differential corpus agrees on both
>   backends.
>
> Everything else below is the survey's own measurement and is recorded as
> received.

> **DECIDED — the `idna_adapter` pin is rejected. Do not re-propose it.**
>
> Owner's call, 2026-08-08: the unicode-rs backend is the stale one.
> `idna_adapter` 1.1.0 is the older line, and the backend behind it is what
> `idna` used *before* ICU4X — so the fifteen crates it removes are bought
> with Unicode tables that lag. That is not a neutral trade in this path:
> IDN decides **which host we connect to**, and a mapping table that is a
> Unicode version behind is a difference in destination, not in polish. The
> same reasoning is why this project rejected `IdnToAscii` (see the table
> below) — an older standard that answers *almost* the same is the failure
> mode, not the fallback.
>
> The +126 KiB / -15 crates measurement below stands as a measurement. It
> simply is not a trade this project takes.

# ICU ecosystem survey: what exists, what it costs, what it cannot do

Input for the `idn-platform-crate` task. It answers the three questions
asked of it — the `rust_icu` family's real build cost, whether macOS has a
public IDN API, and whether ICU4X can be reduced to UTS-46 alone — plus a
fourth added later by the owner: whether the browser's `URL.hostname` can
serve as the wasm backend.

Two of its findings **contradict notes in
`.superpowers/sdd/v02/idn-platform-brief.md`**, and one contradicts an
earlier revision of this document. All three are recorded with their
evidence rather than quietly corrected, because each was believed on
reasonable grounds and would otherwise be re-derived:

- Windows' ICU exports are **not** version-suffixed (Q1, the Correction
  section) — the brief said they are, and that note drives real machinery.
- The "1.9 MB" motivating this whole task is **vendored source, not
  binary** (Q3); the binary cost is ~147 KiB.
- macOS **does** have a public API and it is **UTS-46 non-transitional**
  (Q2) — this document previously said there was none.

Every "works" below is a command and its output. Every "cannot" is a quoted
error. Anything not executed is marked **unverified** and says what would
settle it.

## Measurement environment

All figures reproduced from this box; they are Linux-host numbers and are
labelled as such where that matters.

```
$ rustc -V
rustc 1.97.1 (8bab26f4f 2026-07-14)
$ pkg-config --modversion icu-uc
78.2
$ command -v icu-config || echo "icu-config: ABSENT from PATH"
icu-config: ABSENT from PATH
```

Binary-size projects use one profile throughout, and differ only in the
dependency under test:

```toml
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

Each reads the domain from stdin, so the conversion cannot be const-folded
away. Only the **deltas against the baseline** are meaningful; the absolute
numbers carry std's backtrace machinery.

---

## Q1 — the `rust_icu` family

### `rust_icu_uidna` does not exist

```
$ cargo info rust_icu_uidna
    Updating crates.io index
error: could not find `rust_icu_uidna` in registry `https://github.com/rust-lang/crates.io-index`

$ cargo search uidna --limit 20
(no output — zero crates on crates.io match "uidna")
```

Neither do `icu4c-sys`, `icu_sys`, `idna-sys`, `libidn2-sys`, `idn2`,
`libicu`, or `rust-icu` (hyphenated); each returns the same
`could not find … in registry`. `cargo search libidn --limit 15` returns
nothing, so **there is no libidn2 binding on crates.io either**.

### `rust_icu_sys` does not expose `uidna` — measured, not read off a table

The crate that does exist binds 26 ICU headers, and `uidna` is not among
them (`build.rs:39-66`, `BINDGEN_SOURCE_MODULES`). That list is also an
allowlist, so regenerating bindings on the fly does not widen it.

Verified against a real ICU 78.2 install, on freshly generated bindings
rather than the checked-in ones:

```
$ grep -c "uidna\|UIDNA" target/debug/build/rust_icu_sys-*/out/lib.rs
0
$ grep -c "unorm2_getNFCInstance_78" target/debug/build/rust_icu_sys-*/out/lib.rs
1
```

Zero `uidna` symbols; the control symbol is present. Using `rust_icu` for
this task means **forking it** to add `uidna` to the module list.

### Build cost: cheaper than the brief assumed on the host, impossible off it

The brief anticipated `pkg-config` **and** `bindgen`. Corrected: 5.7.0 needs
`pkg-config` (not `icu-config`, which this box does not even have) and runs
`bindgen`, requiring `libclang`. It pulls **49 packages** and builds clean
on the host:

```
$ cargo add rust_icu_sys
      Adding rust_icu_sys v5.7.0 to dependencies
             Features: + bindgen + icu_config + renaming + use-bindgen
     Locking 49 packages to latest Rust 1.97.1 compatible versions
$ cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.24s
```

It detected ICU correctly and handles symbol renaming through a generated
`versioned_function!` macro:

```
icu-version: 78.2
icu-has-renaming: true
cargo:rustc-link-search=native=/usr/lib/x86_64-linux-gnu
cargo:rustc-link-lib=dylib=icuuc
```

**Cross-compilation is where it fails, and it fails badly.**

musl:

```
$ cargo build --target x86_64-unknown-linux-musl
/usr/bin/x86_64-linux-gnu-ld.bfd: /usr/lib/x86_64-linux-gnu/libc.a(qsort.o):
  undefined reference to `__gcc_personality_v0'
collect2: error: ld returned 1 exit status
error: could not compile `rusticu` (bin "rusticu") due to 1 previous error
```

This is not ambient toolchain breakage. The identical project **without**
`rust_icu_sys` cross-compiles to musl fine:

```
$ cd base && cargo build --target x86_64-unknown-linux-musl
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.17s
```

The cause is visible in the link line: the build script emits
`cargo:rustc-link-search=native=/usr/lib/x86_64-linux-gnu` — the **host's
glibc** ICU directory — into a musl static-pie link, so `-lc` resolves to
glibc's `libc.a`. The build script detects the *host's* ICU and applies it
to the *target*.

Windows:

```
$ cargo build --target x86_64-pc-windows-msvc
error: failed to run custom build command for `rust_icu_sys v5.7.0`
  .../out/wrapper.h:3:10: fatal error: 'unicode/ubrk.h' file not found
  Error: while autodetecting ICU
```

With no system ICU at all, `icu_config` autodetection is what fails; the
crate offers `icu_version_in_env` + prebuilt `lib_NN.rs` to sidestep
detection, but that path bakes in a **fixed ICU major version** chosen at
build time — **unverified**, and it would be settled by building with
`--no-default-features --features icu_version_in_env,renaming` and
`RUST_ICU_MAJOR_VERSION_NUMBER` set.

**Verdict: rejected.** The API is absent, and the build is host-coupled in a
way that breaks the two targets (musl, Windows) this project cares about.

### What *does* have `uidna`: `windows-sys`

`windows-sys` **0.61.2**, feature `Win32_Globalization`, already declares the
whole surface — `src/Windows/Win32/Globalization/mod.rs:757-766`:

```rust
windows_link::link!("icuuc.dll" "C" fn uidna_openUTS46(options: u32, perrorcode: *mut UErrorCode) -> *mut UIDNA);
windows_link::link!("icuuc.dll" "C" fn uidna_nameToASCII_UTF8(idna: *const UIDNA, name: PCSTR, length: i32, dest: PCSTR, capacity: i32, pinfo: *mut UIDNAInfo, perrorcode: *mut UErrorCode) -> i32);
windows_link::link!("icuuc.dll" "C" fn uidna_close(idna: *mut UIDNA));
```

plus `uidna_nameToASCII`, `uidna_nameToUnicode(UTF8)`, `uidna_labelTo*`, the
`UIDNA` / `UIDNAInfo` types, and every constant the task needs
(`mod.rs:3685-3707`):

```rust
pub const UIDNA_NONTRANSITIONAL_TO_ASCII: i32 = 16i32;    // 0x10
pub const UIDNA_NONTRANSITIONAL_TO_UNICODE: i32 = 32i32;  // 0x20
pub const UIDNA_DEFAULT: i32 = 0i32;
pub const UIDNA_USE_STD3_RULES: i32 = 2i32;
```

plus the full `UIDNA_ERROR_*` set, which is what makes a *typed* error
possible instead of a generic failure.

This does not remove `unsafe` — no crate offers a safe wrapper over system
`uidna` — but it removes the need to *write* the `extern` block: the
declarations are generated from Microsoft's own Win32 metadata. What remains
is the safe wrapper (open → two-pass call → close).

---

## Correction — Windows exports are **not** version-suffixed

The brief's platform notes say:

> ICU has been in Windows 10 since 1703, but the combined `icu.dll` only
> since 1903, and the exported symbols are **version-suffixed**. Resolve at
> runtime and fall back rather than failing to start.

The first two clauses are right. **The third is not**, and it is the one
that drives implementation complexity.

Evidence, two independent sources:

1. The Windows SDK's own `icu.h`, as carried in Microsoft's `win32metadata`
   repository, begins with

   ```c
   #define U_DISABLE_RENAMING 1
   ```

   and `urename.h` — the header that performs ICU's suffix renaming — is
   marked "No supported content". Renaming is off in Windows' ICU.

2. `windows-sys` binds the **unsuffixed** name against `icuuc.dll` through
   `windows-link` 0.2.1, whose macro expands to

   ```rust
   #[link(name = $library, kind = "raw-dylib", modifiers = "+verbatim")]
   ```

   A `raw-dylib` import of a plain `uidna_openUTS46` would fail at load if
   the DLL exported only a suffixed name.

The contrast with Linux is real and measured here, which is likely where the
note came from:

```
$ nm -D --defined-only /usr/lib/x86_64-linux-gnu/libicuuc.so | grep uidna_openUTS46
000000000017d780 T uidna_openUTS46_78
```

Linux ICU **is** suffixed (`_78`, moving with each major release). Windows
ICU is not. So the suffix problem is a reason to reject *Linux* system ICU —
it is not a cost the Windows path has to pay.

Consequences for the crate: no `GetProcAddress` suffix probing, no runtime
resolution machinery, and no Windows SDK or import `.lib` (raw-dylib
synthesises the import — it even cross-compiles from Linux). Targeting
`icuuc.dll` rather than `icu.dll` also keeps the floor at Windows 10 1703
rather than 1903.

Two things I did **not** verify, both needing a Windows runner:

- Whether `uidna_openUTS46` requires `CoInitializeEx` first. Microsoft
  documents this for Win32 apps using `icuuc.dll`/`icuin.dll`, waived on
  1903+ with combined `icu.dll`. **unverified** — settled by calling
  `uidna_openUTS46` on a `windows-latest` runner with and without
  `CoInitializeEx` and comparing `UErrorCode`.
- Whether a `raw-dylib` import of `icuuc.dll` loads on a clean 1703-era
  image. **unverified** — settled by the same runner, or accepted as a
  documented minimum-version claim.

---

## Q2 — macOS: the API exists, and it is UTS-46 non-transitional

> **This section replaces an earlier one that said "no public API". That
> was wrong.** The correction matters more than the original claim, so it
> is recorded rather than quietly overwritten: the question "is there an
> API?" and the question "what does it do?" are separate, and here they
> have different answers. Apple's implementation is open source, so the
> second one is settled from the call site rather than from behaviour
> reports.

### The call site, and the option word

`swift-foundation`,
[`Sources/FoundationInternationalization/URLParser+ICU.swift`](https://github.com/swiftlang/swift-foundation/blob/main/Sources/FoundationInternationalization/URLParser%2BICU.swift)
(© 2024 Apple Inc.) — `struct UIDNAHookICU`:

```swift
private static let idnaTranscoder: UIDNAPointer? = {
    var status = U_ZERO_ERROR
    let options = UInt32(
        UIDNA_CHECK_BIDI                    |
        UIDNA_CHECK_CONTEXTJ                |
        UIDNA_NONTRANSITIONAL_TO_UNICODE    |
        UIDNA_NONTRANSITIONAL_TO_ASCII
    )
    let encoder = uidna_openUTS46(options, &status)
    ...
```

`0x04 | 0x08 | 0x20 | 0x10` = **0x3C** — exactly the flag word this project
requires, plus the two optional conformance checks. Encoding goes through
`uidna_nameToASCII_UTF8`, the same UTF-8 entry point the Windows path uses.

**So Foundation agrees with the `idna` crate: `straße.de` →
`xn--strae-oqa.de`, `faß.de` → `xn--fa-hia.de`.** macOS is *not* a second
`IdnToAscii`. On the to-ASCII path the error policy is strict —
`allowedErrors = 0`, so any `UIDNAInfo.errors` bit rejects.

**unverified** only in that this was read, not run: a `macos-latest` probe
printing `URL(string: "https://straße.de/")?.host` closes it, and it is
cheap enough to be worth doing for the record.

### Versions

- `URLComponents.encodedHost` — the accessor that returns the A-label — is
  declared `@available(macOS 13.0, iOS 16.0, tvOS 16.0, watchOS 9.0, *)`.
- `URL(string:)` IDNA-encoding arrives with the RFC 3986 parser, which Apple
  gates on *"apps linked on or after iOS 17 and aligned OS versions"* — a
  **linked-SDK** gate, not merely an OS-version one.

### Why it is still rejected — four reasons, none of them "it does not exist"

1. **It is a URL parser, not a domain-to-ASCII entry point.** The conversion
   happens while assembling the URL string, and failure takes the whole URL
   with it (`URLParser.swift`):

   ```swift
   } else if let idnaEncoded = IDNAEncodeHost(String(host)),
             validate(host: idnaEncoded, knownIPLiteral: false) {
       finalURLString += idnaEncoded
   } else {
       return nil
   }
   ```

   A host holding `/` or `?` therefore changes where the URL *ends* rather
   than being reported as a bad host.

2. **No error detail.** `UIDNAInfo.errors` is consumed by `shouldAllow` and
   discarded; every path returns `String?`. There is no equivalent of the
   `UIDNA_ERROR_*` reporting the Windows path gets for free.

3. **Behaviour is scheme-dependent.** `shouldPercentEncodeHost` consults a
   list — `tel`, `telprompt`, `callto`, `facetime*`, `imap`, `pop`,
   `addressbook`, `contact`, … — whose hosts are **percent-encoded instead
   of IDNA-encoded**. `http`/`https` do get IDNA, so this does not bite us,
   but it confirms the primitive is "parse a URL", not "convert a domain".

4. **The accessors are treacherous.** For `https://münchen.de/`:

   | accessor | returns |
   |---|---|
   | `URL.host` / `host(percentEncoded: false)` | `xn--mnchen-3ya.de` (A-label) |
   | `URL.host(percentEncoded: true)` | `m%C3%BCnchen.de` — IDNA-**decoded**, then percent-encoded |
   | `URLComponents.host` | `münchen.de` — Unicode |
   | `URLComponents.encodedHost` | `xn--mnchen-3ya.de` (A-label) |

   `percentEncoded: true` returning the *less* encoded host is the kind of
   inversion that produces a wrong-origin bug rather than a compile error.

Also recorded: with no ICU hook available Foundation does not fail — it
silently **percent-encodes** the host instead
(`guard _uidnaHook() != nil else { return true }`) — and
`maxHostBufferLength = 2048` caps input, returning `nil` beyond it.

### Reachability from Rust

`UIDNAHook` is a `package protocol` and `UIDNAHookICU`'s members are
`private`. **There is no public C or ObjC entry point to the conversion
itself.**

- **`core-foundation` 0.10.1** — the cheap pure-C route, and probably a dead
  end: Foundation's own CF path calls `_CFURLCopyHostName`, an
  **underscore-prefixed private SPI**, on the legacy `_BridgedURL` class, not
  through `UIDNAHookICU`. **unverified** — settled by `CFURLCreateWithBytes`
  + `CFURLCopyHostName` on `https://münchen.de/`, checking for `xn--`.
- **`objc2-foundation` 0.3.2** — the route that certainly works, via
  `NSURL URLWithString:` then `-host`. Costs `objc2` + `objc2-foundation`,
  Objective-C message sends, and a whole-URL round trip per domain.

**Verdict: macOS takes the bundled `idna`** — on cost and ergonomics, with a
documented fallback, not because nothing exists. `libicucore.dylib` remains
separately out of reach: no headers ship for it, Apple documents its symbols
as not for third-party linking, and since Big Sur the system dylibs are not
on disk at all. If platform IDN on macOS is ever wanted, the
`objc2-foundation` route is viable and its semantics are now known to match.

---

## Q3 — ICU4X: yes, UTS-46 alone is separable. But the premise needs correcting first.

### The 1.9 MB never reaches the binary

The brief motivates the whole task with *"roughly 1.9 MB of vendored source,
almost all of it Unicode tables… on a part with 256–512 KB of flash that is
the entire budget."* The 1.9 MB is real, and it is one crate:

```
$ du -sh .../icu_properties_data-2.2.0 .../icu_normalizer_data-2.2.0
1.9M	icu_properties_data-2.2.0
452K	icu_normalizer_data-2.2.0
```

But that is **vendored source on disk, not flash**. ICU4X stores these as
compressed tries (`zerotrie`/`zerovec`) and the linker keeps only referenced
markers. Measured:

| build | binary | Δ vs base | `.rodata` | Δ `.rodata` | deps |
|---|---|---|---|---|---|
| baseline, no IDN | 297,528 B | — | 22,848 B | — | 0 |
| `idna` 1.1.0, ICU4X backend | 448,504 B | **+147.4 KiB** | 128,720 B | **+103.4 KiB** | 30 |
| `idna` 1.1.0, unicode-rs backend | 577,336 B | +273.2 KiB | 257,872 B | +229.5 KiB | 10 |
| `icu_normalizer` UTS-46 mapper only | 383,520 B | +84.0 KiB | 93,200 B | +68.7 KiB | 28 |

**The whole IDN feature costs ~147 KiB of binary, of which ~103 KiB is
Unicode data — not 1.9 MB.** ~2.35 MB of vendored source compiles down to
~103 KiB of `.rodata`. This does not make the feature free on a 256 KB part,
but it is a different decision than the one the brief frames, and it is the
single most decision-relevant number in this survey.

All three IDN builds were checked to actually work, and to agree with the
brief's `idna` column:

```
$ echo "straße.de" | ./withidna     # ICU4X backend
xn--strae-oqa.de
$ echo "faß.de" | ./withidna
xn--fa-hia.de
$ echo "straße.de" | ./withidna     # unicode-rs backend, pinned 1.1.0
xn--strae-oqa.de
```

### Can UTS-46 be taken alone? Yes — and it is not enough

`idna_adapter` 1.2.x uses exactly four ICU4X inputs
(`idna_adapter-1.2.1/src/lib.rs:25-28`):

```rust
use icu_normalizer::properties::CanonicalCombiningClassMapBorrowed;
use icu_normalizer::uts46::Uts46MapperBorrowed;
use icu_properties::props::GeneralCategory;
use icu_properties::CodePointMapDataBorrowed;
```

So **ICU4X does ship a UTS-46 mapper** (`icu_normalizer::uts46`), despite the
ICU4X charter's *"ICU4X will not include Spoof Checker (UTS 39) or IDNA
(UTS 46)"* — the charter means the IDNA *protocol*, not the UTS-46 mapping
table.

That mapper is separable, and it does **not** pull the 1.9 MB crate —
`icu_properties` is an optional feature of `icu_normalizer`:

```
$ cd uts46only && cargo tree -e normal --prefix none | sort -u | grep icu
icu_collections v2.2.0
icu_locale_core v2.2.0
icu_normalizer v2.2.0
icu_normalizer_data v2.2.0
icu_provider v2.2.0
```

`icu_properties_data` is absent. Cost: **84 KiB binary, 452 KB source**,
versus 147.4 KiB / ~2.35 MB for the full thing.

**But what you get for that 84 KiB is mapping only.** Measured:

```
$ echo "straße.de" | ./uts46only
straße.de
$ echo "MÜNCHEN.de" | ./uts46only
münchen.de
```

Case folding and NFC, non-transitional (`ß` preserved — correct) — and **no
punycode, no length checks, no bidi rule, no CONTEXTJ, no disallowed-category
validation**. Those are what the 1.9 MB `icu_properties_data` is for
(`GeneralCategory`, `JoiningType`, `BidiClass`), and they cost the difference:
~63 KiB of binary.

So the honest answer to "can we assemble UTS-46 from ICU4X and skip the
tables": **yes for the mapping, no for a usable `domain_to_ascii`.** Building
the remainder in-house means hand-writing exactly the IDNA validation rules
whose subtlety this brief already documents — for ~63 KiB. That is a bad
trade for an HTTP client, where those checks are origin-determining.

### `idna_adapter` backend pinning — measured, and **rejected**; see the note at the top

Recorded as a measurement only. The decision is at the head of this
document: **the pin is rejected, do not re-propose it.** What follows is
what it would have bought, so that the rejection rests on numbers.

`idna_adapter` is a supported backend seam, selected by **pinning its
version**, not by a feature — from its README: 1.2.x = ICU4X 2.2,
1.2.0 = ICU4X 1.x, 1.1.0 = unicode-rs, 1.0.0 = stub. One command, no code
change, and byte-identical output on the corpus above:

```
$ cargo update -p idna_adapter --precise 1.1.0
```

Measured trade, ICU4X → unicode-rs: **+126 KiB of binary** against a smaller
graph. Two corrections to the first draft of this section, both from the
re-measurement at the top of this document:

- the crate delta against **`http-ng-proto`** is **35 → 20**, not the
  30 → 10 this survey's standalone probe crate showed;
- **`syn`, `quote` and `proc-macro2` stay either way** — `thiserror-impl`
  needs them. Five ICU4X derive macros do leave (`yoke-derive`,
  `zerofrom-derive`, `zerovec-derive`, `displaydoc`, `synstructure`), but
  "zero proc-macros" was wrong and is withdrawn.

Source size does not improve either (`idna_mapping` 1.5M +
`unicode-normalization` 772K + `unicode-bidi` 328K + `unicode-joining-type`
128K ≈ 2.73 MB, slightly worse than ICU4X's 2.35 MB).

There is also a structural limit worth keeping even now the trade is
refused: **the pin lives in the top-level `Cargo.lock`, so a library cannot
express it.** `http-ng` could document it, but only the final binary's
author could choose it — which is why `idna_adapter` is a version-pin seam
rather than a feature (Cargo has no `global-features`). So it was never a
substitute for the platform crate, independently of the tables being stale.

---

## Q4 — the browser: `URL.hostname` works, and it is the right flavour

Not one of the three questions originally asked; added after the owner
proposed it. Measured in **Chrome 151 on Linux via Playwright**, against the
Rust side running this project's exact contract
(`Uts46::new().to_ascii(_, AsciiDenyList::URL, Hyphens::Allow,
DnsLength::Ignore)`).

### On IDN proper, they agree — twelve rows, no exceptions

| input | Chrome 151 `.hostname` | Rust `idna` | |
|---|---|---|---|
| `straße.de` | `xn--strae-oqa.de` | `xn--strae-oqa.de` | agree |
| `faß.de` | `xn--fa-hia.de` | `xn--fa-hia.de` | agree |
| `münchen.de` | `xn--mnchen-3ya.de` | `xn--mnchen-3ya.de` | agree |
| `MÜNCHEN.de` | `xn--mnchen-3ya.de` | `xn--mnchen-3ya.de` | agree |
| `xn--mnchen-3ya.de` | passthrough | passthrough | agree |
| `日本.jp` | `xn--wgv71a.jp` | `xn--wgv71a.jp` | agree |
| `☃.net` | `xn--n3h.net` | `xn--n3h.net` | agree |
| `example.com` | `example.com` | `example.com` | agree |
| `a..b` | `a..b` | `a..b` | agree |
| `-lead.de` | `-lead.de` | `-lead.de` | agree |
| `٠.com` | `TypeError` | error | agree |
| `١٢٣.com` | `TypeError` | error | agree |

The first two rows settle the flavour: **non-transitional**. The rest pin
individual UTS 46 parameters — `-lead.de` surviving means
`CheckHyphens = false`, `a..b` surviving means `VerifyDnsLength = false`,
the Arabic-digit rows throwing means `CheckBidi = true`. That is the WHATWG
*domain to ASCII*, which is what `AsciiDenyList::URL` reproduces.

### But it is a URL parser, and two of its failures are silent

| input | Chrome `.hostname` | Rust `idna` | |
|---|---|---|---|
| `ex/ample.com` | **`ex`** | error | **silent truncation** |
| `ex@ample.com` | **`ample.com`** | error | **silent origin change** |
| `ex?ample.com` | `ex` | error | silent truncation |
| `ex#ample.com` | `ex` | error | silent truncation |
| `ex ample.com` | `ex%20ample.com` | error | percent-encoded, not rejected |
| `ex%61mple.com` | `example.com` | error | percent-**decoded** before IDNA |
| `xn--a.de` | `xn--a.de` | error | invalid punycode accepted |
| `xn--.de` | `xn--.de` | error | invalid punycode accepted |
| `""` | `TypeError` | `""` (ok) | diverges the *other* way |

`ex@ample.com` is the one that matters: the actual host is `ample.com` and
`ex` became userinfo. Handing an unvalidated string to `new URL()` and
trusting `.hostname` is a wrong-origin bug generator.

**It is fixable with code this crate already has.** Every dangerous row
turns on a byte in the WHATWG forbidden-domain set — `/ ? # @ : % \ space`
and the C0 controls — which is exactly what `is_forbidden_domain_byte` /
`AsciiDenyList::URL` rejects. Run that check **before** `new URL()` and the
family disappears; reject empty input and the `""` row goes too.

That leaves one genuine divergence: invalid punycode labels — Chrome
accepts, `idna` rejects. That is UTS 46's `IgnoreInvalidPunycode`, which
`idna` fixes at `false` and the browser evidently treats as true. It needs
a corpus row and a decision, not a workaround.

#### Update: it was called non-fixable, it was measured on Apple, and it is fixed

This whole family — the invalid punycode row, the `""` row, and one this
section did not predict, upper-case ASCII surviving unchanged — turned out
not to be about browsers at all. It is what **any URL parser** does, and it
is what `macos-latest` measured on Apple's Foundation the first time
`http-ng-idn`'s corpus ran there: three rows of 32, `EXAMPLE.COM`, `""` and
`xn--zzzz.test`, exactly the shapes above.

The cause is one sentence, and `swiftlang/swift-foundation` states it in
code: the IDNA hook runs **only when the host is not ASCII**.
`URLParser.swift` copies a host that passes RFC 3986 `reg_name` validation
into the URL verbatim (`finalURLString += host`), so an ASCII host is never
lower-cased, never has its `xn--` labels decoded, and never gets an error
word. Chrome's `new URL()` behaves the same way for the same reason.

`crates/http-ng-idn/src/policy.rs` closes it, for every backend rather than
per platform: an all-ASCII name never reaches the platform at all, ACE
labels are decoded there (RFC 3492 needs no Unicode data), and the answer
is the platform's only for the labels that actually need Unicode. **The
decision on `IgnoreInvalidPunycode` is `false`, i.e. `idna`'s**, and the
reason is in `policy.rs`: an A-label this crate could not decode is a host
it could not check, and accepting it would mean a request going to a name
nobody validated — while refusing it costs a caller who really does own
`xn--a.de` nothing but spelling the name some other way. It would also make
the same program resolve differently on macOS and on Windows, which is the
defect class the whole crate exists to prevent.

So a browser backend, when it is built, inherits the fix rather than
needing it: it would go through the same `to_ascii_over`, and `new
URL()` would only ever be asked about a host with non-ASCII in it.

### Cost, and what is unknown

`web-sys` **0.3.103 is already a dependency of `http-ng-fetch`**, and `Url`
is one more feature on that existing entry — no new crate. On
`wasm32-unknown-unknown` this needs **no `unsafe` at all**, which is a
materially better position than the Windows path.

**unverified: Firefox and Safari.** Only Chrome 151 was measured. The WHATWG
spec is prescriptive so agreement is likely, but this project already
records a Chrome/Safari split in the fetch `Capabilities` model, so it must
not be assumed. Settled by the same probe under the existing `browser` CI
job (`wasm-pack test --headless --chrome|--firefox`).

### The reverse direction

`punycode` 0.4.1 has **zero dependencies** (`cargo tree` shows the crate
alone), so it brings no Unicode tables — punycode is a pure RFC 3492
algorithm with nothing to tabulate. Two caveats: last release
**2019-05-25**, effectively unmaintained (though the RFC is frozen and it
has 4.6M downloads); and **decoding punycode is not IDNA `toUnicode`** — it
skips the mapping and validation steps, so it is a display convenience, not
the inverse of `domain_to_ascii`.

More to the point: **this crate's stated surface is one-directional** — a
Unicode domain in, an A-label out — and `http-ng-proto`'s
`UriError::NonAsciiHost` path does not need the reverse. Confirm a caller
exists before adding the dependency.

## Verdicts

| candidate | verdict | one-line reason |
|---|---|---|
| `rust_icu_uidna` | **rejected** | does not exist on crates.io |
| `rust_icu_sys` / family | **rejected** | no `uidna` (measured: 0 symbols); build script host-couples, breaking musl and Windows cross-builds |
| `icu4c-sys`, `icu_sys`, `idna-sys`, `libidn2-sys`, `idn2`, `libicu` | **rejected** | none exist in the registry |
| `windows-sys` `Win32_Globalization` | **fits — use it** | full `uidna_*` surface + all `UIDNA_*` constants; raw-dylib, unsuffixed, no SDK needed |
| `icu_capi` / ICU4X FFI | **rejected** | ICU4X is a Rust reimplementation with bundled data; `icu_capi` exposes it *to* C — wrong direction |
| `icu_normalizer::uts46` alone | **partial** | separable and avoids the 1.9 MB crate (84 KiB), but mapping only — no punycode, no validation |
| `idna_adapter` pinning | **rejected** (owner, 2026-08-08) | 35 → 20 crates for +126 KiB — but the unicode-rs tables lag, and a library cannot express a top-level pin anyway |
| browser `URL.hostname` via `web_sys` | **fits — use it on wasm** | measured non-transitional, agrees with `idna` on all 12 IDN rows; `web-sys` already in `http-ng-fetch`; **no `unsafe`** — pre-filter with `AsciiDenyList::URL` first |
| macOS Foundation `URL`/`URLComponents` | **rejected on ergonomics** | genuinely UTS-46 non-transitional (`0x3C`, read from Apple's source) — but URL-parser-only, discards `UIDNAInfo.errors`, linked-SDK gated |
| `libicucore.dylib` | **rejected** | private, no headers, App Store rejections, not on disk since Big Sur |
| libidn2 | **rejected** | no Rust binding on crates.io; IDNA2008-by-default (UTS-46 only via `IDN2_NONTRANSITIONAL`); no presence guarantee |
| `unic-idna` | **rejected** | last release 2019-03-03; vendored tables, same class as `idna` |
| `punycode` 0.4.1 | **fits if a caller exists** | zero dependencies, no Unicode tables — but unmaintained since 2019, and not the inverse of `domain_to_ascii` |

## Open items

Ordered by value. Items 1-2 need a Windows runner and are the ones that
gate `http-ng-idn`'s Windows path; they should land as a probe the crate's
own CI job can carry, not as a one-off run.

1. **`CoInitializeEx` before `uidna_openUTS46`** — unverified. Microsoft
   documents the requirement for Win32 apps using `icuuc.dll`/`icuin.dll`
   and waives it on 1903+ with combined `icu.dll`, which is exactly the
   1703..1902 window the crate's resolver falls back into.
2. **`raw-dylib` load against `icuuc.dll` on a 1703-era image** — unverified;
   same runner, or accept as a documented floor.
3. **Firefox and Safari agreement with the Chrome rows in Q4** — unverified;
   the existing `browser` CI job settles it.
4. **Foundation's actual output for `straße.de`** — unverified by execution
   only; the flag word is read from Apple's source and is unambiguous. A
   `macos-latest` probe closes it cheaply.
5. **Whether a *Rust* binary on macOS gets the RFC 3986 parser at all** —
   unverified. Apple gates the behaviour on "apps linked on or after
   iOS 17"; a Rust binary reporting an older linked SDK would silently get
   the old parser. Only matters if the `objc2-foundation` route is ever
   taken.
6. **Whether public `CFURLCopyHostName` performs the conversion** —
   unverified; decides whether the cheap `core-foundation` route exists at
   all, or only the `objc2-foundation` one.
7. **`rust_icu_sys` via `icu_version_in_env`** — unverified; would only
   matter if the family were reconsidered, which the missing `uidna` already
   rules out.

Removed: *"`idna_adapter` 1.1.0 against this workspace's MSRV"*. Moot on
both counts — the pin is rejected, and the MSRV policy is now "latest
stable" with no pinned version and no `msrv` CI job.
