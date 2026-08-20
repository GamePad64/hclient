# hclient

**An HTTP client complete enough to build a new curl on — or a browser.**

Those two want opposite things. A command-line tool wants a static binary
with no runtime, honest exit codes and no surprises. A browser engine wants
streaming bodies, connection reuse, protocol negotiation, cancellation that
actually cancels. A client that serves one ends up wrong for the other.

Most HTTP clients are shaped by the first thing they were used for. This one
is shaped by refusing to choose. The same application code runs on a native
socket, on `wasi:http`, on the browser's own `fetch` and on Apple's
`URLSession` — and no backend is allowed to claim a capability it does not
have. That last clause is enforced rather than asked for: a setting a
backend cannot honour is an error at `build()`, not a value quietly
dropped.

It is a kit, not a monolith. The transport, the TLS stack, the resolver and
the async runtime are each a seam with more than one part behind it: rustls
or the platform's own TLS, the system resolver or hickory, tokio or smol.
Take the pieces you need. A build with no room for TLS or a resolver takes
neither, and still makes requests.

HTTP/1.1, HTTP/2 and HTTP/3 are all spoken; connections are pooled and h2
can be multiplexed on request; request and response bodies stream, with
real full duplex on h3. WebSocket and WebTransport are their own crates,
behind their own seams, because a transport that cannot do them should be
a compile error rather than a runtime refusal. Cookies, an RFC 9111 cache,
redirects, decompression, proxies (HTTP `CONNECT` and SOCKS5) and
`multipart/form-data` are each one implementation shared by every backend,
which is what makes "the same answers everywhere" more than a slogan.

Published as `0.1.0`, and `AGENTS.md` says what that promise costs: six
public types took a breaking change in the month before the first release,
so read the seams as young rather than settled.

- [`AGENTS.md`](AGENTS.md) — how it is built, and why each piece is there.
  Long, and the length is the point: every seam records the argument that
  produced it and the measurement that settled it.
- [`docs/competitive-gaps.md`](docs/competitive-gaps.md) — what `reqwest`
  and `ureq` do that this does not, what it does that they cannot, and
  which of the differences are deliberate.
- The acceptance documents — [v0.1](docs/v01-acceptance.md),
  [v0.2](docs/v02-acceptance.md), [v0.3](docs/v03-acceptance.md),
  [v0.4](docs/v04-acceptance.md) — each with what that version claims, the
  evidence, and what it deliberately does not do.
