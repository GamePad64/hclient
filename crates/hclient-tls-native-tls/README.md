# hclient-tls-native-tls

`TlsConnect` over the platform's own TLS stack: SChannel on Windows,
Security.framework on Apple, OpenSSL elsewhere.

Use it when trust decisions have to live in the OS store, such as
enterprise roots pushed by policy or a FIPS-validated provider. It reports
neither the protocol version nor the cipher suite, because the platform
APIs do not expose them, and it cannot do QUIC.

`NativeTls::new()` is the platform's defaults. `with_root_certificate` adds
a trust root on top of the OS store, and `with_client_identity` sets the
client certificate for mutual TLS. Both take the `Certificate` and
`Identity` types this crate re-exports from `native-tls`. It holds that one
certificate and gives it no name, so a request that asks for a named
identity is refused rather than handed this one. For several identities
selected per request, use `hclient-tls-rustls`.

`dangerous-insecure` adds `danger_accept_invalid_certs()`, which skips
certificate verification; it is off unless asked for.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
