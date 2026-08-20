# The race, built — and the premise that had to change first

`docs/v04-design.md` §W1 deliverable 5, the last unbuilt piece of that
vertical. `docs/v04-w1-acceptance.md` §7 is the measurement it was blocked
on and the policy that measurement argued for;
`docs/connect-only-seam.md` and `docs/v04-staged-connect.md` are the seam it
is built out of. Nothing here re-argues any of them.

What this records is: whether the premise really changed and how that was
established, the three decisions §7 left open, one number that had to be
re-measured, one defect the first draft had that no test would have caught
in review, the mutation run, and what is still not verified.

`crates/hclient-select/src/race.rs`, about 200 lines of code under 200 lines
of module doc, plus a `hedging` setter and one field on `Selecting`.

---

## 1. The premise: it has changed, and here is the establishing

§7's headline, and the reason nobody built this:

> A race made of two `Transport::execute` calls races **requests**, not
> connections — the seam has no connect-only entry point — so with no head
> start the losing arm's request reaches the origin, measured in 5 of 6
> arms… So a head start is a **safety** mechanism before it is a latency
> one.

Three things were checked before writing a line, in increasing strength.

**By grep.** `crates/hclient-native/src/staged.rs` and
`crates/hclient-h3/src/staged.rs` contain no call to `send_request`, and no
call to anything that writes a request.

**By reading the code, which is stronger, because the absence of one
function name is not the absence of a write.** On the native side,
`Native::stage` ends at `handshake_for(conn, protocol, id)` and packs the
result into `Staged`; every byte of the request is written by
`established::exchange`, which only `StagedConnect::exchange` calls. On the
QUIC side `H3::stage` ends at `checkout`, and the request stream is opened
in `H3::finish`'s `one_attempt`. **The property is structural rather than
promised**: the request is not handed to a stream in either `connect`, so
there is no code path on which a `connect` could write one.

The one place it could have been subtle is 0-RTT, where the point of early
data is to send *with* the handshake. It is not subtle: `H3::stage` obtains
`quinn`'s `ZeroRtt` verdict and stores it in the handle, and nothing is put
into early data until `one_attempt` opens a stream — in `finish`.

**By measurement, which is the one that counts.**
`crates/hclient-select/tests/race.rs`'s
`with_no_head_start_both_stacks_connect_and_exactly_one_request_is_sent`
runs the built race at `Duration::ZERO` against two live servers behind one
authority, and asserts that the QUIC endpoint counted an attempt, the TCP
listener counted an accept, and `quic_answered + tcp_answered == 1`. That
is the direct negation of the 5-of-6 row, at the setting the 5-of-6 row was
taken at.

So the safety premise has changed. **The head start stops being a safety
mechanism and becomes a cost knob**, which is a different question, and §2
is its answer.

---

## 2. Decision 1 — the head start

**It stays at 250 ms, and it is now a number a caller may set to anything
including zero.** What changed is the floor and the argument, not the value.

### What changed

**The floor moved to zero.** Below one QUIC handshake the head start used to
mean a duplicated request at the origin — a correctness cliff whose location
the client cannot see, since it sits at *one QUIC handshake, wherever that
happens to be*. That is why the number had to be generous: it had to cover a
handshake on a path nobody had measured. It now means one TCP connect and
TLS handshake the request will probably not use, **checked into the pool
warm rather than thrown away**. `Duration::ZERO` is therefore a setting
rather than a bug, and it is the right one for a caller who knows their
network blocks UDP/443.

**The cost of being generous is now bounded by something that did not exist
when 250 ms was chosen.** §7.7's item 4 said the race needed *"a failure
memory, or the head start is paid again on every request to a blocked origin
— 250 ms × every request"*. `hclient_select::H3Failures` was built for the
sequential fallback since, and §4 below feeds it from the race, so the head
start is paid **once per origin per `H3_FAILURE_TTL`** and not once per
request. That is the difference between 250 ms per request and 250 ms per
five minutes.

