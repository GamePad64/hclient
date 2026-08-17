# Nagle, `TCP_NODELAY`, and where the opinion lives

`docs/v04-w1-acceptance.md` §8 finding 4 recorded that every `Native`
connection carried Nagle's algorithm and that it cost 41 ms on the head of
a cold TLS exchange, and left the policy question open: *"whether the
default is right is a policy question for `http-ng-rt` and not for this
crate"*. This is the answer, the re-measurement it is based on, and the
one thing the change broke.

The short version: **`TcpOpts::default()` is unchanged, all-off.
`Native::new` asks for `nodelay`, and asks only where the runtime's
`TcpConnect::APPLIES` says it applies it.** A cold TLS 1.3 exchange on
loopback goes from 41.8 ms to 0.82 ms, and a third-party backend that
never declared `APPLIES` connects exactly as it did before.

---

## 1. The number, re-measured

`crates/http-ng-native/tests/nagle_cost.rs`, `#[ignore]`d and printing
rather than asserting — the rule this workspace follows for timing, and
the same shape as `http-ng-select`'s `race_cost.rs`:

```
cargo nextest run -p http-ng-native --test nagle_cost --run-ignored all \
    --no-capture -j1
```

Five arms, n = 10 per arm, each sample a **fresh** `Native` and therefore
a fresh connection — `Native::new` pools, and a pooled second request
never handshakes. One loopback server, TLS 1.3, `rustls` on both sides,
`https://127.0.0.1:port/`, an IP literal so no resolver is involved.

`execute` → response head, median (min – max):

| arm | debug | release |
|---|---|---|
| **`Native::new` and nothing else — after this change** | **0.935 ms** (0.850 – 2.306) | **0.821 ms** (0.257 – 1.053) |
| `tcp_opts(TcpOpts::default())` — Nagle on, as before | **42.21 ms** (41.07 – 43.65) | **41.82 ms** (40.95 – 42.07) |
| `tcp_opts(TcpOpts { nodelay: true, .. })` | 0.945 ms (0.863 – 1.92) | 0.338 ms (0.254 – 1.113) |
| Nagle on the client, `TCP_NODELAY` on the **server** | 41.86 ms (41.04 – 43.39) | 41.00 ms (40.92 – 41.06) |
| plaintext `http://` over `NoTls`, client default | 0.218 ms | 0.091 ms |

Response head → end of body, every arm, both profiles: **0.0009 – 0.011
ms.** All of the cost is in the head and none of it in the body, which is
what §7.3 found and what makes "the request waited" the only candidate
left.

The v0.4 figures were 41.8 ms and 0.464 ms, release. They reproduce.

## 2. It is Nagle, and it is the request head

§7.3 named Nagle **by elimination** — unchanged by an IP literal, so not
resolution; unchanged between debug and release, so not crypto. Elimination
cannot say which direction waited: a server that took 40 ms to answer and a
request that took 40 ms to arrive are the same picture from the client. So
this harness puts the observer on the wire, at the server: `Recorded` wraps
the accepted `TcpStream` and stamps every read that returns bytes **before
TLS sees them**, against a clock the client shares.

Release, one connection, times from that sample's own `execute`:

```
Nagle on (TcpOpts::default())        nodelay on
+ 0.132 ms   233 bytes               + 0.041 ms   233 bytes    ClientHello
+ 0.567 ms     6 bytes               + 0.154 ms     6 bytes    change_cipher_spec
+41.636 ms   137 bytes               + 0.246 ms    74 bytes    Finished
                                     + 0.257 ms    63 bytes    the GET
+41.999 ms   response head           + 0.277 ms   response head
```

Four things, and together they are conclusive:

- **The gap is on the inbound side, at the server**, between the client's
  third flight and its second. 40.1 – 41.1 ms in every sample of every run.
  The server answers 0.36 ms after it finally reads the request, so nothing
  on the response side is slow.
- **The 137 bytes are 74 + 63**, coalesced. That is Nagle's actual
  mechanism visible in the byte counts: two writes held and merged into one
  segment, sent when the outstanding data was acknowledged. With `nodelay`
  the same two writes arrive as two segments, 11 µs apart.
- **`TCP_NODELAY` on the server changes nothing** (41.00 ms). `TCP_NODELAY`
  and the delayed-ACK timer are different mechanisms on different hosts, so
  this is the arm that says which side is holding: the client's.
- **Plaintext `http://` does not stall at all** (0.091 ms). There the
  request head is the client's *first* write on the connection, and Nagle
  never holds a first write — there is nothing unacknowledged to wait for.
  A stall here would have meant the diagnosis was wrong.

So: the client's TLS Finished record goes out; the HTTP request is written
immediately after it, while the Finished is still unacknowledged; Nagle
holds it; the peer's delayed-ACK timer takes ~40 ms to acknowledge a
record it has nothing to answer. Classic write-write-read.

