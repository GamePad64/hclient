# http-ng-h3

**HTTP/3 over QUIC, as its own crate.**

It could not have been a feature of `http-ng-native`, and the reason is the
type system rather than the 56-crate QUIC stack: it is bounded on `R:
UdpBind + Spawn` and `T: QuicTlsConnect`, neither of which the native
transport has, and Cargo's features are additive. It streams request bodies
and is genuinely full duplex. A QUIC connection nobody polls is not idle,
it is dying, so the driver is spawned — and that turned out to be necessary
and not sufficient, since driving a connection is what lets it *send* a
keep-alive, not what makes it decide to.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