### What did not change, and why the number is the same

The reason to keep a head start at all is no longer safety; it is that
**without one the hedge quietly overrules the chooser.** That reason got
sharper rather than weaker while this was being built, because of §3's
re-measurement: on loopback TCP is now *faster* than QUIC, and a race with
no head start is won by TCP at an origin whose HTTP/3 works perfectly well.
Loopback has no round trip, so it is not the general case — on a real path
QUIC's handshake is one round trip and TCP-plus-TLS-1.3's is two — but the
client cannot see the path, and a hedge that fires before the preferred
stack has had a fair chance is a hedge that decides.

So the number is chosen against the **success** side, exactly as §7.4 said
it must be, and 250 ms is still the right constant for the same reason:
`hclient_proto::happy_eyeballs::HeConfig::default()`'s `attempt_delay`, RFC
8305 §5's Connection Attempt Delay, this codebase's existing answer to *"how
long do I give the preferred option before trying the other"* one layer
down.

**The honest derived form is unchanged and still unbuilt.** §7.4's
origin-keyed RTT observation is what this should be, and there is still
nowhere in `hclient-select` to keep one; the Alt-Svc cache is keyed by origin
but holds a server-supplied lifetime, and an RTT is neither server-supplied
nor covered by `ma`.

### The one thing that is deliberately not a default

`Selecting::new` does not race. `Selecting::hedging(head_start)` is the only
way to turn it on, and it takes the number rather than defaulting it, so the
value is visible at the call site. `DEFAULT_HEAD_START` is exported for
callers with no reason to choose another.

`DefaultTransport` does not become `Selecting`, which is unchanged from
`docs/v04-design.md` §W1 — and one reason it gives is now met and the other
is not. *"It wants the measurement"* is met. *"A default that opens UDP
sockets is a decision about what a plain client does on a network that
blocks UDP/443"* is not, and this deliverable does not touch it.

`the_race_is_off_until_it_is_asked_for` is the A/B: one fixture, one bound,
one difference.

---

## 3. The TCP floor, re-measured

§7.3's TCP rows predate `docs/nagle-and-nodelay.md`, which taught
`Native::new` to ask for `TCP_NODELAY` where the runtime says it applies it.
Re-run on the same harness and the same host (`cargo nextest run -p
hclient-select --test race_cost --run-ignored all --no-capture -j1
--release`, M2a and M2d):

| exchange, cold, loopback | §7.3 | now | note |
|---|---|---|---|
| QUIC by name | 1.8 – 3.3 ms | **min 2.5, median 7.8, max 19.6 ms** | unchanged in kind |
| **TCP by name** | **41.8 – 42.5 ms** | **min 1.4, median 2.6, max 10.8 ms** | `nodelay`, now the default |
| TCP by IP literal | 41.6 – 42.7 ms | min 1.8, median 2.2 ms | |
| TCP with `nodelay` explicitly off | — | min 42.3, median 45.2 ms | the control: §7.3's number reproduces |
| TCP `execute`-to-head, `nodelay` | — | median 2.5 ms | where the 41 ms used to be |
| TCP `execute`-to-head, Nagle | — | median 46.5 ms | |

**The order of the two stacks has flipped on loopback**, and the flip is the
finding rather than the milliseconds. §7.3's table had a 13× gap in QUIC's
favour, which is why its M3 row could say *"the QUIC arm answered in every
one of the twenty-four"*: the fixture was giving QUIC a 40 ms head start for
free. It no longer does, and M3 re-run confirms it — at head start 0 with
`nodelay` on, **the TCP arm wins outright** and the QUIC server answers
nothing.

Two consequences, both used above:

- The head start is now doing visible work in the test suite rather than
  being masked by Nagle, which is what makes `R1` (the head start not slept)
  a three-test kill instead of a subtle one.
