# Sharing an h2 connection on `hclient-native` — investigated, then built

> **Built in v0.4.** §0–§10 are the investigation, kept as they were
> written and not retro-fitted. **§11 is what it became**, including the
> three places the investigation was short of the mechanism, the
> re-measured cost, the mutation table and the answers to §9's open
> questions. `Native::multiplexed()` is the opt-in.


`docs/grpc-yardstick.md` found no defects and three limitations, and said
that the second and third hang off the first:

> **L1. No multiplexing.** An h2 connection is checked out exclusively, so
> two concurrent calls cost two connections and two handshakes.
> **L2.** Cancellation closes the connection rather than sending
> `RST_STREAM(CANCEL)`. **L3.** A `PING` on a pooled connection is answered
> by the next call rather than promptly.

This document answers the question that was left open: **could
`hclient-native` share an h2 connection when the runtime has `Spawn`, the
way `hclient-h3` does, and what would it cost?**

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
`pool.rs`, in `http2.rs`, in `hclient-h3`'s module doc that mirrors them,
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
| P15 | `ReuseSupport` and `CancelSupport` need no new variant | **Read.** `ReuseSupport::Supported` is *"a second request to an origin need not pay for a TCP and TLS handshake again"*, which stays exactly true; `CancelSupport::Supported` is a duty owed on a dropped future, and it goes on being owed — what changes is the frame the peer sees. **No `hclient-core` change is needed** |

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

`hclient_rt::Spawn` declares **zero bounds** — `pub trait Spawn<F: Future<Output = ()>> { fn spawn(&self, f: F); }` — so
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
added to `crates/hclient-native/src/lib.rs` and `src/http2.rs`,
`cargo build -p hclient-native --all-features --tests` was green, so were
`hclient-select` and `hclient-tungstenite`, and the change was reverted.
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
`hclient-fetch`'s `Rc<RefCell<..>>` — and a spawned driver is where it
collides with `Spawn`'s `Send + 'static`. **`hclient-h3` met the same
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

`crates/hclient-native/src/pool.rs` is built around **take**: `Pool::take`
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
`hclient-rt-embassy`, which is expected to have no `Spawn` at all
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
6. **`hclient-core` does not change.** P15.
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
writes the driver, and it is the one place §8's "`hclient-core` does not
change" might not hold.

### 9.3 Whether `without_pool()` and `multiplexed()` can both be asked for

There is nowhere to share a connection without a pool, so the pair is
either a refusal at construction — the shape `hclient-select` uses when two
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
hclient-rt          = { path = "…/crates/hclient-rt" }
hclient-rt-tokio    = { path = "…/crates/hclient-rt-tokio" }
hclient             = { path = "…/crates/hclient" }
hclient-core        = { path = "…/crates/hclient-core" }
hclient-native      = { path = "…/crates/hclient-native", features = ["http2"] }
hclient-dns         = { path = "…/crates/hclient-dns" }
hclient-dns-system  = { path = "…/crates/hclient-dns-system" }
hclient-tls         = { path = "…/crates/hclient-tls" }
hclient-tls-rustls  = { path = "…/crates/hclient-tls-rustls" }
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
error[E0277]: the trait bound `FakeRt: hclient_rt::Spawn<H2Driver<FakeStream>>` is not satisfied

$ RUSTFLAGS="--cfg probe_a4_neg" cargo build     # Rc hook + multiplexed()
error[E0277]: `Rc<Cell<usize>>` cannot be sent between threads safely

$ RUSTFLAGS="--cfg probe_a4_pos" cargo build     # Arc hook + multiplexed()
    Finished `dev` profile
```

The same field and constructor were then added to this tree's real
`Native` over `NativeIo<R, T>`, built with
`cargo build -p hclient-native --all-features --tests`, and reverted.

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

## 11. Built (v0.4)

Everything above is the investigation, kept as it was written. This
section is what it became, and where it differs from what §0–§9 predicted.

`crates/hclient-native/src/{lib,pool,http2,established,staged}.rs`;
`crates/hclient-native/tests/http2_multiplex.rs` plus siblings in
`tests/http2.rs` and `tests/grpc_shape.rs`.

