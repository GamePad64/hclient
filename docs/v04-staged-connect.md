# The staged connect, built — and the first thing it was built for

`docs/connect-only-seam.md` is the investigation. This is what came of
building it: the seam, an implementation on each backend that has a
connector, and the consumer that made it worth having — Alt-Svc's negative
half in `http-ng-select`, the memory of *"HTTP/3 did not get through to
this origin"*.

Nothing here re-argues the investigation. What it records is the three
things that document left open and this work had to decide, the shape the
two implementations actually took, the scope decision the negative half
needed, and what is still not verified.

## 1. The shape

`connect` → an opaque handle carrying its own way back into the pool →
`exchange`.

```rust
// http-ng-native
pub trait StagedConnect: Transport {
    type Staged;
    fn connect(&self, prepared: Prepared) -> impl Future<Output = Result<Self::Staged, Refused>>;
    fn exchange(&self, staged: Self::Staged)
        -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>;
}

// http-ng-h3 — the same shape, declared separately
pub trait StagedConnect: Transport {
    type Staged;
    fn connect(&self, req: http::Request<RequestBody>)
        -> impl Future<Output = Result<Self::Staged, Refused>>;
    fn exchange(&self, staged: Self::Staged)
        -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>;
}
```

**Not a method on `Transport`**, for §4's reason and not by family
resemblance. `wasi:http` 0.3's client interface is one function with no
connection resource anywhere in the WIT, so `http-ng-wasi` could answer
nothing; the browser's one connect-shaped API is a `<link rel="preconnect">`
hint with no handle, no readiness signal and no way to bind a later
`fetch()` to it, so `http-ng-fetch` would be implementing *"ask nicely, then
return `Ok(())`"*. Two of four backends `Unsupported`, one of them
dishonestly — the shape this workspace refuses.

**One trait per crate**, as §4 decided, and building it produced the
concrete reason the document could only predict: the two do not agree on
what `connect` takes. `Native` takes a `Prepared`, because it has a record
lookup worth composing with; `H3` takes a request, because it has none.
A shared trait would have had to erase that difference or carry both.

**`Refused` hands the request back.** It is `established::Failed::NotSent`'s
shape, reached for the same reason: a caller that is going to send this
request somewhere else has to still have it, and a caller that is not can
take the error. This is what makes the negative half's fallback *not* a
retry — there is no second request, because the first one never left.

### 1.1 What each backend could honestly implement

| | `http-ng-native` | `http-ng-h3` | `http-ng-wasi` | `http-ng-fetch` |
|---|---|---|---|---|
| connect as a phase | yes — `run`'s steps 1 and 2 | yes — resolve, then checkout | **no** — one `send` function, no connection resource in the WIT | **no** — `preconnect` is a hint |
| what the handle owns | the connection, taken out of the pool | a **claim** on a connection the pool also holds | — | — |
| a dropped handle | checks in (a `Drop` impl) | nothing to do; the pool already has it | — | — |
| can it bound the phase | yes | yes | **yes, and cannot reach it** — `request-options.set-connect-timeout` | no; `timeouts.connect = false` |

The `wasi:http` row is the one worth keeping: a backend can be able to
**bound** a phase it can never **reach**, and a seam that confused the two
would look satisfiable there and not be.

## 2. The three open questions

### 2.1 Does `H3` need a handle at all — §9's *"a handle may be more than is required"*

**Yes, and the reason is the bound rather than the connection.**

The weakest form is a success/failure signal: `connect() -> Result<(), _>`,
the connection stays in `H3`'s pool, and the caller then makes an ordinary
`Transport::execute` that finds it there. That is refused, and not because
of ownership: **`H3::execute` resolves the origin's address before it looks
in the pool** — a real lookup, through the caller's `Resolve` — and it runs
that lookup, and any dial the pool cannot save it from, inside
`Timeouts::connect`. A second call reads the same bound off the same request
and can spend it again. That is the criterion §6 chose the handle for,
applied to a stack §6 was not looking at.

**What the handle turned out to be is not what it is on `Native`.**
`H3::connect` builds an h3 client on every connection it makes and spawns
that client's driver **before** it has anything to hand back, and `checkout`
inserts the result into the pool before its caller sees it. There is no
state in which `H3` holds a connection nobody has claimed — which is
exactly why a connect-only entry point could never have served WebTransport
(`docs/v04-w2-webtransport.md` §4b, and `docs/rt-quinn-extraction.md` §5).

