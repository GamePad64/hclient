# http-ng-dns-hickory

**`Resolve` over hickory-dns, in process.**

For builds that want DNS behaviour independent of the host's configuration
— a container with no resolver, a test that must not touch the network, a
program that needs the same answers on every platform.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