### 11.1 The opt-in, and what it does not touch

```rust
let transport = Native::new(Tokio, tls, dns).multiplexed();
```

`Native::multiplexed()`, bounded on
`R: Spawn<H2Driver<NativeIo<R, T>, H>>` and on nothing else. The spawner
is stored as `Option<fn(&R, H2Driver<NativeIo<R, T>, H>)>` — §2.2's shape,
unchanged from the probe — so **no signature a `Spawn`-less runtime meets
gains a bound**. `crates/hclient/tests/two_runtimes.rs` and
`tests/h1.rs::works_on_a_bare_futures_executor_with_no_spawn` are green
and untouched, which is the property this whole shape exists for.

The refusals are `compile_fail` doctests on the constructor, each next to
a control that differs in one token — a `Spawn`-less runtime beside the
same runtime with a `Spawn` impl added, and an `Rc` hook beside the same
hook behind an `Arc`. The messages were read once by building the two
calls as an ordinary test: ``the trait bound `NoSpawn: Spawn<H2Driver<Conn<
TokioIo, NoStream>, NoHooks>>` is not satisfied`` and
``` `Rc<Cell<usize>>` cannot be sent between threads safely ```, both
pointing at the `multiplexed()` call. (`compile_fail,E0277` would say so
in the fence, but rustdoc's error-code annotation is unstable and is not
enforced on stable — measured: the same block passes annotated `E0432`.)

**One ordering rule was not foreseen and is a real cost.** The spawner's
type names `H`, because the driver carries the hook, so `Native::hooks`
cannot carry that pointer across a change of `H`: `.hooks(..)` must come
**before** `.multiplexed()`, and the other order compiles and shares
nothing. That is the same shape, stated the same way, as `tcp_opts`
replacing the whole option set, and it is pinned by a pair of orders
(`hooks_before_multiplexed_shares_and_hooks_after_it_does_not`) rather
than only written down.

### 11.2 Single-flight connect, which §3 did not predict

**Sharing needs the connect to be single-flight, and this is not an
optimisation.** A burst of N concurrent requests to a **cold** origin has
no first request to share from: all N find the pool empty, all N connect,
and each shared connection is then shared with nobody. Without it the
headline row is unreachable and `grpc_shape.rs`'s
`two_concurrent_calls_take_two_connections_rather_than_two_streams` does
not invert, because both of its calls start cold.

So `Pool::begin_connect` hands out a mark per origin, `Connecting`'s
`Drop` releases it and wakes everyone waiting, and a waiter **waits once**:
after the wake it looks in the pool again, and if there is still nothing
there it takes the mark itself and connects. That bounds the cost of a
failed connect to one extra wait per waiter, where a loop would let a
permanently unreachable origin hold a request for ever
(`a_failed_shared_connect_releases_everyone_waiting_for_it`).

Two things about it are decisions:

- **The mark is taken before the pooled lookup, not before the connect**,
  so that the window between "the pool is empty" and "I am connecting" is
  not a window at all. It is released the moment there is a connection to
  find — on the pooled path as well as after a fresh connect — because a
  waiter is waiting for a connection to *exist*, and making it wait for
  somebody else's response as well would turn one slow server into a
  queue. Both releases are load-bearing: removing the pooled one deadlocks
  the eight-call burst against a barrier server.
- **`Timeouts::connect` is spent once.** The wait is bounded by the
  caller's connect bound and what is left of it is what the waiter's own
  connect gets — the arithmetic `hclient-select`'s h3 fallback does one
  layer up and `Client`'s `425` replay does one layer down. Pinned by an
  A/B in which neither arm asserts on a duration
  (`waiting_for_a_shared_connect_spends_the_callers_connect_bound`), and
  the first version of that test was **wrong**: its two sleeps left no
  margin the mutation could show, so it passed with a fresh budget too.

Coalescing is asked for only when the transport shares connections *and*
`may_speak_h2` — so `http://`, an IP-literal-only build without TLS, a
backend that cannot report ALPN and a `RequireVersion(HTTP_1_1)` demand
never pay for it.

### 11.3 What the pool became

§3 was right about the shape and one sentence short of the mechanism.

