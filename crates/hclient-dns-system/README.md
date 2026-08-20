# hclient-dns-system

**`Resolve` over the platform resolver — `getaddrinfo`, `res_query`, `DnsQuery_UTF8`.**

What most programs should use: it honours `/etc/hosts`, the search domain
list, and whatever the OS has been configured to do. It also answers
HTTPS/SVCB queries, which is what lets a connection be steered to h3 or to
an alternative endpoint before it is opened.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
