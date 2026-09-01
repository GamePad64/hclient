# hclient-tls-rustls

`TlsConnect` over rustls, using the platform trust store.

The default TLS backend. It reports the negotiated ALPN, which is what
lets `hclient-native` offer HTTP/2, and it implements the QUIC seam behind
a `quic` feature.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
