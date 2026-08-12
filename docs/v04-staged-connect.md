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
| `crates/http-ng-native/tests/staged.rs` | 6 | a counting TCP server: connections accepted and request heads read |
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

Anchor **MUTATION_ANCHOR tests**, `cargo nextest run -p http-ng-native -p
http-ng-h3 -p http-ng-select --all-features`, verified before the run and
again after every restore. Each patch had to match exactly once or the
mutation was not run. The harness scores from the **names** of failing tests
and refuses to score a run where the count of names disagrees with nextest's
own `Summary`; restores are `git checkout` followed by `os.utime`, because a
restore that preserves mtime leaves cargo holding the mutant, and the tests
were committed before the first mutation, because a restore that deletes an
uncommitted test invalidates the run that used it.

MUTATION_TABLE

## 6. What is not verified

- **The 30 s a real UDP block costs.** Nothing here measures it. It is
  `docs/v04-w1-acceptance.md` §7.3's number, it is quinn's
  `max_idle_timeout` rather than anything of ours, and it is a
  `Timeouts::connect` question rather than this memory's.
- **The default-port DNS row** — §3.4. Inferred, for §9.6's reason.
- **`http-ng-native`'s staged pair has no consumer in this workspace.** It
  is implemented, tested and green, and the thing that would use it is the
  race, which is not built. Two implementations is not decoration here: it
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
- **A `Staged` whose connection dies while the caller holds it.** The
  behaviour is decided (§2.3) and no test produces the window, because
  producing it means closing a connection between a checkout and a write.
- **The race is still not built**, and nothing here unblocks it beyond what
  §7.7 already said: item 3 is discharged, items 1 (an origin-keyed RTT
  store), 2 (a budget rule that subtracts — now built for the *sequential*
  case, in `spend_connect_budget`, which is the same arithmetic one shape
  simpler) and 4 (a failure memory — built, here) are where they were.
