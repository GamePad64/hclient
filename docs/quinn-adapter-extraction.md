# `SeamRuntime` is `hclient-quinn`

`docs/v04-w2-webtransport.md` §4 recorded a debt and named the two ways to
pay it. This is the second one.

> `hclient-h3` exposes no `quinn::Connection`, or an endpoint. […] `mod
> runtime` is private — of which only the `QuinnTask` type alias is
> re-exported. **`SeamRuntime`**, the `quinn::Runtime` over
> `hclient_rt::{Timer, Spawn, UdpBind}`, is therefore unreachable: 302
> lines of code (494 with its documentation), including the `WakeAll`
> fan-out. […] **What would close it** is one of: 1. `pub use
> runtime::SeamRuntime;` — one line. The honest version of that is a move
> to a crate of its own (`hclient-quinn`) […]; 2. a **connect-only
> entry point** on `H3`.

Option 1, in its honest version. Option 2 is a separate question and
`docs/connect-only-seam.md` is where it is being answered; §5 below is why
the two are **not** alternatives, which is the one thing §4 got wrong.

## 1. Why a crate and not the one-line `pub use`

The same argument every other pluggable thing in this workspace is made
of, and it survives the fact that this one was never a feature.

`docs/w4-upgrade-seam.md` §8's version is about additivity: `tungstenite`
was a `websocket` feature of `hclient-native`, and Cargo's features are
additive, so one crate anywhere in a graph switching it on put the framing
into every other crate's build of the transport. A dependency in the other
direction cannot be switched on from outside.

`SeamRuntime` was **not** a feature, so that argument does not transfer
unchanged. It was private, which is the other way a shared thing stops
being shared, and the failure mode is different in a way that matters:
a feature that is on when it should be off costs a graph, and an adapter
that cannot be reached costs a **copy**. `hclient-webtransport` did not
copy it, and the cost of not copying it was that the crate takes a
`quinn::Connection` from its caller.

Where the additive argument *does* bind is on the alternative nobody
proposed and somebody eventually would have: a `quinn` feature on
`hclient-rt`. That would put `quinn-proto`, `quinn-udp` and `ring` into
every build of the seam in any graph that switched it on — and the seam
re-declares `RecvMeta` and `EcnCodepoint` rather than re-exporting
`quinn-udp`'s precisely so that cannot happen (`just graph-no-quic`). So
the QUIC vocabulary and the seam's vocabulary meet in exactly one place,
and that place is a crate of its own.

The `pub use` would have worked. It would also have left every future
consumer of the adapter depending on `hclient-h3` — 57 crates, `h3`,
`h3-quinn`, `tokio-util` — to obtain a `quinn::Runtime` that needs none of
them.

## 2. What moved, and what did not

`git mv crates/hclient-h3/src/runtime.rs crates/hclient-quinn/src/lib.rs`.
The whole file, with its documentation, its four unit tests and every
`send-bound-exception: amendment-C10` marker.

| item | where it is now |
|---|---|
| `QuinnTask` | `hclient-quinn`, re-exported by `hclient-h3` |
| `SeamRuntime<R>` + `quinn::Runtime` impl | moved, `pub` as before |
| `SeamTimer<R>` + `quinn::AsyncTimer` impl | moved, private as before |
| `WakeAll` | moved, private as before |
| `SeamSocket<S>` + `quinn::AsyncUdpSocket` impl | moved, private as before |
| `SeamPoller<S>` + `quinn::UdpPoller` impl | moved, private as before |
| `to_quinn_ecn` / `from_quinn_ecn` | moved, private as before |
| `until` | moved, private as before |
| `endpoint(&rt, local)` | moved, **`pub(crate)` → `pub`** |

**One visibility change and no signature changes.** `endpoint` is the
crate's reason to exist for anyone who is not `hclient-h3`: it is the
function that knows to use `new_with_abstract_socket` rather than
`Endpoint::client`, so that the capability actually required is
[`UdpBind`] — the trait every runtime here can implement — rather than
`UdpAdoptStd`, which only the `quinn::Runtime` trait's own
`wrap_udp_socket` demands. That reasoning was already written down beside
the function; it was simply unreadable from outside.

**What stayed in `hclient-h3`:**

- `H3::endpoint`, the v4/v6 slot cache and its lazily-on-first-use policy.
  It calls `hclient_quinn::endpoint` and is otherwise untouched. The
  policy is `H3`'s (one endpoint per address family, per transport), not
  the adapter's.
