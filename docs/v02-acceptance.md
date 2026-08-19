# v0.2 acceptance

What v0.2 claims, what proves each claim, and — at the same length, because
it is worth as much — what it deliberately does not do and what nobody has
checked. Written as the work lands rather than at the end, so the reasoning
is recorded while it is still the reasoning and not a reconstruction.

Every file named here was checked to exist and to prove what the row says at
the commit this document was written. Rows are not carried forward on trust.
Sections for W3 (HTTP/2), W5 (compression) and W7 (embassy) arrive when they
do.

| claim | proof |
|---|---|
| Dropping an in-flight request stops the exchange | `crates/http-ng-native/tests/cancel.rs`, `crates/http-ng-wasi/tests/live_roundtrip.rs`, `crates/http-ng-fetch/tests/transport.rs` — one per backend, and in every case the **observer is outside the client**: a server reporting the socket closed, a wasmtime guest that outlives its own drop, and the browser rejecting its own promise with `AbortError`, which our side cannot synthesise |
| Connections are reused | `crates/http-ng-native/tests/pool.rs` — a server counting *accepted* connections, not a counter we also wrote. Two requests, one accept with the pool on; two accepts with `PoolConfig::disabled()` |
| …and on WASI they are **not**, which is now measured rather than assumed | `crates/http-ng-wasi/tests/live_roundtrip.rs` — the same accept-counting observer, on a host thread outside the sandbox, with the guest under `wasmtime`. Two sequential requests to one origin: **two** accepts. `wasmtime_wasi_http::p3::default_send_request` opens the socket and runs the HTTP/1 handshake inside the per-request function, so there is no pool for the second request to find, and the request heads carry no `Connection: close` either. `WasiHttp` declares `ReuseSupport::None`, pinned in the same file; the control is two requests down one socket from a native client, which the same server counts once |
| Two clients with different trust cannot share a socket | `crates/http-ng-tls-rustls/tests/config_id.rs` — fails a `TypeId`-shaped or per-call `config_id`, which is what a naive implementation would reach for |
| A response that never starts, or a body that goes silent, is cut | `crates/http-ng-native/tests/timeouts.rs` — three misbehaving servers (answers never; head then silence; stalls mid-body), each paired with a control that must **hang** with the bound unset, plus a dribbling server that takes twice the bound in total and must not be cut. `first_byte` and `between_bytes` were declared `true` in the same commit that enforced them, which is the rule v0.2 W4's middle bullet was written under |
| An operation as a whole can be bounded | `crates/http-ng/tests/deadline.rs` — a server that answers in milliseconds and then drips one byte every 20 ms for ever. The test cannot pass without the bound |
| …including a body that goes **completely silent** after the head | the same file — a server that sends the head under a `Content-Length` of ten million and then nothing at all. Nothing will wake the body wrapper, so only the sleep it holds can end this; with the sleep never polled, the bound fires at 6 s instead of 400 ms, off a wake the test harness happened to supply, and the tightness assertion catches it. The observer is the server watching for the client's FIN |
| Idle connections are closed, not merely refused | `crates/http-ng-native/tests/reaper.rs` — the server watches its own end of the socket and reports when the client's `FIN` arrives: 299.7 ms (`Tokio`) and 300.6 ms (`Smol`) after the response, under a 300 ms idle timeout. Its control differs in one call (`pool` where the claim has `with_reaper`) and requires the socket still be open 1200 ms later |
| Adding a bound does not change the client's type | `crates/http-ng/tests/deadline_client_type.rs` — `struct App { http: Client }` with `total_timeout` applied. The `: Client` annotations are the assertion; the `assert_eq!` beside them is what stops the file passing if `total_timeout` stored nothing |
| A cookie the server set comes back on the next request | `crates/http-ng/tests/cookies.rs` — a loopback server recording the `Cookie` header it was actually sent, never the jar's view of itself, plus the same server with no jar configured as the control. A jar that stores perfectly and attaches nothing passes every test in `http-ng-cookie` and fails this one |
| Cookies are handled per redirect hop, not per operation | the same file, in both directions: a `Set-Cookie` on a 302 reaches the very next hop, and a cookie scoped `Path=/one` does **not** ride a same-origin 302 into `/two` — `next_hop` clones the previous hop's headers and `SENSITIVE_HEADERS` strips `Cookie` only across origins, so nothing but re-deriving it per hop gets this right |
| A server that answers without reading the request body does not lose its response | `crates/http-ng-native/tests/stream_reset.rs` — a real `h2::server` that answers and then drops the request stream, which is what makes h2 send `RST_STREAM(NO_ERROR)`. Head **and** body are asserted, because the defect had two halves and fixing one leaves a `200` whose body fails. Three controls sit beside it: a connection that dies at the same moment, a reset whose reason is not `NO_ERROR`, and a stalled streaming body that puts the reset at a different write |
| A client-side jar against a jar-owning backend is refused, not ignored | the same file — `UnsupportedCapability { what: "cookie_jar" }` at `build()`, with both controls beside it: the same jar against a backend that keeps none builds, and a client that never mentioned cookies builds against one that does (which is the line `Client::new()` takes in a browser) |

