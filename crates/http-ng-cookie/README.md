# http-ng-cookie

**An RFC 6265 cookie jar: sans-io and clockless.**

It takes a `now` rather than reading a clock, and it does no IO, so the
same rules serve every backend. The public suffix list is compiled in
(+77 KiB, which is why the whole thing is off by default one crate up) and
is pluggable — `NoList` is a real choice for a build that has no room for
it.

Part of [http-ng](https://github.com/actcore/http-ng) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
