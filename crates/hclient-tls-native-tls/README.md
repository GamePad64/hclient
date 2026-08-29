# hclient-tls-native-tls

**`TlsConnect` over the platform's own stack — SChannel, Security.framework, OpenSSL.**

For deployments whose trust decisions live in the OS store: enterprise
roots pushed by policy, a FIPS-validated provider. That is a fact about an
environment rather than a preference. It reports less back than rustls
does, and its own module doc says exactly what — no protocol version and
no cipher suite.

Two things this text used to say and that measurement has since removed:
the negotiated ALPN **is** reported, and a smartcard's key is **not**
reachable here — `native_tls::Identity` is built from PKCS#12 or PKCS#8
bytes, so a key the OS will not export is as far out of reach as it is
through rustls.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