## The rule this vertical kept applying

Three capabilities were added in v0.2 — `cancel_on_drop`, `connection_reuse`,
and (pending) the ALPN and socket-option facts — and the same question was
asked each time, because v0.1 got it wrong four separate times in the other
direction:

**A variant exists only if a caller decision turns on it.** `CancelSupport`
started as three values, splitting `Supported` by *who* performs the
cancellation — the transport tearing down its own socket versus asking an
ambient host. Nobody could name a caller decision that turned on the
difference, so it is two values and the distinction lives in a doc comment.
`ReuseSupport` was proposed as three by the design doc and shipped as two
for the same reason, with the condition for a third written down: it arrives
when a portable client-level pool setting exists that a host-pooled backend
would have to refuse, together with the `check_supported` branch that
refuses it. That is exactly how `RedirectSupport::Internal` earns its
variant — not because ownership differs, but because a setting exists that
it must reject.

**A default must never be stronger than the truth.** This is the M2 lesson
from `RedirectSupport::Transparent`, and it decided three separate questions
this vertical: `Capabilities::none()` reports `CancelSupport::None`, because
silence and "cannot cancel" mean the same thing to a caller; `TcpOptsSupport`
defaults to `NONE` rather than `ALL`, so a runtime that forgets to declare
what it applies under-claims instead of promising socket options it silently
drops; and `TlsConnect::reports_alpn()` defaults to `false`, so a TLS backend
that cannot report the negotiated protocol is not assumed to be able to —
the cost of that particular over-claim is a `PROTOCOL_ERROR` on every
request.

## Three things that were found rather than designed

**`Spawn` was never usable here, so the pool never had a choice** — **and
this, the origin of a sentence three later pieces of work built on, is
wrong.** It read: "`http_ng_rt::Spawn<F>` requires `F: Send + 'static`, and
the native IO is deliberately not `Send` — `connect.rs`'s `FakeStream` holds
an `Rc<()>` for the sole purpose of proving it. So 'a pool driven by a
spawned background task' does not compile on this seam at all." Measured
afterwards, both halves fail. `Spawn<F>` declares **no bounds**; `Send +
'static` belongs to the `Tokio` and `Smol` impls. `FakeStream` is a test
stub, and the inference ran from "the test does not require `Send`" to
"production cannot have it" — a fixture built to prove an absence taken as
evidence about a presence. A reaper over this pool (`Arc<Inner<I>>` around a
`Mutex`) compiles and runs on the shipped `Tokio` and the shipped `Smol`,
measured against a real socket with the server observing the close.

What is true is narrower and was not noticed: `Spawn<F>` makes the future a
type parameter of the *trait*, so a bound must name it, and an `async` block
has no name. Generic library code cannot spawn a future it wrote itself,
whatever the auto traits say — the same wall the h2 bullet below hits with
hyper's private `H2ClientFuture`, not recognised there as a property of the
trait. And the pool's conclusion survives on a different footing:
`Native` is generic over `R`, not every `R` has a `Spawn` impl, so a reaper
in `Native::new` would be a default stronger than the truth. Opt-in with a
bound, not impossible. What exists today is still a poll at checkout plus a
second look while the request is still ours.

**A hang in hyper's dispatcher that would otherwise have shipped.** Reading
it to implement retry turned up a window where a keep-alive connection
ending gracefully with a request already queued strands the callback inside
the `Connection` we hold. It is a typed error now, pinned by a poll-by-poll
unit test. Nothing in the plan predicted this; it came out of reading the
dependency rather than trusting it.

