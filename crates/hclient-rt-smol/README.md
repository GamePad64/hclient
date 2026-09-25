# hclient-rt-smol

`hclient-rt` implemented over smol and `async-io`.

Use it if your program is built on smol rather than tokio. `Smol` has no
precondition: it works from any thread and under any executor, including
a bare `futures_executor::block_on`.

UDP sockets for HTTP/3 are built in, with no feature to enable. A connection's socket
is reachable through `AsFd` (`AsSocket` on Windows), for example with
`socket2::SockRef::from(&io)`.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
