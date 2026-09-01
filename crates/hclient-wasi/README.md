# hclient-wasi

`Transport` over `wasi:http` 0.3, for `wasm32-wasip2`.

The host does the networking, so nothing here opens a socket and the graph
contains no tokio, hyper or h2. Request options the host rejects become
errors rather than being silently dropped.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