- `H3Runtime`. It is `hclient-h3`'s public API and it is named for
  `hclient-h3`; moving it would have renamed a trait for no gain. It also
  buys less than it looks, and the reason is worth knowing before anyone
  tries to reuse it: a supertrait list cannot carry associated-type
  bounds, so `R: H3Runtime` tells the compiler nothing about `R::Sleep` or
  `R::Socket` and every impl block repeats them anyway. The new crate
  therefore spells its bound out in `endpoint`'s `where` clause, exactly
  as the function did before it moved.
- Everything else: `H3`, the pool, `H3Body`, the pump, the hooks, 0-RTT.

**`ring` went with the endpoint.** `hclient-h3`'s `quinn` is now
`default-features = false, features = ["futures-io"]`; `ring` is
`hclient-quinn`'s. Binding an endpoint is the single operation in
either crate that wants a crypto provider independently of any TLS
session — an HMAC key for stateless resets, an AEAD for retry tokens,
which is why `quinn::EndpointConfig` has no `Default` without one — and
that call is not in `hclient-h3` any more. Nothing about the resolved
graph changes: `ring` is still there, through this dependency and through
rustls. What changes is which manifest asks for it, and therefore which
comment is true.

## 3. The graph

`cargo tree -e normal --prefix none`, unique crates, this tree.

| crate | before | after |
|---|---|---|
| `hclient-h3` | 57 | **58** |
| `hclient-h3 --all-features` | 57 | **58** |
| `hclient-quinn` | — | **42** |
| `hclient-webtransport` | 49 | 49 (untouched) |

The diff of `hclient-h3`'s two lists is one line long and it is
`hclient-quinn` itself. Nothing arrived, nothing left, no feature of any
third-party crate changed.

The 42 are `hclient-rt`'s graph plus `quinn`, `quinn-proto`, `quinn-udp`,
`ring` and what those pull. `hyper` and its inert `tokio` `sync` leaf are
in it because `hclient-rt` depends on hyper unconditionally — the fact
AGENTS.md's dependency table already states twice, measured in a third
place here and not a new one.

What is **not** in the 42 is the point: no `h3`, no `h3-quinn`, no
`tokio-util`, no `hclient-tls`, no `hclient-dns`. A crate that wants bare
QUIC over this workspace's runtime seam takes 42 crates rather than 58,
and takes no opinion about HTTP at all.

## 4. `WakeAll`, and the half of it that had no test

The task this extraction was done under names the fan-out as the part to
be careful with, and it was right, though not for the reason it gave: the
fan-out moved intact — it is a `git mv` — and what the move exposed is
that **one of its two arms was never tested.**

What it is for, restated because a waker fan-out moved without its reason
is how a subtle bug arrives. [`UdpDatagrams::poll_writable`] takes `&self`
and a `Context`, so an implementation over `tokio::net::UdpSocket::
poll_send_ready` or `async_io::Async::poll_writable` stores **one** waker:
the last caller to register wins and every earlier one sleeps for ever.
quinn creates a `UdpPoller` per connection *and* one for the endpoint
driver, and says so in its own source — *"Each `UdpPoller` is responsible
for notifying at most one task"* — and they can all be blocked on
writability at once when a send buffer fills. `WakeAll` keeps the list
quinn assumes without asking the seam to keep it.

The list has to be drained in two places, and only the obvious one had a
test:

- the socket became writable, so the fan-out waker fires and `wake_all`
  runs from `impl Wake for WakeAll`. `a_socket_with_one_waker_slot_still_
  wakes_every_waiting_poller` covers this.
- **a poller was handed `Poll::Ready`, which consumed the inner socket's
  one registration.** The fan-out waker it was holding is spent and the
  socket is now holding nothing, so every *other* poller on the list is
  waiting on a wake-up that can no longer come from anywhere. That is the
  same stall, arriving through the success path.

`poll_writable_shared` calls `wake_all()` on that arm, with a comment
saying exactly why. **Deleting the line survived the entire suite** (M8
below), because the only socket fixture in the file returns `Pending` for
ever and nothing ever reached the success path.

`a_ready_answer_wakes_the_pollers_it_did_not_answer` is the test, on a
`ReadyOnSecondPoll` fixture — Pending once, writable after, which is the
shape a real socket has the moment its send buffer drains. It is the only
test added by this work, and it is an addition rather than a change: the
four that moved are byte-identical to the four that left.

## 5. Can `hclient-webtransport` build its own connection now?

