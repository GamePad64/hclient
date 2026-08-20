# hclient-mock

**`MockTransport` — a `Transport` that answers from a queue.**

It is how every capability refusal in this workspace is tested, because "a
jar against a jar-owning backend is refused at `build()`" is a fact about a
type that never sends anything. It records what it was asked for, so a test
can assert on the request as well as script the response.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
