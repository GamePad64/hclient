# hclient-rt-smol

**`hclient-rt` over smol / `async-io`.**

The second runtime, and it exists to prove the seam is real rather than
decorative: the same generic transport code runs under it on a bare
`futures_executor::block_on`, with no spawn and no tokio reactor anywhere in
its path. CI runs both.

Part of [hclient](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
