# hclient-core

**The seams every `hclient` backend and every caller share.**

`Transport`, `Capabilities`, `RequestBody`/`RetryKind`, the `Hooks` event
seam, and the error taxonomy. It is the crate that makes *no backend may
claim a capability it does not have* enforceable rather than aspirational:
`Capabilities` has two kinds of field — a **gate**, which guards a `Client`
setting so `build()` can refuse, and a **report**, which states a transport
fact nothing at the client level could refuse — and which kind each field
is is checked by an exhaustive destructure in this crate rather than
described.

Part of [hclient](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