## 3. The shape, and the two that were rejected

### 3.1 Not `TcpOpts::default()` gaining `nodelay: true`

The obvious fix, and it is a compatibility break aimed precisely at the
people the current design protects.

`TcpConnect::APPLIES` defaults to `TcpOptsSupport::NONE` deliberately —
*"a default is a claim made by silence, and it must never be stronger than
the truth … `NONE` makes it understate itself, so the worst case is one
refused connect too many rather than an option dropped on the floor
without a trace."* And `TcpOpts::reject_unsupported` means a set option is
not a preference but a **refusal**: a runtime that cannot apply one the
caller set must fail the connect.

Put those together. Today a third-party `TcpConnect` that forgot to
declare `APPLIES` works — quietly, without nodelay. With `nodelay: true`
in `TcpOpts::default()`, that same backend **refuses every connect**, for
an option its caller never mentioned, with a message about a socket
setting it has never heard of. The `NONE` default's stated worst case,
"one refused connect too many", becomes *all* of them.

There is a second reason, independent of the first: `http-ng-rt` is a
socket seam and does not know who is writing. The 41 ms is the
write-write-read shape of a request over TLS. A protocol that streams one
way is exactly the one Nagle helps, and a default there would impose one
caller's protocol on every other caller of the trait.

### 3.2 Not `Native` asking for it unconditionally either

"HTTP wants nodelay" is a fact about HTTP, so the opinion belongs in the
HTTP client — that much is right. But `Native::new` writing `TcpOpts {
nodelay: true, .. }` has the *same* refusal hazard with a narrower blast
radius: `Native::new` returns `Self` and cannot refuse, so the failure
lands at connect time on a request that had nothing to do with it. That is
the exact shape `Native::tcp_opts` returning `Result` was introduced to
avoid — *"a configuration that can never work should not need traffic to
say so."*

### 3.3 What was built: the transport asks, the declaration decides

```rust
opts: TcpOpts {
    nodelay: <R as TcpConnect>::APPLIES.nodelay,
    ..TcpOpts::default()
},
```

The opinion is in the HTTP client, and it is conditioned on what the
runtime declared. **This is `TlsConnect::applies_ech`'s shape, one seam
over, and for the same reason.** An HTTPS record's `ech_config_list` is
passed on only to a TLS backend that says it applies one, because
`http-ng-tls-rustls` *refuses* a non-`None` `ech` and a connector that
filled the field from every record would make every ECH-publishing origin
unreachable. Same structure here: a connector that asks for something the
backend declared it cannot do turns a working build into a refusing one.

`TcpConnect::reports_alpn`'s sibling, `TlsConnect::reports_alpn`, is the
other precedent in this crate: `http-ng-native` offers `h2` only over a
TLS backend that says it can read the negotiated ALPN back. A capability
constant, defaulted to the understating value, read by the layer above to
decide whether to ask. That is three occurrences of one idea, and this is
the third.

**Which direction silence fails in** is the whole argument. Under §3.1
silence costs a *refused connection*; here it costs a *slow* one, and the
slow one is exactly what that backend had before this change. A claim made
by silence must fail in the direction that leaves the silent party
working.

### 3.4 What it costs: `tcp_opts` replaces the whole set

`Native::tcp_opts(opts)` takes the caller at their word, all six fields —
so `tcp_opts(TcpOpts { keepalive: Some(..), ..Default::default() })` turns
`nodelay` back off along with everything else, and the 41 ms comes back.

That is a decision and it is written on the method. The alternative — a
`tcp_opts` that quietly OR-ed its own `nodelay` back in — takes the choice
away from a caller who has a reason to want Nagle, and makes "the socket
parameters for every attempt this transport makes" a claim with an
exception in it.
`tcp_opts_replaces_the_whole_set_including_the_nodelay_new_asked_for` says
so on the record, and the `nagle_cost` harness uses exactly this to build
its control arm.

## 4. What a backend declaring `NONE` does

Two answers, because there are two questions.

**It was not asked, so nothing changed for it.** `Native::new` reads its
`APPLIES`, sees `nodelay: false`, and stores an all-off `TcpOpts` — the
same value it stored before this change. `connect` proceeds. Nagle stays
on, which is the cost of not declaring, and is a slower connection rather
than no connection.
`crates/http-ng-native/tests/tcp_opts.rs`'s
`a_runtime_that_declares_nothing_is_asked_for_nothing` reads the `TcpOpts`
handed to `TcpConnect::connect` — the transport passes its set there and
nowhere else, so the argument is the answer — and then states the
consequence in the runtime's own words:

```rust
opts.reject_unsupported(<Silent as TcpConnect>::APPLIES)
    .expect("a transport built with `new` alone never refuses a connect on any runtime");
```

