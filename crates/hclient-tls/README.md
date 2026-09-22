# hclient-tls

The TLS seams for `hclient`, and no TLS stack.

- `TlsConnect` — a handshake over a byte stream, handed a `TlsRequest`
  (built with `TlsRequest::new(server_name, alpn)`) and answering with a
  wrapped stream and a `TlsInfo`.
- `quic::QuicTlsConnect` — what a QUIC stack asks of TLS, which is not a
  handshake over a stream at all. It answers a declarative
  `QuicCryptoConfig` and an opaque `Session` of the backend's choosing, so
  this crate links no QUIC stack and no cryptography.
- `TlsIdentity` and `TlsConfigId` — the configuration identity both seams
  require, so one connector has one identity.

`NoTls` is a real choice, not a placeholder: it is for builds with no room
for a TLS stack, where `https://` fails at connect with a typed error
rather than silently going plaintext. Implement `TlsConnect` or
`QuicTlsConnect` to use a TLS library `hclient` does not ship a backend
for; `hclient-tls-rustls` and `hclient-tls-native-tls` are the two it
does.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
