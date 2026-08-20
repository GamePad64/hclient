# hclient-tls-native-tls

**`TlsConnect` over the platform's own stack — SChannel, Security.framework, OpenSSL.**

For deployments whose trust decisions live in the OS store: enterprise
roots pushed by policy, smartcard client certificates, a FIPS-validated
provider. That is a fact about an environment rather than a preference. It
reports less back than rustls does, and its own module doc says exactly
what — in particular it cannot report the negotiated ALPN, so protocol
selection driven by ALPN needs the rustls one.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
