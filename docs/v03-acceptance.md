# v0.3 acceptance — HTTP/3

What the HTTP/3 work claims, what proves each claim, and — at the same
length, because it is worth as much — what it deliberately does not do and
what nobody has checked. Written as the work landed, so the reasoning is
the reasoning rather than a reconstruction.

Everything below was checked at the commit this was written. Rows are not
carried forward on trust.

| claim | proof |
|---|---|
| HTTP/3 is spoken over real QUIC, not merely compiled in | `crates/http-ng-h3/tests/live.rs` — a `quinn` + `h3` server on loopback with an `rcgen` certificate. An HTTP/1.1 or HTTP/2 client gets nothing at all from that server, so a green run is not something a fallback could have produced. `Response::version()` reads `HTTP_3` |
| Requests on one connection are multiplexed, not serialised | same file, `requests_are_multiplexed_not_serialised` — the server holds each request 300 ms; two concurrent requests finish in ~300 ms, and the server's own accept count stays at 1. Timing is the observer because nothing else can tell the two apart |
| Connections are reused | `two_requests_share_one_connection` — the **server's** accept count, not a counter we also wrote |
| A pooled connection survives an idle gap, and only because of the keep-alive | `an_idle_connection_survives_only_because_of_the_keep_alive` — an A/B with the driver spawned in both arms, so the keep-alive is the only variable: 1 accept with it, 2 without |
| The keep-alive is on by default | `crate::tests::a_pooled_connection_is_kept_alive_by_default` — the live A/B cannot reach the default, measured (M9) |
| Cancelling one request does not disturb its neighbours | `dropping_one_request_does_not_disturb_the_others` — and on this transport the property is not vacuous, because there really are neighbours on the connection |
| A request enters early data only if the caller marked it | `early_data_is_offered_only_to_a_request_the_caller_marked` plus three unit tests in `src/early.rs` covering every body kind. The two requests have the same body; the extension is the only difference |
| 0-RTT is really taken, and a rejection is replayed rather than surfaced | `a_rejected_0_rtt_request_is_replayed_and_the_caller_never_sees_it` — two servers, one certificate, separate ticketers. It cannot pass unless `into_0rtt` took the shortcut, the rejection was detected and the replay went out; checked for vacuity by mutating the replay away, which turns it red |
| 0-RTT is **accepted** end to end, and the data really left before the handshake | `crates/http-ng-h3/tests/zero_rtt.rs` (W1) — two observers, neither of them the client. On the wire, a UDP relay reading cleartext long headers counted 7 zero-RTT packets carrying 7003 bytes before the client's first Handshake packet; on the server, the request was resolved at 2.7 ms against a handshake completing at 151.5 ms, the separation forced by the relay holding the server's flight for 150 ms. The unmarked warm-up is the asserted negative control: no early data at all |
| The UDP seam's offloads are enforced, not merely published | `crates/http-ng-rt-tokio/tests/udp.rs`, and since W1 `crates/http-ng-rt-pair-check/tests/udp_pair_property.rs` on **both** backends — a socket refuses a GSO batch one segment past what it declared, accepts one exactly at the limit, and a declared batch really arrives as that many datagrams |
| A socket reports the ECN it can actually observe | same files — the claim is checked against a real loopback round trip, in both directions: a socket claiming ECN must deliver the codepoint, one not claiming it must report `None` |
| The UDP seam is a seam and not a design | W1 — `UdpBind`/`UdpAdoptStd`/`UdpDatagrams` on `http_ng_rt_smol::Smol` behind a `udp` feature, and one generic body (`exercise_udp<R>`) run under both runtimes rather than two similar test files. `both_backends_report_the_same_kernel` makes each backend the other's oracle, which is the check neither runtime crate could make alone |
| HTTP/3 runs on two runtimes, from one function | `crates/http-ng-h3/tests/two_runtimes.rs` (W1) — `fetch_once<R>` under `TokioHandle` in a real `tokio::runtime::Runtime` and under `Smol` on a bare `futures_executor::block_on`. Sensitive rather than merely green: adding `R::Instant: PartialEq<std::time::Instant>` breaks the tokio instantiation alone |
| Neither seam carries a QUIC dependency | the `dependency-graph` job's *"the runtime and TLS seams contain no QUIC"*, plus its companion proving the ban is not vacuous |
| A handshake that never completes is cut at the caller's bound | `a_connect_timeout_cuts_a_quic_handshake_that_never_completes` — a bound UDP port that answers nothing, a 300 ms `Timeouts::connect`, and `ErrorKind::Timeout(Phase::Connect)` with the bound readable off `ConnectTimedOut` rather than out of a message. Its control is the same black hole with no bound at all, which must still be waiting at 1200 ms; measured by mutation, the unbounded handshake takes **30 s** to fail on quinn's own idle timeout. `a_client_may_now_set_a_connect_timeout_over_h3` is the caller-visible half — before this it was an `UnsupportedCapability` at `build()` — and `h3_declares_the_timeouts_it_enforces_and_no_others` pins the declaration beside the measurement |

## The seam changes, and one that turned out not to be needed

Three were proposed by `docs/h3-research.md`. **Two were made and the third
was measured away**, which is worth recording explicitly because a
recommendation left un-closed gets re-derived.

**`Timer` is untouched.** Recommendation (2) was that it must gain an
absolute-deadline sleep, because `quinn::AsyncTimer::reset(i)` re-arms to an
absolute `Instant` while this seam has `sleep(Duration)` over an opaque
`Instant` with no conversion either way. Checked in quinn's source:
`pub(crate) use std::time::{Duration, Instant}` (`quinn-0.11.11/src/lib.rs:56`)
on every non-wasm target, and `Runtime::now` defaults to
`std::time::Instant::now()`. The deadline quinn hands over and the clock
`crate::runtime` subtracts from are therefore the same clock, and `reset(i)`
is exactly `sleep(i - now)` — no conversion, no seam change, no new method
anybody has to implement.

**`http-ng-rt` gained `UdpBind`/`UdpAdoptStd`/`UdpDatagrams`.** Three things
in it depart from the research's sketch, each because building it said so:

- **`bind` is sync**, where `TcpConnect::connect` is async. Binding performs
  no round trip on any runtime here, and the one call site that has to be
  served — `quinn::Runtime::wrap_udp_socket` — is itself synchronous, so an
  async `bind` could not have served it.
- **`try_send` and `poll_writable` are separate**, not a fused
  `poll_send(cx, t)`. A QUIC endpoint has several tasks that may all be
  waiting to write and one socket stores one waker, so waiting has to be
  expressible *without a datagram in hand*. The fused shape reads better and
  cannot implement `quinn::UdpPoller`.
- **No `Send`, `Sync`, `'static` or `Debug` bound is on the seam.** quinn
  requires all four; they are paid in `http_ng_h3::H3`'s `where` clause, so
  the compile error lands on whoever asked for QUIC rather than on whoever
  implemented UDP — and an `embassy-net` backend can still implement the
  trait honestly.

**`http-ng-tls` gained `TlsIdentity`, and `http-ng-tls-quic` is a new
crate.** `TlsConnect` cannot carry QUIC and it is not close: the
intersection of its four methods with `quinn_proto::crypto::Session`'s
eleven is empty, so an adapter **type-checks with an empty body** rather
than failing to compile, which is worse than a compile error. Both traits
need the same configuration identity, and declaring `config_id` on each
would make every concrete-typed call `E0034`.

