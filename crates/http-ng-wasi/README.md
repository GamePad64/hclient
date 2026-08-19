# http-ng-wasi

**`Transport` over `wasi:http` 0.3, for `wasm32-wasip2`.**

No tokio in the graph at all — 27 crates total, machine-checked. Proven
under a real `wasmtime` host rather than in theory, and a `wasi:http` host
rejecting a request option becomes a typed error rather than a silently
dropped setting, which is held in place by a static-analysis rule in CI.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