**hyper's HTTP/2 client cannot be used without a spawner, and not for
reasons of cost.** `Http2ClientConnExec` is a **sealed** trait
(`hyper-1.11.0/src/rt/bounds.rs:51`), so an executor that queues futures for
our own `poll_frame` to drive cannot be written at all. v0.2 therefore
drives the `h2` crate directly, exactly as it drives `http1::Connection`
today. The design doc's `http2 = ["hyper/http2"]` was not merely a worse
option; it was not an option.

## The defect v0.3 reported here, and the half of it the report did not see

`docs/v03-acceptance.md` ends its `STOP_SENDING` section with a sentence
about this crate: *"`http-ng-native`'s HTTP/2 path has the same shape …
Same discard, different protocol."* It does, it was, and it is fixed. What
the report could not see from reading is that the discard happened **twice**
on this path, and that fixing the half it named leaves the other half
delivering a `200` whose body cannot be read.

RFC 9113 §8.1 carries the same MUST NOT as RFC 9114 §4.1: *"A server MAY
request that the client abort transmission of a request without error by
sending a `RST_STREAM` with an error code of `NO_ERROR` after sending a
complete response … Clients **MUST NOT** discard responses as a result of
receiving such a `RST_STREAM`."* h2's own server produces the case without
being asked to: dropping a request `RecvStream` while the response is
already complete schedules exactly that frame
(`h2-0.4.15/src/proto/streams/streams.rs:1601-1618`, `maybe_cancel`).

**Half one, the reported one.** `poll_pump` turned `poll_capacity`'s
`Poll::Ready(None)` into `StreamClosedWhileSendingTheRequestBody` — under a
comment that already named the case — and `exchange` returned
`Failed::Sent` on it **without ever polling `resp_fut`**.

**Half two, found by fixing half one.** h2 records end-of-stream as a
*state* rather than as an event (`recv_data` with `END_STREAM` calls
`state.recv_close()` and queues nothing, `.../recv.rs:714`), and the
`RST_STREAM` arriving afterwards **overwrites that state**
(`.../state.rs:258-290`). The frames already received are still handed out;
the clean end behind them is gone, so once the queue drains
`ensure_recv_open` returns the reset as an error. Measured on this transport
with only half one fixed: `status=200 version=HTTP/2.0`, then the body
failing with `Reset(StreamId(1), NO_ERROR, Remote)`. A response whose body
cannot be read is a response discarded by a slower route.

**One question the report could not answer by reading, answered by
measurement.** Does h2's `ResponseFuture` still resolve after the peer's
`RST_STREAM(NO_ERROR)`? **Yes** — `pending_recv` keeps the decoded response
and `poll_response` hands it out (`.../recv.rs:336-365`) — so the fix has
h3's shape after all rather than a different one. That was checked before
the fix was written, not assumed from the h3 case.

**The fix is one question asked in one place, and one reason code trusted
in another.** On the write side, `SendStream::poll_reset` at the top of the
pump loop; on the read side, `H2Body` ends the body rather than failing it
when the reset reason is `NO_ERROR`. The second is what hyper does, at
`hyper-1.11.0/src/body/incoming.rs:250-259`, for the reason RFC 9113 gives:
`NO_ERROR` *is* the server's statement that the response was complete, and
the `END_STREAM` that would prove it independently has been overwritten by
the time anything can look.

**Why `poll_reset` rather than reading each write's error.** Four writes on
the request stream can meet a reset stream, and three of them fail with a
`UserError::InactiveStreamId` whose public shape (`reason() == None`,
`is_reset() == false`) cannot be told apart from an API misuse of ours —
narrowing by variant would have to be by `Display` string. Asking first
means those three never see the reset. hyper asks the same question in the
same place (`.../proto/h2/mod.rs:145-156`) and answers it the **opposite**
way, with an error, which is not a disagreement about the RFC: hyper's body
pipe is a separate spawned task from its response future, so failing the
write there leaves the response alone. Here they are the same future,
because this module has nowhere to spawn.

**The fourth site is not this defect and is not touched.**
`send.send_data(now, false)` cannot meet a reset stream: h2 reclaims a reset
stream's send capacity (`.../send.rs:467-476`), `send.capacity()` is read in
the same synchronous block as the `send_data` that follows it, and no frame
can be processed in between, since the connection is only driven from
`exchange`'s own poll. Zero capacity routes to `poll_capacity` instead.

### What the tests pin, and what they do not

