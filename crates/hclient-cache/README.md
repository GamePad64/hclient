# hclient-cache

**An RFC 9111 response cache: sans-io and clockless.**

Freshness, validation, `Vary`, and the directives on both sides. A
**private** cache, a user agent's rather than a shared one, and three rules
turn on that. `Lookup` has four answers, because *send it*, *send it with
these fields added* and *do not send it at all* are three instructions and
an `Option` carries two. A `304` deliberately does not relabel the stored
bytes.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