**Mechanically yes, and it still should not, and the reason is not the one
§4 implied.** `hclient-webtransport` is untouched by this work.

§4's list reads as if options 1 and 2 were alternatives — either the
adapter becomes reachable, or `H3` grows a connect-only entry point.
They are not, and the agent writing `docs/connect-only-seam.md` found
why while this was landing: **a connect-only entry point on `H3` cannot
serve WebTransport at all.** `H3::connect` builds an
`h3::client::builder().build(..)` on the connection and spawns its driver
before it has a connection to hand back, and `Session::connect` builds
*its own* h3 client with `enable_extended_connect(true)`. Two h3 clients
on one QUIC connection open two control streams, which RFC 9114 §6.2.1
makes `H3_STREAM_CREATION_ERROR`. §4b already gives that as the first of
three reasons a session cannot share a *pooled* connection; it applies
just as hard to a fresh one, because `H3` never hands out a connection it
has not already claimed.

So option 1 is the one that closes the gap, and what remains on
`hclient-webtransport`'s side is its own dialling: `hclient_quinn::
endpoint`, a `QuicTlsConnect` for the crypto with ALPN `h3`, and an
address from somewhere. That is a small amount of code. It is not done
here, for two reasons that are about the crate rather than about the
extraction.

**It is an addition, not a replacement.** `Session::connect(conn, uri)`
stays whatever else happens — a caller who already holds a QUIC connection
is exactly the internal seam `docs/w4-upgrade-seam.md` §8 argues for, and
the tests use it. So the question was never "can it stop taking a
connection from outside" but "should it gain a second constructor beside
that one", which is a different decision and not this one.

**And it is measured rather than free.** Adding `hclient-quinn`,
`hclient-tls-quic` and `hclient-dns` to that crate takes it from **49
crates to 58** — measured, then reverted — and the nine are `hclient-rt`,
`hclient-quinn`, `hclient-tls`, `hclient-tls-quic`, `hclient-dns`,
`hyper`, `ring`, `untrusted` and `getrandom`. `ring` is the one AGENTS.md
names as *"the visible consequence of owning no endpoint"*, so the
sentence would have to change with the code.

The part that would want deciding first is the one the crate count hints
at rather than states: a dialling constructor there would be a **second**
place in this workspace where "how a QUIC connection is made" is settled —
which crypto, which ALPN, which keep-alive, whether 0-RTT, which endpoint
per address family — with nothing making it agree with `hclient-h3`'s.
That is the duplication this extraction removed one layer down, reappearing
one layer up. It wants the same answer (a shared thing, in one place) and
that shared thing cannot be `hclient-quinn`, because `QuicTlsConnect`
and `Resolve` would drag TLS and DNS into an adapter whose whole claim is
that it has neither.

**Recorded, not done.** What it needs: a decision about where a shared QUIC
*dial* lives, given that `hclient-h3`'s is entangled with its pool and its
h3 client.

## 6. Mutations

Anchor verified before the first and after the last: **5 tests, 5 passed**.
The harness is `crates/hclient-quinn/mutations.py` — the
`hclient-webtransport` one with a new table — and restore is `git checkout`
plus an explicit `os.utime`.

The run's purpose is not the usual one. This is a **move**, so what is
being checked is that the tests which travelled with the code still reach
it from the new crate: a move whose tests went green because they stopped
touching the code would look exactly like a move that worked.

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1 | `wake_all` wakes the first waiter only | killed | `a_socket_with_one_waker_slot_still_wakes_every_waiting_poller` |
| M2 | `register` drops the `will_wake` guard | killed | `re_registering_the_same_poller_does_not_grow_the_list` |
| M3 | `register` registers nothing at all | killed | three of five |
| M4 | `until` subtracts backwards | killed | `a_deadline_already_past_is_a_zero_sleep_not_a_panic` |
| M5 | `until` uses `deadline - Instant::now()` | **survived** | nothing — see below |
| M6 | `to_quinn_ecn` swaps `Ect0`/`Ect1` | killed | `ecn_survives_the_round_trip_through_both_conversions` |
| M7 | `from_quinn_ecn` swaps them back, so the round trip is clean | killed | the same test's `as u8` assertion |
| M8 | `poll_writable_shared` drops `wake_all()` from the `Ready` arm | killed | `a_ready_answer_wakes_the_pollers_it_did_not_answer` |
| M9 | **control** — `w.wake()` → `w.wake_by_ref()` | **survived, as intended** | nothing; they are the same operation one clone apart |

**Seven killed, two survived**, and the second survivor is the finding.

**M8 was a survivor on the first run and is a kill on this one.** §4 is
what happened in between. It is recorded here in both states rather than
only in the second, because the interesting fact is that a documented,
commented, deliberate line had no test at all.

**M5 is a control nobody planned.** `impl Sub<Instant> for Instant` calls
`duration_since`, which has **saturated since Rust 1.60** — so
`deadline - Instant::now()` and `deadline.saturating_duration_since(
Instant::now())` are the same function, and the mutant is not a mutant.
Measured on 1.97 rather than read: `past - Instant::now()` where `past` is
sixty seconds ago prints `0ns`.

That makes `until`'s doc comment stale rather than wrong. It said
`saturating_duration_since` was chosen *"rather than a subtraction"*
because a past deadline *"must become a zero-length sleep, not a panic"* —
and today the subtraction does not panic either. The call stays as it is,
for the reason std's own documentation gives beside that change: *"future
versions may reintroduce the panic in some circumstances"*. The comment now
says that, so the next reader who runs this mutation finds the answer
instead of the puzzle.

**The first attempt at this run was invalid and is worth one line.** It was
made with the new test uncommitted, and the harness's first `git checkout`
deleted it — the anchor came back as 4 rather than 5 and the harness
refused to report, which is the failure mode it was built to have. Commit
before a mutation run; the anchor check is what tells you when you did not.

## 7. What is checked, and what it fails on

`just graph-quinn-adapter-is-shared`, in the `dependency-graph` job. Three
assertions, and the third is the one that is not obvious:

- **absent** `^(h3|h3-quinn|hclient-h3) ` from `hclient-quinn
  --all-features`. The adapter must stay usable by anything that wants
  bare QUIC over the seam.
- **present** `^quinn ` in `hclient-quinn`, or the ban above would pass
  a crate that had been emptied out.
- **present** `^hclient-quinn ` in `hclient-h3`. This is the one that
  fires when someone re-adds a private `mod runtime` and drops the
  dependency — the shape "no copying" takes when it goes wrong, which no
  `absent` check can see.

All three were verified failing before being committed: `h3 = "0.0.8"`
added to the adapter fires the first, deleting the dependency from
`hclient-h3` fires the third, and a malformed manifest fires
`tree-guard.sh`'s own cannot-run-must-not-pass arm.

## 8. Unchanged, and checked to be

- **`hclient-h3`'s public API.** `QuinnTask` is a type alias, so a
  re-export of it *is* it; `hclient_h3::QuinnTask` names the same
  `Pin<Box<dyn Future<Output = ()> + Send>>` it always did.
  `crates/hclient-h3/tests/hooks_cost.rs` implements `Spawn<QuinnTask>`
  through that path and is unmodified apart from one doc-comment
  cross-reference.
- **`H3<R, T, D, H = NoHooks>`** and every bound on it. `hclient-select`
  compiles unchanged and was not opened.
- **`tests/two_runtimes.rs`**, including its sensitivity: adding
  `R::Instant: PartialEq<std::time::Instant>` to `fetch_once`'s where
  clause still produces three `E0277: can't compare tokio::time::Instant
  with std::time::Instant` and **no** error mentioning `Smol`. Re-run for
  this change, not quoted from the file's own doc.
- **Test counts.** `hclient-h3` went from 73 to 69 and `hclient-quinn`
  from 0 to 5 — the four unit tests that moved, plus the one added in §4.

## 9. Not done

- **A second consumer.** §5. The adapter is reachable now; nothing but
  `hclient-h3` reaches it yet, which means the "second consumer" argument
  for the crate is still an argument rather than a demonstration.
- **`H3Runtime` as a shared bound.** §2 says why it stayed. If a second
  consumer does arrive, the 8-term `where` clause is what it will copy,
  and *that* is when a named bound in `hclient-quinn` earns its keep —
  with the caveat that the associated-type bounds cannot travel in it.
- **`hclient-quinn` under `smol`.** Its own five tests are unit tests
  with a hand-written socket; the two-runtime evidence for this code lives
  in `hclient-h3`'s `two_runtimes.rs`, which exercises it under
  `TokioHandle` and `Smol` and still does. A pair test in the new crate
  would duplicate that rather than add to it.

[`UdpBind`]: ../crates/hclient-rt/src/caps.rs
[`UdpDatagrams::poll_writable`]: ../crates/hclient-rt/src/caps.rs