**A caller who asks for it anyway is still refused, by name, at
construction.** `Native::new(Silent, ..).tcp_opts(TcpOpts { nodelay: true,
.. })` is an `Err` before any traffic, `ErrorKind::Unsupported`, with an
`UnsupportedTcpOpts` payload naming `nodelay` and nothing else.

The message gained a sentence, because half its readers are on the wrong
side of it:

```
this runtime cannot apply these TCP socket options, and does not ignore
them: nodelay (a runtime that does apply one declares it in
TcpConnect::APPLIES)
```

The option's name sends a backend author to their `connect` body, where
the code is very likely correct; the constant's name sends them to the
defect. That is not hypothetical — it happened once in this workspace
already: `docs/v04-w1-acceptance.md` §8 finding 5, `TokioHandle` applying
every option and declaring none, found by measurement rather than by
reading. (It is fixed in this tree; `handle.rs:181` declares `ALL`, and
`every_shipped_runtime_declares_the_option_this_transport_asks_for` is the
tripwire, checked at compile time so a regression is a build failure.)

## 5. Mutations

Anchor before each run, verified by re-running the restored tree:
**1353 passed, 19 skipped**, `cargo nextest run --workspace --all-features
--retries 0 --no-fail-fast -j 4`. `-j 4` rather than the default because of
§6. Restored with `git checkout` plus `os.utime`, never `shutil.copy2` —
that preserves mtime and cargo then keeps the mutated artifact, which is how
a mutation run gets scored against a binary that was never rebuilt.

| # | mutation | result | killed by |
|---|---|---|---|
| M1 | the default reverted — `Native::new` stores `nodelay: false` | **killed** | `native::tcp_opts::a_runtime_that_applies_nodelay_is_asked_for_it` (1) |
| M2 | the gate removed — `nodelay: true` unconditionally, §3.2's shape | **killed** | `native::tcp_opts::a_runtime_that_declares_nothing_is_asked_for_nothing` (1) |
| M3 | requested but not applied, at the builder: `tcp_opts` validates and does not store | **killed** | `native::tcp_opts::tcp_opts_replaces_the_whole_set_including_the_nodelay_new_asked_for` (1) |
| M3b | requested but not applied, at the wire: `self.opts` never reaches `TcpConnect::connect` | **killed** | the two above (2) |
| M4a | the refusal removed at the builder — `Native::tcp_opts` no longer calls `reject_unsupported` | **killed** | 8, all of `native::tcp_opts` |
| M4b | the refusal removed at its source — `reject_unsupported` returns `Ok(())` always | **killed** | 16: 6 in `http-ng-rt`'s `caps::tests`, 8 in `native::tcp_opts`, 2 in `http-ng-rt-embassy` |
| M5 | the `TcpConnect::APPLIES` pointer removed from the message | **killed** | `rt::caps::tests::the_message_names_the_constant_an_implementor_would_have_to_change`, `native::tcp_opts::a_caller_who_asks_a_silent_runtime_for_nodelay_is_still_refused_by_name` (2) |
| M6 | **the control** — the message's *leading sentence* reworded, naming no option and not the pointer | **survived, as intended** | — (1353 passed, 19 skipped: the anchor exactly) |

M6 is the reason the other seven mean anything. Nothing pins that
sentence, and nothing should — it is prose, and a test matching it would
go red for a rewording that changed nothing. A run in which M6 also
"died" would have been a harness failing for its own reasons rather than
eight mutations being caught.

M4a and M4b are two different removals of the same duty and are both
kept: M4a is the builder's early answer, M4b is the seam's. A single
mutation of either alone would leave the other place unmeasured, and the
`http-ng-rt-embassy` rows under M4b are the evidence that the seam's check
is load-bearing for a runtime that really does refuse things.

## 6. What this broke, and it is not fixed

**`http-ng-select`'s `alt_svc` suite became flaky, and the cause is the
41 ms going away.** Measured, same binary, same machine:

| | `-j16` | `-j1` |
|---|---|---|
| before the change | 0 failing runs in 20 | — |
| after | **9 failing runs in 20** | 0 in 20 |

Every failure is the same one:
`Error { kind: Connect, source: hyper::Error(Shutdown, BrokenPipe) }`.

The mechanism is a race `h1.rs` already documents as residual. `Native`
pools; `alt_svc`'s fixture answers one request per connection and then
closes it, with no `Connection: close` — a server's right, and what
`pool.rs`'s own `responses_before_close: Some(1)` does. The pool polls a
checked-out connection for liveness and `h1::exchange` polls it once more
before handing the request over, and between them they catch the peer's
close whenever its `FIN` has already arrived. `h1.rs` names the rest:
*"the residual race the checkout poll cannot close: the peer closed this
connection while it was idle, and one poll ago it had not shown yet."*
Past that point the request is inside `try_send_request`, hyper reports
`message: None` for it, and `Failed::Sent` is not retryable.

