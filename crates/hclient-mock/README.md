# hclient-mock

`MockTransport`: a `Transport` that answers from a queue you fill.

For unit-testing code that takes a `Client`, with no socket. It records
what was sent, including request bodies, so a test can assert what your
code posted. Behind `hclient`'s `test-util` feature.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
