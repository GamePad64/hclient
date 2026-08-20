# hclient-native

**The native transport: TCP, TLS, HTTP/1.1 and HTTP/2.**

hyper for h1 and the `h2` crate directly for h2, over pluggable runtime,
TLS and resolver seams. Pooled by default, with Happy Eyeballs, HTTPS/SVCB
discovery, socket options, proxies (HTTP `CONNECT`, SOCKS5, SOCKS4/4a),
Unix-domain sockets, `Expect: 100-continue`, `1xx` reporting and per-phase
timeouts. h2 is behind a feature and off by default; `capabilities()`
deliberately reports the HTTP/1.1 **floor** either way, because an
over-claimed `full_duplex` costs a caller a deadlock where an under-claimed
one costs a buffered copy.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
