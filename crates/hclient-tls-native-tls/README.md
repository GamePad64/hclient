# hclient-tls-native-tls

`TlsConnect` over the platform's own TLS stack: SChannel on Windows,
Security.framework on Apple, OpenSSL elsewhere.

Use it when trust decisions have to live in the OS store, such as
enterprise roots pushed by policy or a FIPS-validated provider. It reports
neither the protocol version nor the cipher suite, because the platform
APIs do not expose them, and it cannot do QUIC.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
