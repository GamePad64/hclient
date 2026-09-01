# hclient-webtransport

WebTransport sessions over a `quinn::Connection`.

`Session::connect`, `open_bi`, datagrams, and clean close through RFC
9297's capsule. It takes a connection from outside rather than dialling
one itself, so it adds no QUIC endpoint of its own.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
