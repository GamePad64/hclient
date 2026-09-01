# hclient-rt-smol

`hclient-rt` implemented over smol and `async-io`.

Use it if your program is built on smol rather than tokio. It also works
on a bare `futures_executor::block_on` with no spawning.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