- Loopback is now a *worse* case for the hedge than a real network, not a
  better one. Every assertion here that says "QUIC wins" is being made
  against a path where TCP has the CPU advantage and no round trip to lose.

The failure numbers are untouched, and this work re-measured none of them:
30.002–30.006 s for a black hole and for an origin with no h3 server alike,
PTO₀ at 1.001 s. They are quinn's, not ours.

---

## 4. Decision 3 — the losing arm

Two questions, and they had different answers than expected.

### The disposal is not a choice this crate gets to make

Both arms are dropped the instant the other produces a connection, and
neither drop needs a line from here: `hclient_native::Staged`'s own `Drop`
checks its connection into the pool, and `hclient_h3::Staged` is a claim on
a connection the pool already holds.

**"Warm" is the only disposal the seam offers.** `Staged`'s `Drop` checks in
whenever reuse is on, and reuse is always on under a `Selecting`, because
`connection_reuse` is one of `combine`'s *same-or-refuse* fields —
`Native::without_pool()` against an `H3` does not construct. There is no
`discard` on the handle. So the question *"is warm right for a stack the
caller just declined"* is not one this crate can answer differently; what it
can do is say whether the answer is acceptable, and it is: nothing was
spoken on that connection, it is indistinguishable from the one an ordinary
TCP request would have left behind, and the pool's own idle policy governs
it from there.

The QUIC side inherits the cost `hclient_h3::staged` wrote down and
explicitly handed to the race — *"a declined QUIC connection goes on sending
its `DEFAULT_KEEP_ALIVE` PING every five seconds for as long as the
transport lives"*. It is inherited unchanged, and it is milder than that
sentence suggests **here**, because the race only ever declines a QUIC
connection that *succeeded*, which is a connection to an origin that speaks
HTTP/3 and that the next request there will want.

### A QUIC arm that loses the race teaches the failure memory

This is the decision, and it widens what `H3Failures` means by a deliberate
hair: it held *"an `H3::connect` failed"* and now holds *"HTTP/3 did not
produce a connection in time to be worth using"*.

The reason is §7.7 item 4: without it, the head start is paid again on every
request to a blocked origin, which is the cost that made the race not worth
building. The reason it is not an over-reach is the arithmetic — a QUIC
handshake is one round trip where TCP-plus-TLS-1.3 is two, so an arm still
connecting when a TCP connect that started a whole head start later has
finished is not a slow HTTP/3, it is one that is not getting through.

**A QUIC arm that wins teaches it nothing**, and the pair is the decision:
either half alone reads as an accident.
`a_quic_arm_that_lost_the_race_teaches_the_memory` and
`a_quic_arm_that_won_teaches_it_nothing` are the two, and `R6` and `R7` are
the two mutations that say so — the second of which (teach on every race)
passes the first test.

The cost of being wrong is the one `failures.rs` already names: HTTP/3 held
off at that origin for `H3_FAILURE_TTL`, after which the next request races
again with the record and the advertisement exactly as they were.

---

## 5. Decision 2 — the budget, and the part of it nothing can witness

§7.5's arithmetic is implemented as written: the QUIC arm carries the
caller's `Timeouts::connect`, the hedge carries `C − H` so both arms hold the
same deadline measured from the same instant, and the routed request carries
`C −` what the race spent, because the race *is* the connect phase and is
charged for once.

**`H < C` is a precondition and it is met by not racing.** A caller who sets
`connect: Some(100ms)` against a 250 ms head start has set a bound with no
room for two connects; the hedge does not run and the request takes the
sequential path, which is exactly the behaviour this crate had before the
race existed. Refusing the *request* would be worse than the thing it
replaces, so §7.5's *"refused or documented"* is answered with documented —
and pinned, in
`a_head_start_that_does_not_fit_the_bound_leaves_the_sequential_fallback`.

### What the mutation run found, and it is worth more than the code

