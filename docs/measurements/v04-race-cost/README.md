# v0.4 W1 deliverable 5 — the race's cost, raw

`release-run.txt` is one verbatim run of
`crates/hclient-select/tests/race_cost.rs`, kept so that the tables in
`docs/v04-w1-acceptance.md` §7.3 have a provenance a reader can check
without spending three minutes.

It is a **snapshot and not a baseline.** Nothing compares against it, no CI
job reads it, and it will go stale the moment quinn changes a default. The
harness is the artefact; this is a receipt.

```
cargo nextest run --release -p hclient-select --test race_cost \
    --run-ignored all --no-capture -j1
```

Taken on an Intel i7-14700K (28 threads), Linux 7.0.0-29-generic,
`rustc` 1.97.1, over the v4 loopback, with nothing else running. ANSI
colour and nextest's per-test framing stripped; every `M*` line is the
harness's own `println!`.

**Which figures depend on this machine and which do not**, since that is
the only reason the header above matters:

- **Do not.** Everything in the 30 s / 1 s / 3 s / 7 s / 15 s family. Those
  are `quinn_proto::TransportConfig::default()`'s `max_idle_timeout` and
  the PTO ladder off its `initial_rtt` guess of 333 ms. They are the same
  to a few milliseconds in debug and release, and they would be the same
  across the Atlantic.
- **Do not, and this one surprised.** The ~41 ms in every TCP row. It is
  Nagle against the peer's delayed ACK, and Linux's delayed-ACK quantum is
  what sets it — not the CPU. Debug and release agree.
- **Do.** Every success figure: 0.4 to 7.6 ms here, all of it CPU, and all
  of it a *floor* because loopback has no round trip. On a real path add
  one RTT for QUIC and two for TCP-plus-TLS.

The interesting rows to read first are `M3`'s — the same head start with
the TCP arm's `nodelay` both ways, and the server's counters read at the
drop and again a second later. That pair is what established that a race
with no head start sends the request twice.
