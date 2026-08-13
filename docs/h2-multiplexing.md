# Sharing an h2 connection on `http-ng-native` — investigated

`docs/grpc-yardstick.md` found no defects and three limitations, and said
that the second and third hang off the first:

> **L1. No multiplexing.** An h2 connection is checked out exclusively, so
> two concurrent calls cost two connections and two handshakes.
> **L2.** Cancellation closes the connection rather than sending
> `RST_STREAM(CANCEL)`. **L3.** A `PING` on a pooled connection is answered
> by the next call rather than promptly.

This document answers the question that was left open: **could
`http-ng-native` share an h2 connection when the runtime has `Spawn`, the
way `http-ng-h3` does, and what would it cost?**

**Nothing here is code, and no library code was written for it.** Every
claim below is marked with how it is known — *compiled* (a probe crate
built or refused to build), *measured* (a real `h2::server` or a real
rustls handshake on loopback), or *read* (source of `h2 0.4.15`,
`hyper 1.11.0`, or this workspace). §10 has the probe verbatim so that the
measurements can be re-run rather than believed.

## 0. The verdict, in four lines

1. **Sharing is expressible, and the `Spawn`-less path is untouched.**
   Compiled, both directions: the spawner is captured as a **function
   pointer** on an opt-in constructor, so nothing on `Native`, on
   `Transport::execute`, or in any signature a `Spawn`-less runtime meets
   gains a bound. A runtime that cannot spawn gets `E0277` where the caller
   wrote `multiplexed()`.
2. **L2 and L3 are not separate work.** Both are measured to fall out of
   the driver being spawned, with no code of their own — the
   `RST_STREAM(CANCEL)` `http2::Pump`'s `Drop` already queues reaches the
   wire, and a `PING` is answered in 0 ms while idle.
3. **The cost is 8× the sockets and 8× the handshakes for a concurrency of
   8** — measured through the real `Native` over real TLS: 480 accepts for
   480 requests where a shared connection needs 60.
