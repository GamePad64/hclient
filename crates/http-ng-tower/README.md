# http-ng-tower

**A `tower::Service` adapter, in both directions.**

So this client fits a stack that already speaks that vocabulary — and so a
`tower` stack can sit underneath the client as its transport.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
