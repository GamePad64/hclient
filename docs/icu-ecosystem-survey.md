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

# ICU ecosystem survey: what exists, what it costs, what it cannot do

Input for the `idn-platform-crate` task. It answers three questions asked of
it — the `rust_icu` family's real build cost, whether macOS has a public IDN
API, and whether ICU4X can be reduced to UTS-46 alone — and reports one
finding that **contradicts a platform note in
`.superpowers/sdd/v02/idn-platform-brief.md`**, with the evidence for the
contradiction, because acting on the note as written would add machinery the
platform does not require.

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

## Q2 — macOS: no public API. Use the bundled path.

**Answer: there is no public macOS API that converts a Unicode domain to an
A-label.** The brief may drop the "unverified" mark on the conclusion, but
should keep it on one residual behaviour, below.

**`libicucore.dylib` is out.** Apple ships it but publishes no headers for
it; it is private API, and linking it has produced App Store rejections
(documented across the j2objc discussion and tilemill#561). Since Big Sur
the system dylibs are not on disk at all — they live in the dyld shared
cache — so there is no path to `dlopen` either.

**CFNetwork/CFURL expose no conversion entry point.** The Swift Forums
thread on exactly this question ("Is there any standard method available to
transform these IDN to Punycode to be used with `URL()`?") is answered
"Not in the standard library", with the surrounding discussion noting that
`URL` and `URLComponents` did not support IDN and that third-party libraries
were required.

**The one real nuance — and why it still is not usable.** For apps linked
against iOS 17 / macOS 14 and later, `URL` moved to RFC 3986 parsing, and
Apple's documentation states that URL *"automatically percent- and
IDNA-encodes invalid characters to help create a valid URL"*. So IDNA
processing does exist inside Foundation. It is nonetheless not a primitive
this crate can use:

- it is a **side effect of URL construction**, not a named conversion
  function — there is no documented `toASCII`;
- there is **no control over UTS-46 flags**, so the transitional /
  non-transitional distinction that this entire task turns on is not
  selectable, and not documented either way;
- it **has no error channel** — it "helps create a valid URL", silently,
  where this crate must return a typed error;
- **it moved between OS versions** (iOS 16 → 17 changed URL parsing
  outright), so it is a behavioural moving target across the support range;
- from Rust it is reachable only by round-tripping a whole URL through
  Foundation via `objc2`, a far heavier dependency than the single C call
  the Windows path needs.

**unverified:** what Foundation actually returns for `straße.de` — i.e.
whether it agrees with `idna` at all. Settled by a probe on a
`macos-latest` runner setting `URLComponents.host = "straße.de"` and reading
back `encodedHost` / `url?.absoluteString`. Worth running once for the
record, but it does not change the verdict: even if it agreed, the absence
of flag control and of an error channel disqualifies it.

**Verdict: macOS uses the bundled `idna`. Say so plainly and move on.**

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

### The genuinely cheap lever: `idna_adapter` backend pinning

`idna_adapter` is a supported backend seam, selected by **pinning its
version**, not by a feature — from its README: 1.2.x = ICU4X 2.2,
1.2.0 = ICU4X 1.x, 1.1.0 = unicode-rs, 1.0.0 = stub. One command, no code
change, semantics preserved (verified identical output above):

```
$ cargo update -p idna_adapter --precise 1.1.0
```

Measured trade, ICU4X → unicode-rs: **deps 30 → 10**, and all four
proc-macro crates plus both `syn` versions leave the graph — against
**+126 KiB of binary**. Source size does not improve
(`idna_mapping` 1.5M + `unicode-normalization` 772K + `unicode-bidi` 328K +
`unicode-joining-type` 128K ≈ 2.73 MB, slightly worse than ICU4X's 2.35 MB).

This matters because CLAUDE.md states the pain as **"36 crates with `idn`,
10 without"** — a *crate-count* and compile-time complaint. Backend pinning
addresses that directly, with zero `unsafe`, zero FFI and zero new crate.
It is worth putting in front of the owner as an alternative to the whole
task, not because the platform crate is wrong, but because the two are
solving different halves of the stated problem and only one of them is free.

**unverified:** whether `idna_adapter` 1.1.0 satisfies this workspace's MSRV
(latest stable, 1.97) and whether pinning survives `cargo update` in CI —
settled by applying the pin in the real workspace and running
`cargo nextest run --workspace --all-features`.

---

## Verdicts

| candidate | verdict | one-line reason |
|---|---|---|
| `rust_icu_uidna` | **rejected** | does not exist on crates.io |
| `rust_icu_sys` / family | **rejected** | no `uidna` (measured: 0 symbols); build script host-couples, breaking musl and Windows cross-builds |
| `icu4c-sys`, `icu_sys`, `idna-sys`, `libidn2-sys`, `idn2`, `libicu` | **rejected** | none exist in the registry |
| `windows-sys` `Win32_Globalization` | **fits — use it** | full `uidna_*` surface + all `UIDNA_*` constants; raw-dylib, unsuffixed, no SDK needed |
| `icu_capi` / ICU4X FFI | **rejected** | ICU4X is a Rust reimplementation with bundled data; `icu_capi` exposes it *to* C — wrong direction |
| `icu_normalizer::uts46` alone | **partial** | separable and avoids the 1.9 MB crate (84 KiB), but mapping only — no punycode, no validation |
| `idna_adapter` pinning | **fits, and is free** | deps 30 → 10 by one `cargo update`; +126 KiB binary; no `unsafe` |
| macOS Foundation / CFURL | **rejected** | no named conversion API, no UTS-46 flag control, no error channel |
| `libicucore.dylib` | **rejected** | private, no headers, App Store rejections, not on disk since Big Sur |
| libidn2 | **rejected** | no Rust binding on crates.io; IDNA2008-by-default (UTS-46 only via `IDN2_NONTRANSITIONAL`); no presence guarantee |
| `unic-idna` | **rejected** | last release 2019-03-03; vendored tables, same class as `idna` |

## Open items

1. **`CoInitializeEx` before `uidna_openUTS46`** — unverified; needs a
   `windows-latest` probe.
2. **`raw-dylib` load against `icuuc.dll` on a 1703-era image** — unverified;
   same runner, or accept as a documented floor.
3. **Foundation's actual output for `straße.de`** — unverified; a
   `macos-latest` probe. Does not change the verdict.
4. **`idna_adapter` 1.1.0 against this workspace's MSRV and CI** — unverified;
   apply the pin and run the suite.
5. **`rust_icu_sys` via `icu_version_in_env`** — unverified; would only
   matter if the family were reconsidered, which the missing `uidna` already
   rules out.