4. **It is not a pure win, and the three prices are named in §6.** The
   sharpest is that a spawner nobody drives turns from "sockets stay open"
   (the reaper's failure mode, already written down) into "every request on
   that connection hangs for ever" — measured.

**Recommendation: build it, and settle §9.1 by measurement first** — the
second-connection policy is the one open question that is not an
implementation decision, because sharing everything is measurably worse
than today at a low peer stream limit. The work is a week with this
project's mutation discipline, and it changes a policy stated in
`pool.rs`, in `http2.rs`, in `http-ng-h3`'s module doc that mirrors them,
in `AGENTS.md` and in the yardstick — five places that agree on purpose —
so it should not be done as a side effect of something else.

## 1. Premises, and how each is known

| # | claim | how known |
|---|---|---|
| P1 | `h2::client::SendRequest<B>` is `Clone`, and clones open concurrent streams on one connection | **Measured.** 8 concurrent calls off 8 clones: **1 accept, 8 requests**, wall 152 ms against a server answering after 150 ms (B1) |
| P2 | `<R as Spawn<F>>::spawn` coerces to `fn(&R, F)` and can be stored in a field that imposes no bound | **Compiled**, in the probe and then against the real `Native`/`NativeIo<R, T>` in this tree (A1) |
| P3 | A runtime with **no `Spawn` impl at all** still instantiates the whole type and reaches `execute` | **Compiled and ran.** `FakeRt` implements `TcpConnect + Timer` and nothing else (A2) |
| P4 | `multiplexed()` on such a runtime is a compile error at the call site | **Compiled — refused.** `E0277: the trait bound FakeRt: Spawn<H2Driver<FakeStream>> is not satisfied` (A3) |
| P5 | A driver carrying the hook makes `multiplexed()` unavailable to a `!Send` hook, and only to `multiplexed()` | **Compiled, all three directions.** An `Rc` hook builds the transport and runs it (A4a); `Rc` + `multiplexed()` is `Rc<Cell<usize>> cannot be sent between threads safely` (A4b); the same with `Arc` compiles (A4c) |
| P6 | With the driver spawned, dropping one call sends `RST_STREAM(CANCEL)` and leaves its neighbour alone | **Measured.** Server saw `resets = ["CANCEL"]`, the survivor got `200`, `accepts = 1` (B2) |
| P7 | A `PING` on an idle shared connection is answered promptly | **Measured.** Server-initiated ping round trip **0 ms**, 200 ms into an 800 ms silence (B3) |
| P8 | A driver that is *dropped* fails its requests; a driver that is *never polled* hangs them | **Measured.** Dropped: `connection closed because of a broken pipe`. Held and never polled: no verdict in 500 ms (B4a, B4b) |
| P9 | A `SendRequest` clone held "in the pool" keeps the connection alive; dropping the last one ends the driver | **Measured.** Driver still running with one clone held after the request finished; ended within 200 ms of dropping it (B5) |
| P10 | `SendRequest::poll_ready` is a **liveness** check, not a capacity check | **Read and measured.** `poll_pending_open` (h2 `src/proto/streams/streams.rs:996`) tests a connection error, the next stream id, and *this clone's own* pending stream — nothing about the peer's limit. A fresh clone against a server allowing one stream, with that stream in use, answers `Ready(Ok)` (B8) |
| P11 | A pooled clone learns of a `GOAWAY` **without being polled first** | **Measured.** `poll_ready` resolved `Ready(Err)` **20 µs** after the first poll, the peer having gone away 100 ms earlier while nothing touched the clone (B7) |
| P12 | Beyond the peer's `MAX_CONCURRENT_STREAMS`, h2 queues silently | **Measured.** Limit 2, six concurrent calls: all six succeed, one connection, per-call 203/203/405/405/607/607 ms (B6) |
| P13 | Today, N concurrent calls cost N connections and N h2 handshakes | **Measured** through the real `Native` with `http2` on: 1/2/4/8 concurrent → 1/2/4/8 accepts and the same number of h2 handshakes; the same 8 calls **sequentially** → 1 accept, 1 handshake (C1, C2) |
| P14 | `hyper/http2` is still unusable here | **Read, unchanged.** `Http2ClientConnExec` is sealed and the executor is handed the h2 connection itself — `http2.rs`'s module doc, re-checked against hyper 1.11.0. This document does not reopen it: what changes is that *we* would have a spawner, not that hyper's would become nameable |
| P15 | `ReuseSupport` and `CancelSupport` need no new variant | **Read.** `ReuseSupport::Supported` is *"a second request to an origin need not pay for a TCP and TLS handshake again"*, which stays exactly true; `CancelSupport::Supported` is a duty owed on a dropped future, and it goes on being owed — what changes is the frame the peer sees. **No `http-ng-core` change is needed** |

## 2. The type-level question — expressible, and the `Spawn`-less path is untouched

### 2.1 Why the reaper's shape is the precedent and is not enough

`Native::with_reaper` already carries a `Spawn` bound **on a constructor
alone** — `R: Clone + Spawn<Reaper<R, NativeIo<R, T>>>` — and its doc
comment states the rule this whole crate is built on: *"Because `Native` is
generic over `R`, and not every `R` has a `Spawn` impl at all … a default
stronger than the truth, which is the one thing this crate refuses
everywhere else."*

It is not enough for a connection driver, and the difference is *when*.
`with_reaper` spawns **at construction**, where the bound is in scope. A
connection driver has to be spawned **at connect time**, inside
`Transport::execute` — a trait method, whose impl cannot carry an extra
bound, and which a `Spawn`-less runtime must go on instantiating.

Two shapes were considered and neither is needed:

- **Two impls of `Transport`, one bounded on `R: Spawn`.** Coherence
  forbids it; there is no specialisation on stable.
- **A fifth type parameter** (`Native<R, T, D, H, M>` with `M: Multiplex`
  defaulted to a ZST). Expressible, but it puts the policy in the type name
  — the thing `docs/v04-design.md` §"the shape" already declined once — and
  every `Native<R, T, D>` in the workspace would keep working only through
  a defaulted parameter that shows up in every error message.

### 2.2 What works: capture the spawner as a function pointer

`http_ng_rt::Spawn` declares **zero bounds** — `pub trait Spawn<F: Future<Output = ()>> { fn spawn(&self, f: F); }` — so
`<R as Spawn<F>>::spawn` is an ordinary function item, and it coerces to
`fn(&R, F)`. A field of that type imposes nothing on `R`: a function
pointer type is well-formed whatever its parameters are.

```rust
pub struct Native<R, T, D, H = NoHooks>
where R: TcpConnect + Timer, T: TlsConnect
{
    // …
    /// `None` is today's transport, exactly. `Some` is a spawner captured
    /// where the bound held.
    #[cfg(feature = "http2")]
    share_h2: Option<fn(&R, http2::H2Driver<NativeIo<R, T>, H>)>,
}

impl<R: TcpConnect + Timer, T: TlsConnect, D, H> Native<R, T, D, H> {
    pub fn multiplexed(mut self) -> Self
    where R: Spawn<http2::H2Driver<NativeIo<R, T>, H>>
    {
        self.share_h2 =
            Some(<R as Spawn<http2::H2Driver<NativeIo<R, T>, H>>>::spawn);
        self
    }
}
```

`H2Driver` has to be a hand-written struct rather than an `async` block for
the reason `Reaper` is: `Spawn<F>` makes the future a parameter of the
*trait*, so a bound has to name it.

**Compiled against this tree**, not only in the probe: the field, the
constructor and a call site inside an impl block with no `Spawn` bound were
added to `crates/http-ng-native/src/lib.rs` and `src/http2.rs`,
`cargo build -p http-ng-native --all-features --tests` was green, so were
`http-ng-select` and `http-ng-ws-tungstenite`, and the change was reverted.
The only diagnostic was a `Debug` warning on the new struct.

### 2.3 What the negative side costs, and where the refusal lands

| what a caller writes | what happens | how known |
|---|---|---|
| `Native::new(spawnless_rt, …)` | builds and runs, no driver spawned | compiled + ran (A2) |
| `Native::new(spawnless_rt, …).multiplexed()` | `E0277: the trait bound FakeRt: Spawn<H2Driver<FakeStream>> is not satisfied` | compiled — refused (A3) |
| `Native::new(tokio, …).hooks(Rc<Cell<usize>>)` | builds and runs | compiled + ran (A4a) |
| …`.multiplexed()` on top of that | `E0277: Rc<Cell<usize>> cannot be sent between threads safely` | compiled — refused (A4b) |
| the same with an `Arc` hook | compiles | compiled (A4c) |

The last two are the price of P5 and are worth reading twice. The hook
seam's `!Send` allowance (P13 in `docs/v04-design.md`) has a real subject —
`http-ng-fetch`'s `Rc<RefCell<..>>` — and a spawned driver is where it
collides with `Spawn`'s `Send + 'static`. **`http-ng-h3` met the same
collision and could not close it**: `quinn::Runtime::spawn` wants `Send`,
so `CloseReason::Ended` has no emitter there. Here it *is* closable,
because the bound sits on the opt-in rather than on the transport: an
`Rc`-holding hook keeps the transport it has today and is refused only the
multiplexing, at the line where it asked.

The alternative — a driver that carries no hook — is worse in a way this
project has already ruled on: a shared connection dies inside its driver,
so a `Closed` event has no other emitter, and a build that turned
multiplexing on would silently stop reporting the ends of its connections.

## 3. What the pool would become

`crates/http-ng-native/src/pool.rs` is built around **take**: `Pool::take`
removes an entry, an exchange owns it, and a `CheckIn` puts it back. That
is right for HTTP/1 and for an exclusive h2 connection, and it is the wrong
verb for a shared one.

### 3.1 `take` becomes `borrow`, and the entry never leaves

A shared entry holds a `SendRequest` clone and no `Connection` — the
`Connection` is inside the spawned driver. Checkout clones the sender and
leaves the entry where it is; there is nothing to hand back, so
**`CheckIn` is not used on this path at all** and `H2Body::hand_back_to_pool`
becomes a no-op for the shared variant. That is a subtraction, not an
addition: the whole `Reuse { checkin, sender }` dance in `http2.rs` exists
to survive an exclusive check-out.

`Established` gains a third variant. It carries no `I`:

```rust
pub(crate) enum Established<I> where I: Read + Write + Unpin {
    H1(crate::h1::Established<I>),
    #[cfg(feature = "http2")] H2(Box<crate::http2::Established<I>>),
    #[cfg(feature = "http2")] H2Shared(crate::http2::Shared), // { sender, id }
}
```

`PoolKey` needs **nothing new** (read): a shared connection and an
exclusive one never coexist in one `Native`, because the choice is made
once at construction. If that ever stops being true the key needs a fourth
component, and that is the same sentence `pool.rs` already writes about
`TlsConfigId`.

### 3.2 `is_reusable` gets simpler and *more* correct

Today's h2 `is_reusable` polls `Connection` once and then `poll_ready`,
and its doc explains that one poll is the only moment anything is noticed
because *nothing polls an idle connection*. With a driver, that premise is
gone: **P11 measured a `GOAWAY` reaching a pooled clone 20 µs after the
first poll, having arrived 100 ms earlier while nothing touched it.**
Liveness becomes push rather than a checkout-time poll, and the shared
`is_reusable` is one `poll_ready`.

**P10 is the trap.** `poll_ready` is *not* a capacity check — it reports a
connection error, the next stream id, and this clone's own pending stream,
and nothing about the peer's `MAX_CONCURRENT_STREAMS`. Two consequences:

- The existing rule "`Pending` means not worth a request" is safe on a
  shared entry, because a full connection does not answer `Pending`.
- **There is no way to ask h2 whether a connection is full.** A pool that
  wants to open a second connection above some concurrency has to count
  live streams itself. §9.1.

### 3.3 The idle timeout gets its second meaning back, for free

`pool.rs`'s consequence 2 — *"by default the idle timeout is a filter, not
a reaper"* — is a fact about HTTP/1 and about an exclusive h2 connection:
nothing is polling, so nothing can close one. On a shared entry, dropping
the pooled clone **is** the close (P9): the driver ends when the last
`SendRequest` goes, and `Pool::take`'s existing "drop the expired entries
you walk past" therefore closes a socket rather than merely declining to
offer it. No reaper, no new code, and the property is a measurement rather
than an argument.

It also means the pooled clone is what *keeps the connection alive*, which
inverts a sentence: today a pooled connection is inert and a request
revives it; a shared one is running, and the pool entry is its owner.

### 3.4 `max_idle_per_key` stops describing the right thing

With sharing, one entry per key serves any number of concurrent calls, so
"how many idle connections may I keep" is no longer the bound anybody
wants. The bound that matters becomes "how many *connections* to this
origin", and it is only reached when §9.1's second-connection policy fires.
`PoolConfig` therefore grows a field or `max_idle_per_key` changes meaning
on this path — a decision, not a mechanic, and §9.1 has to be settled
first.

## 4. L2 and L3 are not separate work

**L2.** `http2::Pump`'s `Drop` already queues `send_reset(Reason::CANCEL)`,
and its own doc comment says why it is unobservable — *"the connection is
owned by the same future or the same `H2Body` that owns the pump and is
dropped in the same breath … It is kept for the day that stops being
true."* With a spawned driver the connection outlives the stream, and
**P6 measured the frame arriving**: the server recorded `CANCEL` for the
cancelled call and answered its neighbour on the same connection.

One test has to gain an arm rather than change:
`grpc_shape.rs`'s `cancelling_a_call_ends_it_at_the_server_and_leaves_the_
next_one_alone` asserts `vec![Ending::ConnectionGone]`, and its `Ending`
enum already has a `Reset(String)` variant that nothing produces. Under
multiplexing that assertion becomes `Ending::Reset("CANCEL")`, and the
second half of the test — *"the second request gets its own connection"* —
stops being vacuous and starts being the neighbour that must survive.

**L3.** P7 measured a server-initiated `PING` answered in 0 ms on a
connection that had been silent for 200 ms. `is_reusable`'s doc sentence —
*"a checkout is the only moment a `GOAWAY` or a closed socket is
noticed"* — stops applying to a shared connection, and so does the
yardstick's L3 paragraph.

Both are consequences of P9 rather than of anything written for them, which
is exactly what the yardstick predicted when it classified them as
"downstream of L1".

## 5. The measured cost

All figures on this host, loopback, `x86_64` Linux, debug builds.

### 5.1 The count, through the real `Native` (C1/C2)

`Native<Tokio, FakeTls, SystemDns<Tokio>>` with `http2` on, against a real
`h2::server` that answers after 150 ms. The TLS backend is
`tests/http2.rs`'s: it encrypts nothing and reports `h2`, so the bytes on
the wire are real HTTP/2 and the only thing simulated is the ALPN report
`Native` reads to decide it may speak h2 at all.

| concurrency | TCP accepts | h2 handshakes | wall |
|---|---|---|---|
| 1 | 1 | 1 | 153 ms |
| 2 | 2 | 2 | 154 ms |
| 4 | 4 | 4 | 155 ms |
| 8 | 8 | 8 | 154 ms |
| 8, **sequential** | 1 | 1 | — |

The same 8 concurrent calls with one spawned driver and 8 cloned senders:
**1 accept, 8 requests, 152 ms** (B1).

### 5.2 The price, over real TLS (D)

rustls on both ends with ALPN `h2`, a self-signed IP-SAN certificate,
`Native<Tokio, Rustls, IpLiteralOnly>`. 60 bursts of 8 concurrent calls =
480 requests. CPU is `/proc/self/stat` user+system, the instrument
`docs/v02-acceptance.md` used for the busy-spin — loopback has no RTT, so
wall time hides the whole cost and CPU does not. **Both ends run in this
process**, so a figure covers the client's handshakes *and* the server's.

Eight runs, on a host shared with other work. **D0 is the instrument's own
check and it earned its place**: on a ninth run it read 10 ms instead of
0 ms — the host was busy — and that run's D1 read 2.01 s against a median
of 790 ms. That run is excluded, by the rule "D0 must be zero", decided
from the reading rather than from the outlier.

| row | shape | accepts | requests | cpu (median of 8) | cpu (range) |
|---|---|---|---|---|---|
| D0 | both runtimes up, nothing asked of them, 400 ms | — | — | **0 ms** | 0 ms in all eight |
| D1 | `Native`, 60 **cold** bursts of 8 concurrent | **480** | 480 | **790 ms** | 690–850 |
| D3 | shared connection, 60 **cold** bursts of 8 concurrent | **60** | 480 | **325 ms** | 220–510 |
| D4 | `Native`, **warm** pool, 60 bursts of 8 concurrent | 8 | 488 | **235 ms** | 170–330 |

Read it this way, and only this way:

- **The counts are exact and are the claim.** Eight times the sockets and
  eight times the TLS+h2 handshakes, for the same 480 requests, at a
  concurrency of 8. D1's 480 accepts against D3's 60 is `N` versus `1` per
  burst, measured rather than reasoned.
- **The CPU is directional, not precise.** D3's spread (220–510 ms) is
  half its own median, so the honest statement is "roughly 2× less CPU for
  8× fewer handshakes", not a ratio to two figures. The ranges do not
  overlap (690–850 against 220–510), which is what makes the direction
  safe to state at all. D0 rules out idle spin as the source: **0 ms** over
  a comparable window with both runtimes alive.
- **D4 is the row that keeps this honest.** A warm pool pays no handshake
  at all: 8 accepts for 60 bursts. So in steady state L1's cost is
  **sockets held open**, not CPU — 8 connections and 8 server-side
  connection states per origin at a concurrency of 8, versus 1. The CPU
  cost is a *cold* cost, and it is paid again on every idle timeout, every
  `GOAWAY`, and every network change.
- **D2 is deliberately not in the table.** 60×8 sequential calls on one
  pooled connection took 3.4 s of wall for 480 requests, so its CPU figure
  is dominated by a four-times-longer window rather than by handshakes. It
  is not a control and was not used as one.

## 6. What sharing costs that today does not

### 6.1 A spawner nobody drives — and here it is much worse

`pool.rs` already names this as the third thing no bound can catch:
*"`Spawn::spawn` returns `()`. An executor that is never `run` accepts this
task, drops it, and cannot report that it did; the sockets then sit open
exactly as they would with no reaper, and the type system is happy
throughout."*

For the reaper that costs file descriptors. **For a connection driver it
costs the client.** P8 measured both shapes:

| what happened to the driver | what a request on that connection does |
|---|---|
| dropped on the floor | fails: `connection closed because of a broken pipe` |
| held, never polled | **hangs — no verdict in 500 ms** |

The first is survivable; the second is the one an executor that is never
run produces, and it is unbounded. `Timeouts::first_byte` would cut it for
a caller who set one, which is the only mitigation available and is not a
default.

This does not change the answer, because the failure needs a caller to hand
`multiplexed()` a spawner and then never run it — the same mistake the
reaper's doc already warns about. It does change how loudly the opt-in has
to say so, and it is the strongest argument against ever making this the
default.

### 6.2 Head-of-line at the peer's limit, and h2 will not tell us

P12: with `MAX_CONCURRENT_STREAMS = 2`, six concurrent calls against a
server answering in 200 ms finished at 203, 203, 405, 405, 607, 607 ms —
three waves on one connection. **Today the sixth call opens its own
connection and finishes in ~200 ms.** So sharing trades sockets for
queueing, and at a low peer limit the trade goes the wrong way for latency.

P10 is what makes this a design question rather than a tuning one: h2's
public API has no "is this connection full". A second-connection policy has
to count live streams in our own code. §9.1.

### 6.3 An `Rc` hook cannot multiplex

P5/A4b. Stated in §2.3; repeated here because it is a cost rather than a
mechanism. The refusal is at the call site and names the missing bound,
which is the best available shape, but a build whose hook holds an `Rc` has
to choose.

### 6.4 Code a build that never opts in still carries

The choice is a runtime one, so a build with `http2` on and no call to
`multiplexed()` still compiles the shared variant of `Established`, the
shared arm of `NativeBody`, and the shared `exchange`. That is the same
kind of cost the `Protocol` enum already pays and is small, but it is not
zero and it should be said rather than discovered.

## 7. The rejected alternative — one `Connection` behind a lock

There is a shape that multiplexes with **no `Spawn` at all**: put the
`h2::client::Connection` behind a `Mutex` inside the pool entry, and have
every in-flight request poll it when it is polled. `pool.rs` rejects it in
writing — *"a request whose caller stopped polling would stop driving the
connection its neighbours are waiting on"* — and that rejection is
slightly stronger than it needs to be: as long as **any** holder is being
polled the connection moves, so the failure needs every caller to stall at
once.

It is still the wrong shape here, for two reasons that are about what it
cannot do rather than about what might go wrong:

- **It fixes L1 only.** An idle pooled connection has no holder at all, so
  L3 stays exactly as it is, and a stream dropped after the last live
  neighbour finished has nobody to write its `RST_STREAM` — L2 stays too.
- **It is where the wakers go wrong.** A connection polled from N request
  futures needs each of their wakers registered, and h2 registers the waker
  of whoever last polled. Getting that right is a `Mutex` plus a waker list
  plus a rule about who re-polls after a wake — more machinery than the
  driver, for a strictly smaller result.

It is worth recording because it is the only shape that could ever serve
`http-ng-rt-embassy`, which is expected to have no `Spawn` at all
(`docs/w7-embassy-research.md`). If multiplexing on a spawn-less runtime is
ever wanted, this is the door, and it should be opened for that reason
rather than as a way of avoiding an opt-in.

## 8. Decisions, if this is built

1. **An opt-in constructor, `Native::multiplexed()`, bounded on
   `R: Spawn<H2Driver<NativeIo<R, T>, H>>`.** Not a default, not a type
   parameter, not a feature. The precedent is `with_reaper` and the reason
   is the same rule: a default stronger than the truth is the one thing
   this crate refuses.
2. **The spawner is captured as a function pointer**, so no signature a
   `Spawn`-less runtime meets changes. §2.2.
3. **The driver carries `H`**, so a shared connection's `Closed` has an
   emitter — at the cost of `H: Send + 'static` on the opt-in and nowhere
   else. §2.3.
4. **The shared pool entry is borrowed, never taken**, and `CheckIn` is not
   used on that path. §3.1.
5. **`is_reusable` on a shared entry is one `poll_ready`**, and the
   liveness it reports is real because a driver is polling. §3.2.
6. **`http-ng-core` does not change.** P15.
7. **`Capabilities` do not change.** They already report the HTTP/1.1
   floor, which is what makes this invisible to a caller who did not opt
   in; `cancel_on_drop` stays `Supported` and the frame the peer sees gets
   better.
8. **The policy paragraphs move together.** `pool.rs`'s "What an h2
   connection is checked out for", `http2.rs`'s "One stream per connection,
   and what W1 is resting on", the yardstick's L1/L2/L3 and `AGENTS.md`'s
   "Three limitations" all state the same decision from different ends, by
   design. W1's rule — *cancelling one stream must not tear down the
   others* — stops holding vacuously and starts needing a test; P6 is that
   test's shape.

### 8.1 Which assertions encode the policy

Grepped rather than guessed, and the pleasant surprise is that **three of
the five already say they are the ones that would change** — the discipline
of writing the reason next to the assertion is what makes this list short.

| test | today | under sharing |
|---|---|---|
| `http2.rs::dropping_one_exchange_leaves_a_concurrent_one_alone` | `accepted == 2`, *"which is what makes the survivor's connection unreachable from the cancelled request"* | `1`. Its own doc already says: *"The day that number becomes 1, this test's other assertion is what has to keep holding, and it will not hold for free"* — W1's rule, no longer vacuous |
| `grpc_shape.rs::two_concurrent_calls_take_two_connections_rather_than_two_streams` | `accepted == 2`, *"a recorded limitation with `Spawn` behind it"* | inverts, name included |
| `grpc_shape.rs::cancelling_a_call_ends_it_at_the_server_and_leaves_the_next_one_alone` | `vec![Ending::ConnectionGone]`, and `accepted == 2` because *"the cancelled one was not returned to the pool"* | `Ending::Reset("CANCEL")` — the enum already has the variant and nothing produces it — and `1`, the connection surviving its cancelled stream |
| `grpc_shape.rs::a_ping_to_a_pooled_connection_waits_for_the_next_call` | an A/B: nothing within 750 ms while pooled, answered by the next call | inverts (L3) |
| `http2.rs`, two **sequential** requests reuse one connection (`accepted == 1`) | unchanged | unchanged — sharing does not change what pooling already did |
| `grpc_shape.rs::a_goaway_costs_a_connection_and_not_the_next_call` | `accepted == 2` | unchanged: a `GOAWAY` costs the connection either way |

The new test the change owes is the one
`dropping_one_exchange_leaves_a_concurrent_one_alone` names: **cancelling
one stream must not tear down its neighbour on the same connection**. P6 is
its shape — a survivor and a doomed call on one connection, the server
recording a `CANCEL` and answering the survivor, `accepted == 1` so that
the two provably shared.

## 9. What is deliberately left open

### 9.1 When to open a second connection — needs measuring first

Sharing everything is not right (§6.2) and sharing nothing is today. The
policy needs a number, and the number needs a measurement this
investigation did not make: at what concurrency does queueing on one
connection cost more than a handshake on a second? It depends on the peer's
`MAX_CONCURRENT_STREAMS` (which h2 will not report to us, P10) and on the
handshake cost, which is a network property rather than a loopback one.

What is already known: the count has to be ours, because
`SendRequest::poll_ready` cannot answer it (P10), and the counter has to
live next to the pool entry rather than inside the exchange, because it
must survive the exchange.

The comparable decision one layer down is `HeConfig::default()`'s
`attempt_delay` of 250 ms, chosen against a measurement rather than a
guess; this deserves the same treatment.

### 9.2 What a `Closed` event says for a shared connection

Today `Closed { reason: CloseReason::Stale }` is emitted at checkout, by
the code that walked past a dead entry. A shared connection dies inside its
driver, so the driver emits — and the reasons available there are not the
same set. Whether `CloseReason` needs a variant is a question for whoever
writes the driver, and it is the one place §8's "`http-ng-core` does not
change" might not hold.

### 9.3 Whether `without_pool()` and `multiplexed()` can both be asked for

There is nowhere to share a connection without a pool, so the pair is
either a refusal at construction — the shape `http-ng-select` uses when two
members' capabilities disagree, naming the field — or a documented
last-call-wins. Not decided here.

### 9.4 What happens to an h2 connection at a network change

`Selecting::network_changed()` exists because a `Transport` cannot see what
the caller can. A shared connection with a spawned driver survives a
network change more visibly than a pooled idle one: the driver is still
polling a socket that will never carry anything again. Whether `Native`
needs the same entry point is a new question that only sharing raises.

## 10. The probe, to re-run

Not kept as a test — it depends on `h2` directly, builds a `Native`
against a certificate it generates, and none of it is a claim about this
workspace's code. Recorded the way `docs/v04-w2-webtransport.md` §10
records the `wtransport` spike.

`Cargo.toml` (outside the workspace; the paths are this checkout's):

```toml
[package]
name = "h2mux-probe"
version = "0.0.0"
edition = "2024"
[workspace]

[dependencies]
bytes = "1"
h2 = "0.4"
http = "1"
tokio = { version = "1", features = ["full"] }
futures-util = "0.3"
hyper = { version = "1.11", default-features = false, features = ["client", "http1"] }
rcgen = "0.14"
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
tokio-rustls = { version = "0.26", default-features = false, features = ["ring"] }
http-ng-rt          = { path = "…/crates/http-ng-rt" }
http-ng-rt-tokio    = { path = "…/crates/http-ng-rt-tokio" }
http-ng             = { path = "…/crates/http-ng" }
http-ng-core        = { path = "…/crates/http-ng-core" }
http-ng-native      = { path = "…/crates/http-ng-native", features = ["http2"] }
http-ng-dns         = { path = "…/crates/http-ng-dns" }
http-ng-dns-system  = { path = "…/crates/http-ng-dns-system" }
http-ng-tls         = { path = "…/crates/http-ng-tls" }
http-ng-tls-rustls  = { path = "…/crates/http-ng-tls-rustls" }
```

### 10.1 Probe A — the type-level claims

`H2Driver`/`H2DriverH` stand in for `h2::client::Connection` driven to
completion; `FakeRt` implements `TcpConnect + Timer` **and no `Spawn`**.

```rust
pub struct H2Driver<I> { io: I }
impl<I: Unpin> Future for H2Driver<I> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> { Poll::Ready(()) }
}

pub struct NativeLike<R: TcpConnect + Timer> {
    rt: R,
    spawn_h2: Option<fn(&R, H2Driver<<R as TcpConnect>::Stream>)>,
}

impl<R: TcpConnect + Timer> NativeLike<R> {
    pub fn new(rt: R) -> Self { Self { rt, spawn_h2: None } }

    pub fn multiplexed(mut self) -> Self
    where R: Spawn<H2Driver<<R as TcpConnect>::Stream>> {
        self.spawn_h2 = Some(<R as Spawn<H2Driver<<R as TcpConnect>::Stream>>>::spawn);
        self
    }

    /// Stands in for `Transport::execute`: no `Spawn` bound, and it still
    /// reaches the spawner when there is one.
    pub fn execute(&self, io: <R as TcpConnect>::Stream) -> bool {
        match self.spawn_h2 {
            Some(spawn) => { spawn(&self.rt, H2Driver { io }); true }
            None => false,
        }
    }
}
```

`H2DriverH<I, H>` and `NativeLikeH<R, H>` are the same with the hook added.
Results:

```
$ cargo run
A1 ok (compiled), A2 ok (ran, no driver spawned), A4a ok (Rc hook builds)

$ RUSTFLAGS="--cfg probe_a_neg" cargo build      # NativeLike::new(FakeRt).multiplexed()
error[E0277]: the trait bound `FakeRt: http_ng_rt::Spawn<H2Driver<FakeStream>>` is not satisfied

$ RUSTFLAGS="--cfg probe_a4_neg" cargo build     # Rc hook + multiplexed()
error[E0277]: `Rc<Cell<usize>>` cannot be sent between threads safely

$ RUSTFLAGS="--cfg probe_a4_pos" cargo build     # Arc hook + multiplexed()
    Finished `dev` profile
```

The same field and constructor were then added to this tree's real
`Native` over `NativeIo<R, T>`, built with
`cargo build -p http-ng-native --all-features --tests`, and reverted.

### 10.2 Probe B — the h2 facts

Plaintext h2 on loopback; the client's `Connection` is spawned and
`SendRequest` is cloned per call.

```
B1  8 concurrent calls, spawned driver: accepts=1 requests=8 wall=152ms
B2  cancel one of two: survivor=Ok(200) requests=2 resets=["CANCEL"] accepts=1
B3  PING while idle, spawned driver: round trips (ms) = [0]
B4a driver dropped: failed: connection closed because of a broken pipe
B4b driver held but never polled: request HUNG for 500ms (no verdict)
B5  driver ended while a pooled clone was held: false; after dropping it: true
B6  MAX_CONCURRENT_STREAMS=2, 6 concurrent: ok=6 accepts=1 per-call ms=[203,203,405,405,607,607]
B7  GOAWAY while 'pooled': poll_ready resolved after 20.4µs as Ready(Err)
B8  poll_ready with the peer's one stream in use: Ready(Ok) — a second stream is allowed
```

### 10.3 Probes C and D — the cost

C uses `tests/http2.rs`'s `FakeTls` verbatim in behaviour; D uses real
rustls on both ends with a generated IP-SAN certificate.

```
C1  Native+http2, 1 concurrent: accepts=1 h2-handshakes=1 requests=1
C1  Native+http2, 2 concurrent: accepts=2 h2-handshakes=2 requests=2
C1  Native+http2, 4 concurrent: accepts=4 h2-handshakes=4 requests=4
C1  Native+http2, 8 concurrent: accepts=8 h2-handshakes=8 requests=8
C2  Native+http2, 8 SEQUENTIAL: accepts=1 h2-handshakes=1 requests=8

D0  idle baseline, both runtimes up, 400ms wall: cpu=0ns
D1  Native / real TLS, 60 COLD bursts of 8 concurrent: accepts=480 requests=480 cpu=790ms
D3  shared connection, 60 COLD bursts of 8 concurrent:  accepts=60  requests=480 cpu=325ms
D4  Native / real TLS, WARM pool, 60 bursts of 8:       accepts=8   requests=488 cpu=235ms
```

The accept counts are the same on every run; the CPU figures are medians
of eight, and a run whose `D0` is not `0ns` is a run on a busy host and is
not one of the eight.
