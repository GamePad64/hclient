# hclient-tungstenite

WebSocket over `hclient-native`, with `tungstenite` framing.

`Tungstenite::new(&native).websocket(request)` borrows a transport that a
`Client` may already own. It is a separate crate so that switching
WebSocket on does not put `tungstenite` into every build that uses the
transport.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
