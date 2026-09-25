# hclient-rt-tokio

`hclient-rt` implemented over tokio.

The usual choice on native targets. `hclient`'s `default-transport`
feature selects it for you.

Two runtime types:

- `Tokio` reads the runtime from the current thread, which is what you want
  inside `#[tokio::main]`. Off a runtime thread it panics.
- `TokioHandle` carries a `tokio::runtime::Handle`, so the check happens
  once, when you build it, as a `Result`, and it then works from any
  thread.

The `udp` feature adds the UDP sockets HTTP/3 needs. A connection's socket
is reachable through `AsFd` (`AsSocket` on Windows), for example with
`socket2::SockRef::from(&io)`.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