Two things were established before writing this down, and one of them is
a negative result:

- **It is not "a pooled connection against a server that closes".**
  `crates/http-ng-native/tests/stale_reuse.rs` does exactly that, forty
  consecutive requests through one pooled transport, and fails **0 of 40**
  — with `nodelay` and without. The two polls catch it every time.
- **It needs contention as well as speed.** The `-j1` column above is the
  identical binary. A starved client is one whose reactor has not yet
  delivered the `FIN` when `h1.rs` takes its one deliberately
  non-suspending look.

So the honest statement is that the 41 ms was covering a window this crate
knows about, and the window is now reachable.

**It is not fixed here, and the reason is not effort.** One attempt is
worth recording because it failed and because failing was informative.
`h1::exchange` returns `Failed::Sent` the moment the connection future
errors, *without* polling the send future — where the `Ready(Ok(()))` arm
three lines above falls through and polls it. Making the error arm fall
through too, so that hyper is *asked* for the request back rather than
assumed to have it, is a strict improvement in principle; it moved 9
failures in 20 to 6 in 20, which at n = 20 is not a result, and it was
reverted rather than shipped on a number that could not carry it.

> **Since fixed, and this attempt is why it looked impossible.** It was
> right to ask and wrong about *how*: a request still sitting in hyper's
> dispatch queue has its promise resolved by `Envelope::drop` and by
> nothing else, so polling the send future while the dispatcher is still
> alive polls a promise nothing will ever fulfil. Dropping the connection
> first is the whole of it. `h1::claim_back` does that, and the window
> below turns out to have had **three** points rather than two — the
> middle one being a request hyper had taken and refused to write, which
> this crate reported as `Failed::Sent`. `docs/pooled-reuse-race.md` has
> both deterministic reproductions, the before/after sweep on a real
> socket, and why §6's two named fixes are still refused. The two
> paragraphs below stand as written: they are about the third point,
> which is unchanged.

Why it could not have helped much is written down four hundred lines
below it, on
`a_connection_that_ends_with_the_request_queued_fails_instead_of_hanging`:
hyper's `should_error_on_eof` is `!state.is_idle()`, so an EOF on a
**fresh** connection is an error — and *"an error is the one case hyper
does hand the request back for"* — while an EOF on a **pooled** one,
which is `KA::Idle`, is a graceful close with the queued request stranded
in a channel the dispatcher will never read. Our failures are pooled
connections by construction. By the time the connection reports anything,
`try_send_request` has been polled and hyper holds the request; it will
not give it back, and it is right not to, because at that point it can no
longer tell whether the bytes went out.

That leaves two real fixes and neither is small. Close the window before
`try_send_request` is called — which costs the *"exactly one poll, and it
never suspends"* property written down where that poll is, i.e. a
scheduler round trip on every pooled request. Or be able to replay a
request hyper will not hand back — a design change, and `RetryKind` is the
vocabulary it would have to use, with `Client`'s `425` replay as the
precedent for what a second attempt costs and who is allowed to make it.

> **Both are still refused, and `docs/pooled-reuse-race.md` §4 gives the
> sharper reason for each.** The first because a yield is not a fence: it
> moves the window rather than closing it, so it buys an unbounded
> improvement in probability for a bounded cost on every pooled request.
> The second because it needs a notion of method safety this codebase
> deliberately does not have — the same one `docs/h3-research.md` §3.5
> declines for 0-RTT — and `RetryKind` answers only half of the question.
> What was built instead is neither of them and costs one `drop`.

## 7. What was not verified

- **Anything but Linux on loopback.** Every number here is one machine,
  one kernel, `127.0.0.1`. The 40 ms is a delayed-ACK timer and other
  systems pick other constants; what generalises is the *shape* — two
  writes coalesced into one segment, the second held — not the figure.
- **The real-network cost.** Loopback removes the round trip, so every
  success figure is a floor. The Nagle stall is a *timer* and does not
  scale with the path, so its share of a real request is smaller than
  41/42; on a 20 ms path the same exchange would be roughly 40 ms of
  handshake plus 40 ms of stall, and the stall would still be the larger
  half of what a `nodelay` saves.
- **`http-ng-rt-embassy`.** It declares `nodelay: true` and applies it
  through `TcpSocket::set_nagle_enabled`, so `Native::new` now asks for it
  there too. Nothing in this work ran on that runtime; its own
  `tuntap.rs` suite is green, which is a check that nothing broke and not
  a measurement that anything improved.
- **HTTP/2 and WebSocket.** Both go through the same `self.opts`, so both
  now get `nodelay` on any runtime that declares it, and neither was
  measured. h2's own framing makes the write-write-read shape less likely
  to matter; that is an expectation, not a number.