`crates/http-ng-native/tests/stream_reset.rs`, four tests against a real
`h2::server` on a real socket. A one-megabyte request body — sixteen times
the 65 535-byte default flow-control window, which these servers never
enlarge because they never read a request body — takes the race out, so
they measure the decision rather than the scheduler. The defect reproduced
on the first run and on every run.

Eight mutations, against an anchor of **124 tests** in `cargo nextest run -p
http-ng-native --all-features` (1025 workspace-wide). Seven killed, one
survived:

| mutation | verdict | killed by |
|---|---|---|
| revert the write half (delete the `poll_reset` gate) | killed | `a_server_that_stops_reading_the_body_still_gets_its_response_read`, at once; `a_stalled_streaming_body_…`, by hanging into its 30 s bound |
| revert the read half (delete both `stopped_after_a_complete_response` checks) | killed | the same two, both on the body |
| revert both — the code as it stood before | killed | the same two |
| tolerance moved from `poll_reset` to `poll_capacity`'s `Ready(None)` | killed | `a_stalled_streaming_body_…`, by hanging — which is the claim its doc comment makes, measured rather than asserted |
| widen the write tolerance: tolerate `poll_reset`'s `Ready(Err(_))` too | killed | `a_connection_that_dies_mid_request_is_still_an_error` — **and see below** |
| widen it maximally: `exchange` treats every `poll_pump` error as "stop pumping" | killed | the same test, the same way |
| widen the read tolerance: end the body on **every** error | killed | `a_reset_that_is_not_no_error_still_fails_the_response_body` |
| move the tolerance before the head: delete both pre-head `Failed::NotSent` returns | **survived** | nothing — see below |

**The two widenings are killed on the error's *kind*, not on its
existence, and that is worth stating plainly** — it is the h3 fix's lesson
applied to its own report. Under either widening the dying connection still
produces an error, because the pump's deferral hands the question to
`resp_fut`, which then reports `ConnectionEndedWithTheRequestQueued`. What
changes is `ErrorKind`: `Connect` where the narrow version says `Body`. The
`expect_err` in that test would not have caught either mutation; the
`assert_eq!` on the kind beside it does. So the guard is real but it is a
guard about categorisation, and a future edit that relaxes the kind
assertion silently removes it.

**The survivor is a mutation of a neighbour, because the intended one
cannot be written.** "Move the tolerance before the request head" has no
expression here: the tolerance *is* "stop pumping and let `resp_fut`
answer", and before `send_request` returns there is no `SendStream` to ask
and no `resp_fut` to defer to — the compiler refuses, which is the same
by-construction argument `http-ng-h3`'s `write_after_head` makes. The
nearest expressible mutation instead deletes `exchange`'s two **pre-head**
`Failed::NotSent` returns, and all 124 tests stay green. That is a real
gap, and it is not this defect's: **no test in this repository exercises a
dead *pooled* h2 connection being walked past to a fresh one.** The HTTP/1
equivalent, `pool.rs`'s `checkout_walks_past_a_dead_connection_to_a_live_one`,
has no h2 counterpart. Recorded here rather than closed, because closing it
is new coverage for the pool rather than a fix for the discard.

**Unrelated, found while counting anchors, and left alone:** that same
HTTP/1 test is flaky on `main` — two failures in twelve consecutive
full-suite runs of `http-ng-native`, always
`Connect / hyper::Error(Io, ConnectionReset)`, and never once in 35 isolated
runs. It has nothing to do with h2 (`http2.rs` is only reachable when ALPN
selects `h2`, and this test speaks `http://`), but anyone doing mutation
work in this crate will meet it and should not read it as a kill.

## The flake above, run down: the fixture, and one sentence of ours it caught

**It was the fixture, and the pool is not at fault — but the argument had
to be made from a captured failure, not from reading the code.** Both were
done, in that order.

*Captured.* 150 runs of the test on its own, with no load added, produced
nothing; the same loop with the machine deliberately busy (40 spinners on
28 cores) failed on run **103**, at `tests/pool.rs`'s `get_ok`:
`Error { kind: Connect, source: hyper::Error(Io, Os { code: 104, kind:
ConnectionReset }) }`, the whole test process gone in **0.564 s** against
a nominal 0.71 s — so the fourth request failed the instant it was issued
rather than waiting for an answer.

*The state at that instant, measured rather than assumed.* An instrumented
copy of the test read `/proc/net/tcp` during the wait: on a healthy run the
exhausted connection sits in **CLOSE_WAIT** — the client is holding a
socket whose peer has closed — while the live one is ESTABLISHED. That is
the state the test is named for, and it confirms the walk is real: the
pool does hold a dead entry and does reach the live one behind it. (That
copy never failed in 300 loaded runs of its own; reading `/proc` before
the request is exactly the kind of delay that hides this race, which is
itself a hint about the cause.)