- **`take` borrows a shared entry and takes everything else**, decided by
  the entry (`Established::borrowed`) rather than by a flag on the pool,
  because a bucket can hold both kinds. `CheckIn` is not used on the
  shared path at all, and `H2Body::hand_back_to_pool` is a no-op there
  because `exchange_shared` builds a body with no `reuse`.
- **A borrowed entry's deadline is restamped**, which §3 does not mention
  and which is `PoolConfig::idle_timeout`'s own rule rather than an
  exception to it: the deadline is measured *"from when the connection was
  last handed out"*, and borrowing is handing out. Without it a shared
  connection under continuous load is dropped one idle timeout after the
  traffic *started*.
- **`Pool::forget_shared` is the other half of borrowing**, and §3 does
  not have it. A clone that `is_reusable` rejects leaves the entry it was
  cloned from in the pool, so a checkout that only dropped the clone would
  borrow the same dead connection **for ever** — not a failed test, a spin.
  It is keyed on a private `SharedId` counter and deliberately **not** on
  `ConnectionId`: in a build with no hook every connection wears
  `ConnectionId::UNWATCHED`, so an eviction keyed on it would empty the
  bucket instead of removing one entry, in exactly the builds with no way
  of noticing.
- **`PoolKey` needs nothing new**, as §3.1 says — but the entry still has
  to go into the right bucket, and only one thing notices. An ordinary
  request would not: `pooled_candidates` offers `[H2, Http11]` and
  `established::exchange` dispatches on the connection rather than on the
  key. A `RequireVersion(HTTP_2)` demand skips the `Http11` bucket
  outright, and the `Reused` event reads its version off the bucket. Both
  are asserted in `a_demand_for_http2_is_served_by_the_shared_connection`,
  which is what killed the misfiled-bucket mutation that everything else
  survived.
- **`max_idle_per_key` did not have to change** (§3.4 left it open). With
  sharing there is normally one entry per key, and the bound goes on
  meaning what it says about the entries there are.
- **`Staged::drop` does not check a borrowed clone back in.** A staged
  connect that is never spent hands its connection back to the pool; a
  shared one was never taken out of it, so putting it back would leave two
  entries naming one connection. `crates/hclient-select` reaches this path
  and nothing there opts in, so the guard is correctness-by-construction
  rather than something a test can see — recorded as a survivor in §11.5.

### 11.4 The cost, re-measured — and the fixture was measuring Nagle

Same shape as §5.2 and the same instrument (`/proc/self/stat` user+system,
both ends in this process), but D3 is now the **real** transport with
`multiplexed()` rather than a hand-built shared connection, and there is a
fifth row. `Native<Tokio, Rustls, IpLiteralOnly>` against a
`tokio-rustls` + `h2::server` on loopback with ALPN `h2` and a generated
IP-SAN certificate; 60 bursts of 8 concurrent calls = 480 requests.
**Nine runs; `D0` read `0 ms` in all nine.** Medians, with ranges.

| row | shape | accepts | requests | cpu | wall |
|---|---|---|---|---|---|
| D0 | both runtimes up, nothing asked of them, 400 ms | — | — | **0 ms** (0 in all nine) | — |
| D1 | exclusive, 60 **cold** bursts of 8 | **480** | 480 | **550 ms** (540–600) | 280 ms |
| D3 | shared, 60 **cold** bursts of 8 | **60** | 480 | **180 ms** (160–210) | 108 ms |
| D4 | exclusive, **warm** pool, 60 bursts of 8 | 7 | 480 | **70 ms** (60–80) | 39 ms |
| D5 | shared, **warm** pool, 60 bursts of 8 | **0** | 480 | **110 ms** (90–130) | 48 ms |

Read it this way, and only this way:

- **The counts are exact and are the claim.** 480 accepts against 60 for
  the same 480 requests: eight times the sockets and eight times the
  TLS+h2 handshakes at a concurrency of 8, measured through the real
  transport in both arms. §5.1's prediction holds.
- **Cold CPU is ~3× less**, and the ranges do not overlap (540–600
  against 160–210). §5.2's "roughly 2× less" was measured on a
  hand-built arm and a Nagle-padded fixture; this is the same direction,
  larger.
