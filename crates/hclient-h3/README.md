# hclient-h3

**HTTP/3 over QUIC, as its own crate.**

A separate crate rather than a feature of `hclient-native`, and the reason
is the 56-crate QUIC stack rather than the type system: `H3`'s declaration
carries no where-clause, so its bounds live on `impl Transport` and an
erased field would keep them off the native transport entirely. What a
feature would cost is dead code in the graph of every build a neighbour
switched it on for. It streams request bodies
and is genuinely full duplex. A QUIC connection nobody polls is not idle,
it is dying, so the driver is spawned — and that turned out to be necessary
and not sufficient, since driving a connection is what lets it *send* a
keep-alive, not what makes it decide to.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
