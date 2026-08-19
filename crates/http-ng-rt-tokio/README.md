# http-ng-rt-tokio

**`http-ng-rt` over tokio.**

`TcpConnect`, `Timer`, `UdpBind`, `Spawn` and `Blocking` on a real tokio
reactor, plus the `hyper::rt` IO adapter. `APPLIES` is `cfg!`-computed
rather than a constant, because `SO_BINDTODEVICE` is Linux-only and
`TcpKeepalive::with_retries` is missing on three more targets — a constant
claiming all of them everywhere would be a capability that lies on macOS.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
