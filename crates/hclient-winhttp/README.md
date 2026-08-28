# hclient-winhttp

Windows' **WinHTTP** behind [`hclient`](https://crates.io/crates/hclient)'s
`Transport` seam — the fifth *ambient* backend, owning no connection of its
own, after `hclient-wasi`, `hclient-fetch` and `hclient-urlsession`.

## Why it is its own crate

For the reason every backend here is: a transport is a leaf, and this one
carries a dependency nothing else should resolve. Cargo unifies features
across a graph, so a `winhttp` *feature* on a shared crate would put
`windows-sys`' WinHTTP bindings — and the `unsafe` FFI layer over them —
into every build in any graph that switched it on. A dependency in the
other direction cannot be switched on from outside.

The dependency is cheap where it is paid: `windows-sys` 0.61 is already in
the graph of any Windows build of `hclient-dns-system` or `hclient-idn`, so
a Windows build of the workspace gains no crate by adding this one.

## Why you would choose it

The list a userspace stack cannot reach on Windows:

- **The machine's proxy, including a PAC script**, evaluated by the OS per
  request. `hclient-proxy` reads WinINET's settings and *refuses* a PAC
  machine with a named error, because running the script would mean
  carrying a JavaScript engine fed by WPAD. WinHTTP runs it in the OS,
  which is the same answer `hclient-urlsession` gives on Apple platforms —
  and until this crate, Windows had no answer at all.
- **SChannel, with the machine's own trust and policy**: enterprise roots
  pushed by Group Policy, and the CryptoAPI store.

That is a fact about a deployment rather than a preference, which is
`hclient-tls-native-tls`' argument one seam over.

## What it refuses to take from the OS

WinHTTP will follow redirects and keep a cookie jar for you. Both are
turned **off**, so `hclient`'s own are the ones in force — the same
decision `hclient-urlsession` makes, and for the same reason: a caller
porting from `hclient-native` must not lose features by changing one line.
It also does **not** enable `WINHTTP_OPTION_DECOMPRESSION`, so it reports
`DecompressionSupport::None` and `hclient` decodes — which the two browser
-shaped backends cannot do, because their platform decodes underneath
them.

## What has not been observed

**No line of this crate has been run.** It is cross-checked with
`cargo check --target x86_64-pc-windows-msvc --all-targets` and nothing
more, for want of a Windows machine. The three obligations WinHTTP's
asynchronous model places on a caller are stated in `src/sys.rs` at the
sites that depend on them, and amendment C18 in `docs/exceptions.md` says
what each one is — so the next person with a Windows box knows exactly what
to check.
