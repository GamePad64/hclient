# hclient-proto

**Internal to hclient — not a public API.** Depend on `hclient`,
`hclient-native` or `hclient-proxy` instead.

The sans-io pieces hclient's transports share: an RFC 9112 response-head
parser, the RFC 8305 Happy Eyeballs scheduler, and two encoders. It is
published only because those crates depend on it; it makes no stability
promise and moves its minor version whenever they need it to. No other
hclient crate exposes its types.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
