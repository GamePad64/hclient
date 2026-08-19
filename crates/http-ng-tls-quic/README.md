# http-ng-tls-quic

**A second TLS seam, for QUIC — and not a widening of the first.**

`QuicTlsConnect` exists because the intersection of `TlsConnect`'s four
methods with `quinn_proto::crypto::Session`'s eleven is **empty**: QUIC
wants key schedules per encryption level and CRYPTO-frame payloads, where
`TlsConnect` can only hand back a wrapped byte stream. The failure mode is
worse than a compile error — an adapter between them type-checks with an
empty body. Its own crate rather than a feature, because Cargo unifies
features and one would put `quinn-proto` into every build in any graph that
wanted h3.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