- **In steady state sharing costs CPU and saves sockets, and that is the
  new row.** D5 against D4 is 110 ms against 70 ms of CPU and 0 accepts
  against 7 — one connection serialises eight streams' framing through
  one task with a cross-thread wake per frame, where eight connections
  frame in parallel with no hand-off. On loopback there is no RTT and no
  handshake to win back, which is exactly where sharing has nothing to
  gain; on a real network D1/D3's 8× handshake difference is the term
  that dominates.
- **D2 is still deliberately not in the table**, for §5.2's reason.

**The fixture had to be fixed first, and the finding is worth more than
the milliseconds.** The first version did not set `TCP_NODELAY` on the
accepted socket, and every *shared* burst then cost **41.8 ms** where the
exclusive ones cost nothing — D5's wall was 2.5 s rather than 48 ms.
Nagle only delays a second small unacknowledged write, and eight responses
on one connection is that pattern where eight responses on eight
connections is not. That is `docs/v04-w1-acceptance.md` §7.3's lesson
arriving from the other direction — *"the fixture had been handing QUIC a
free 40 ms head start, so the earlier row measured Nagle rather than the
protocols"* — and it also explains §5.2's wall figures, which were taken
against a fixture with the same gap.

### 11.5 Mutations

Anchor verified before each run and printed by each run's own `Summary`
line: **240** tests for the first batch, **241** after
`a_demand_for_http2_is_served_by_the_shared_connection` was added; every
run `--no-fail-fast`, restored with `git checkout` plus a fresh mtime. The
five survivors were re-run at 241.

| # | mutation | result | what killed it |
|---|---|---|---|
| M1 | `Pool::take` never borrows — every entry is taken | **killed** (3) | `eight_concurrent_calls_travel_over_one_connection`, `beyond_the_peers_stream_limit_…`, `a_shared_connection_that_keeps_being_used_…` |
| M2 | a borrowed entry's deadline is not restamped | **killed** (1) | `a_shared_connection_that_keeps_being_used_is_not_dropped_for_age` |
| M3 | `checkout` does not `forget_shared` a rejected clone | **killed — by hang** | `a_dead_shared_entry_is_removed_rather_than_borrowed_for_ever` spins; the suite never finished |
| M4 | the shared entry is never published to the pool | **killed** (8) | `eight_concurrent_…`, `multiplexing_turns_those_two_connections_…`, `dropping_one_exchange_…_on_a_shared_connection`, +5 |
| M5 | the driver is never spawned | **killed** (15) | `a_request_body_crosses_a_shared_connection_whole`, `a_shared_connection_carries_a_duplex_exchange`, +13 |
| M6 | **control** — the `CheckIn` is not cleared on the shared path | **survived, as intended** | — |
| M7 | `shares_connections` drops the "there is a pool" conjunct | **survived** | — |
| M8 | no single-flight: the connect mark is never taken | **killed** (6) | `eight_concurrent_…`, `multiplexing_turns_…`, `waiting_for_a_shared_connect_…`, +3 |
| M9 | the wait hands back a fresh `Timeouts::connect` | **killed** (1) | `waiting_for_a_shared_connect_spends_the_callers_connect_bound` |
| M10 | the driver reports `Stale` where it reports `Ended` | **killed** (1) | `the_driver_reports_the_end_of_a_shared_connection` |
| M11 | `shared_is_reusable` always answers `true` | **killed** (1) | `a_dead_shared_entry_is_removed_rather_than_borrowed_for_ever` |
| M12 | a dead shared `poll_ready` is `Failed::Sent`, not `NotSent` | **survived** | — |
| M14 | the connect mark is released *before* the entry is published | **survived** | — |
| M15 | `Staged::drop` checks a borrowed clone back in | **survived** | — |
| M16 | the duplex line returns the pump's `Pending` as its own | **killed** (1) | `a_shared_connection_carries_a_duplex_exchange` |
| M17 | the leftover pump is dropped instead of moving into `H2Body` | **killed** (1) | `a_shared_connection_carries_a_duplex_exchange` |
| M18 | the connect mark is not released on a pooled hit | **killed** (1) | `eight_concurrent_calls_travel_over_one_connection` (deadlock, then the ceiling) |
| M19 | the driver reports nothing at all | **killed** (1) | `the_driver_reports_the_end_of_a_shared_connection` |
| M20 | the shared entry is filed under `Protocol::Http11` | **killed** (1) | `a_demand_for_http2_is_served_by_the_shared_connection` |

