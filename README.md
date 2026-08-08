# http-ng

**An HTTP client complete enough to build a new curl on — or a browser.**

That is the mission, and it is a demanding one, because those two consumers
want opposite things. A command-line tool wants a static binary with no
runtime, honest exit codes and no surprises. A browser engine wants
streaming bodies, connection reuse, protocol negotiation, cancellation that
actually cancels, and a resolver that answers questions DNS was extended to
answer. A client that serves only one of them ends up wrong for the other.

Most Rust HTTP clients are shaped by the first thing they were used for.
This one is shaped by refusing to choose.

## What the mission demands, and what follows from it

**The same application code has to run everywhere the web does.** Not "a
native version and a wasm version" — the same code. That is why `Transport`
is the seam everything hangs off: a socket plus HTTP/1 on native, a
delegated exchange on `wasi:http`, the browser's own `fetch` in a page. The
acceptance test for this is a real consumer, written before this library
existed, ported onto it and built for three targets with **zero** `#[cfg]`
in its own code.

**Nothing may claim a capability it does not have.** A browser decompresses
responses and will not let you set `Accept-Encoding`; WASI delegates the
whole exchange to its host; a platform TLS stack cannot report the
negotiated ALPN. A client that papers over those differences lies to the
caller at the exact moment the caller needs the truth. So every backend
publishes a `Capabilities` record, a configuration a backend cannot honour
is a typed error at build time rather than a silent no-op, and the same
defect — a capability that overstates — has been caught and fixed four
times: once each in the WASI, native and browser backends, and once more in
`Native`'s own constructor, which advertised full TLS regardless of which
TLS implementation was plugged into it.

**Everything protocol-shaped is pure.** Redirect decisions, SSE parsing,
backoff, Happy Eyeballs ordering: functions over values, no I/O, no clock.
They are the part a browser would lean on hardest, and the part that is
cheapest to get right if it never touches a socket.

**Pluggable where it matters.** TLS, DNS, the runtime and the transport are
each a trait with more than one implementation — rustls or the platform
stack, the system resolver or hickory, tokio or smol, or none of them at all
for a build that has no room. `NoTls` and `IpLiteralOnly` exist so a
constrained target can drop the lot and still make requests.

## Where it actually is

v0.1 is HTTP/1.1 over three backends, and it is honest about the rest.
Connection pooling, HTTP/2, HTTP/3, streaming request bodies, WebSocket and
a whole-request deadline are **not** implemented — they are listed, with
what each would take, in [`docs/v01-acceptance.md`](docs/v01-acceptance.md)
alongside the evidence for the four claims v0.1 does make.

A browser needs all of them. This is the foundation, not the finished
building — but it is a foundation with the seams already cut, which is the
part that is expensive to add later.

18 crates, 619 tests, three targets, two browsers in CI.

## Reading further

- [`AGENTS.md`](AGENTS.md) — the engineering detail: what is in the
  dependency graph and why, what each seam costs, what is proven and how.
- [`docs/v01-acceptance.md`](docs/v01-acceptance.md) — the four claims and
  their evidence, plus what v0.1 deliberately does not do.
- [`docs/porting-wasi-fetch.md`](docs/porting-wasi-fetch.md) — migrating
  from `wasi-fetch`, including the one substitution a mechanical migration
  gets wrong.
