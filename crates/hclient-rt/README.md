# hclient-rt

**The async-runtime seams: `TcpConnect`, `Timer`, `UdpBind`, `Spawn`, `Blocking`.**

Five small traits so a transport can be written once and run on tokio, on
smol, on embassy, or on a bare `futures` executor with no reactor at all.
None of them demands `Send`. `TcpConnect::APPLIES` is the shape worth
copying: a constant defaulted to the *understating* value, read by the layer
above to decide whether to even ask — an understated capability costs a
named error, an overstated one costs a silently unapplied option.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
