# hclient-proxy

Proxy protocols for [`hclient`](https://crates.io/crates/hclient) — HTTP
`CONNECT`, SOCKS5 and SOCKS4a — as **sans-io handshakes**, plus the
operating system's own proxy settings behind the `system` feature.

It reports a proxy auto-config (PAC) script's URL and **does not run
one**: that needs a JavaScript engine, which was built here, measured at
114 crates and +3.4 MB of binary, and withdrawn — neither reqwest nor
curl runs one either. A machine configured with a script is a named
refusal rather than a silent direct connection, which is the half that
was actually missing.

Nothing here opens a socket, and nothing here names an IO trait. A
handshake is a state machine: it is handed the bytes that arrived and
answers with the bytes to send, or *not yet*, or *the tunnel is open*.
The transport owns the socket and drives it — `hclient-native`'s driver
is thirty lines and is the only place in the family that knows what a
`poll_read` is.

## Why it is its own crate

This workspace's test for a crate boundary is whether it holds a
dependency that a feature would otherwise spread to every graph
switching it on. The protocols hold none — that is why
`hclient-native`'s `proxy` feature still has no `dep:` line. The
`system` feature does: `proxy_cfg` reads the Windows registry and the
macOS dynamic store, and it carries `url`, and through it `idna` and the
ICU tables — on exactly the two targets `hclient-idn` exists to keep
them off. A feature on the transport would have put those into every
build in any graph that turned it on.

The second reason is one a dependency graph cannot show: a transport
that is **not** `hclient-native` — `hclient-urlsession`, or somebody
else's — can read the same settings and speak the same protocols without
taking `hyper` with it.

## What sans-io bought

Two things, and the second is the one that was paid for.

Every rule in every protocol is testable **without a socket**, against
the exact byte sequences the RFCs print. A mutation in the SOCKS5 reply
parser is killed by a test that never opens a file descriptor.

And `CONNECT` no longer needs an HTTP client to speak HTTP. It used to
drive `hyper`'s h1 dispatcher through `hclient-native`'s upgrade seam,
which tied the whole proxy family to hyper, to hyper's IO traits, and to
a transport. What replaced it is forty lines over `hclient-proto`'s
response-head parser — available because a `CONNECT` response is the one
HTTP message with **no body under any framing rule** (RFC 9110 §9.3.6),
so the hard half of HTTP/1 has no subject here.

## What it costs

A protocol that has to **wrap** the IO cannot be written against this
seam — TLS to the proxy itself is the real example. That is not a
regression: it was unsupported before this crate existed, and
`system::ParseError::TlsToProxyUnsupported` is where the refusal is
already written down.

## Licence

MIT or Apache-2.0, at your option.
