# hclient-tls

The TLS traits for `hclient`: `TlsConnect`, `TlsIdentity` and `NoTls`.

`NoTls` is a real choice, not a placeholder: it is for builds with no room
for a TLS stack, where `https://` fails at connect with a typed error
rather than silently going plaintext. Implement `TlsConnect` to use a TLS
library `hclient` does not ship a backend for.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
