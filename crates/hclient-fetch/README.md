# hclient-fetch

**`Transport` over the browser's own `fetch`, for `wasm32-unknown-unknown`.**

No tokio, no hyper, no h2 anywhere in its graph — machine-checked on every
push. It reports honestly about what a browser will not let it do:
redirects are `Internal` because `redirect: "manual"` hands back an opaque
response with no readable `Location`, and the cookie jar and cache are the
browser's, so configuring ours against it is an error at `build()`. It also
carries `fetch`'s own members — `mode`, `credentials`, `cache`,
`referrerPolicy`.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
