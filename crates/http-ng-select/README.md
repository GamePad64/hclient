# http-ng-select

**One transport that chooses between the TCP and QUIC stacks.**

It decides from the origin's HTTPS record — an `alpn` containing `h3`
chooses QUIC — with an RFC 7838 `Alt-Svc` cache as the slower second tier,
and an optional Happy-Eyeballs-style race for networks that block UDP/443.
The race is off by default, because a default that opens UDP sockets is a
decision about what a plain client does. The capability set is the part
that was not mechanical: the stored value must be true whichever member
serves the request, so five fields take the weaker claim and the
constructor **refuses**, naming the field, where the two are different
claims rather than a stronger and a weaker one.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