**14 killed, 5 survived**, and the survivors are worth reading rather than
counting.

**M6 is the control and was written to be one.** `*checkin = None` in
`share_if_multiplexing` says in code what the shared path is: no check-in.
It cannot be observed, because `established::exchange`'s `H2Shared` arm
ignores its `checkin` argument outright and `exchange_shared` builds a
body with `reuse: None` whatever it is handed. Belt to braces that are
already structural.

**M14 is a control by accident and is worth naming as one.** The two
statements it swaps — publish the shared entry, release the connect mark —
are separated by no `await`, so a woken waiter would have to preempt
within a few hundred nanoseconds of synchronous work to see the
difference. The order is still the right one and costs nothing.

**M7 narrowed a sentence.** The conjunct does *not* make `multiplexed()`
and `without_pool()` order-independent, which is what its doc comment
originally claimed: `share_if_multiplexing` reads the pool's configuration
for the deadline it stamps, so a pool-less transport shares nothing with
or without it. What it buys is that such a transport does no single-flight
work either — the difference is a wait rather than an outcome, so no test
asserts it. The doc comment now says that instead.

**M12 is the residual pooled-reuse race, one protocol over.** With
`forget_shared` doing its job, `exchange_shared` meets a dead `poll_ready`
only when the connection dies between `is_reusable` and `send_request` —
the window `h1.rs` already records as the one no pool can close. No
fixture here creates it. `NotSent` is the conservative answer (it permits
the retry `Native::run` already has) and is what the exclusive path
answers in the same place.

**M15 is a resource property with no observer.** A staged connect that is
dropped unspent would check a borrowed clone back in, leaving two entries
naming one connection — both live, both removed by the same
`forget_shared`, and `max_idle_per_key` bounds the bucket either way. It
is kept because it is right, not because a test says so.

One mutation is missing from the table because it **changed the code**
rather than surviving it: `exchange_shared`'s loop had a `continue` after
the pump finished, copied from `exchange`, and removing it left the whole
suite green. It is genuinely not owed here — the frames the pump queues
are written by the driver, woken by the queueing — so the loop is gone and
the function is straight-line, with the difference from `exchange`
written where the `continue` used to be.

### 11.6 §9's open questions, answered

**§9.1 — when to open a second connection.** *Never: a full connection
queues.* The number §9.1 asks for cannot be chosen honestly here.
`poll_ready` is a liveness check and not a capacity one, so the count
would have to be ours; and the threshold depends on the peer's
`MAX_CONCURRENT_STREAMS` (which h2 will not report) and on the handshake
cost, which is a network property and not a loopback one — a number
measured on this machine would be a number about this machine. What is
measured instead is what queueing costs, and it is asserted as the
server's own high-water mark rather than as a clock: at a limit of 2, six
concurrent calls all succeed on **one** connection with never more than
two streams open
(`beyond_the_peers_stream_limit_requests_queue_on_one_connection`). The
price is stated on `Native::multiplexed` with §6.2's 203/405/607 ms
beside it.

**§9.2 — what a `Closed` event says for a shared connection.**
`hclient-core` did not have to change, which is the half §9.2 doubted.
The driver emits `CloseReason::Ended` when h2 resolves the connection
`Ok` — a clean close or a `GOAWAY` carrying no error, which is exactly
the variant's *"nothing went wrong — there is simply no second request to
be had on it"* — and `CloseReason::Failed` otherwise. This is also the
first `Closed` an h2 connection in this crate has ever emitted:
`established::Inner::H2`'s doc records that the h2 body emits none
because the end can arrive in three places there, and a driver is one
place.

**§9.3 — `without_pool()` and `multiplexed()` together.** Neither a
refusal nor last-call-wins: **the pair does what the weaker of the two
says, whichever order it is written in**, because the shared path is
entered only where a pool exists rather than remembered as a flag. Pinned
in both orders.

