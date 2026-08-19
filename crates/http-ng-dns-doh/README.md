# http-ng-dns-doh

**`Resolve` over DNS-over-HTTPS.**

The interesting problem was never the wire format, it was bootstrapping,
and which constructor compiles says how it is answered: `Doh::pinned` takes
an IP literal and refuses a name, `Doh::bootstrapped` takes a name and
refuses a literal. Failing closed is the default and failing open is
visible in the type — `Doh<C>` is `Doh<C, NoFallback>`. What makes the
request is a `Transport` and **never** an `http_ng::Client`, so "a
resolver's client is not the user's client" is a thing that does not
typecheck rather than a thing that is discouraged.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