**Two of the three budget statements are provably unwitnessable**, and the
proof is the same for both: **the QUIC arm carries `C` from an earlier start
than any hedge, so it always reaches its deadline first, and the race ends
when it does.**

- `C2` — the hedge is handed the caller's whole bound instead of `C − H`.
  Survives. A hedge that started at `H` with a bound of `C` would expire at
  `H + C`, and the race has ended at `C` with the QUIC arm's failure.
- `C3` — `Room::None` is ignored and a race is set up anyway. Survives. The
  hedge sleeps for `H ≥ C`, and the QUIC arm fails at `C` before the sleep
  is over, so the hedge never connects and the outcome is byte-for-byte the
  sequential path.

Both were predicted before the run and both behaved as predicted, which is
the only reason they are recorded as findings rather than as dead code.
**They are kept**, and the argument for keeping them is the one the
observation itself makes: *the budget rule is currently enforced by
`hclient-h3`'s honouring of `Timeouts::connect`, not by this crate.* A
coupling that strong and that unstated is exactly what a locally written
rule is for, and `hclient-h3` is a crate this workstream may not edit.

**The third statement is witnessed**, and getting a witness for it needed a
new fixture. `R11` — the race is not charged against `Timeouts::connect` —
is killed by `the_race_spends_the_connect_bound_once_when_both_arms_fail`,
which needs *both* arms to fail so that the request reaches the end of the
race with nothing left. The QUIC half was already available
(`Quic::BlackHole`); the TCP half is new — `Tcp::Rejecting`, a listener that
counts its accept and closes without a byte, so the client's TLS handshake
fails causally in one round trip. It is `Quic::Rejecting`'s shape one
protocol over, and it earns a second kill on the way: `R12`, a hedge whose
failure ends the race instead of leaving the QUIC arm to finish alone.

---

## 6. The shape the seam turned out to have, and the finding in it

**Neither arm can be given the caller's request.** Both `StagedConnect`
traits take it **by value** and hand it back only through `Refused` — that
is, only when the connect *fails*. A race has to be able to abandon an arm
that has neither failed nor finished, and an arm holding the caller's
request cannot be abandoned, because the request goes with it.

So the arms are handed a **probe**: the same method, URI, version, headers
and extensions, and a body that is empty and agrees about `retry_kind`. The
last clause is the only non-obvious part and it is not decoration —
`hclient-h3`'s early-data admission reads `RequestBody::retry_kind()`, and
that answer is a field of its `PoolKey`, so a probe that disagreed would
connect under one key and the request would look under another.

What follows is the shape of the whole thing:

> **The race's product is two warm connections and a decision.** The caller's
> request is sent afterwards, once, over the winning stack, through the
> ordinary routing — and the hand-off from the winning arm to the request
> goes through the **pool** rather than through the handle.

That is worth stating plainly because `docs/connect-only-seam.md` §5
*rejects* "warm the pool" as the seam's shape, on the grounds that *"the
second call may still connect, so it reads `Timeouts::connect` off the same
request and applies it again"*. Both hold at once, and they are about
different things. The seam is right to hand back a handle: the handle is
what lets the race know **when** each arm is ready and drop the loser
deterministically, which a warmed pool cannot tell it. What the race then
cannot do is *spend* the handle, so it spends the connection the handle left
behind — under a bound that has been charged for the race, which is the half
that makes it honest.

### Three things this costs, all stated rather than fixed

1. **`hclient_native::StagedConnect::exchange` still has no consumer in this
   workspace.** `docs/v04-staged-connect.md` §6 recorded that honestly and
   said the thing that would use it was the race. The race uses
   `connect`— which is the half that carries the trait's whole argument —
   and routes the winner through `Prefetch::execute_prepared`, which also
   buys `Native::run`'s one retry against a pooled connection the peer has
   closed, a window this crate's own fixture has already been bitten by.
   `hclient_h3::StagedConnect::exchange` *is* consumed, through
   `Selecting::over_quic`, as it was before this work.
