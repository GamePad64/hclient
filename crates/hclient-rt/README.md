# hclient-rt

Runtime traits for `hclient`: `TcpConnect`, `Timer`, `UdpBind`, `Spawn`
and `Blocking`.

Five small traits, so a transport can be written once and run on tokio, on
smol, on embassy, or on a bare `futures` executor with no reactor. None of
them requires `Send`. Implement them to run `hclient` on a runtime it does
not ship support for.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
