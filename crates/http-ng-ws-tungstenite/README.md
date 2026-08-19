# http-ng-ws-tungstenite

**WebSocket over `http-ng-native`, with `tungstenite` framing.**

Its own crate and its own trait pair — `WebSocketConnect` for what a
backend implements, `WebSocket` for the message channel — so a transport
that cannot do WebSocket is a **compile error** rather than a runtime
`Unsupported`. The seam is message-oriented on purpose: "hand back the
socket after the 101" is implementable by exactly one of the four backends
here, and the three it shuts out include the browser. RFC 6455 ping/pong
liveness is available and off by default.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