**§9.4 — an h2 connection at a network change.** Not done, and now
sharper than it was: a shared connection's driver goes on polling a socket
that will never carry anything again, where an idle pooled one merely
sits. What it needs is what `Selecting::network_changed()` needed — a
public entry point on `Native`, because a `Transport` cannot see what its
caller can.

### 11.7 What this did not check

- **No `RequireVersion` interaction beyond the pool bucket.** The demand
  narrowing the ALPN offer, and the refusal, are `tests/require_version.rs`'s
  and are untouched by sharing.
- **Nothing about `hclient-select`.** `Selecting` never calls
  `multiplexed()`, and `StagedConnect::connect` does not turn the
  connection it makes into a shared one — it uses one if the pool has one,
  which is a defensible half and is not measured here.
- **No second `Native` sharing one pool.** `PoolKey`'s TLS-identity
  component is still the field kept for the day a pool is shared across
  clients, and sharing connections does not bring that day nearer.
- **The peer's `MAX_CONCURRENT_STREAMS` is only exercised at 2.** The
  waves at higher limits, and what a server that *lowers* the limit
  mid-connection does, are unmeasured.
- **No measurement over a real network.** Every figure in §11.4 is
  loopback, where the handshake saving is the whole of the win and the
  RTT saving is zero.
- **Two workspace-level flakes were seen and could not be attributed.**
  `hclient-native`'s own suite is 12 clean runs at `-j16`, but the full
  `--workspace --all-features` run failed 3 times in 39 on this branch —
  once in `hclient-h3::zero_rtt::early_data_is_accepted_…` and twice in
  `hclient-select::race::with_no_head_start_both_stacks_connect_…`, both
  timing-sensitive races in crates this work does not touch. **The base
  commit flakes too**, 1 in 42, in the same `hclient-select` test; and the
  select test alone is 30 clean runs on each tree. So the flake is
  pre-existing; whether the higher rate here is the extra load of 19 new
  tests (each spawning a server with its own runtime) or noise is **not
  distinguishable at these sample sizes**, and is written down rather than
  claimed either way. The h3 one has a precedent: `docs/v04-*` records the
  same suite at 2 failures in 277 concurrent runs.

### 11.8 The cost harness, to re-run

Not kept as a test, for §10's reason: it is a measurement rather than a
claim about this code, it depends on `h2`, `rcgen`, `rustls` and
`tokio-rustls` directly, and an `#[ignore]`d test nothing runs is the
defect `just test-doc` was fixed for. Recorded here so the table in §11.4
can be re-taken rather than believed. Drop it in as
`crates/hclient-native/tests/zz_cost.rs`, run
`cargo nextest run -p hclient-native --all-features --test zz_cost
--no-capture`, and delete it again; every dependency it names is already a
dev-dependency of that crate.

**`tcp.set_nodelay(true)` on the accepted socket is the line that took a
diagnosis.** Without it every shared burst costs 41.8 ms and every
exclusive one costs nothing, for the reason §11.4 gives; the per-burst
timings that showed it were `Instant::now()` around each `burst(..)` call,
and they are what turned "the shared arm is slow" into "the fixture is
measuring Nagle".

