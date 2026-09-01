# hclient-tower

A `tower::Service` adapter for `hclient`, in both directions.

Wrap a `Client` as a `Service`, or wrap a `Service` as a `Transport`. The
second direction is what lets you drive a real `hclient::Client` against an
`axum::Router` in-process, with no socket and no port.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
