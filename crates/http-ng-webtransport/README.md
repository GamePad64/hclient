# http-ng-webtransport

**WebTransport sessions over `http-ng-h3`'s QUIC.**

`Session::connect`, `open_bi`, datagrams, many sessions on one connection,
and a clean close that can be told apart from a session that vanished. 48
crates, and `quinn` arrives with no `ring`, which is the visible
consequence of owning no endpoint. The close capsule is 59 lines here
because the three crates whose names promise it do not have it — measured,
not assumed.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