```rust
//! What sharing costs and saves, over real TLS. Temporary harness.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]

use bytes::Bytes;
use hclient::Client;
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BURSTS: usize = 60;
const CONC: usize = 8;

fn cpu() -> Duration {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap();
    // `comm` may contain spaces; everything after the closing paren is
    // space separated, and utime/stime are fields 14 and 15 (1-based).
    let after = &s[s.rfind(')').unwrap() + 2..];
    let f: Vec<&str> = after.split_whitespace().collect();
    let ticks: u64 = f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap();
    Duration::from_secs_f64(ticks as f64 / 100.0)
}

fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    (
        CertificateDer::from(cert.cert.der().to_vec()),
        PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap(),
    )
}

struct Fx {
    addr: SocketAddr,
    accepted: Arc<AtomicUsize>,
    served: Arc<AtomicUsize>,
    cert: CertificateDer<'static>,
}

fn spawn_tls_h2() -> Fx {
    let (cert_der, key_der) = identity();
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .unwrap();
    cfg.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let served = Arc::new(AtomicUsize::new(0));
    let (a, s) = (Arc::clone(&accepted), Arc::clone(&served));

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                // Without this the shared rows measure Nagle. See above.
                let _ = tcp.set_nodelay(true);
                a.fetch_add(1, Ordering::SeqCst);
                let acceptor = acceptor.clone();
                let s = Arc::clone(&s);
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let Ok(mut conn) = h2::server::handshake(tls).await else {
                        return;
                    };
                    while let Some(Ok((req, mut respond))) = conn.accept().await {
                        let s = Arc::clone(&s);
                        tokio::spawn(async move {
                            let (_, mut body) = req.into_parts();
                            while let Some(Ok(c)) = body.data().await {
                                let _ = body.flow_control().release_capacity(c.len());
                            }
                            let r = http::Response::builder().status(200).body(()).unwrap();
                            if let Ok(mut send) = respond.send_response(r, false) {
                                let _ = send.send_data(Bytes::from_static(b"ok"), true);
                            }
                            s.fetch_add(1, Ordering::SeqCst);
                        });
                    }
                });
            }
        });
    });

    Fx { addr, accepted, served, cert: cert_der }
}

fn client_tls(cert: &CertificateDer<'static>) -> Rustls {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Rustls::from_config(Arc::new(cfg))
}

type C = Client<Native<Tokio, Rustls, IpLiteralOnly>>;

fn client(fx: &Fx, shared: bool) -> C {
    let t = Native::new(Tokio, client_tls(&fx.cert), IpLiteralOnly);
    let t = if shared { t.multiplexed() } else { t };
    Client::builder(t).build().unwrap()
}

async fn burst(client: &C, url: &str, n: usize) {
    let calls = (0..n).map(|_| async {
        let r = client.get(url).send().await.expect("request");
        assert_eq!(r.status(), 200);
        r.collect().await.expect("body");
    });
    futures_util::future::join_all(calls).await;
}

/// `(accepts, requests, cpu, wall)` for `BURSTS` bursts of `CONC`.
macro_rules! measure {
    ($fx:expr, $body:block) => {{
        let (a0, s0, t0, w0) = (
            $fx.accepted.load(Ordering::SeqCst),
            $fx.served.load(Ordering::SeqCst),
            cpu(),
            Instant::now(),
        );
        $body
        (
            $fx.accepted.load(Ordering::SeqCst) - a0,
            $fx.served.load(Ordering::SeqCst) - s0,
            cpu() - t0,
            w0.elapsed(),
        )
    }};
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cost() {
    let fx = spawn_tls_h2();
    let url = format!("https://{}/x", fx.addr);

    // D0 — the instrument's own check. A run whose D0 is not zero is a
    // run on a busy host and is not one of the nine.
    let t0 = cpu();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let d0 = cpu() - t0;

    let d1 = measure!(fx, {
        for _ in 0..BURSTS {
            let c = client(&fx, false);
            burst(&c, &url, CONC).await;
        }
    });
    let d3 = measure!(fx, {
        for _ in 0..BURSTS {
            let c = client(&fx, true);
            burst(&c, &url, CONC).await;
        }
    });
    let c4 = client(&fx, false);
    burst(&c4, &url, 1).await;
    let d4 = measure!(fx, {
        for _ in 0..BURSTS {
            burst(&c4, &url, CONC).await;
        }
    });
    let c5 = client(&fx, true);
    burst(&c5, &url, 1).await;
    let d5 = measure!(fx, {
        for _ in 0..BURSTS {
            burst(&c5, &url, CONC).await;
        }
    });

    println!("D0 idle 400ms                     cpu={d0:?}");
    for (name, d) in [
        ("D1 exclusive, 60 COLD bursts of 8", d1),
        ("D3 shared,    60 COLD bursts of 8", d3),
        ("D4 exclusive, 60 WARM bursts of 8", d4),
        ("D5 shared,    60 WARM bursts of 8", d5),
    ] {
        println!(
            "{name} accepts={} requests={} cpu={:?} wall={:?}",
            d.0, d.1, d.2, d.3
        );
    }
}
```
