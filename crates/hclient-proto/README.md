# hclient-proto

The sans-io half of `hclient`: bytes and rules, no sockets and no clock.

URI resolution, redirect rules, the retry policy, header grammars,
server-sent events, and the encoders. Everything here is a pure function
over values, so it is testable without a network.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