So `http_ng_h3::Staged` is a **claim on a connection the pool also holds**,
where `http_ng_native::Staged` *owns* the connection it took out. Both
satisfy the property the seam exists for, because that property is about
the second call's code path and not about who owns the socket. Letting
`H3` answer for itself rather than shaping it to `Native`'s answer is what
produced that sentence.

### 2.2 A losing arm that *finishes* connecting — §9's second open question

§7.6 measured only the mid-handshake drop. The general case here is a
**handle nobody spends**, and it is now defined on both stacks, differently,
to the same observable end:

- **`http-ng-native`**: `Staged` has a `Drop` that checks the connection in.
  A connection made for a request that went elsewhere is a **warm**
  connection rather than a closed socket. Nothing was spoken on it, so
  nothing makes it unfit; `is_reusable` polls it at the next checkout as it
  polls every other entry. With `Native::without_pool()` there is no
  check-in and the drop closes the socket — which is the control that
  attributes the reuse to the pool rather than to the fixture.
- **`http-ng-h3`**: no `Drop` at all is needed, because `checkout` pooled
  the connection before the caller saw it. Dropping the handle drops a
  `SendRequest` clone.

Both are tested by the same test in each crate — connect, drop, send, and
count one accepted connection at the server.

**The cost that is not free is stated rather than fixed**: a declined QUIC
connection goes on sending its `DEFAULT_KEEP_ALIVE` PING every five seconds
for as long as the transport lives. That is the trade every pooled HTTP/3
connection makes; what is new is only that the caller may not have wanted
this one. It matters to a **race**, which is not built, and not to the
consumer here, which connects on the arm it intends to use.

### 2.3 What the pool does with a connection produced outside `run` — §7

It works, and the handle carrying its own `PoolKey` is why.

`Pool`, `PoolKey`, `CheckIn` and `Established` are all `pub(crate)`, so *"a
connection produced outside `http-ng-native`"* is not expressible — and
`Staged` keeps it that way: produced by `connect`, consumed by `exchange`,
and the fact that a caller holds it in between changes nothing about who
made it. The check-in is minted in `connect`, from the key that connect
computed, because the negotiated protocol is known only at the end of it;
`exchange` recomputing one would be two places holding one fact.

Three of the pool's four opinions were already satisfied by the shape. The
fourth — §7's *"it must be allowed to answer: I already had one"* — is
built and counted: a staged connect at an origin the pool is already
serving costs no connection.

**One thing the pool wanted that `exchange` does not give it.**
`Native::run` retries once when hyper hands the request back unsent — a
pooled connection the server had closed. `exchange` does not, and the reason
is §6's property rather than an omission: a retry means another pooled
candidate or a fresh dial, and the dial is the code path that has to be
absent. Half a retry — pool-only, never dialling — would be the rule with an
exception. So a `Staged` whose connection died in the caller's window costs
that request, where `execute` would have opened another. It is paid only on
the staged path, and it is the reason `connect` is worth calling as late as
a caller can manage.

## 3. The consumer: Alt-Svc's negative half

`docs/v04-w1-acceptance.md` §9.3 recorded it as unbuilt behind two blockers.
Both are discharged, and neither by the race.

**Blocker 1 — *"without a fallback it degrades the caller rather than
protecting them"*, because the fallback would be *"request-level retry with
a `RequestBody::retry_kind()` condition on it"*.** With a staged connect it
is sequential and it is not a retry: `Selecting` asks `H3` to connect, and
where that fails it routes the request — untouched, unsent, never handed to
a transport — over TCP. `http-ng-native`'s sentence is true of it verbatim:
*this is not a second request, it is the first one, which never left.* The
memory therefore costs a **slow** first request per window per origin, not a
failed one.

**Blocker 2 — *"loopback cannot produce the failure … the arm under test
would be a multi-second handshake timeout"*.** The right worry about the
wrong premise. What the memory records is *the connect failed*, and it does
not read the reason. A real `quinn` server offering an ALPN this client will
not accept fails **causally**, in one round trip, and produces exactly that
fact. The black hole is still in the fixture and is used once — where a test
needs a bound to be *spent* rather than a connect to fail.