2. **The winning stack's connect is re-entered.** For QUIC that is a real
   address resolution plus a pool lookup (`H3::execute` resolves before it
   looks in the pool, which is the fact that made `H3` need a handle at
   all); for TCP it is a pool lookup and no I/O. Both are inside the charged
   bound. `the_head_start_keeps_the_hedge_from_firing_when_quic_answers`
   asserts `quic_tried == 1` and
   `the_connection_the_hedge_made_is_the_one_the_request_is_sent_on` asserts
   `tcp_accepted == 1`, so the re-entry is a pool hit and not a second
   connection — on both stacks, watched from both servers.
3. **DNS.** A raced request that falls back to TCP makes the hedge's own
   connector lookup as well as the routed request's. At an origin's default
   port that is up to two type-65 queries where the sequential fallback made
   one; away from it, none, because `hclient-native` does no discovery
   there. Same shape and same reason as `docs/v04-staged-connect.md` §3.4,
   and the default-port row is inferred there for the same reason it is
   inferred here: an unprivileged test process cannot bind 443.

---

## 7. The defect the first draft had, and what it cost

The first version had the race do its own routing: `raced` called
`over_quic` on a QUIC win and `execute_prepared` on a hedge win, and
`execute` called *both* `raced` and `over_quic`.

It was green on `-p hclient-select`. On `--workspace --all-features` two
tests **aborted with a stack overflow** — `hclient-select::alt_svc::
one_client_moves_itself_to_http_3_on_the_second_request` and
`hclient-select::choice::one_client_reaches_both_servers_depending_only_on_
the_record`, which are the two that go through `hclient::Client` rather than
calling `Transport::execute` directly.

`execute` is an `async fn`, so every future it may await is a field of one
state machine, and a second copy of the QUIC arm inside it is a second copy
of `H3::connect`, `H3::exchange` and `Native::execute_prepared`. Measured,
`size_of_val` of `Selecting::execute`'s future, debug build, same
instantiation:

| | bytes |
|---|---|
| before the race | 22,680 |
| first draft | **45,856** |
| as built | 23,592 |

The fix is what `Raced` is for: **the race hands back a decision, and the
routing happens once.** `Selecting::serve_quic` has exactly one call site for
each member, hedged or not, and the growth is 4 %.

Two things are worth carrying out of this. It was caught by running the
whole workspace rather than the crate, which is why `AGENTS.md` says to; and
the 2× is not a subtlety of this code but a property of `async fn` — a
routing decision written twice costs twice, at every level, and `Client`'s
own wrappers sit on top of whatever `execute` costs.

---

## 8. How it is checked

Eleven tests in `crates/hclient-select/tests/race.rs`, plus five unit tests
in `race.rs` itself. Every count is a **server's**, and every one that can be
is a delta across one hop.

| what | test |
|---|---|
| **the premise** — no head start, both stacks connect, one request | `with_no_head_start_both_stacks_connect_and_exactly_one_request_is_sent` |
| the head start keeps the hedge from firing | `the_head_start_keeps_the_hedge_from_firing_when_quic_answers` |
| **the feature** — a blocked origin costs a head start, not 30 s | `a_blocked_origin_is_answered_without_waiting_for_quinn_to_give_up` |
| the pool is the hand-off | `the_connection_the_hedge_made_is_the_one_the_request_is_sent_on` |
| genuinely optional | `the_race_is_off_until_it_is_asked_for` |
| a version demand is not hedged | `a_demand_for_http_3_is_not_hedged` |
| a request the record sent to TCP is not hedged | `a_request_the_record_sent_to_tcp_never_touches_the_hedge` |
| `H >= C` leaves the sequential fallback | `a_head_start_that_does_not_fit_the_bound_leaves_the_sequential_fallback` |
| the bound is spent once | `the_race_spends_the_connect_bound_once_when_both_arms_fail` |
| the memory learns from a lost race | `a_quic_arm_that_lost_the_race_teaches_the_memory` |
| …and nothing from a won one | `a_quic_arm_that_won_teaches_it_nothing` |

