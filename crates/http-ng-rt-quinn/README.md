# http-ng-rt-quinn

**The `quinn` adapter for `http-ng-rt`'s seams.**

41 crates, and **no `h3` among them** — a crate that wants bare QUIC over
these seams takes this rather than `http-ng-h3`'s 56 and no opinion about
HTTP. It was extracted from `http-ng-h3`, where it was 302 lines nothing
outside could reach.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