### 3.1 Where the veto sits, and why it is not a fourth tier

The record and the cache answer *"does this origin speak HTTP/3"*. Only the
origin can answer that, and a failed connect of ours is not evidence against
it — a network blocking UDP/443 says nothing about the server behind it. So
the memory is a **veto applied after both tiers**: it does not overrule the
record, does not remove the advertisement, and when the window closes the
very next request tries QUIC again with both exactly as they were.

Both tiers go through one function, which is the point of it being one: an
origin publishing a record that lists `h3` on a network that blocks UDP/443
is precisely the case a veto covering only `Alt-Svc` would leave paying a
failed connect on every request.

`RequireVersion(HTTP_3)` is answered **before** the veto, as it is before
the resolver and the cache — one rule, in one place — and it does not fall
back: a caller who demanded HTTP/3 gets the connect error rather than
`Native`'s `VersionNotAvailable` in place of the real answer.

### 3.2 Scope, and the asymmetry with the cache next door

The positive half's scope decision is `Selecting::network_changed()`: RFC
7838 §2.2 conditions its own SHOULD on *"information about network state"*
that a `Transport` does not have, so nothing is persisted and the caller —
who can see what the transport cannot — is the only entry point. **A failure
memory has exactly the same problem**, and gets the same answer: it lives on
one `Selecting`, is never written to disk, and `network_changed()` is what
ends a suppression early.

**What differs is what a network change does to each, and that is the
decision.** The advertisement cache keeps `persist=1` entries, because that
flag is the *origin's* claim that what it advertised is a property of the
origin rather than of the path. Nothing says that about a failure:
*"UDP/443 did not get through"* is a fact about the network alone, no peer
ever asked us to carry it, and it is exactly the entry a network change
makes certainly wrong. So the failure memory clears **everything**, with no
`persist` notion to carry.

The direction of being wrong differs too, and it is why a fixed window is
honest here where a fixed lifetime for an HTTPS record was not (§9.1).
Remembering too long costs HTTP/3 at an origin that could now serve it;
forgetting too soon costs one bounded connect attempt, after which the
request still succeeds over TCP. Both are bounded, both self-correct, and
neither is a wrong answer — where an invented TTL for someone else's DNS
answer drifts silently against the resolver's.

**What counts as a failure** is any failure of `H3::connect`, and the reason
is deliberately not read. Every reason a connect fails is a reason not to
spend the next request's time trying again at once, and a list of admitted
`ErrorKind`s would be a third place to keep in step with two crates' error
vocabularies, for a decision whose cost of being wrong is one missed chance
to speak HTTP/3. A failure **after** the connect is not one: the staged seam
draws that line for us, since `exchange`'s errors never reach the memory.

### 3.3 The budget, which is the part that had to be built

`Timeouts::connect` is one bound for one request, and a sequential fallback
is the plainest way to double it: the QUIC arm spends it, and the TCP arm
reads the same field off the same request and spends it again. *"A bound a
server can double by answering `425` is not a bound"* — `Client`'s rule,
same arithmetic.

So the request handed to the TCP member carries what is **left** of the
caller's bound, and where nothing is left the QUIC failure stands as the
answer. That is §7.5's precondition met by refusing rather than by silently
degrading, and it is never worse than the behaviour it replaces: before
this, a QUIC origin that could not be reached simply failed the request.

It is a rewrite of the caller's extension, which this transport otherwise
never does. The value written is not a policy of ours — it is the caller's
own bound minus what has been spent against it.

### 3.4 What it costs

One extra type-65 query, **on the fallback path only**, and paid by the
request that discovers the failure and by no other — which is what the
memory is for. The record the request's `Prepared` carried was consumed when
the QUIC arm took the request, and there is deliberately no way to pair a
record with a request it was not fetched for, so the TCP member does its own
lookup exactly as it does for a request that never met this crate.

Measured at the fixture's port it is **zero** extra, because
`http-ng-native` does no discovery away from an origin's default port. The
default-port row is inferred rather than measured, for §9.6's own reason: an
unprivileged test process cannot put a server on 443.

## 4. How it is checked

Every count below is a **server's**, and every assertion is a delta across
one hop.