*The cause.* `serve` writes the response that exhausts the connection and
drops the socket a few instructions later — two operations by a thread the
OS may deschedule between them. The test waited a wall-clock 100 ms for
the second one. Under load that is not always enough, and a checkout
landing in the gap finds a connection which is **not yet closed** — which
no poll can distinguish from a live one, because there is nothing to see.
The request then goes out on it; the server closes with those bytes still
unread; its kernel answers `RST`; and hyper reports the failure with the
request already on the wire.

*Proved deterministically before anything was changed.* A copy of the
fixture with a 150 ms delay between the last response and the close
produces that error to the byte, **five runs out of five**, in 0.564 s —
the captured failure's own duration — against a control differing only in
that delay being zero, which passes.

**The fix is a barrier, not a longer sleep.** `Behaviour` gains `closes`,
an observer the server bumps *after* dropping the socket, and the test
waits on it before the 100 ms sleep, which is now slack after a fact
rather than a guess at one. `Behaviour::close_delay` keeps that honest:
this test's server now closes **300 ms late on purpose**, so deleting the
barrier fails it **8 runs out of 8** instead of one in forty. The same
barrier is on the two neighbours with the identical race
(`a_connection_the_server_closed_while_idle_is_not_handed_out`,
`a_request_that_loses_the_race_is_retried_on_a_fresh_connection`), where
it is not separately measured and the code says so.

Three mutations, against an anchor of **124 tests** in `cargo nextest run
-p http-ng-native --all-features`, all 124 green before each:

| mutation | verdict | killed by |
|---|---|---|
| `Native::checkout` stops walking — one candidate, then `None` | killed | `checkout_walks_past_a_dead_connection_to_a_live_one` alone, on the count: `left: 3, right: 2` |
| `Native::checkout` drops the liveness check — `self.pool.take(key, now)` | killed | the same test the same way, **and** `a_request_that_loses_the_race_is_retried_on_a_fresh_connection` |
| the new barrier deleted from the walking test | killed | itself, 8 runs out of 8, with the flake's own `ConnectionReset` |

The second mutation is the one that says the fixture change did not cost
the test its subject: with the barrier in place it is still the walk, and
still the count, that fails. `a_connection_the_server_closed_while_idle_is_not_handed_out`
survives that mutation, which is not a gap — its pool holds one
connection, so being handed the dead one and retrying reaches the same
two accepts, exactly as `pool.rs`'s doc comment says.

**And the sentence it caught.** `pool.rs`'s module doc said the retry is
"the reason this pool does not make previously reliable requests fail
intermittently", and the bullet below says the race is "recoverable rather
than visible". Both are true of the window the retry covers and false of
the one beyond it: the retry fires on `Failed::NotSent`, which is hyper
handing the request back because not a byte of it reached the wire. Once
the bytes are out, a reset is `Failed::Sent` and the caller sees it — and
that is a **deliberate** at-most-once choice, not an oversight, because
resending a request a server may already have acted on needs a notion of
method safety this codebase does not have (the same notion `docs/h3-research.md`
§3.5 declines for 0-RTT). What is corrected is the claim, not the code.

## Deliberately not done in v0.2

Recorded, not hidden — and each with the reason, because a bare list invites
someone to "fix" an item whose absence is the decision.

- **No reaper for idle sockets *by default*** — and one on request since
  this section was written. Without it the idle timeout is a filter applied
  at checkout, not a background task that closes what has gone stale, and a
  client that goes quiet for an hour holds its sockets until its next
  request or until `Drop`.

  Not "because `Spawn` cannot run one" — see the correction above, where
  that claim was measured and withdrawn. Because `Native` is generic over
  `R` and not every `R` can spawn, so starting one in `Native::new` would be
  a default stronger than the truth. `Native::with_reaper(PoolConfig)` is
  the opt-in, bounded on `R: Clone + Spawn<Reaper<R, NativeIo<R, T>>>` so
  that a runtime which cannot spawn is a compile error where the caller
  wrote it. Proven from outside the client — the server watching its own end
  of the socket, under a 300 ms idle timeout: closed 299.7 ms after the
  response on `Tokio` and 300.6 ms on `Smol`, against a control differing in
  that one call which still held the socket 1200 ms later
  (`crates/http-ng-native/tests/reaper.rs`).

  What it still cannot promise, because no bound can: `Spawn::spawn`
  returns `()`, so an executor nobody drives accepts the task, drops it, and
  has no way to report it. A reaper is at most as good as the executor under
  it.
