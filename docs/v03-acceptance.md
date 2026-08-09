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
| The UDP seam's offloads are enforced, not merely published | `crates/http-ng-rt-tokio/tests/udp.rs` — a socket refuses a GSO batch one segment past what it declared and accepts one exactly at the limit |
| A socket reports the ECN it can actually observe | same file — the claim is checked against a real loopback round trip, in both directions: a socket claiming ECN must deliver the codepoint, one not claiming it must report `None` |
| Neither seam carries a QUIC dependency | the `dependency-graph` job's *"the runtime and TLS seams contain no QUIC"*, plus its companion proving the ban is not vacuous |

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
- **No HTTPS/SVCB discovery, and no Alt-Svc.** h3 is chosen by constructing
  `H3`. `http_ng_dns::SvcbEndpoint` already carries `alpn` and the resolvers
  already answer, so tier 2 of the research's discovery ladder is a task
  rather than a vertical; Alt-Svc still needs a store with an eviction
  policy, a negative cache and a rule for `clear`, none of which is protocol
  work.
- **No timeouts.** All three `TimeoutSupport` fields are honestly `false`.
  `connect` is the cheapest to add and is not added, because a declaration
  and its enforcement belong in the same change.
- **No smol UDP backend.** The seam is runtime-agnostic and `async_io::Async
  <UdpSocket>` exposes the same descriptor `quinn-udp` needs — the research
  measured `gso=64 gro=64` through it. Only tokio implements `UdpBind`
  today, so `two_runtimes.rs`'s two-runtime acceptance has no h3 counterpart
  and the seam's portability claim rests on one implementation here.
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
- **A ticket issued over TCP offered to a QUIC handshake.** rustls keys its
  session store by `ServerName` alone while a QUIC ticket also carries
  `quic_params`. `http-ng-tls-rustls`'s QUIC path uses a **separate** store,
  which removes the question rather than answering it, for the price of one
  `Arc`. What would settle the original question: one server serving both
  TLS-over-TCP and QUIC with a shared ticketer, one shared
  `ClientSessionStore`, a TCP handshake then a QUIC 0-RTT attempt.
- **0-RTT ACCEPTANCE has not been observed end to end here; rejection has.**
  `a_rejected_0_rtt_request_is_replayed_and_the_caller_never_sees_it` runs
  the research's scenario 3 — two servers, one certificate, separate
  ticketers — and proves three things at once, because it cannot pass
  without any of them: `into_0rtt` really took the shortcut, the rejection
  was detected, and the replay went out. Checked for vacuity by mutation:
  replacing `if verdict.await` with `if true` (i.e. never replaying) turns
  that test red and leaves the other ten green.
  What is NOT covered is the happy path — a server that *accepts* the early
  data, with the response arriving before the verdict. It needs the timing
  instrumentation the research spike had and this suite does not, and the
  server here already sets `max_early_data_size = u32::MAX`, so the material
  exists.
- **`two_requests_share_one_connection` failed once, under the full
  922-test workspace run, and has not been reproduced.** Not in twelve
  isolated runs of that test, not in five runs of the h3 crate alone, and
  not in three further full-workspace runs. It is recorded rather than
  waved away because a pooling assertion that fails one run in five is
  either a real race under CPU starvation or a test that is measuring the
  scheduler; both matter and neither is settled. What would settle it: the
  `assert_eq!` prints the observed accept count, so the next occurrence
  says whether the connection was replaced (2) or something else went
  wrong. Nothing was loosened to make it pass.
- **The idle A/B's timings are loose on purpose and could still flake.** A
  1500 ms gap under a 1000 ms idle timeout with a 300 ms keep-alive has
  wide margins, and the multiplexing test's 450 ms threshold against two
  300 ms requests has wider ones. Neither is a benchmark; both would rather
  pass on a loaded runner than measure anything precisely.
