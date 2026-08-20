# hclient-rt-embassy

**`hclient-rt` over `embassy-net`, for embedded targets with `std`.**

A `TcpConnect` and `Timer` on a real `embassy-net` stack, with live
scenarios over a TAP device in CI. This is what makes an embedded target
reachable without a separate backend: `Native<Embassy, NoTls,
IpLiteralOnly>` is the embedded transport. `no_std` is still out, and the
obstacle is `http` 1.x rather than anything here.

Part of [hclient](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