- **No pool shared between clients.** Each `Native` owns its own, which is
  why the TLS configuration identity in `PoolKey` is a constant within any
  one pool today. The field is not decoration: it must be in the key before
  a shared pool exists, because that is the moment its absence becomes a
  security defect rather than a redundancy.
- **A nanosecond race survives.** A server can close a connection between
  our checkout poll and our write. Every HTTP/1 pool has this, hyper and
  reqwest included; the retry is what makes it recoverable rather than
  visible — **for as long as hyper can still hand the request back**, which
  is the correction the flake above forced. `Failed::NotSent` is that
  window: not a byte on the wire, so the request is resent because it is
  the same request, not a second one. Past it, a reset is `Failed::Sent`
  and the caller sees the error. That is at-most-once on purpose — resending
  bytes a server may have acted on is a promise this client does not make,
  and it has no notion of method safety with which to make it selectively.
  Measured, not reasoned: a fixture whose server closes 150 ms after its
  last response fails the request with
  `Connect / hyper::Error(Io, ConnectionReset)`, five runs out of five.

  **Since narrowed, and the bullet is kept because what it says is still
  true of what remains.** The window turned out to have three points and
  not two, and the middle one — hyper holding the request in its queue and
  *refusing to write it*, because a closed read side makes
  `can_write_head()` false — was being reported as `Failed::Sent` for a
  request not one byte of which had reached the wire. `h1::claim_back`
  drops the connection, which is what makes `Envelope::drop` hand the
  request back, and asks hyper once. Reproduced deterministically twice —
  scripted poll by poll in `h1.rs`, and from outside the client on a real
  socket through `LateEof(Tokio, n)` — with the middle row going from an
  error and one accept to `200` and two, and the third row unchanged in
  both. `docs/pooled-reuse-race.md`.
- ~~**`total` does not cut a body that goes completely silent after the
  head.**~~ **Done. It cuts it now**, and the row is in the claims table
  above. The bullet is kept rather than deleted because it was wrong twice
  in two different ways, and both are worth not repeating.

  It first read: "`Timer::sleep` is an RPITIT, so its future cannot be
  stored in a struct field, and boxing it would make *every* response body
  `!Send`." The first half was fixed at the source — `Timer` carries an
  associated `Sleep` type, so the future has a name and can be a field. The
  second half was right only about `Pin<Box<dyn Future>>`; a box around a
  *concrete* `Tm::Sleep` is transparent to auto traits, so `Send` survives.
  That is not left to the eye either: `tests/deadline.rs` moves a whole
  bounded response body across a `tokio::spawn`, which does not compile for
  a `!Send` body.

  It then read "a decision not to change behaviour in that work item" —
  i.e. possible but deferred for wanting its own measurement and its own
  tests. It has them now: `Deadline` holds `Pin<Box<Tm::Sleep>>`, built in
  `Deadline::new` for the budget the head left over and polled whenever the
  wrapped body answers `Pending`. Measured before the change, with a
  counting waker and no executor running at all: the elapsed-time wrapper
  registers **zero** wakes after one `Pending` poll on a silent body and
  can therefore never fire, while a wrapper holding the sleep registers
  one.

  **`between_bytes` is still a different thing**, and that has not changed
  either — only its declaration has, in the same week and by a different
  route (see the row above): it bounds the GAP between two frames and
  restarts on each one, catching a stall anywhere in an arbitrarily long
  transfer, where `total` bounds the operation once from `Client::execute`'s
  entry. A body dripping a byte every 50 ms for an hour passes
  `between_bytes` and is cut by `total`; a transfer that legitimately takes
  an hour and stalls for ten minutes in the middle is the reverse. Both are
  now reachable on `http-ng-native`, and neither replaces the other: a
  caller that sets only one of them has bounded only one of those two
  shapes.
