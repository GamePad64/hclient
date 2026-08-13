# v0.4 acceptance

The counterpart to `v01-acceptance.md`, `v02-acceptance.md` and
`v03-acceptance.md`, and deliberately the shortest of the four: v0.4's
arguments were written down as they were made, one document per topic,
and repeating them here would create a second copy to go stale. This
document is the index, the *deliberately not done* list, and the
*unverified* list — the three things a reader cannot reconstruct from
the per-topic documents, because each of those knows only its own half.

## What v0.4 built

| what | where the argument lives |
|---|---|
| `http-ng-select` — one transport choosing between the TCP and QUIC stacks, from the HTTPS record | `v04-w1-acceptance.md` |
| Alt-Svc, the slow tier, with `ma` as the cache lifetime SVCB could not supply | `v04-w1-acceptance.md` §9 |
| `StagedConnect` — a connect asked for on its own, one trait per crate | `v04-staged-connect.md` |
| `H3Failures` — the negative half of discovery, unblocked by the staged connect | `v04-staged-connect.md` |
| The race, off by default, whose head start stopped being a safety mechanism | `v04-race.md` |
| Hooks on all four backends, and `Head::version` becoming an `Option` | `v04-w2-hooks-ambient.md` |
| `http-ng-webtransport` — sessions, streams, datagrams, close capsules | `v04-w2-webtransport.md`, `-datagrams.md`, `-capsules.md` |
| `http-ng-rt-quinn` — `SeamRuntime` extracted, 42 crates against h3's 58 | `rt-quinn-extraction.md` |
| `http-ng-ws-tungstenite` — the WebSocket framing as its own crate | `w4-upgrade-seam.md` §8 |
| h2 multiplexing, opt-in behind `Native::multiplexed()` | `h2-multiplexing.md` |
| `TCP_NODELAY` asked for where the runtime says it applies | AGENTS.md, and the 41 ms it saves |
| The gRPC yardstick — 21 requirements, 15 tests, **no library code changed** | `grpc-yardstick.md` |

Four defects were fixed that no plan contained, and all four are the
same shape — a complete response or a live connection discarded by our
own code: h3's `STOP_SENDING`, h2's `RST_STREAM(NO_ERROR)` in two
halves, h3's 0-RTT rejection on the **control** stream, and a cancelled
upload poisoning a shared QUIC connection. Three reached `main` and were
found by capturing a failure rather than by reasoning about one.

## Deliberately not done

**The `urlsession` backend.** Deferred by the owner mid-version, not
blocked. Nothing in this tree depends on it.

**WebTransport, four things**, each with what it needs rather than a
shrug: `GOAWAY`; more than one session per connection (`PoolKey` has no
field to tell two apart); the capsule protocol beyond
`CLOSE_WEBTRANSPORT_SESSION`; and server-initiated unidirectional
streams, which are **unreachable rather than unimplemented** — the arm
that would keep them is guarded by a SETTINGS flag `h3` 0.0.8's *client*
cannot set, and that impossibility is asserted by a test rather than
described, so an `h3` that grows the setter fails a line instead of
leaving a stale paragraph here.

**h2 multiplexing is not the default**, and beyond the peer's
`MAX_CONCURRENT_STREAMS` requests **queue** rather than opening a second
connection. The threshold that would decide otherwise needs the peer's
limit — `h2` will not report it — and a handshake cost that is a network
property, so a loopback number would be a number about this machine.

**`Capabilities::proxy` is inert, and this is a decision waiting rather
than a finding acted on.** Measured: the field is set in exactly one
place — `Capabilities::none()`'s `false` — no backend sets it, nothing
in `http-ng` branches on it, and there is no proxy setting on `Client`,
no seam on `Transport` and no implementation anywhere, so a caller
cannot ask for a proxy and learning that the transport has none is not
actionable. It has no doc comment, alone among its neighbours. That is
the shape `UpgradeSupport`'s four variants had when they were deleted —
*a variant exists only if a caller decision turns on it* — but unlike
those, this one names a feature a general HTTP client is expected to
have, so the two ways out are opposite: delete the field, or build
proxy support and make the field mean something. Both are the owner's
call; recorded here rather than taken.

**Publishing.** Unchanged and not a v0.4 question: every crate still says
`0.1.0`, and the trigger is the owner's.

## What v0.4 has not checked

**Chrome browser tests do not run on this machine.** `wasm-pack test
--headless --chrome` fails acquiring a driver with `http status: 404`,
and it reproduces on `main` independently of any branch, so it is the
environment rather than the code. Firefox passes, and the `browser` CI
job runs both.

**Two workspace-level flakes, seen on this tree and on the tree before
it, and deliberately not attributed.**
`http-ng-select::race::with_no_head_start_both_stacks_connect_and_exactly_one_request_is_sent`
and `http-ng-h3::zero_rtt::early_data_is_accepted_and_the_wire_shows_it_leaving_before_the_handshake`
each fail at a rate near 1 in 40 full
workspace runs under load, and neither has been captured. Both crates'
own suites are clean run alone. Whether the rate moved with v0.4's added
load or is noise is **not distinguishable at these sample sizes**, which
is why this paragraph says so rather than picking a side.

A further **200 full workspace runs on the merged tree produced exactly
one failure**, the select one — so the rate here is nearer 1 in 200 than
1 in 40. The asymmetry worth recording is the machine: the 3-in-39 runs
were taken while other work was in flight and these were not, which fits
"timing-sensitive race" and is the first thing to vary if anyone sets out
to capture it.

**That one failure was not captured, and losing it was a method mistake
rather than bad luck.** The run's output went through `tail -2` for the
summary line, which discarded the failure detail that was the entire
reason to be watching — in a project whose rule for flakes is *capture
rather than reason about it*. The 120-run loop set up afterwards, which
does keep the full output, did not reproduce it. Anyone resuming this
should run the workspace under competing load and keep every byte. The project's
record argues for capturing rather than reasoning: the three flakes
chased this way each turned out to be a real defect, one of them an RFC
9114 violation that took three sightings.

**The residual pooled-reuse race** recorded in `http-ng-native`'s
`h1.rs` is unchanged. `TCP_NODELAY` made it *visible* rather than worse —
Nagle's 41 ms had been padding the window — and the two fixes it would
take are named where the race is.

**`StagedConnect::connect` uses a shared h2 connection when the pool has
one and never makes one.** `http-ng-select` is unaffected, and that is
checked rather than assumed: its TCP arm goes through
`Prefetch::execute_prepared`, the ordinary pooled path, in both the
hedged and unhedged call sites — `stage`/`exchange` is the QUIC arm
only. The gap is reachable by a direct caller of
`http_ng_native::Staged`.