**One assertion in the file is on a clock**, and the margins are stated where
it is: a blocked origin must answer inside **5 s**, which is six times below
the 30.002–30.006 s that hop is measured to cost without the hedge and a
hundred times above the ~50 ms it costs with it. Everything else is a
counter.

The fixture gained two things, both in `tests/servers.rs`. A **`Quic::
BlackHole` that counts datagrams** — a hole has no endpoint, so it accepts
nothing however many `ClientHello`s it swallows, and counting is the only
way *"this hop did not try QUIC"* becomes a delta rather than an absence.
And **`Tcp::Rejecting`**, §5.

---

## 9. Mutation testing

Anchor **136 tests**, `cargo nextest run -p hclient-select --all-features
--no-fail-fast`, verified before the first mutation and again after every
restore. Each patch had to match exactly once or the mutation was not run;
the harness scores from the names of failing tests and refuses to score a
run whose total is not the anchor; restores are `git checkout` followed by
`os.utime`, because a restore that preserves mtime leaves cargo holding the
mutant; and the tests were committed before the first mutation.

**Fifteen applied: twelve killed, three survived — one designated control
and two predicted, both for the same reason (§5).**

| # | mutation | verdict | killed by |
|---|---|---|---|
| **R1** | the head start is not slept, so both arms start together | **killed** (3) | `the_head_start_keeps_the_hedge_from_firing_when_quic_answers`, `a_quic_arm_that_lost_the_race_teaches_the_memory`, `a_quic_arm_that_won_teaches_it_nothing` |
| **R2** | the race runs even where nobody asked for one | **killed** (3) | `race::the_race_is_off_until_it_is_asked_for`, and — unasked — `h3_failure::the_fallback_spends_what_is_left_of_the_connect_bound_and_no_more`, `dns_cost::a_request_chosen_onto_quic_at_the_default_port_asks_once` |
| **R3** | the race runs for a request that may not fall back | **killed** (1) | `a_demand_for_http_3_is_not_hedged` |
| **R4** | the losing arm is awaited rather than dropped | **killed** (3) | `a_blocked_origin_is_answered_without_waiting_for_quinn_to_give_up`, `a_quic_arm_that_lost_the_race_teaches_the_memory`, `the_connection_the_hedge_made_is_the_one_the_request_is_sent_on` |
| **R5** | the winning hedge's connection is never handed back to the pool | **killed** (1) | `the_connection_the_hedge_made_is_the_one_the_request_is_sent_on` |
| **R6** | a QUIC arm that lost the race teaches the memory nothing | **killed** (1) | `a_quic_arm_that_lost_the_race_teaches_the_memory` |
| **R7** | the memory is taught whenever a race runs, won or lost | **killed** (1) | `a_quic_arm_that_won_teaches_it_nothing` |
| **R8** | a probe's body says nothing about the caller's retry kind | **killed** (1) | `race::tests::a_probe_body_agrees_with_the_callers_about_retrying_and_nothing_else` |
| **R9** | a probe does not carry the caller's extensions | **killed** (2) | `race::tests::a_probe_carries_everything_a_connector_reads`, `the_race_spends_the_connect_bound_once_when_both_arms_fail` |
| **R10** | a race the hedge won still sends the request over QUIC | **killed** (4) | `the_race_is_off_until_it_is_asked_for`, `a_blocked_origin_is_answered_without_waiting_for_quinn_to_give_up`, `a_quic_arm_that_lost_the_race_teaches_the_memory`, `the_connection_the_hedge_made_is_the_one_the_request_is_sent_on` |
| **R11** | the race is not charged against `Timeouts::connect` | **killed** (1) | `the_race_spends_the_connect_bound_once_when_both_arms_fail` |
| **R12** | a hedge that failed ends the race | **killed** (1) | `the_race_spends_the_connect_bound_once_when_both_arms_fail` |
| **C1** | **CONTROL** — `Selecting`'s `Debug` stops reporting the hedge | **survived, as intended** (0) | nothing, and nothing should: no test asserts on that output and no code path reads it. Without a control, twelve kills would be indistinguishable from a harness that reports "killed" unconditionally |
| **C2** | the hedge arm is handed the caller's whole bound instead of `C − H` | **survived, as predicted** (0) | §5 — the QUIC arm holds the same deadline from an earlier start, so the race ends before a hedge bound of `C` could be exceeded |
| **C3** | `Room::None` is ignored and a race is set up anyway | **survived, as predicted** (0) | §5 — the hedge sleeps for `H ≥ C` and the QUIC arm fails at `C` first, so the outcome is the sequential path |