- ~~**A `Client` cannot be given a jar over a caller-supplied public suffix
  list.**~~ **Closed**, by the second of the two routes below and not the
  first. `ClientBuilder::cookie_jar` is `cookie_jar<P>` now, and what it
  stores is `jar.map_suffixes(AnyList::new)` — `AnyList` being a
  `Box<dyn PublicSuffixList + Send>` that *implements the trait it erases*,
  so `CookieJar<AnyList>` keeps the jar's whole API rather than a subset.
  The conversion is written in `http-ng` rather than in `http-ng-cookie`,
  which the paragraph below expected to be impossible: `map_suffixes` is a
  public method on the jar, so the private field is never touched. The
  `Send` bound sits on this one opt-in call and nowhere else — amendment
  C12, the same shape `redirect_predicate` takes — so no signature a
  single-threaded caller meets gains a bound. `crates/http-ng/src/erased.rs`,
  and `AnyStore` beside it does the same for the response cache. The
  original text follows, because the route it rejected is still the right
  one to reject.

  `CookieJar<P>` is generic over the list — that is the seam
  `http-ng-cookie` built so a caller can supply a fresher snapshot than
  the compiled-in one — and `ClientBuilder::cookie_jar` takes
  `CookieJar<BuiltinList>` only.

  Not an oversight, and not cheap to fix either. The two ways through are
  a type parameter on `Client` (rejected: "adding a bound does not change
  the client's type" is a claim in the table above, and a jar is a far
  smaller reason to grow the type than a timeout was), or a
  `CookieJar<P> -> CookieJar<Box<dyn PublicSuffixList>>` conversion, which
  has to be written in `http-ng-cookie` because the field is private —
  and which would need a `Send + Sync` on the trait object that `http-ng`
  is not allowed to declare (the crate's own no-declared-auto-traits
  invariant). Reachable by taking `http-ng-cookie` directly and driving
  the jar by hand; not reachable through this builder.
- **No per-request cookie control**, and no `Client`-level way to turn the
  jar off for one call. Same reasoning as the missing per-request `total`
  one bullet up: there is no invalid state to reject and no caller
  decision anyone has named. A caller who wants a request outside the jar
  sets `Cookie` themselves, which the client then leaves entirely alone —
  though note that "leave alone" covers the attaching half only, and the
  jar still stores what comes back.
- **`SameSite` is parsed, reported and not enforced** — inherited from
  `http-ng-cookie`, which says so, and unchanged by the wiring: acting on
  it needs an initiating browsing context that a non-browser client does
  not have.
- **The concurrency limit bounds requests, not sockets.** `tower` releases
  its permit when the `call` future completes — at the response head — so a
  streaming body holds its connection outside the limit. The design doc
  claimed otherwise and has been corrected.
- **No per-request `total` override.** The client-level form has no invalid
  state to reject: the clock arrives with the bound, so "a total without a
  clock" is unrepresentable. A per-request setter would need either a
  runtime flag existing only to be refused, or negative reasoning coherence
  does not offer. A per-handle bound costs one `Arc` bump and covers most of
  the same ground.
- **`MockTransport` reports `Capabilities::none()` *by default*** — since
  narrowed, and the default is still the honest one rather than a lazy one:
  its `execute` completes synchronously, so `cancel_on_drop: None` is what
  is true of it. What the bullet said and no longer does is that this is all
  it can report. `MockTransport::with_capabilities(caps)` overrides it,
  which is what lets the mock stand in for a backend whose *capability* is
  the thing under test — every `build()` refusal in this workspace is tested
  that way.
- **A `tower::buffer::Buffer` in the stack breaks the cancellation
  contract**, because it spawns a worker and the request outlives the
  dropped future. Such a stack must declare `None` even when the transport
  underneath can cancel. Written down in `http-ng-tower`.

## One decision taken against the brief, and why

The cookie wiring was asked to take its `now` from the clock `Client`
already carries — `Tm: Timer`, the same one the total timeout uses — and
it does not. It calls `SystemTime::now()`.

`Timer` cannot answer the question. `Timer::Instant` is `Copy +
PartialOrd` and `elapsed_since` returns a `Duration`: a stopwatch with no
epoch, which is all a timeout needs and is not a date. `Expires` is a
date. The bridge would be an anchor — one `SystemTime::now()` kept
alongside one `Tm::Instant`, advanced by `elapsed_since` — and that is
where it fails rather than merely being roundabout: `NoClock::
elapsed_since` returns `Duration::ZERO` for ever, so on a clockless client
the jar's `now` would be frozen at the anchor. Every `Expires` in the
future, every `Max-Age=0` deletion ignored, silently, for the whole life
of the process — the exact silent no-op `NoClock`'s own doc comment exists
to prevent, arriving through the back door. And a clockless client may
perfectly well keep cookies: nothing about a jar needs a timer, so there
is no type-level guard to lean on the way `total_timeout` has one.