| file | tests | what it watches |
|---|---|---|
| `crates/http-ng-native/tests/staged.rs` | 7 | a counting TCP server: connections accepted and request heads read |
| `crates/http-ng-h3/tests/staged.rs` | 5 | the h3 fixture's `accepted`/`requests` counters over real QUIC |
| `crates/http-ng-select/tests/h3_failure.rs` | 6 | the two real servers behind one authority, with a QUIC arm that refuses |
| `crates/http-ng-select/tests/failure_memory.rs` | 10 | the memory itself, `now` handed in |

The select fixture gained two things. **`Quic::Rejecting`** — a real quinn
server offering an ALPN this client will not accept, so a connect fails
causally in one round trip instead of after quinn's 30 s
`max_idle_timeout` — and **`quic_attempted`**, bumped before the handshake
is awaited, because *"the second request did not go to QUIC"* and *"QUIC was
never chosen"* are the same number on a counter of answered requests. Every
negative-half test reads it as a delta: a hop that tried and failed moves
it; a hop the memory held back does not.

`Quic::BlackHole` — a bound, silent UDP socket, which is a `DROP` and not a
`REJECT` (§7.1's mechanism) — is used in exactly one place: the budget test,
where the QUIC arm has to *spend* the bound rather than fail.

**The budget test is an A/B and neither arm asserts on a duration.** Same
bound, same fallback, same two servers; what differs is how much of the
bound the QUIC arm spends. A refused handshake spends ~1 ms, so the TCP arm
runs and answers. A black hole spends all of it, so the TCP server must
accept **nothing** and the answer is the QUIC arm's own `Timeout(Connect)`.
A fallback handed a fresh copy of the bound passes the first arm and fails
the second.

## 5. Mutation testing

Anchor **416 tests**, `cargo nextest run -p http-ng-native -p
http-ng-h3 -p http-ng-select --all-features`, verified before the run and
again after every restore. Each patch had to match exactly once or the
mutation was not run. The harness scores from the **names** of failing tests
and refuses to score a run where the count of names disagrees with nextest's
own `Summary`; restores are `git checkout` followed by `os.utime`, because a
restore that preserves mtime leaves cargo holding the mutant, and the tests
were committed before the first mutation, because a restore that deletes an
uncommitted test invalidates the run that used it.

**Twenty applied: nineteen killed, one control survived as intended, none
survived unintentionally.** The first run of them was scored wrong and is
worth recording: without `--no-fail-fast` nextest stops at the first failure
and reports a partial total, so eleven kills came back "unscorable" against
an anchor of 415. The harness caught that itself — it refuses a run whose
total is not the anchor's — and the second run carries the flag.

| # | mutation | verdict | killed by |
|---|---|---|---|
| **N1** | a dropped handle discards its connection instead of checking it in | **killed** (1) | `native::staged::a_handle_nobody_spends_leaves_a_warm_connection` |
| **N2** | the staged connect never looks in the pool and always dials | **killed** (2) | `a_staged_connect_finds_a_pooled_connection_rather_than_dialling`, `a_staged_exchange_returns_its_connection_to_the_pool` |
| **N3** | the handle from the pool is minted with no way home | **killed** (1) | `a_staged_exchange_returns_its_connection_to_the_pool` |
| **N4** | a fresh connection is staged with no check-in | **killed** (2) | `a_handle_nobody_spends_leaves_a_warm_connection`, `a_staged_exchange_returns_its_connection_to_the_pool` |
| **N5** | **CONTROL** — `Staged`'s `Debug` reports nothing instead of what it holds | **survived, as intended** (0) | nothing, and nothing should: no test asserts on that `Debug` output and no code path reads it. Without a control, nineteen kills would be indistinguishable from a harness that reports "killed" unconditionally |
| **H1** | the staged connect emits no `Connected`/`Reused` event | **killed** (13) | every `http-ng-h3::hooks` arm, plus `hooks_cost::the_same_two_requests_with_a_hook_do_read_it` |
| **H2** | the exchange never replays a request whose early data was refused | **killed** (2) | `h3::live::a_rejected_0_rtt_request_is_replayed_and_the_caller_never_sees_it`, `h3::hooks::a_replayed_0_rtt_request_reports_one_head_and_one_connection` |
| **H3** | the version demand is not answered before the connect | **killed** (3) | both `h3::require_version` arms, plus `h3::staged::a_version_demand_this_stack_cannot_meet_hands_the_request_back` |
| **H4** | the scheme is not checked, so `http://` reaches the QUIC dial | **killed** (2) | `h3::live::plaintext_http_is_refused_rather_than_silently_upgraded`, `h3::staged::a_refused_connect_hands_the_request_back` |
| **S1** | the failure is never recorded | **killed** (3) | `an_origin_whose_quic_connect_failed_is_not_tried_again`, `a_record_that_offers_h3_is_vetoed_by_a_failure_just_the_same`, `a_reported_network_change_lets_a_failed_origin_be_tried_again` |
| **S2** | the failure is recorded and never read | **killed** (3) | the same three |
| **S3** | the veto is looked up for the wrong origin — the scheme's default port rather than this one | **killed** (3) | the same three |
| **S4** | the failure is recorded against the wrong origin | **killed** (3) | the same three |
| **S5** | the memory never expires | **killed** (4) | `failure_memory::a_failure_lives_exactly_as_long_as_the_ttl_says`, `a_lapsed_entry_is_forgotten_and_does_not_come_back`, `a_second_failure_restarts_the_window`, `the_window_starts_when_the_connect_failed` |
| **S6** | `Timeouts::connect` is handed to the fallback untouched, so it is spent twice | **killed** (2) | `h3_failure::the_fallback_spends_what_is_left_of_the_connect_bound_and_no_more`, and — unexpectedly — `dns_cost::a_request_chosen_onto_quic_at_the_default_port_asks_once` |
| **S7** | a reported network change does not clear the failure memory | **killed** (1) | `a_reported_network_change_lets_a_failed_origin_be_tried_again` |
| **S8** | the veto also holds back a request that demands HTTP/3 | **killed** (1) | `a_demand_for_http_3_does_not_fall_back_but_still_teaches_the_memory` |
| **S9** | a demand for HTTP/3 falls back to TCP like everything else | **killed** (1) | the same one |

Three are worth reading twice.

**N3 survived the first time it was run**, and the test that now kills it
was written because of that. `a_staged_connect_finds_a_pooled_connection_
rather_than_dialling` passes against a handle minted with no way home,
because it never asks what happened to the connection *afterwards*. The
check-in is minted at two sites — the pooled branch and the fresh one — so
the new test covers both, and N3 and N4 are the pair that says so.

**S6's second killer was not written for it.**
`dns_cost::a_request_chosen_onto_quic_at_the_default_port_asks_once` sets a
300 ms connect bound and points at an origin with no QUIC server; the bound
is spent, nothing is left, and the fallback does not run — so the count
stays at one. Hand the fallback a fresh copy of the bound and it runs,
`http-ng-native`'s connector makes its own type-65 query, and the count
becomes two. A DNS test noticing a timeout arithmetic error is the same
shape as `docs/v04-w1-acceptance.md` §9.8's M22, and it is the second
witness the budget rule deserved.

**S8 and S9 are the two directions of one decision** and either alone reads
as an accident: a demand for HTTP/3 is not held back by the memory, and is
not sent over TCP when the connect fails. The hop that kills S8 was added
after the first mutation run, because the test as first written put the
demanded request first, where nothing was suppressed yet.

## 6. What is not verified

- **The 30 s a real UDP block costs.** Nothing here measures it. It is
  `docs/v04-w1-acceptance.md` §7.3's number, it is quinn's
  `max_idle_timeout` rather than anything of ours, and it is a
  `Timeouts::connect` question rather than this memory's.
- **The default-port DNS row** — §3.4. Inferred, for §9.6's reason.
- **`http-ng-native`'s staged pair has no consumer for its `exchange`**,
  and the race — since built, `docs/v04-race.md` — did not give it one.
  It uses `connect`, which is the half that carries the trait's whole
  argument, and then routes the winner through `Prefetch::execute_prepared`.
  The reason is a fact about the trait rather than about the race: `connect`
  takes the request **by value** and hands it back only through `Refused`,
  so an arm that has neither failed nor finished cannot be abandoned without
  the request going with it — and a racer must be able to abandon one. So
  both arms are handed a *probe*, and the winner's connection is spent
  through the pool. `docs/v04-race.md` §6.

  The paragraph below is what this bullet said before that, and it stands
  for `connect`: Two implementations is not decoration here: it
  is what shows the trait was not shaped to one member — the same evidence
  `WebSocketConnect` got when the browser fitted it unchanged — and it is
  what answered §10's *"that shape H compiles"* and §7's questions about
  the pool. But a seam whose second implementation has no caller is a
  thing to watch, and it is recorded rather than dressed up.
- **The absence assertions in the two `staged.rs` files** — *"the connect
  sent no request"* — are taken after a bounded wait for the server's
  accept, so the server thread is known to have run past `accept`. What
  they cannot rule out is a write the server has not yet read. The second
  half of each test is what makes them worth reading: the same counter that
  stays at zero across the connect reaches one across the exchange.
- ~~**One flake, seen once and not since.**~~ **A defect, and an old one** —
  [`docs/v04-h3-0rtt-control-stream.md`](v04-h3-0rtt-control-stream.md). In
  the first mutation pass
  `http-ng-h3::hooks::a_replayed_0_rtt_request_reports_one_head_and_one_
  connection` failed under a mutation that cannot reach it — `S1`, in
  `http-ng-select`. It passed 15 times in isolation afterwards and did not
  recur in the 21 full-suite runs of the second pass, so it read as a
  load-dependent flake rather than a defect this work introduced; but
  `H3::execute` was refactored into `stage` + `finish` in this branch, and
  "not reproduced" is weaker than "not there". Both halves of that turned
  out to be the right instinct: it was not this branch's, and it was not a
  flake — a 0-RTT rejection landing on h3's control stream instead of on the
  request stream, which nothing had ever handled.
- **A `Staged` whose connection dies while the caller holds it.** The
  behaviour is decided (§2.3) and no test produces the window, because
  producing it means closing a connection between a checkout and a write.
- ~~**The race is still not built**~~ — **built**,
  [`docs/v04-race.md`](v04-race.md), out of exactly this seam and out of the
  half of it §2.2 below said mattered *"to a race, which is not built"*.
  Of §7.7's items: 3 was discharged here, 4 is this memory, 2 is built and
  two thirds of it turn out to be unwitnessable, and **1 — an origin-keyed
  RTT store — is still where it was**, so the head start is still a constant.

  §2.2's sentence about a declined QUIC connection pinging for ever now has
  its subject, and is milder than it reads there: the race only declines a
  QUIC connection that **succeeded**, which is one to an origin that speaks
  HTTP/3 and that the next request will want.


## ~~The flake this branch may have introduced~~ — settled, and it was neither

**It was a real defect, older than this branch by 292 commits**, and
[`docs/v04-h3-0rtt-control-stream.md`](v04-h3-0rtt-control-stream.md) is the
capture, the cause, the deterministic reproduction and the fix. The section
below is what stood here before that, kept because the suspicion it records
was right and its suspect was not.

In one line: h3 opens its control stream **in early data** on a connection
`into_0rtt` handed back, and a server refusing that early data while
`h3::client::builder().build(..)` is still writing SETTINGS makes RFC 9114
§6.2.1 close the QUIC connection — so the rejection reached the caller as
`ErrorKind::Connect`, which is the one outcome `crate::early` says cannot
happen. `H3::connect` now dials once more without the shortcut. `H3::execute`'s
`stage`/`finish` split moved the call site and changed nothing inside it.

---

`http-ng-h3::hooks::a_replayed_0_rtt_request_reports_one_head_and_one_connection`
failed once during the branch's own mutation run, under a mutation that
cannot reach it, and once again during the merge review — in a full
workspace run, which is the condition CI uses.

It did not reproduce afterwards: **0 failures in 20 isolated runs, 15 runs
of the whole `http-ng-h3` suite, and 6 full-workspace runs**, all
immediately after the observed failure and on the same machine.

Recorded rather than dismissed, and with the fact that matters: **`H3::execute`
was refactored in this branch** into `stage` + `finish`, so "not reproduced"
is a weaker statement than "not there". It shares a profile with the
`zero_rtt` flake recorded in `docs/v03-acceptance.md` — rare,
load-dependent, and invisible in isolation — and that one turned out to be
two real bugs rather than a fixture's impatience.

What would settle it is what settled the pool flake: capture the failure
with its own output rather than a `FAIL` line. Nobody has yet.

**Somebody has now**, and the recipe was the one this section was pointing
at: eight concurrent runs of the whole suite with every run's output kept —
2 failures in 277 — rather than more runs of one test.
