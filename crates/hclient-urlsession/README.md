# hclient-urlsession

`Transport` over Apple's `URLSession`.

Use it on Apple platforms for things a userspace stack cannot reach: a
per-app VPN, the system proxy and its PAC script, background transfer. It
turns off `URLSession`'s own cookie store, cache and redirect handling, so
`hclient`'s behaviour is the same here as on any other backend.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
