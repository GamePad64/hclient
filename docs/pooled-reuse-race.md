# The pooled-reuse race, reproduced — and the half of it that was ours

`h1.rs` has called this "the residual race the checkout poll cannot
close" since v0.2 W2. `pool.rs` says the retry covers it *"for as long as
hyper can still hand the request back"*. `docs/v02-acceptance.md` calls
it "a nanosecond race" and records it as deliberately not closed.
`docs/nagle-and-nodelay.md` §6 names the two fixes it would take and
rejects both on cost. `docs/v04-acceptance.md` says it is unchanged.

All of that is about a race between a server's `FIN` and a client's
write, and all of it is true. What none of it says is that the window
had **three** distinguishable points in it rather than two, and that
this crate had been reporting the middle one — a request not one byte of
which had reached the wire — as `Failed::Sent`.

The short version: **the reproduction is deterministic, in two
independent forms; the middle point is now retried; the far point is
unchanged and still surfaces to the caller; and the two expensive fixes
are still rejected, now for sharper reasons than cost.**

---

## 1. What the window actually contains

A pooled HTTP/1 connection is inert — nothing polls it between requests
(`pool.rs`'s module doc). The peer's `FIN` therefore sits unread until
something looks, and there are exactly four looks between a request
arriving and its bytes reaching the socket. Two of them are this crate's
and two are hyper's:

| # | who looks | what it can do about it |
|---|---|---|
| 0 | `Native::checkout` → `h1::is_reusable` | drop this candidate, walk to the next, or dial |
| 1 | `h1::exchange`'s poll before `try_send_request` | hand the request straight back: it is still ours |
| 2 | hyper's first read, inside `Connection::poll` | *(see below)* |
| 3 | hyper's read after it has written the request | nothing: the bytes are gone |

Point 2 is the one that was mis-read. `try_send_request` puts the
request into hyper's dispatch queue **eagerly**, at the moment the
future is created rather than when it is first polled
(`client/conn/http1.rs:243`, `self.dispatch.try_send(req)` before the
`async move` block). From then on the request is hyper's. But hyper's
`poll_loop` calls `poll_read` **before** `poll_write` on every turn
(`proto/h1/dispatch.rs:172-174`), and a graceful EOF on an idle
connection sets `close_read`, which makes `can_write_head()` false
(`proto/h1/conn.rs:573-576` — for a client `should_read_first()` is
`false`, so a closed read side closes the write side to new heads). So
at point 2 the dispatcher **refuses to write**, reaches
`Dispatched::Shutdown`, and completes with `Ok(())` — saying nothing at
all about the request, which is still sitting in its queue as a whole
`http::Request`.

This crate's answer to that was `Failed::Sent`, with a comment saying
*"we no longer own the request, so there is nothing to hand back"*. The
first half was true and the second was not.

## 2. The reproduction, twice, deterministically

A race nobody can reproduce on demand cannot be fixed with any
confidence, so both forms below place the close at a chosen point rather
than waiting for one.

### 2.1 Scripted, in `h1.rs`, driven poll by poll

`ScriptIo` already existed for `a_connection_that_ends_with_the_request_queued_…`;
it answers reads only after something has been written and produces its
EOF after `n` `Pending`s. `race_lost_after(n)` runs a first exchange (so
the connection is `KA::Idle` — see below), drains it into the pool,
takes it back out, arms the script, and runs a second exchange with a
noop waker and a 64-poll ceiling. Nothing in it waits on the outside
world.

`n` counts `Pending`s and therefore starts at §1's **look 1**: this
helper hands the connection straight to `exchange`, so look 0 —
`Native::checkout`'s — is not on the path at all.

| `n` | §1's look | who finds the close | before | after |
|---|---|---|---|---|
| 0 | 1 | `exchange`'s own poll | `NotSent` — `ConnectionWentAwayBeforeTheRequest` | unchanged |
| 1 | 2 | hyper's first read, request queued | **`Sent` — `ConnectionEndedWithTheRequestQueued`** | **`NotSent` — same cause** |
| 2 | 3 | hyper's read after the write | `Sent` — `IncompleteMessage` | unchanged |
| 3 | 3 | later still | `Sent` — `IncompleteMessage` | unchanged |

**Why the connection has to have served a request first.**
`should_error_on_eof` is `!state.is_idle()`, so an EOF on a *fresh*
connection (`KA::Busy`) is an error — and hyper's error path
(`Client::recv_msg(Err(..))`, `proto/h1/dispatch.rs:700-739`) looks in
its own queue and hands the request back by itself. Only a connection
that completed an exchange is `KA::Idle`, and only there is an EOF a
graceful end that tells the request's promise nothing.

### 2.2 On a real socket, through the whole transport

`tests/pool.rs` already had `LateEof(Tokio, n)` — a runtime whose
sockets hide their first `n` EOFs, one per read that would have reported
one. That is a **deterministic blindfold**, and what it stands in for is
a probabilistic one: `tests/stale_reuse.rs` records that this race needs
*contention* rather than speed, because a starved client is one whose
reactor has not yet delivered the peer's `FIN` when the crate takes its
one deliberately non-suspending look.

Server: answers one request, then closes with no `Connection: close`,
bumping a `closes` observer after the socket is dropped. Client: a real
`Native` with a real pool, `http://` so no TLS layer adds reads.
Assertions are the server's accept count and what the caller got.

Eight sweeps of six arms each, before and after:

| EOFs hidden | before | after |
|---|---|---|
| 0 — checkout poll finds it | `200`, 2 accepts | `200`, 2 accepts |
| 1 — `exchange`'s look finds it | `200`, 2 accepts | `200`, 2 accepts |
| 2 — hyper finds it, request queued | **`Connect / ConnectionEndedWithTheRequestQueued`, 1 accept** | **`200`, 2 accepts** |
| 3 | `Connect / hyper::Error(IncompleteMessage)`, 1 accept | unchanged |
| 4 | same | unchanged |
| 5 | same | unchanged |

48 arms each side, no disagreement within a column.

**The mapping is only exact while the server's close is late**, which is
worth its paragraph because it cost an arm. With a prompt close, the
*first* exchange's own teardown read can meet the `FIN` and spend one of
the hidden EOFs, shifting every row by one. It showed up exactly once —
one arm of one sweep answering out of turn — and disappeared when
`Behaviour::close_delay` went into the fixture. `LateEof`'s own doc
comment had said the post-write window "cannot be reached this way,
because how many times hyper reads per `Connection` poll is its own
business and not a count a test may pin". It can be, and the count is
stable; the caveat was aimed at the wrong hazard.

## 3. The fix, and why it is a drop rather than a poll

hyper answers "did this request reach the wire" in two ways, and this
crate had been asking for only one of them.

- **`TrySendError::message`**, read from the request future. `Some`
  while the dispatcher has not dequeued the request; `None` once
  `poll_msg` has taken it apart into a `RequestHead` and a body — the
  moment past which no `http::Request` exists to give back
  (`proto/h1/dispatch.rs:667-687`).
- **A drop.** `Envelope::drop` sends
  `TrySendError { message: Some(req) }` for every request still queued
  when the receiver goes away (`client/dispatch.rs:218-227`), and that
  receiver is inside the `Connection` value `exchange` is holding.

So at point 2 the request is **one drop from being ours again**, and
nothing short of that drop can release it. `h1::claim_back` does exactly
that: drop the connection, poll the request future once, take hyper's
answer. Exactly one poll and it never suspends — the same shape and the
same reason as `is_reusable` and the look at the top of `exchange`.

It is an `async fn`, which in this workspace is a thing to measure rather
than assume: `Native::execute`'s future is **15,480 bytes** with it and
was 15,480 without, unchanged to the byte, against
`tests/future_size.rs`'s 24 KiB ceiling. `claim_back` is only awaited on
a branch that is already failing, and its own state is one `Pin<Box<_>>`
and an `Error`.

**The verdict stays hyper's.** `Failed`'s doc comment says the split is
"not our judgement about what looks safe to resend"; that is unchanged.
`claim_back` makes no judgement — it removes the one thing standing
between hyper and answering, and reports what hyper says. Row 2 of the
table above still says `Sent` for the same reason it always did.

**The error is the connection's cause, not hyper's answer to our own
drop.** Dropping a dispatcher produces `dispatch_gone` — *"runtime
dropped the dispatch task"* — which describes the drop rather than why
the connection died. That distinction is not decoration: `Native::run`
discards a `NotSent` error because it retries, but `Staged::exchange`
deliberately carries no retry and surfaces it through
`Failed::into_error`, so a caller of the staged connect reads this
string. Pinned on the source type, so a rewording survives and a
substitution does not (mutation M4, §5).

### 3.1 This is why the attempt in `nagle-and-nodelay.md` §6 failed

That section records an attempt: make the connection's *error* arm fall
through and poll the send future rather than assuming hyper has the
request. It moved 9 failures in 20 to 6 in 20 and was reverted on a
number that could not carry it.

It could not have worked, and the reason is §3's first paragraph: it
**asked without dropping**. A request still sitting in the queue resolves
its promise from `Envelope::drop` and from nowhere else, so polling the
future while the dispatcher is still alive polls a promise nothing will
ever fulfil. Mutation M2 is that attempt, applied to the current code,
and it fails both new tests.

## 4. The two expensive fixes, re-evaluated

`docs/nagle-and-nodelay.md` §6 names two. Neither is built, and the
reasons are now sharper than "not small".

### 4.1 Close the window before `try_send_request` is called

*"which costs the 'exactly one poll, and it never suspends' property
written down where that poll is, i.e. a scheduler round trip on every
pooled request."*

Still refused, and cost is the weaker half of the argument. **A yield is
not a fence.** Suspending until the reactor has had a turn does not
establish that the peer's `FIN` has arrived — it establishes that it had
one more chance to. The window moves; it does not close. So this trades a
bounded cost on *every* pooled request for an unbounded improvement in
probability with no guarantee at the end of it, which is the shape this
workspace refuses under a different name every time it declines a default
stronger than the truth.

It also contradicts a contract written where the poll is:
`is_reusable`'s *"that it does not suspend is what keeps checkout from
ever hanging… a false negative costs one socket; waiting could cost the
whole request."* A version of that which waits needs a bound, and the
bound would be a new `Timeouts` field for a phase no RFC names.

The cheaper cousin — force a real `read(2)` rather than trusting the
reactor's cached readiness — is refused one seam down: `TcpConnect`
hands back an associated `Stream` with `hyper::rt::Read` and nothing
else, so "peek at the socket regardless of readiness" would be a new
capability on the seam, and one that `hclient-rt-embassy` has no way to
implement. And it still only moves the window.

### 4.2 Replay a request hyper will not hand back

*"a design change, and `RetryKind` is the vocabulary it would have to
use, with `Client`'s `425` replay as the precedent."*

Refused, and by a rule this workspace states rather than by cost.
`pool.rs`'s own module doc: *"repeating a request a server may have acted
on is at-least-once, and choosing it selectively would need a notion of
method safety this codebase does not have"*, the same notion
`docs/h3-research.md` §3.5 declines for 0-RTT. `AGENTS.md` says it twice
more, once for the pool and once for `AllowEarlyData`.

`RetryKind` cannot stand in for it, and AGENTS.md already says why in
the 0-RTT section: *"`POST /transfer` with `RequestBody::Full(..)` is
`RetryKind::Free` and is precisely what must never go into early
data."* "Can I send this again" and "is it safe to send this again" are
different questions, and past point 2 only the second one is being
asked.

The `425` precedent argues *against* rather than for: there the **server**
asked for the repeat, which is what made the decision `Client`'s to take.
Here nobody asked, and the client cannot even tell whether the server saw
the bytes.

### 4.3 Three more that were considered

- **A reaper.** `Native::with_reaper` already exists and genuinely lowers
  the *rate*: a connection closed for age is one not raced. It does not
  narrow the window by a nanosecond, needs `R: Spawn`, and is "at most as
  good as the executor under it" (`pool.rs`).
- **Ask `SendRequest::is_closed()` before handing over.** It reads the
  same `want::Giver` state the poll one line above already refreshed;
  there is no second fact in it.
- **Keep our own copy of the request and resend past point 3.** This is
  §4.2 wearing a different hat, and it additionally costs a clone or a
  `RequestBody::Rewindable` on every pooled request to pay for an
  outcome the caller cannot be told is safe.

## 5. Mutations

Anchor: **278 tests**, `cargo nextest run -p hclient-native
--all-features --no-fail-fast`, green before each. The two tests named
most often below are `h1.rs`'s
`a_connection_that_ends_with_the_request_still_queued_hands_it_back` and
`tests/pool.rs`'s `a_request_hyper_took_but_never_wrote_is_retried_too`
— the scripted and the real-socket form of the same point.

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1 | `claim_back` reports `Sent` without asking hyper at all — the code as it stood | killed | both, at once |
| M2 | `claim_back` asks, but keeps the connection alive — §3.1's attempt | killed | both, at once |
| M3 | **CONTROL**: the connection's *error* arm answers `Sent` itself instead of routing through `claim_back` | **survived** | nothing — see below |
| M4 | `claim_back` reports hyper's post-drop error instead of the connection's cause | **survived**, then killed | nothing at first; the scripted one after §5's assertion — see below |
| M5 | delete the `Poll::Pending if conn_done` arm | killed | the scripted one on its 64-poll ceiling, the real-socket one by hanging into its 30 s bound |
| M6 | delete `exchange`'s look before the request is handed over | **survived on the event**, then killed | the two sequence rows from the start; `a_connection_found_dead_before_the_request_is_handed_over_is_retryable` once the recording hook below was added |
| M7 | `claim_back` gets the request back and reports `Sent` anyway | killed | both, at once |

**M4 was a gap, and closing it is the finding.** It survived all 278
tests, and it is not a control: `Staged::exchange` surfaces a `NotSent`
error to its caller (§3), so the string is reachable from a public API.
An assertion on the error's **source type** now pins it, and M4 is killed
by the scripted test alone. Recorded as it happened rather than as a
clean row, because "survived, therefore a gap, therefore an assertion" is
the sequence worth copying.

**M6's kill was about the sequence rather than the outcome, and that
exposed the one gap this run found besides M4.** Deleting `exchange`'s
poll removes §1's look 1, so every count lands one look later than the
tests are calibrated for — which is what fails them. What did *not* fail
is the **outcome** at the first point: the request still comes back,
through `claim_back` instead of through the deleted poll. So that poll is
no longer load-bearing for the retry's **correctness**; it is
load-bearing for one fewer poll and for the `CloseReason::Stale` a hook
reads there — and that second half had no test anywhere, because
`hooks.rs`'s stale test is satisfied by `Native::checkout`'s emitter, one
look earlier.

`race_lost_after` now carries a recording hook, and the three points
assert the reason as well as the verdict — so M6 is killed on the
**event** by the first point's test as well as by the two whose count it
shifts.

### 5.1 The control, and how it was verified

M3 is the connection's `Poll::Ready(Err(e))` arm answering for itself,
which is what the code did before. It differs from routing through
`claim_back` **only** when hyper would hand the request back at that
point, so the question is whether it can.

*Read.* Every error inside hyper's `Connection::poll` reaches
`poll_catch`, which calls `Client::recv_msg(Err(e))`
(`proto/h1/dispatch.rs:123-141`). That function returns `Ok(())` —
making the connection complete with `Ready(Ok(()))`, not `Ready(Err(_))`
— whenever it holds either the callback *or* a request still in the
queue, and it answers the promise in both cases. So `Ready(Err(_))`
implies hyper holds neither, which cannot be true while our request is
unresolved.

*Executed, because reading is not the standard here.* The arm replaced by
`panic!`: **278 pass** in this crate, and **1615 pass across the whole
workspace** — which is the run that matters, because `hclient-select`'s
Alt-Svc failures in `docs/nagle-and-nodelay.md` §6 were a
`hyper::Error(Shutdown, ..)` over TLS and this is the arm they would have
to arrive on. It is never reached.

The arm is kept rather than deleted, and routed through `claim_back`
rather than answering for itself, because the argument above is about
hyper's internals: one rule beats two, and the version with two would be
the one that goes stale.

**The same probe on `claim_back`'s own `Sent` arms**: `panic!` in place
of both, **278 pass** here and **1615 across the workspace**. When
`claim_back` is reached at all, hyper hands the request back — because a
callback hyper still holds is always answered inside the very poll that
ends the connection, so the
`Pending`-with-`conn_done` state is only reachable with the request still
queued. Those arms stay because that reasoning is hyper's to change, and
a `Failed::Sent` there is the fail-closed answer where an
`unreachable!()` would be a panic in a client on a path no test can
reach.

### 5.2 What that recording found, and is not fixing

The three scripted points report `Stale`, `Ended`, `Ended` — and the
first two are **one socket in one state.** Both names are literally true
at §1's look 2: the peer closed it after a response, *and* it was handed
out already closed. Which one a caller is told is decided by which of two
adjacent polls noticed, and the event carries no field for that.

This work neither introduced the asymmetry nor changes it. What it
changes is the number of readers: `CloseReason::Stale`'s own doc calls it
*"the event that explains the [`Connected`] following it"*, and after
`claim_back` there is a `Connected` following look 2 as well — where
before there was an error and no retry at all.

It is pinned rather than corrected. Correcting it means deciding the
reason from what the *request* did rather than from which poll noticed,
which moves the emission below the request's own outcome and puts the
one-`Closed`-per-socket rule behind three exits instead of one — where
`h1.rs`'s module doc leans on there being exactly two ("`exchange` and
`H1Body::poll_frame`… between them they are every place hyper's
`Connection` future can complete"). That is a change to the hooks seam
wanting its own measurement and its own mutations, not a by-product of a
race fix.

## 6. What is not closed, and what was not verified

- **Point 3 stands, and that is the decision.** A request hyper has
  taken apart and written out is `Failed::Sent`, the caller is told, and
  no retry happens. §4.2 is why. `a_request_already_on_the_wire_is_not_retried`
  is the control that keeps the widening from drifting into "retry
  everything".
- **The rate is not measured, only the outcome at each point.** How often
  a real client lands on point 2 rather than 1 or 3 depends on when the
  reactor delivers a readiness event relative to two polls a few
  instructions apart, which is a scheduler property and not a property of
  this crate. Every number here is *which* answer each point produces,
  never how often a point is reached.
- **This is not shown to be the cause of any flake on record.** The
  `alt_svc` failures in `nagle-and-nodelay.md` §6 were
  `hyper::Error(Shutdown, BrokenPipe)` — a connection *error*, and §5.1's
  probe says that arm is never reached anywhere in the workspace's 1615
  tests as they stand — and that fixture has since been fixed to announce
  its close. Claiming the fix for those failures would be reasoning where
  this project asks for capturing.
- **Linux, loopback, one machine.** Both reproductions are deterministic
  and neither depends on a clock, so what generalises is the table rather
  than any duration in it.
- **HTTP/2 is untouched.** `http2.rs` has its own pre-head
  `Failed::NotSent` returns and no equivalent hand-back from `h2`;
  `docs/v02-acceptance.md` already records that no test in this
  repository walks a dead *pooled* h2 connection past to a fresh one, and
  that is still true.
- **One workspace flake was captured while hunting for another, and it is
  not this one.** 30 full workspace runs at `-j8` produced one failure:
  `http2_multiplex::beyond_the_peers_stream_limit_requests_queue_on_one_connection`,
  `Connect / Reset(StreamId(5), REFUSED_STREAM, Remote)`. It is on the h2
  path, which this work does not touch, and the A/B says so rather than
  the reading: the same test file looped 40 times at `-j16` fails **2 of
  40** with this change and **4 of 40** with `h1.rs` reverted to the
  commit before it. Two against four at n = 40 is not a result either
  way, which is the point — it is pre-existing and unattributed, the same
  shape as the two `docs/v04-acceptance.md` §*flakes* already records.
  The select-race flake that document names was **not** seen in those 30
  runs.
- **The `CloseReason` asymmetry at look 2**, §5.2. Pinned, not fixed.
