# http-ng-dns

**The `Resolve` seam, and the SVCB/HTTPS record types.**

A resolver is a `Stream` of addresses rather than a future for a list, so
Happy Eyeballs can dial the first answer while the rest are still arriving.
The DNS record decoder sits behind a `codec` feature, so an `IpLiteralOnly`
build carries no parser at all: 13 crates without it, 16 with.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
