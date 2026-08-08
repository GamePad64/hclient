# http-ng

**An HTTP client complete enough to build a new curl on — or a browser.**

Those two want opposite things. A command-line tool wants a static binary
with no runtime, honest exit codes and no surprises. A browser engine wants
streaming bodies, connection reuse, protocol negotiation, cancellation that
actually cancels. A client that serves one ends up wrong for the other.

Most HTTP clients are shaped by the first thing they were used for. This one
is shaped by refusing to choose. The same application code runs on a native
socket, on `wasi:http`, and on the browser's own `fetch` — and no backend is
allowed to claim a capability it does not have.

It is a kit, not a monolith. The transport, the TLS stack, the resolver and
the async runtime are each a seam with more than one part behind it: rustls
or the platform's own TLS, the system resolver or hickory, tokio or smol.
Take the pieces you need. A build with no room for TLS or a resolver takes
neither, and still makes requests.

v0.1 is HTTP/1.1 over those three backends. Connection pooling, HTTP/2 and
/3, streaming request bodies and WebSocket are not built yet — this is the
foundation, with the seams cut where they are expensive to add later.

- [`AGENTS.md`](AGENTS.md) — how it is built, and why each piece is there.
- [`docs/v01-acceptance.md`](docs/v01-acceptance.md) — what v0.1 claims, the
  evidence for each claim, and what it deliberately does not do.
