# hclient-rt-embassy

**`hclient-rt` over `embassy-net` — and the workspace's only `!Send`
runtime.**

A `TcpConnect` and `Timer` on a real `embassy-net` stack, with live
scenarios over a TAP device in CI.

**Read this before reaching for it.** Its configuration is *`std` plus
`embassy-net`*, and those rarely meet: `http` 1.x forbids `no_std`
outright, so this crate needs `std`, while `embassy-net` is the stack you
use when you have none. On a device with `std` — esp-idf, embedded Linux
— you get `std::net`, and the runtime is `hclient-rt-smol` or
`hclient-rt-tokio`, which need nothing special. Measured: `esp-idf-svc`
0.51 integrates `embassy-sync`, `embassy-time-driver` and
`embassy-futures`, and not `embassy-net`.

**What it is for today is keeping the seams honest.** Every other
implementor in the workspace names a `Send` future; this one cannot,
because `embassy_net::Stack<'d>` is `&'d RefCell<Inner>` and the crate
carries no `unsafe impl Send` anywhere. That makes it the single piece of
evidence that `TcpConnect::Connecting` had to be an associated type rather
than a `Send` box, and that `SendTransport` had to be a second trait
rather than a bound on the first — a runtime that cannot promise `Send`
is still a runtime here, and `tests/seam.rs` pins exactly that.

If `no_std` becomes reachable — `http` growing it, or this workspace
dropping `http` from its public API — this stops being a witness and
becomes the runtime it is named for.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
