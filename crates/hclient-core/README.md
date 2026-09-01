# hclient-core

Shared types for the `hclient` HTTP client: the `Transport` trait,
`Capabilities`, `RequestBody`, the `Hooks` event types and the error enum.

You do not normally depend on this directly. `hclient` re-exports what a
caller needs; this crate exists so that backends and the client can share
these definitions without depending on each other.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
