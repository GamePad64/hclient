# hclient-idn

A drop-in replacement for `idna` that takes UTS 46 from whatever the
platform already carries, so the Unicode tables stay out of your binary.

**It is a standalone crate.** It depends on nothing from `hclient` — no
runtime, no transport, no HTTP types at all — and it is versioned and
released on its own. Use it in anything that has to turn a Unicode domain
into its A-label form.

```toml
[dependencies]
hclient-idn = "0.1"
```

```rust
assert_eq!(hclient_idn::domain_to_ascii("münchen.de")?, "xn--mnchen-3ya.de");
assert_eq!(hclient_idn::domain_to_unicode("xn--mnchen-3ya.de")?, "münchen.de");
```

Two functions and an error type. That is the whole surface.

## What it saves

The `idna` crate compiles the Unicode tables into your binary. Measured on
one program converting one name — `opt-level = "z"`, fat LTO,
`panic = "abort"`, stripped:

| target | this crate | with `idna` | saved |
|---|---|---|---|
| `aarch64-linux-android` | 304.5 KiB | 443.5 KiB | **139.0 KiB — 31%** |
| `x86_64-linux-android` | 334.9 KiB | 478.3 KiB | **143.3 KiB — 30%** |

Android and Windows are the two targets that save; everywhere else this
crate is the `idna` crate under another name.

A third of a small native library, per ABI. If you ship to phones, that is
the reason to use this; if you ship a desktop binary of several megabytes,
it probably is not.

## How it picks

| target | backend |
|---|---|
| Windows | `icuuc.dll`, linked, through `windows-sys` |
| Android | `android.icu.text.IDNA` (ICU4J), over JNI |
| Apple, Linux, other ELF unixes, wasm | the `idna` crate |

There is no system UTS 46 to reach for on Linux or wasm, so those take the
bundled tables and this crate changes nothing for them. Apple is in that
row too: Foundation converts an IDN host only as a side effect of parsing
a URL, so it does not case-fold ASCII and does not validate an ACE label
— close enough to look right and not close enough to be it.

The `idna` feature — off by default — forces the bundled crate on every
target, for a build that would rather carry the tables than call the
platform.

## The answers are the same

That is the point, and it is checked rather than hoped for. A platform
backend is used only after it converts `straße.de` and `faß.de` the way
`idna` does, in **both** directions; one that does not is refused and the
crate returns `IdnError::NoImplementation` rather than a different host.
A differential corpus compares the platform against `idna` row by row on
every push, on Windows; the Android backend has been run on a device,
where thirteen cases agree.

## Licence

MIT or Apache-2.0, at your option.