`SystemTime::now()` has one cost and it is written where the setter is:
it panics on `wasm32-unknown-unknown`, where `std` has no clock. The
combination that reaches it is a browser build driving a transport other
than the browser's own — the browser's own is refused by the capability
gate.

## What remains unverified

- **`http-ng-fetch` declares `ReuseSupport::Supported` with no external
  observer.** Browsers do keep connections alive and declaring `None` would
  be a lie in the other direction — but from inside the browser we cannot
  watch sockets, and CI does not check it. This is the one declaration in
  the set that no test stands behind, and it says so where the value is set.

  **Not WASI, which was wrongly included here — and the observer it was
  missing has now been written, and it did not confirm the declaration.**
  `http-ng-wasi`'s live suite already runs a real `TcpListener` on a host
  thread, with the guest as a wasmtime subprocess, so an accept-counting
  observer was available there on the same terms as the native pool's. It is
  in `crates/http-ng-wasi/tests/live_roundtrip.rs` now
  (`two_guest_requests_to_one_origin_open_two_connections`), and two
  sequential requests from one guest to one origin arrive on **two**
  connections. `caps.connection_reuse` is `ReuseSupport::None` accordingly;
  the row in the claims table above says what stands behind it.
- **No test covers a dead *pooled* HTTP/2 connection being walked past to a
  fresh one.** Found by mutation while fixing the `RST_STREAM(NO_ERROR)`
  discard: deleting both of `http2::exchange`'s pre-head `Failed::NotSent`
  returns leaves all 124 tests in `http-ng-native` green. The HTTP/1
  equivalent is pinned (`pool.rs`'s
  `checkout_walks_past_a_dead_connection_to_a_live_one`, ~~itself flaky~~ —
  the flake was its fixture and is fixed, see the section above); h2 has no
  counterpart.

  **The h1 work makes the h2 counterpart cheap, and it is deliberately not
  built here.** `Behaviour::closes` is the piece that was missing — an h2
  version needs the same "the server has closed, not `sleep` and hope"
  barrier or it inherits exactly the flake just run down — and the rest is
  `tests/http2.rs`'s existing `h2::server` fixture doing what
  `responses_before_close: Some(2)` does here, on a `GOAWAY`-then-close
  instead of a bare `FIN`. Named rather than done, because it is new
  coverage for the h2 pool and belongs with whoever owns that, not smuggled
  into a fixture fix.
- **Cancellation on a naive embassy backend does not work**, measured: the
  server sees nothing for two seconds, because `TcpSocket::drop` removes the
  socket from smoltcp before the queued FIN can become a packet. W7 must
  either declare `CancelSupport::None` or own a closing list the stack task
  drains. Recorded in `docs/w7-embassy-research.md`.
- **`http-ng-tls-native-tls` sends a proposed ALPN and cannot report the
  selected one.** Harmless while `http/1.1` is the only proposal; the moment
  h2 is proposed it becomes a `PROTOCOL_ERROR` per request. `reports_alpn()`
  is the fix and it is in W3's scope, not yet landed.
- **Two things about `http-ng-idn` that no test reaches.** The gate that
  refuses to trust an ICU until it has answered `straße.de` correctly has
  covered *content* and uncovered *presence*: deleting the filter leaves
  every test green, because killing that mutation needs a machine where ICU
  is present but wrong, and no runner is one. And on Windows the absence of
  `icuuc.dll` is a `STATUS_DLL_NOT_FOUND` at process start with no mention
  of IDN at all — the 1703 floor the crate's docs state is checked by
  nothing, at build time or at run time. Both are written where the code
  is, not only here.
- **The platform IDN question is open on one measurement, not two.**
  macOS is settled from Apple's own source: `swift-foundation`'s
  `UIDNAHookICU` calls `uidna_openUTS46` with `0x3C`, i.e. non-transitional,
  which agrees with `idna` — so the *flavour* question is answered and only
  the live confirmation is missing. Chrome was measured directly and agrees
  too. What still needs a runner is whether `CoInitializeEx` must precede
  `uidna_openUTS46` on Windows. All of it is in
  `docs/icu-ecosystem-survey.md`, including the one divergence no
  pre-filter fixes: browsers accept invalid punycode (`xn--a.de`) where
  `idna` refuses.
