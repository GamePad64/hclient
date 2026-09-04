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
```

One function and an error type. That is the whole surface — this crate
converts U to A and nothing else, because that is the only direction an
HTTP client needs: `http::Uri` refuses a non-ASCII authority, and nothing
downstream takes a U-label back.

## What it saves

The `idna` crate compiles the Unicode tables into your binary. Measured on
one program converting one name — `opt-level = "z"`, fat LTO,
`panic = "abort"`, stripped:

| target | this crate | with `idna` | saved |
|---|---|---|---|
| `aarch64-linux-android` | 304.5 KiB | 443.5 KiB | **139.0 KiB — 31%** |
| `x86_64-linux-android` | 334.9 KiB | 478.3 KiB | **143.3 KiB — 30%** |
| `wasm32-unknown-unknown` | 20.7 KiB | 143.0 KiB | **122.3 KiB — 86%** |

The browser row is the largest share because a wasm module has almost
nothing else in it, and there the weight is download time. It is measured
through the whole `wasm-pack` pipeline: the raw `.wasm` carries a section
of descriptors that `wasm-bindgen` consumes and nothing ships.

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
| the browser | `new URL()`, whose host parsing the WHATWG standard defines as UTS 46 |
| Apple, Linux, other ELF unixes, WASI | the `idna` crate |

There is no UTS 46 to reach for on Linux or WASI, so those take the
bundled tables and this crate changes nothing for them.

**WASI is where that costs most**, and not because the tables are bigger:
a component carries them itself and nothing in the component model shares
them, so a deployment of ten components pays for ten copies.
`wasi:http` offers no conversion — its `set-authority` refuses anything
that is not a syntactically valid, and therefore ASCII, URI authority,
measured against wasmtime rather than read off the specification. If that
weight matters more than accepting Unicode host names, the lever is your
own: turn the `idn` feature off and convert before you build the URL.

Apple and the browser are reached through a URL parser rather than a
UTS 46 entry point, so each gets the case folding and the ACE check the
parser leaves out — all of it for Foundation, one line for the browser,
because the WHATWG standard defines its host parsing as UTS 46 and Apple
documents nothing. Apple is in that
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