What moved when `config_id` went up into a supertrait, stated precisely
because it was got half right twice: **generic call sites do not move** (a
call under a `T: TlsConnect` bound resolves through the supertrait, so
`http-ng-native`'s production code is untouched); **implementations move**
(four test stubs in `http-ng-native`, three real backends); and
**concrete-typed call sites move too**, because the trait has to be in
scope — `http-ng-tls-rustls/tests/config_id.rs` imports `TlsIdentity` now.

## Two decisions, and the arguments they rest on

**HTTP/3 requires `R: Spawn`, and connections are shared.**

An idle HTTP/1 socket in a pool needs nobody; the kernel holds it. **A QUIC
connection that nobody polls is not idle, it is dying**, because the PING
that resets the peer's idle timer comes from the connection's driver. So the
driver is spawned, and that is not a preference: without it the pool is
useless, and with a pool that is useless HTTP/3 buys a handshake it did not
need.

Once the driver is spawned, v0.2 W3's reason for handing out h2 connections
*exclusively* has no subject. That reason was: without a spawner, nobody
drives a shared connection but the in-flight request futures, so a caller
that stopped polling one request would stall its neighbours. A driver that
is nobody's request future cannot be stalled by any request's polling
behaviour. Both facts are written next to their own policy — `H3`'s module
doc and `http-ng-native`'s pool — so that changing one does not silently
import the other's justification.

The `Spawn` bound excludes only runtimes that were already excluded.
`embassy-net` has no descriptor at all, so `quinn-udp` cannot be asked about
GSO/GRO/ECN and quinn cannot wrap the socket; the bound takes nothing from
it that UDP had not already taken. And it is a bound on `H3`, not on the
seam and not on `Native`, so W7's direction is untouched.

**0-RTT is admitted by the caller, per request, and by nothing else.**

`AllowEarlyData` in the request's extensions is the gate.
`RequestBody::retry_kind()` is checked underneath it and is **not a second
gate**: it answers *can these bytes be sent again*, which the transport
needs because a rejected 0-RTT request is replayed after the handshake and a
single-pass body cannot be. It does not answer *may an attacker send them
again*, and the two come apart in the direction that matters — `POST
/transfer` with a fully buffered body is `RetryKind::Free`, trivially
replayable, and exactly the request that must never enter early data. The
notion that would answer the safety question does not exist in this
codebase, deliberately, so the judgement is the caller's and the code says
so where a reader would otherwise assume the check covers it.

The acceptance verdict is never written to `TlsInfo::early_data_accepted`.
In QUIC it resolves **after the response** — 8.63 ms against a response at
8.58 ms, measured in the research — so a field on a handshake result could
only hold it by waiting for the handshake, which is the round trip 0-RTT
exists to skip.

## One defect the live tests found and this document's own reasoning missed

Worth its own section, because it is the case the acceptance model is for:
a claim nobody had written down, broken in a way no amount of reading would
have caught, surfaced by a test that only failed when the machine was busy.

**`a_real_request_over_real_quic` failed about once in twenty under load
and never once in isolation** — 0 failures in 150 isolated runs, 4 in 90
runs of six concurrent processes — always with `Error { kind: Body, source:
"Remote reset: 0x0" }`, and always on a connection's first request. Two
other tests in the same file failed the same way. It was not a port, not a
bind race and not a timeout: the client was discarding a response it had
already been sent.

The chain, each link read from source rather than inferred:

1. the client writes its HEADERS frame; the server resolves the request;
2. the server answers **without reading the request body** — what any
   server answering `404`, `401` or `413` does — and drops its half.
   Dropping a `quinn::RecvStream` that was not read to the end sends
   `STOP_SENDING(0)` (`quinn-0.11.11/src/recv_stream.rs:534`);
3. the client then calls `finish()`, which on a connection's **first**
   request is not a no-op: h3 writes a grease frame there
   (`h3-0.0.8/src/connection.rs:1101-1119`). That write fails
   `Stopped(0)`, which `h3-quinn` maps to `StreamTerminated`
   (`h3-quinn-0.0.10/src/lib.rs:425`) and h3 to
   `StreamError::RemoteTerminate`;
4. `one_attempt` propagated it and returned **before `recv_response`**.

`STOP_SENDING` acts on one direction. The response stream was untouched,
and RFC 9114 §4.1 says so in as many words: *"Clients MUST NOT discard
complete responses as a result of having their request terminated
abruptly."*

**The fix is one tolerated variant, on the write side, after the head.**
`write_after_head` in `crates/http-ng-h3/src/lib.rs`; every other
`StreamError` propagates unchanged. It cannot slide before the head by
accident: it takes a `Result<(), _>` and `send_request` yields the stream,
so the compiler refuses.

**The tests pin the behaviour and not the narrowness, and the difference is
worth stating** — this document exists to record what was checked, not what
was intended. `crates/http-ng-h3/tests/stop_sending.rs` has two: a server
that stops reading still has its response read, and a server that *dies* at
the same moment still produces an error. A four-megabyte body — past
quinn's 1.25 MiB stream window — takes the race out, so both measure the
decision instead of the scheduler.

What the second test does **not** do is keep the tolerance narrow, though
the first draft of this section and of the code's own comment both claimed
it did. Measured afterwards: widening the match to `Err(_) => Ok(true)`
leaves all 31 tests green. The expected failure — "swallowing a
`ConnectionError` means `recv_response` hangs on a dead connection" — does
not happen; that call returns an error of its own, reported as the same
`ErrorKind::Body`, so the wide and narrow spellings are indistinguishable
from outside. Nor can a white-box test close the gap: `StreamError`'s
variants are `#[non_exhaustive]` outside an h3 feature flag, so a
`ConnectionError` cannot be synthesised to feed the function directly.

So the narrow match is defended by construction rather than by measurement,
and that argument is written where the match is: `RemoteTerminate` is the
one variant whose meaning is known here — one direction stopped, the other
untouched — and for the rest there is no ground to claim the response
stream survived. Recorded this way round because a test named as a guard,
which is not one, is worse than no guard at all: the next person reads the
name and stops checking.

**`http-ng-native`'s HTTP/2 path has the same shape and is not fixed here**
(a crate this task does not own; reported rather than touched). RFC 9113
§8.1 carries the same MUST NOT for a `RST_STREAM(NO_ERROR)` after a
complete response, and `http2.rs`'s `poll_pump` returns `Err` on
`poll_capacity` answering `Ready(None)` — under a comment that names the
case, *"the peer reset the stream"* — after which `exchange` returns
`Failed::Sent(..)` without ever polling `resp_fut`. Same discard, different
protocol.

## Deliberately not done

Recorded, not hidden, and each with the reason — a bare list invites
someone to "fix" an item whose absence is the decision.

- **`425 Too Early` is not this transport's, and the retry that is
  somebody's owes this crate one line.** RFC 8470 §5.2's third failure
  path: a server unwilling to risk a replayable request answers `425`, and a
  user agent *"SHOULD retry automatically, but any retries MUST NOT be sent
  in early data"*. The retry is a status-code branch in `Client` — only the
  client owns the retry loop — and `a_425_reaches_the_caller_untouched` pins
  that this layer passes a `425` through untouched, which stays true however
  `Client` evolves.

  The second half is a live obligation rather than a note for later, and it
  is aimed at whoever writes that retry (v0.3's `425` work, in flight
  separately): **the replayed request must have `AllowEarlyData` removed
  from its extensions.** The mark is the only thing that can put a request
  into early data, so removing it is necessary and sufficient.

  It is tempting to think the duty is vacuous — by the time a `425` comes
  back the handshake completed long ago, and streams opened after that are
  1-RTT whatever the request asks for. That is true only of the connection
  this crate happens to still have pooled. The mark is part of the pool key,
  so a marked replay asks for the early-data connection specifically; if
  that entry was evicted, closed by the peer or timed out, the replay builds
  a fresh connection and `into_0rtt` puts it straight back into early data,
  against the server that just refused to risk one. `src/early.rs`'s module
  doc carries the argument next to the code.
- **No streaming request body, and no full duplex.** HTTP/3 does both — a
  QUIC stream's halves are independent — and `execute` does neither: it
  writes the whole body, then reads the head. Both capabilities are declared
  `false` accordingly, and a streaming body is a typed refusal naming the
  capability rather than a quiet buffering. This is a limitation of the
  implementation and the capability describes the implementation.
- **No ECH on the QUIC path.** rustls builds ECH through a different builder
  entry point, so honouring it is a second construction path and a third
  cache dimension. An ECH config that arrived from a DNS answer is a typed
  refusal, not a silent drop: a caller who asked for encrypted SNI and did
  not get it is worse off than one who was told no.
- ~~**No HTTPS/SVCB discovery, and no Alt-Svc.**~~ **Tier 2 landed in W2**,
  in `http-ng-native` rather than here — see the section at the end of this
  document. h3 is still chosen by constructing `H3`, and that has not
  moved: what tier 2 delivers is everything an HTTPS record says *except*
  the protocol choice, because there is nowhere in this codebase for
  "choose between two protocol stacks" to live. Alt-Svc still needs a store
  with an eviction policy and a rule for `clear`; what it no longer needs
  to carry is the negative cache, which W2 showed belongs to whoever makes
  the failed attempt.
- ~~**No timeouts.** All three `TimeoutSupport` fields are honestly
  `false`.~~ **`connect` is done**, and the row is in the claims table
  above. The bullet is kept rather than deleted because the reason it gave
  for not doing it — "a declaration and its enforcement belong in the same
  change" — is what shaped the change when it came: `c.timeouts.connect =
  true`, the race in `execute`, and the tests all landed together.

  **`first_byte` and `between_bytes` stay `false`, and neither is a line
  away.** `first_byte` would have to bound `one_attempt` and then answer
  what a 0-RTT replay does to the budget — the replayed attempt is a second
  `one_attempt` on the same request, so "restart the bound" and "carry what
  is left" are different promises and nobody has picked one.
  `between_bytes` needs a body wrapper holding a sleep, the shape
  `http_ng_native::IdleTimeout` has and `H3Body` does not; an elapsed-time
  check cannot cut a body that goes completely silent, which v0.2 W4
  measured with a counting waker.
- ~~**No smol UDP backend.**~~ **Done in W1**, and the two things the design
  flagged as unverified are now measured. `Smol` satisfied every bound of
  `H3Runtime` except the UDP triple with nothing else changed — compiled,
  not read: `Timer` with `Sleep: Send + 'static`, `Spawn<QuinnTask>` and
  `Clone + Send + Sync + 'static` all held, and the only error was the two
  missing traits. And `async_io::Async::poll_writable(&self, cx) ->
  Poll<io::Result<()>>` is the seam's separated shape argument for argument,
  so the one place W1 could have become a seam change did not: `http-ng-rt`
  is untouched.

  One asymmetry between the backends came out of it, and it is written into
  the shared test rather than left as folklore: **the `WouldBlock` retry is
  load-bearing on tokio and never taken on smol.** Measured by replacing the
  retry with a `panic!` — tokio panics on its very first send, smol never
  does. tokio's `try_io(Interest::WRITABLE, ..)` refuses *before* the
  syscall when it holds no cached readiness, which is exactly a freshly
  bound socket; smol's `try_send` reaches `sendmsg`. Both are within the
  seam's contract, and a caller written against smol alone would have a bug
  only tokio finds.
- **No reaper for dead pooled connections.** A connection the peer closed is
  dropped at the next checkout, not before. Same shape as W2's HTTP/1 pool
  and the same reason.

## What remains unverified

- **GSO, GRO and ECN on macOS and Windows.** Measured here: `gso=64 gro=64
  ecn=true may_fragment=false`. The `udp` tests **print** those numbers
  rather than asserting them, because 64 is this kernel's
  `UDP_MAX_SEGMENTS` and a virtualised runner may honestly answer 1/1 — a
  job asserting them would be flaky by construction. What the tests assert
  is the relationship between what a socket claims and what it delivers,
  which holds on any of them.
- **One mutation survives, and it is the environment rather than the test.**
  M14 replaces `ecn: ecn_is_really_on(&io)` with `ecn: true`. On this kernel
  ECN works, so the mutant is indistinguishable from the truth and the test
  cannot see it. This is the same class as v0.2's ICU finding — *"killing
  that mutation needs a machine where ICU is present but wrong, and no
  runner is one"* — with one difference in this case's favour: a runner in
  the matrix **is** such a machine.
  `ecn_is_reported_from_the_kernel_on_a_dual_stack_socket_too` binds a
  dual-stack v6 socket, which is precisely the case `quinn-udp` documents
  macOS and iOS as unable to set `IP_RECVTOS` on, so the mutant is killable
  there. Not yet observed being killed; what would settle it is one run of
  that test on `macos-latest` with the mutation applied.

  **W1's second backend does not change this, as the design predicted, and
  the reason is worth restating**: two runtimes on one kernel is still one
  kernel, and the same mutation applied to `http-ng-rt-smol`'s twin of that
  line survives for exactly the same reason. What W1 *did* add against it
  is `both_backends_report_the_same_kernel` — which kills the mutation only
  when applied to one backend and not the other, so it catches a divergence
  and not a shared over-claim. A wholesale `caps: UdpCaps::NONE` in either
  backend **is** killed, by the ECN half of `ecn_claim_matches_reality`: a
  socket that claims no ECN and then delivers `Ect0` fails there.
- **A ticket issued over TCP offered to a QUIC handshake.** rustls keys its
  session store by `ServerName` alone while a QUIC ticket also carries
  `quic_params`. `http-ng-tls-rustls`'s QUIC path uses a **separate** store,
  which removes the question rather than answering it, for the price of one
  `Arc`. What would settle the original question: one server serving both
  TLS-over-TCP and QUIC with a shared ticketer, one shared
  `ClientSessionStore`, a TCP handshake then a QUIC 0-RTT attempt.
- ~~**0-RTT ACCEPTANCE has not been observed end to end here; rejection
  has.**~~ **Observed in W1**, and from outside both endpoints: a relay
  between client and server counts packets and shows request bytes leaving
  before the handshake completes, with the server's own timing as the
  second witness. `crates/http-ng-h3/tests/zero_rtt.rs`. What follows was
  true when written and is kept because the rejection half is still the
  half that carries the replay:
  `a_rejected_0_rtt_request_is_replayed_and_the_caller_never_sees_it` runs
  the research's scenario 3 — two servers, one certificate, separate
  ticketers — and proves three things at once, because it cannot pass
  without any of them: `into_0rtt` really took the shortcut, the rejection
  was detected, and the replay went out. Checked for vacuity by mutation:
  replacing `if verdict.await` with `if true` (i.e. never replaying) turns
  that test red and leaves the other ten green.

  **The happy path is covered as of W1** — `tests/zero_rtt.rs`, the row in
  the table above — and two things it turned up are worth keeping here.

  *The obvious server-side signal does not exist*, and the test was written
  against it first. `Connecting::into_0rtt` hands back a `ZeroRttAccepted`
  future, which reads as "did I accept the peer's early data" and on a
  server does not: `quinn_proto::Connection::accepted_0rtt` is assigned
  inside a block guarded by `if self.side.is_client()`
  (`quinn-proto-0.11.16/src/connection/mod.rs:2540`, with a
  `debug_assert!(self.side.is_client())` inside it for good measure), so a
  server always reports `false`. It read `0` on a connection whose 0-RTT
  packets the relay had just counted going past.

  *What replaced it is an ordering the relay makes causal.* The server
  records when it resolved the request and when its own handshake
  completed; the relay holds the server's flight for 150 ms, and a client's
  handshake completes on processing that flight, so inside the window a
  request cannot reach the server by any route but early data.

  Still **not** asserted, deliberately: the response arriving before the
  verdict, `docs/h3-research.md` §3.2's 8.58 ms against 8.63 ms. Both
  events are inside the client and fifty microseconds apart, so an
  assertion on that ordering would be measuring the scheduler. What stands
  in for it is structural: `execute` returns the response without awaiting
  the verdict at all, and reads it only on the error path.
- ~~**`two_requests_share_one_connection` failed once, under the full
  922-test workspace run, and has not been reproduced.**~~ **Explained, and
  the cause is fixed** — see the defect section above. The mechanism found
  there matches this signature exactly: `Remote reset: 0x0`, only under
  load, only on a connection's first request, from a server that answers
  without reading the body. That this particular failure was that
  mechanism is not *proven* — it was never reproduced to be captured — but
  the two other h3 live tests that failed the same way under a concurrency
  campaign were, and they were this. Recorded this way rather than deleted,
  because a flake closed by inference is a weaker claim than one closed by
  capture, and the difference should be visible. The original note follows:
  922-test workspace run, and has not been reproduced.** Not in twelve
  isolated runs of that test, not in five runs of the h3 crate alone, and
  not in three further full-workspace runs. It is recorded rather than
  waved away because a pooling assertion that fails one run in five is
  either a real race under CPU starvation or a test that is measuring the
  scheduler; both matter and neither is settled. What would settle it: the
  `assert_eq!` prints the observed accept count, so the next occurrence
  says whether the connection was replaced (2) or something else went
  wrong. Nothing was loosened to make it pass.
- **The connect-timeout fixture holds its UDP socket for a reason no run
  here confirms.** `black_hole()` binds the port rather than picking an
  unused one, on the argument that a datagram to a port nobody holds could
  draw an ICMP port-unreachable and turn the control into a prompt error
  instead of a silence. Mutated and run: dropping the socket changes
  nothing on this Linux runner, so that ICMP is not reaching quinn here.
  The socket stays as a portability precaution, named as one where it is
  written. What would settle it is the same mutation on `macos-latest` or
  `windows-latest`.
- **The idle A/B's timings are loose on purpose and could still flake.** A
  1500 ms gap under a 1000 ms idle timeout with a 300 ms keep-alive has
  wide margins, and the multiplexing test's 450 ms threshold against two
  300 ms requests has wider ones. Neither is a benchmark; both would rather
  pass on a loaded runner than measure anything precisely.

---

## W2 — tier 2 discovery: the HTTPS record, consumed

*Everything from here to the end of the document is v0.3 W2 parts 2 and 3
(`crates/http-ng-native`), written the way the rest of this file is: the
claim, the thing that proves it, and — at the same length — what it does
not do.* Part 1, the rustls backend's ECH refusal, is already recorded
above and in `crates/http-ng-tls-rustls/tests/ech.rs`.

The plumbing had been in the tree since v0.2 and was used by nothing:
`SvcbEndpoint` carried `alpn`, `port`, `ipv4hint`, `ipv6hint` and
`ech_config_list`, `Resolve::supports_svcb` said who could ask, and
`connect.rs` said in its own module doc that it did not ask and passed
`ech: None`. It asks now. `crates/http-ng-native/src/discovery.rs` holds
the record-shaped half (which record wins, what an `alpn` set means, the
negative cache), `connect.rs` the connection-shaped half (the port, the
addresses Happy Eyeballs starts from, the ALPN offer, the ECH slot).

### The claims

Every row is measured by a **peer socket** unless it says otherwise:
`crates/http-ng-native/tests/svcb.rs` runs a `TcpListener` that records the
first flight it is sent and then closes, so "the record was used" means a
connection arrived where the record said, carrying what the record allowed.
No test in that file reads a field of `Native`.

| claim | proof |
|---|---|
| The record's `port` is where the connection goes | `the_port_from_the_record_is_where_the_connection_goes` — the peer is on an ephemeral port nothing else in the request names, and the origin's own endpoint (`127.0.0.1:443`) has no listener at all |
| A resolver that reports it cannot ask is not asked | `a_resolver_that_says_it_cannot_ask_is_not_asked` — the same records, `supports_svcb()` false, nothing arrives. The control for every row above and below it |
| A default-port record is not applied to a URI that named its own port | `a_record_is_not_applied_to_a_uri_that_named_its_own_port` — two peers; the URI's port receives, the record's does not. RFC 9460 §9.5: the record for a non-default port lives under a prefixed name, and this one was fetched for the origin name |
| `ipv4hint`/`ipv6hint` reach Happy Eyeballs | `the_address_hints_reach_happy_eyeballs` — the resolver answers **neither** family, so the hint is the only address that exists; a connection arrives anyway |
| The record narrows the ALPN offer to the SVCB-ALPN set | `the_record_narrows_the_alpn_offer` (h2 withdrawn by a record advertising `http/1.1` alone) with `a_record_that_advertises_h2_leaves_it_in_the_offer` as its control. The ClientHello is parsed rather than grepped — `\x02h2` turns up in a 32-byte random field about once in half a million hellos |
| `h3` in a record never reaches the TCP path's ALPN offer | `h3_in_a_record_is_never_offered_on_the_tcp_path` — and the rest of the record's set still is, so this is a filter and not a refusal |
| An origin publishing an ECH config is still reachable | `an_ech_publishing_origin_is_still_reachable` — see "the ECH decision" below; this is the row the whole item had to settle first |
| …and the name it asked to protect goes out in the clear | `the_name_a_record_asked_to_protect_goes_out_in_the_clear` — the cost of that decision, exhibited by the same observer that asserts the connection survived |
| An AliasMode record does not outrank the ServiceMode record beside it | `an_aliasmode_record_does_not_outrank_the_service_it_precedes` — priority 0 is *numerically lowest* and carries no parameters, so a selection that did not skip it would reliably pick the one endpoint with nothing in it |
| A failed discovery is not repeated by the next request | `a_failed_discovery_is_not_repeated_by_the_next_request` — two requests, one arrival |
| …and the memory is a window, not a verdict | `the_negative_cache_expires` — three requests, two arrivals, with `SVCB_FAILURE_TTL` passing between the second and the third through the `Timer` seam |
| A failed discovered endpoint is retried on the origin's own terms | `src/connect.rs`'s `a_failed_discovered_endpoint_is_retried_without_the_record` — **not** a peer socket, and the reason is below |
| A record that sets nothing is not an endpoint | `a_record_that_sets_nothing_does_not_buy_a_second_race` — same observer, same reason |

### The ECH decision, and the measurement that forced it

Part 1 made `http-ng-tls-rustls` *refuse* a non-`None` `TlsRequest::ech`
rather than drop it. That turned part 2 into a trap: a connector that
filled the field from every HTTPS record would make **every origin
publishing an ECH config unreachable** through the default TLS backend.
Discovery would have turned a working request into a failing one.

The premise was checked rather than reasoned about, because that is the
whole method here: the naive version was written first — `ech:
endpoint.and_then(|e| e.ech.as_deref())` — and run against the fixture.
`an_ech_publishing_origin_is_still_reachable` fails with **`left: 0, right:
1`**: zero bytes at the peer. Not a degraded connection, no connection.

Three ways out, and the third is what shipped:

- **(a) read the field and drop it** — the silent no-op part 1 existed to
  end, now with a config in hand rather than absent;
- **(b) fill it and let those origins fail** — measured above, and not an
  option;
- **(c) fill it only for a backend that says it applies one.**
  `TlsConnect::applies_ech()`, defaulted to `false`, is the sibling of
  `TlsConnect::reports_alpn` — the same shape, the same asymmetry against
  `tls_support`'s `Full` default, and the same reason: a default must never
  be stronger than the truth. No backend in this workspace overrides it, so
  the field is `None` today — but it is `None` because a backend said it
  would not use one, not because nobody asked.

**Where the cost is written, since it is a privacy fact and not an
implementation detail.** A client that cannot do ECH and connects anyway to
an ECH-publishing origin sends the server name in the clear. That sentence
is on `TlsConnect::applies_ech` (where a caller reads the capability), in
`connect.rs`'s module doc (where the decision is made), next to
`caps.tls_config` in `Native::new` (where a caller looks for what TLS this
transport does, including why this is *not* a `Capabilities` field: the
answer is the backend's and the caller already holds it), and on the wire
in `the_name_a_record_asked_to_protect_goes_out_in_the_clear`.

### The structural limit, honoured rather than worked around

**Tier 2 cannot choose HTTP/3**, and nothing here pretends otherwise.
`http-ng-h3` is a different crate with different bounds (`R: UdpBind +
Spawn<..>`, `T: QuicTlsConnect`), `Native<R, T, D>` has neither, and
`Client<T>` names exactly one transport type. So an `alpn` containing `h3`
is read, is not offered on the TCP path (`h3_in_a_record_is_never_offered_
on_the_tcp_path`), and is not an error either. Acting on it is a racing
transport owning both stacks — a vertical, not a task — and no line in this
change moves towards one.

### Mutations

Baseline before each run: `cargo nextest run -p http-ng-native
--all-features`, **134 tests, all passing**. Each mutation was applied to
one site, the suite run in full, and the source restored. Anchor counts
were checked before each (the patch had to match exactly once, or the
mutation was not run).

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1 | `ech` filled from the record unconditionally (the trap: part 1's refusal makes those origins unreachable) | **killed** | `svcb::an_ech_publishing_origin_is_still_reachable` (`left: 0, right: 1` — nothing on the wire), `svcb::the_name_a_record_asked_to_protect_goes_out_in_the_clear` |
| M2 | the record is fetched and then ignored (`filter(\|_\| false)`) | **killed**, by 11 tests | all ten `svcb` record tests, plus `connect::tests::a_failed_discovered_endpoint_is_retried_without_the_record` |
| M3 | the port from the record is ignored | **killed**, by 10 tests | `svcb::the_port_from_the_record_is_where_the_connection_goes` and every other `svcb` test whose peer is reached through that port |
| M4 | the address hints do not reach Happy Eyeballs | **killed** | `svcb::the_address_hints_reach_happy_eyeballs`, `connect::tests::a_failed_discovered_endpoint_is_retried_without_the_record` |
| M5 | the negative cache never records | **killed** | `svcb::a_failed_discovery_is_not_repeated_by_the_next_request`, `svcb::the_negative_cache_expires` |
| M6 | the negative cache never expires | **killed** | `svcb::the_negative_cache_expires` |
| M7 | the record's `alpn` does not narrow the offer | **killed** | `svcb::the_record_narrows_the_alpn_offer` |
| M8 | AliasMode (priority 0) is not skipped | **killed** | `svcb::an_aliasmode_record_does_not_outrank_the_service_it_precedes` |
| M9 | `supports_svcb()` is not consulted | **killed** | `svcb::a_resolver_that_says_it_cannot_ask_is_not_asked` |
| M10 | no retry on the origin's own terms after a discovered endpoint fails | **killed** | `connect::tests::a_failed_discovered_endpoint_is_retried_without_the_record` |
| M11 | an inert record counts as a discovered endpoint | **killed** | `connect::tests::a_record_that_sets_nothing_does_not_buy_a_second_race` |

Two of those (M10, M11) are killed by a unit test on `FakeRt`'s attempt log
rather than by a peer, and that is a limitation of the fixture rather than
a preference — see the first bullet under "what W2 has not checked".

### Decisions worth knowing before touching this

- **Only `https://`, and only at the scheme's default port.** For an
  `http://` origin an HTTPS RR means *upgrade the scheme* (RFC 9460 §9.5),
  which is a redirect-shaped decision this connector will not make on its
  own; taking the record's port and hints while ignoring that would be half
  a rule. And a URI that names a non-default port needs the record under a
  prefixed name (`_8443._https.…`), which is not the one `lookup_svcb(host)`
  returns.
- **The lowest ServiceMode priority wins and nothing else is tried.** RFC
  9460 expects a walk down the list; walking it would multiply a request's
  connect budget by the number of records an attacker-influenced answer may
  contain. The fallback that matters is the one that is still there when
  every record is wrong: the origin's own addresses.
- **A `lookup_svcb` error is treated as "no record", not as a failure.**
  HTTPS queries are answered with SERVFAIL by a long tail of middleboxes,
  and a client that failed there would be unable to reach origins it
  reaches today. Nothing below is swallowed with it: the address lookups
  keep their own error handling, so `ErrorKind::Cancelled` still reaches
  the caller through `connect::drive`'s machinery, kind intact.
- **The retry after a failed discovered endpoint spends the caller's
  budget, not a fresh one.** `Timeouts::connect` wraps the whole
  `connect::connect` future exactly once, so both races share the one
  deadline. The error the caller sees is the second attempt's, because that
  is the one about the origin the caller named.
- **The negative mark is per origin, flat, and 5 minutes.** Not exponential
  (that needs a failure counter and a cap, and the condition being waited
  out is usually a DNS change), and not a cache of the *lookup* — "this
  origin has no HTTPS record" is a DNS answer with a TTL of its own, which
  `SvcbEndpoint` does not carry, and inventing a lifetime for someone
  else's answer is how two caches drift apart.
- **The pool key still uses the URI's port.** A connection made to a
  discovered endpoint is pooled under the origin, which is correct — the
  record's claim is precisely that this endpoint serves that origin — but
  it means a request made while the negative mark stands may reuse a
  connection opened through the record. Same origin, same server name, same
  trust configuration; noted because it is easy to read the key as an
  endpoint and it is not one.

### What W2 has not checked

- **The in-request retry is not observed from outside, and cannot be by
  this fixture.** The retry goes to the origin's own endpoint, which for a
  default-port `https` URI is `:443`, and an unprivileged test process
  cannot put a listener there (this one runs as uid 1000). So M10 and M11
  are killed on `FakeRt`'s attempt log instead — a real observation of
  behaviour rather than of state, but inside the process. What would settle
  it from outside: a runner where the test may bind 443, or a `Resolve`
  fixture paired with a transport whose default port is configurable, which
  is a change to `connect::port` rather than to the test.
- **The cost of the extra query is measured on one host, and it is a cost
  the default transport now pays.** `SystemDns::supports_svcb()` is `true`
  on Linux, so `DefaultTransport` asks for an HTTPS record before every
  connect that opens a new connection. Measured here (systemd-resolved
  active, `res_query` through the stub): a **cold** `lookup_svcb` is 37.5 ms
  — one real round trip, the same order as the `A` lookup beside it (39.3
  ms) — and repeats are **54 µs to 1.4 ms**. The repeats are cheap only
  because the OS stub caches; `http-ng-dns-system` caches nothing of its
  own (grep it for `cache`), so on a host with no caching stub every
  request adds a full query. Not measured: that second machine. What would
  settle it is the same timing with `systemd-resolved` stopped.
- **The query is serialised before the address lookups, deliberately, and
  the alternative is not free.** RFC 9460 §10 has the HTTPS query running
  *in parallel* with A/AAAA; this connector awaits it first, because the
  record's `port` applies to every attempt in the race and `drive` takes one
  port for all of them. Running them in parallel means either starting
  attempts that may have to be abandoned when the record arrives, or
  teaching Happy Eyeballs a per-attempt port. Neither is written; the cost
  of not writing them is the row above.
- **`no-default-alpn` does not cross the `SvcbEndpoint` seam.**
  `http-ng-dns-system` parses the parameter (`RawParam::NoDefaultAlpn`) and
  `SvcbEndpoint` has no field for it, so `alpn_offer` cannot tell "the
  default set applies" from "the record switched it off" and assumes the
  former. That is the safe direction — assuming the latter would leave a
  client with nothing to offer for every record that did not mention
  `http/1.1` — but it means a record that really did set
  `no-default-alpn` gets `http/1.1` offered to it anyway. The fix is a
  field on `SvcbEndpoint`, in a crate this change deliberately did not
  touch.
- **The record's target name is never resolved.** RFC 9460 §2.5 lets a
  ServiceMode record point at another name whose addresses should be used;
  this connector uses the record's hints and the *origin's* addresses.
  A record whose target differs and carries no hints therefore contributes
  only its port, ALPN and ECH. One lookup rather than two, and honest, but
  it is not the whole rule.
- **No live origin has been contacted through this path.** Every test here
  drives a fixture resolver against a loopback peer. `cloudflare.com`
  publishes exactly the record this consumes (`1 . alpn="h3,h2"
  ipv4hint=… ipv6hint=…`), and reading it through `SystemDns` is what the
  timing above did — but no test in this repository makes a request to a
  real origin, and none should start now for this.
- **The `http2` feature is what makes the ALPN rows observable.** Without
  it this transport offers `http/1.1` alone whatever a record says, so
  `the_record_narrows_the_alpn_offer` and its two companions are
  `#[cfg(feature = "http2")]`. A build without the feature keeps the code
  path and loses the observation.

# v0.3 W3 — DNS over HTTPS

Written as W3 landed, on the same terms as everything above it: what is
claimed, what proves it, and — at the same length — what is deliberately
absent and what nobody has checked.

`crates/http-ng-dns-doh`, a `Resolve` over any `Transport`. 22 crates in
`cargo tree -e normal`, **no `tokio`, no `hyper`, no `h2`** — measured;
the runtime and the HTTP stack arrive with whatever `C` the caller supplies,
which for the tests here is `Native<Tokio, NoTls, IpLiteralOnly>` and for a
browser build could be `Fetch`.

## The bootstrap, which is the section's own warning

§W3's title says the bootstrap is the actual problem, and the four shapes it
lists are answered by **which constructor compiles**, not by prose:

| §W3 shape | how this crate says it |
|---|---|
| 1. an IP-literal endpoint | `Doh::pinned(transport, uri)` — checks the host is a literal, refuses a name, and names the other constructor in the error |
| 2. the system resolver, once, for the DoH host | `Doh::bootstrapped(transport, uri)` over a transport carrying `SystemDns` |
| 3. caller-supplied bootstrap addresses | the same constructor, over a transport carrying a resolver that holds them. This crate cannot tell 2 from 3 and does not need to: both are "the inner transport knows how" |
| 4. RFC 9461 `dohpath` | **not done.** Circular for the first lookup. `http_ng_dns::svcb::RECOGNISED_KEYS` still excludes key 7, and a record making it mandatory is still one this client refuses to use — `a_record_making_dohpath_mandatory_is_ignored_and_the_usable_one_is_kept` |

The two constructors **partition** the space — `pinned` refuses a name,
`bootstrapped` refuses a literal — so which one compiles is a statement
about the URI rather than a name someone chose. A URI change that quietly
turns a bootstrap-free deployment into a bootstrapped one is an error at
construction.

**The cost of pinning is real and this crate cannot mitigate it**, and that
is written on `Doh::pinned` rather than here: a pinned address that stops
answering leaves the resolver with nothing to ask, and there is no discovery
to route around it. Pair it with a fallback, or expect to ship a new
address.

## What resolves the DoH server's name, and what makes the request

**`C` is a `Transport`, never an `http_ng::Client`, and that is the whole
answer to §W3's *"a resolver's client is not the user's client"*.** A cookie
jar, a redirect policy and an `Authorization` header are all things `Client`
owns and `Transport` has never heard of. So the first bug report §W3
predicted — a cookie sent to a DNS provider — is not merely made awkward
here; it does not typecheck. The costs of the choice, both stated where they
bite: there is no `total` timeout (that is `Client`'s, and `Timeouts` has no
such field), and no redirect following (which DoH does not need).

## The cycle: §W3's unverified type-level claim, now measured

§W3 predicted a compile error and asked for the error to be read.
`crates/http-ng-dns-doh/tests/no_cycle.rs` carries four transcripts, each
produced by putting the definition in a file and building:

| written | rustc |
|---|---|
| `type Cycle = Native<Tokio, NoTls, Doh<Cycle>>;` | **E0391** cycle detected when expanding type alias |
| `struct Cycle(Native<Tokio, NoTls, Doh<Cycle>>);` | **E0072** recursive type has infinite size, *"recursive without indirection"* |
| the `Box` rustc's own `help` then suggests | **E0277** `Box<Cycle>: Transport` is not satisfied — *"required for `Doh<Box<Cycle>>` to implement `Resolve`"*. The recursive type becomes definable and stops being a resolver |
| `Arc<dyn Resolve>`, the hatch §W3 named | **E0038** not dyn compatible, because `lookup_ipv4` returns an `impl Trait`. Same for `dyn Transport`, for the same reason on `execute` |

So the claim holds, in every spelling, at compile time. **The last row is an
accident rather than a promise**, and the file says so: `impl Stream` was
chosen for RFC 8305, not to shut this door. Two ordinary changes *elsewhere*
would reopen it — an object-safe `Resolve` with boxed streams, or a blanket
`impl<T: Transport> Transport for Box<T>` — and neither is a change to this
crate, which is why the note lives where someone might read it.

The finite composition is not forbidden and is checked by running:
`Native<Tokio, NoTls, Doh<Native<Tokio, NoTls, IpLiteralOnly>>>` resolves a
name over a loopback DoH server, and three levels compose too, which is the
real shape of "DoH bootstrapped through a second, pinned DoH endpoint".

## Fail closed, or fail open — in the type

`Doh<C>` is `Doh<C, NoFallback>` and **fails closed**.
`Doh::with_fallback(r)` gives `Doh<C, F>`, and that type is written into
every transport holding it: `Native<R, T, Doh<C, SystemDns<R>>>` says on its
face that this client resolves through the system when its DoH server is
down.

Neither is a good default for everyone, which is why neither is silent.
Failing closed leaves a working network unusable. Failing open is a
downgrade an attacker mounts for the price of one dropped connection — deny
the DoH endpoint, and every subsequent lookup moves to the plaintext
resolver the caller was avoiding.

The rule, and the half that is easy to get wrong: a **successful** DoH
answer is never second-guessed. NXDOMAIN and an empty answer section are
answers, and asking a fallback for a second opinion on them would be that
same downgrade on every lookup rather than only under attack. Two tests
assert it, and the stub fallback counts its own calls so that "did not
consult" is observed rather than inferred.

## Claims and proofs

Every test named below observes from **outside**: a loopback HTTP server
that knows nothing about this crate, building DNS wire format by hand, and
recording what arrived on the socket. Nothing reads a field of `Doh`.

| claim | proof |
|---|---|
| `dns-message-parser` can **encode** a query, not only decode a response (§W3's unverified codec premise) | `tests/query_bytes.rs` — the expected bytes are written out by hand from RFC 1035 §4.1 and compared against what the server received. Not produced by the library under test |
| The DNS ID is zero (RFC 8484 §4.1) | `two_identical_lookups_produce_byte_identical_queries` — two identical lookups must be byte-identical, which a randomised ID would take away |
| POST, to the endpoint's own path, with both media types | `the_request_is_a_post_to_the_endpoints_own_path_with_both_media_types` — the path matters: dropping it works against most public endpoints and fails against `https://dns.example/prefix/dns-query` |
| "Asked and found nothing" stays apart from "could not ask" | `tests/lookup.rs` — no records of the type and NXDOMAIN are **empty streams**; SERVFAIL, `TC`, a clear `QR`, a mismatched question, a non-200, a wrong content-type and an undecodable body are **errors** |
| The TTL is the record's own, per record | `each_address_carries_the_ttl_that_came_with_its_own_record` — two records with **different** TTLs, so an implementation taking the RRset minimum or the first record's value fails it. This is the first consumer of `ResolvedAddr::ttl` §W3 asked for |
| A CNAME chain does not break an aliased host | `addresses_behind_a_cname_are_returned_and_the_cname_is_stepped_over` — the owner name is deliberately not compared, and this test is what stops the check that would look like hardening and break every CDN-hosted origin |
| **`supports_svcb()` is `true`, and the capability is real** | `tests/svcb.rs` — the flag is never asserted on its own. A ServiceMode record round-trips all six fields `SvcbEndpoint` holds, including the ECHConfigList **with RFC 9460 §7.3's redundant length prefix intact**, which is the form rustls parses |
| RFC 9460 §8 is applied, both halves | `a_record_making_dohpath_mandatory_is_ignored_and_the_usable_one_is_kept` (ignore one record, keep the RRSet) and `a_mandatory_key_the_record_does_not_carry_is_an_error` |
| An IP-literal URL still connects | `tests/bounds.rs` — `http_ng_native::connect` hands `Uri::host()` to the resolver unconditionally, so `192.0.2.1` arrives here as a *name*. The assertion is that **no request reached the server**. See the defect below |
| A server does not get to choose how long we wait | `a_first_byte_bound_ends_a_lookup_a_silent_server_would_not`, with the control that the same silent server **hangs** with every bound unset, and a third test that the default is finite |
| A server does not get to choose how many bytes we read | `a_response_larger_than_a_dns_message_can_be_is_refused` — `MAX_RESPONSE_BYTES` is the largest a DNS message can be, so the cut can only fall on a body that was never an answer |

## One defect found by asking what the mutations were not covering

**An IP literal was being sent to the DoH server as a name**, which made
`client.get("https://192.0.2.1/")` fail to connect at all.

Found by reading `http_ng_native::connect` rather than by a bug report: it
calls `dns.lookup_ipv4(host)` on whatever `Uri::host()` returned, with no
literal shortcut above the `Resolve` seam — which is exactly why
`IpLiteralOnly` exists as a *resolver* rather than as a branch in the
connector. A DoH resolver that queried for the "name" `192.0.2.1` gets
NXDOMAIN from any honest server.

`Doh` now answers a literal from the string, on `IpLiteralOnly`'s rule: the
literal goes to its own family's stream, the other family gets an empty one.
The bracket stripping that had been in `encode_query` went with it — a
literal no longer reaches that function, so all the rule could still do was
turn `[foo]` into a query for `foo`.

## The RFC 9460 client semantics moved crates

`RawBinding`, `RawParam` and `endpoint_from_binding` were `pub(crate)` in
`http-ng-dns-system/src/svcb.rs`. That file's own doc had already given the
reason they could not stay: the rules are identical on every platform and
are the part most likely to be got subtly wrong. It made that argument about
two backends inside one crate; `http-ng-dns-doh` is a third, in another
crate, decoding the same wire format. They are now `http_ng_dns::svcb`,
behind a **`codec` feature** so that a build using only `IpLiteralOnly`
still links no DNS decoder — measured, `cargo tree -e normal` for
`http-ng-dns`: **13 crates without it, 16 with**. `http-ng-dns-system`'s
126 tests are untouched and green; its `SvcbLookupError::MandatoryKeyAbsent`
survives as a mapping at the call site, so nothing outside that crate can
tell the move happened.

## Mutation testing

37 hand-applied mutations, anchor counts checked before each run so that one
matching zero or several places is reported rather than scored — the
convention W1's h3 work established. The script is
`crates/http-ng-dns-doh/mutations.py`; it reverts each edit whether the run
passes or fails. Final state, on a clean tree with nothing else running:
**37 anchors matched (M2 at 2 places, by design; the rest at 1), 37
killed.**

**Two things about that number, because 37/37 on its own is a number to
distrust.**

*Three of them survived first, and none was answered by adjusting a
fixture.* The three below are the finding, and the kills are what the
answers bought.

*The anchor count did real work rather than decorating the run.* An
intermediate pass reported **six ANCHOR MISMATCH — matched 0**, after
rustfmt rewrapped the source and the `Family` refactor renamed a match
arm. Scored as kills they would have been six lies; reported as mismatches
they were six stale `find` strings to fix. That is the whole reason the
count is checked before the edit.

*And the run has a hazard worth writing down, because it cost a commit
here.* The script mutates files in place, so **nothing else may touch the
tree while it runs** — the docs commit was made mid-run with `git add -A`,
and the pre-commit `cargo fmt --all` reformatted the mutant into the
staged tree. Three live mutations reached the branch and were backed out
in the next commit. Editing during a mutation run also corrupts the run
itself: that pass showed two unrelated tests failing under almost every
mutation, which is what a moving source file looks like from the outside.

**Three survived, and none of them was answered by adjusting a fixture.**

- **M33, the echoed question's CLASS is not checked — SURVIVED.** Every
  fixture sent `IN`, so nothing could tell whether the field was read.
  `an_answer_in_a_different_class_is_refused` sends `CH`; it needed a
  message builder that lets the class be chosen, which the fixtures did not
  have.
- **M35, the trailing dot on the asked-for name is not stripped before
  comparison — SURVIVED.** No test passed a fully-qualified name.
  `https://example.com./` is a legal URL and `Uri::host()` hands the dot
  through, so without the strip every fully-qualified host became a
  `QuestionMismatch`.
  `a_fully_qualified_name_with_a_trailing_dot_resolves` is the test.
- **M37, `recover`'s `Https` arm — SURVIVED, and could not have done
  otherwise.** `lookup_svcb` deliberately does not go through `recover`, so
  that arm was unreachable, and an unreachable arm cannot be killed by any
  test. Answered by **deleting** it: `recover` and `addrs` now take a
  `Family` (V4/V6) rather than a `Query`, which is what both callers always
  meant. The replacement mutation — V6 consulting the fallback's V4 stream —
  is killed.

The four §W3 named as the minimum, and what killed each:

| mutation | verdict | killed by |
|---|---|---|
| the response is parsed but the answer ignored (M1) | KILLED | `a_mandatory_key_the_record_does_not_carry_is_an_error`, `a_doh_resolver_composes_into_a_transport_and_that_transport_resolves`, and ten others |
| the TTL is ignored — `ttl: None` (M2, 2 anchors) | KILLED | `each_address_carries_the_ttl_that_came_with_its_own_record`, alone |
| an error RCODE is treated as an empty answer (M3) | KILLED | `servfail_is_an_error_and_not_an_empty_stream`, `a_servfail_also_reaches_the_fallback` |
| `supports_svcb` flipped to `false` (M4) | KILLED | `a_service_mode_record_round_trips_every_field_svcbendpoint_holds`, `a_name_with_no_https_record_is_an_empty_stream_not_an_error` |

## Deliberately not done

- **No cache.** Every `lookup_*` is an HTTP request. Filling
  `ResolvedAddr::ttl` is this crate's job; deciding what to keep and for how
  long is a caller's, and a cache built in here would be one no caller could
  turn off. §W3 warns that DoH is the first place a stale-address bug could
  be ours; not caching is how that stays true of a later, explicit cache
  rather than of this one.
- **No GET.** RFC 8484 §4.1 defines both and a server must support both.
  GET's base64url `?dns=` makes a query cacheable by intermediaries, and
  needs a base64 encoder this workspace does not have; an intermediary cache
  is not obviously what a DNS-over-HTTPS deployment wants anyway.
- **No RFC 9461 `dohpath` discovery** — see the bootstrap table.
- **No `Client`, and therefore no `total` timeout** — see above.
- **No wasm build of this crate has been attempted.** §W3 calls the wasm
  case "the one that would justify the whole crate, and the one nobody can
  test cheaply", and nothing here changes that. There is no `#[cfg]` in the
  crate and its dependencies are all wasm-capable, so it is *expected* to
  build against `http_ng_fetch::Fetch` — expected, not measured.

## What W3 leaves unverified

- **No live DoH endpoint has been queried.** Every test is against a
  loopback fixture. What that leaves untested is exactly §W3's own
  unverified row: **whether a certificate with an IP SAN validates through
  `rustls-platform-verifier`** on Linux, macOS and Windows. Nothing in this
  crate performs a handshake, so nothing in it can settle that; it belongs to
  whichever `TlsConnect` the transport carries, and the check is one live
  request per platform. Until then `Doh::pinned` against a public resolver
  is unproven on all three.
- **The `https` requirement is enforced with one exception, and only one
  half of it is exercised by a live request.** Cleartext is refused unless
  the host is a loopback literal — the local-DoH-proxy shape, and what makes
  these tests cheap. Every test therefore runs over `http://127.0.0.1`, so
  the crate has never actually spoken to a TLS DoH endpoint in CI.
- **No total bound on one query.** `Doh::timeouts` sets `connect`,
  `first_byte` and `between_bytes` — everything `Timeouts` can express. A
  server that answers the head promptly and then dribbles the body one byte
  per `between_bytes` interval is bounded only by `MAX_RESPONSE_BYTES` times
  that interval. Closing it needs a `Timer` in this crate, which `Resolve`
  has no seam for; recorded rather than hidden, and written on the crate's
  module doc as well as here.
- **The DoH transport's `Capabilities` are not consulted.** `Doh` sets
  `Timeouts` in the request's extensions and a transport that reports
  `TimeoutSupport::None` will ignore them silently. A `build()`-time refusal,
  on the shape `ClientBuilder` already uses for `owns_cookie_jar`, would be
  the honest version; it is not written.
- **A DoH answer's own TTL is not used for anything, by anyone.**
  `ResolvedAddr::ttl` is now filled by a second backend and still read by
  nothing in this workspace. That is one step better than v0.2's position —
  the value is checked to be the server's, per record — and no better.
- **A second h3 timing flake, seen once during W3 and not reproduced.**
  `zero_rtt.rs`'s
  `early_data_is_accepted_and_the_wire_shows_it_leaving_before_the_handshake`
  failed once inside a `--workspace --all-features` run — *"the relay was
  supposed to hold the server's flight for 150 ms … 144.71 ms means the
  hold did not happen"* — and passed in isolation immediately afterwards,
  at that commit, at this branch's base and at the commit before it. Nothing
  in W3 touches `http-ng-h3`. Recorded here rather than in the h3 section
  because W3 is where it was seen, and next to
  `two_requests_share_one_connection` above because it is the same class:
  an assertion whose margin is 5 ms out of 150 and whose observer is a
  loaded runner. What would settle it is a wider hold, which is a change to
  a test W3 has no business editing.
- **`SvcbEndpoint` still carries no TTL and no `no-default-alpn`.** Both
  were asked for by the W2 consumer while this work was in flight, and both
  are visible from here (the record's TTL is decoded, and `RawParam::
  NoDefaultAlpn` reaches `endpoint_from_binding` and is dropped). Not added,
  because it is a public struct that three backends construct and one
  read-only-to-this-task crate consumes, and doing it half-way — filled by
  DoH, `None` from hickory and the system — would be exactly the capability
  lie this project has caught four times.
