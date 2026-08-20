# hclient-urlsession

**`Transport` over Apple's `URLSession`.**

For the list a userspace stack cannot reach on an Apple platform: per-app
VPN, the system proxy and its PAC, and background transfer. It deliberately
turns off `URLSession`'s own cookie store, response cache and redirect
handling, because all three are portable behaviour this workspace already
implements once — and it is stronger than the browser backend on the third:
a delegate can refuse a redirect, so this reports
`RedirectSupport::Transparent` where `hclient-fetch` must report
`Internal`.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
