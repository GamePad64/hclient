# hclient-proto

**The sans-io, clockless half: bytes and rules, no sockets.**

RFC 3986 reference resolution, redirect policy, SSE decoding, Happy
Eyeballs scheduling, form and base64 encoding. It owns no IO and reads no
clock, so every backend shares one implementation of the parts most easily
got subtly wrong. `url` was removed from its graph deliberately — one
`join()` call was pulling in `idna` and the ICU tables, 4 MB of Unicode for
a feature a constrained target rarely needs — and survives as the oracle
for a 96-pair differential corpus.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
