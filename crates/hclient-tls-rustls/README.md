# hclient-tls-rustls

`TlsConnect` over rustls 0.23.

The default TLS backend. It reports the negotiated ALPN, which is what
lets `hclient-native` offer HTTP/2, and it implements the QUIC seam behind
a `quic` feature.

No trust store is compiled in by default; choose one:

- `platform-verifier` — `Rustls::with_platform_verifier()`, the operating
  system's own store, which is what `hclient::Client::new()` uses;
- `webpki-roots` — `Rustls::with_webpki_roots()`, a bundled root set;
- no feature — `Rustls::from_config(..)`, with a `rustls::ClientConfig` you
  build. Your `rustls` dependency must then be the same major as this
  crate's, and enable the `ring` provider (or install a process default).

`dangerous-insecure` adds `Rustls::danger_accept_invalid_certs()`, which
skips certificate verification; it is off unless asked for.

rustls' types appear in this crate's constructors, so a breaking rustls
release is a breaking release of this crate — and of no other crate in the
family.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
