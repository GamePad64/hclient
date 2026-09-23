# hclient-rt

Runtime traits for `hclient`: `TcpConnect`, `IpcConnect`, `UdpBind`,
`Timer`, `Spawn` and `Blocking`, plus `Shutdown` for the half-close a
byte stream owes its caller.

Small traits, so a transport can be written once and run on tokio, on
smol, on embassy, or on a bare `futures` executor with no reactor. Each
implementor names its own futures, so none of them demands `Send` of a
runtime that cannot give it — the one exception is `Blocking`, because
work handed to a thread pool crosses a thread by definition. Implement
them to run `hclient` on a runtime it does not ship support for; every
option a runtime can or cannot apply is declared in a report built up
from `NONE`, and what it does not declare it refuses by name.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
