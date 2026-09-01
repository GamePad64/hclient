# hclient-proxy

Proxy support for `hclient`: HTTP `CONNECT`, SOCKS5 and SOCKS4/4a.

```rust
let transport = Native::new(rt, tls, dns).proxy(Proxy::new(Socks5, "127.0.0.1", 1080));
```

The protocols are state machines with no IO in them: a handshake is handed
the bytes that arrived and answers with the bytes to send, or "not yet".
That is what lets every rule be tested without opening a socket.

A proxy replaces the whole resolve-and-connect step rather than wrapping
the connector, so the origin's *name* reaches the proxy instead of being
resolved locally. That matters: local resolution would leak the DNS a
proxy user is often there to hide.

`Proxy::bypass([..])` takes the exact-host, `.domain`, `host:port` and
subnet forms. Nothing is bypassed by default, loopback included.

With the `system` feature it also reads the machine's own proxy settings:
environment variables everywhere, the registry on Windows, the system
configuration on macOS, JVM properties on Android. A configuration it
cannot express exactly is refused by name rather than quietly narrowed —
including a PAC script, which it reports rather than runs.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
