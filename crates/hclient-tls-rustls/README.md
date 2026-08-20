# hclient-tls-rustls

**`TlsConnect` over rustls — the default.**

Memory-safe, identical on every platform, and the only backend here that
can report the negotiated ALPN, which is what HTTP/2 negotiation needs. It
also implements `hclient-tls-quic`'s `QuicTlsConnect` behind a `quic`
feature. It **refuses** a non-`None` ECH config rather than ignoring one,
which is why a connector must ask before passing one.

Part of [hclient](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
