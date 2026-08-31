# hclient-dns-system

**`Resolve` over the platform resolver — `getaddrinfo` for addresses,
[`system-resolver`](../system-resolver) for HTTPS records.**

What most programs should use: it honours `/etc/hosts`, the search domain
list, and whatever the OS has been configured to do. It also answers
HTTPS/SVCB queries, which is what lets a connection be steered to h3 or to
an alternative endpoint before it is opened.

## What is in here and what is not

The platform calls are not. `res_query`, `android_res_nquery`,
`DnsQueryRaw` and `DnsQuery_UTF8` live in `system-resolver`, which hands
back records with their RDATA; this crate applies RFC 9460's **client**
rules to one of those records — AliasMode against ServiceMode, a root
TargetName meaning the owner name, `mandatory` semantics — and wires the
result into `Resolve`.

That split is why this crate contains no `unsafe` at all.

## Building on Linux

`libresolv.so` — the development symlink, not the runtime `.so.2` — must
be installed on gnu targets. The `-lresolv` comes from `system-resolver`
rather than from any file here, so that `res_query` is found on every
glibc, and that is true even on
glibc 2.34 and later, where the symbol has moved into `libc.so.6` and the
library contributes nothing to the result. On Debian and Ubuntu it comes
with `libc6-dev`. musl needs nothing: the symbol is inside `libc.a`.

**Supported minimum: glibc 2.34.** Older ones are expected to work — they
link `res_query` out of `libresolv.so.2` instead, and gain a run-time
dependency on it — and are not tested here. The crate's own module doc has
the measurements.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