Three are worth reading twice.

**R2's second and third killers were not written for it.**
`dns_cost::a_request_chosen_onto_quic_at_the_default_port_asks_once` is the
same test that caught the sequential fallback's budget error
(`docs/v04-staged-connect.md` §5, S6) and it catches a race that runs
uninvited for the same reason: an extra connect makes an extra type-65
query. A DNS test noticing a policy change is now the third instance of that
shape in this workstream.

**R9 was expected to be a unit-test kill and was not only that.** A probe
without the caller's extensions has no `Timeouts`, so the QUIC arm becomes
unbounded and the hop that must end in `Timeout(Connect)` runs into quinn's
30 s instead — caught by the 20 s harness bound. The probe carrying the
extensions is what carries the bound.

**R7 is the half that keeps R6 from being an accident.** A memory taught on
every race passes `a_quic_arm_that_lost_the_race_teaches_the_memory`; only
`a_quic_arm_that_won_teaches_it_nothing` separates *"HTTP/3 did not get
through"* from *"HTTP/3 was raced"*.

---

## 10. What is not verified

- **The head start's value.** 250 ms is chosen against the success side, and
  the success side is a **round trip** this fixture does not have. Nothing
  here measures a race on a path with an RTT, and nothing here can: §7.4's
  origin-keyed RTT store is still the honest form and is still unbuilt.
- **Whether a QUIC arm that loses a race is really a QUIC arm that is not
  getting through.** §4's argument is arithmetic about round trips, not a
  measurement. The one thing loopback can say about it is the opposite of
  reassuring — §3's flip means TCP wins on loopback *without* being faster
  on any real path — which is why the head start exists and why it is 250 ms
  and not 50.
- **The two survived budget mutations, §5.** They are recorded as findings
  because they were predicted, but "predicted and confirmed" is a statement
  about this workspace's fixtures. If `hclient-h3` ever stopped applying
  `Timeouts::connect` to its whole connect, C2 and C3 would become live and
  no test here would notice.
- **The 30 s a real UDP block costs.** Unchanged from
  `docs/v04-staged-connect.md` §6: nothing here measures it, it is
  §7.3's number, and it is quinn's rather than ours. The 5 s assertion in
  `a_blocked_origin_is_answered_without_waiting_for_quinn_to_give_up`
  *depends* on it being far larger than 5 s, and that dependency is stated
  in the test.
- **The default-port DNS row**, §6 item 3. Inferred.
- **Concurrency.** Every test here makes one request at a time. Two raced
  requests to the same origin at once would each stage two connects, and
  nothing says what the two pools do with four connections. The sequential
  fallback has the same gap and it is not new.
- **`hclient_native::StagedConnect::exchange` still has no consumer**, §6
  item 1. The race was the thing that was supposed to give it one and it did
  not, for a reason that is about the trait rather than about the race: a
  handle whose request cannot be replaced can only be spent by a caller that
  knew which request it wanted before it connected, and a racer does not.
