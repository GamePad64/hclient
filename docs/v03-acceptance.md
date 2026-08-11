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
| 0-RTT is **accepted** end to end, and the data really left before the handshake | `crates/http-ng-h3/tests/zero_rtt.rs` (W1) — two observers, neither of them the client. On the wire, a UDP relay reading cleartext long headers counted 9 zero-RTT packets carrying 6370 bytes before the client's first Handshake packet; on the server, the request was resolved at 1.8 ms against a handshake completing at 5.0 ms. **The separation is forced by an event and not by a window**: the relay holds the server's flight until the server has resolved a request, so a handshake cannot complete first at any speed or under any load. It was a 150 ms hold and it was flaky for two reasons at once — the fixture check compared two clocks with different origins, and the relay was single-threaded, so holding the server's flight also stopped forwarding the client's early data. Both fixed; 40 runs under 20 spinning CPU hogs, 40 pass. The unmarked warm-up is the asserted negative control: no early data at all |
| The UDP seam's offloads are enforced, not merely published | `crates/http-ng-rt-tokio/tests/udp.rs`, and since W1 `crates/http-ng-rt-pair-check/tests/udp_pair_property.rs` on **both** backends — a socket refuses a GSO batch one segment past what it declared, accepts one exactly at the limit, and a declared batch really arrives as that many datagrams |
| A socket reports the ECN it can actually observe | same files — the claim is checked against a real loopback round trip, in both directions: a socket claiming ECN must deliver the codepoint, one not claiming it must report `None` |
| The UDP seam is a seam and not a design | W1 — `UdpBind`/`UdpAdoptStd`/`UdpDatagrams` on `http_ng_rt_smol::Smol` behind a `udp` feature, and one generic body (`exercise_udp<R>`) run under both runtimes rather than two similar test files. `both_backends_report_the_same_kernel` makes each backend the other's oracle, which is the check neither runtime crate could make alone |
| HTTP/3 runs on two runtimes, from one function | `crates/http-ng-h3/tests/two_runtimes.rs` (W1) — `fetch_once<R>` under `TokioHandle` in a real `tokio::runtime::Runtime` and under `Smol` on a bare `futures_executor::block_on`. Sensitive rather than merely green: adding `R::Instant: PartialEq<std::time::Instant>` breaks the tokio instantiation alone |
| Neither seam carries a QUIC dependency | the `dependency-graph` job's *"the runtime and TLS seams contain no QUIC"*, plus its companion proving the ban is not vacuous |
| A handshake that never completes is cut at the caller's bound | `a_connect_timeout_cuts_a_quic_handshake_that_never_completes` — a bound UDP port that answers nothing, a 300 ms `Timeouts::connect`, and `ErrorKind::Timeout(Phase::Connect)` with the bound readable off `ConnectTimedOut` rather than out of a message. Its control is the same black hole with no bound at all, which must still be waiting at 1200 ms; measured by mutation, the unbounded handshake takes **30 s** to fail on quinn's own idle timeout. `a_client_may_now_set_a_connect_timeout_over_h3` is the caller-visible half — before this it was an `UnsupportedCapability` at `build()` — and `h3_declares_the_timeouts_it_enforces_and_no_others` pins the declaration beside the measurement |
| A streamed request body really streams, and arrives whole | `crates/http-ng-h3/tests/streaming.rs`, `a_streamed_request_body_arrives_whole` — eight 32 KiB frames, and the byte count in the response is the **server's** own, from reading the request body to its end. A client that wrote one frame and stopped cannot produce that number; measured by mutation, stopping after the first frame turns six tests red |
| The response head arrives **while the request body is still being written** | `a_response_head_arrives_while_the_request_body_is_still_going_out` — causal, not timed. The caller's body does not have its second chunk until `execute` has returned a head, so a transport that wrote the whole body before reading the head deadlocks rather than merely lagging. The server's own clock is the second witness: head sent before the last request byte arrived. This is what `full_duplex: true` rests on; the declaration alone is pinned separately by `capabilities_describe_this_implementation_not_the_protocol`, and neither test is worth anything without the other |
| A stalled upload does not stall the response body | `a_stalled_upload_does_not_stall_the_response_body` — the server answers in full and then holds the request stream open without reading it, so no `STOP_SENDING` comes back and the client's flow-control window stays full for the rest of the test. The response still reads back. This test exists because the mutation it kills survived the other 43 |
| A streaming body stops when the peer stops reading, and the response survives | `a_streaming_body_stops_when_the_server_stops_reading_and_the_response_still_arrives` — RFC 9114 §4.1 across many frames rather than one 4 MiB blob. 64 MiB offered against a 1.25 MiB window and a server that answered without reading: the response arrives complete, and fewer than 1024 of 4096 frames were ever pulled from the caller's body. Tolerating the `STOP_SENDING` without stopping is a separate mutation, and it is caught here too |
| An abandoned upload resets its stream instead of poisoning the connection | `dropping_a_streaming_request_mid_upload_resets_it_and_leaves_the_connection` and `dropping_a_buffered_upload_mid_write_leaves_the_connection_too`. quinn's `SendStream::drop` *finishes* a stream, so an upload dropped mid-frame used to end cleanly with a truncated DATA frame — a connection error of type `H3_FRAME_ERROR` under RFC 9114 §7.1, which on a transport that shares connections takes the neighbours down too. The server reports the stream as reset rather than finished, and a following request lands on the same connection. Both shapes are tested because the defect predates streaming |
| A streaming body is never admitted to early data | `a_marked_streaming_request_is_not_admitted_to_early_data` — the mark is there and the body is single-pass, so `admits_early_data` refuses it, and the refusal is visible from outside as the **server's accept count**: early data is part of the pool key, so an admitted request could not have shared the unmarked connection. The pair to `live.rs`'s `early_data_is_offered_only_to_a_request_the_caller_marked`, where the same mark on a replayable body gives two connections |
| Request trailers are refused by name, not dropped | `a_request_body_that_yields_trailers_is_refused_by_name` — `Capabilities::request_trailers` stays `false`, and streaming is what gave that declaration something to refuse: before it, no caller could hand this transport a trailers frame at all |

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
- ~~**No streaming request body, and no full duplex.**~~ **Both done.** The
  bullet is kept rather than deleted because the reason it gave for the two
  `false`s is what governed the change when it came: `execute` wrote the
  whole body and then read the head, and that — not anything about HTTP/3 —
  was what the capabilities described. `one_attempt` now splits the stream
  (RFC 9000 §2.1: the halves are independent), writes the body from a future
  polled beside `recv_response`, and hands the unfinished write to `H3Body`
  to carry on from `poll_frame`. The declarations moved in the same commit
  as the code, and `full_duplex` is measured from outside rather than
  argued — see the rows above.

  **Three things it costs, and none of them are hidden.** A caller that
  never polls the response body never finishes sending the request body:
  nothing is spawned, so the pump advances only when the body is read. That
  is inherent to duplex without a spawned writer — `http-ng-wasi`'s module
  doc records the same consequence for the same technique — and spawning it
  would be worse, because a dropped response would leave an upload running
  with nowhere for its errors to go. A write failure *after* the head has
  arrived can only be a response-body error, which is cost (2) from
  `http-ng-wasi`'s deferral note, paid rather than deferred. And the
  response body ending does not wait for the request to finish: a body that
  held the caller until the upload completed would hang against a server
  that answered in full and then neither read the request stream nor stopped
  it.

  **`request_trailers` stays `false`**, and it is now enforced rather than
  merely declared: a streaming body that yields a trailers frame gets a
  typed `RequestTrailersNotSent` and the stream is reset with
  `H3_REQUEST_INCOMPLETE` rather than finished as if the request had been
  whole. h3 can send trailers in one call, so this is a scope decision, and
  it stays `false` until it is implemented and measured in one change —
  which is the same rule that kept `Timeouts::connect` off until W1.
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
- **One mutation survives on the write-side tolerance, and it is the same
  one `write_after_head`'s doc has always recorded.** Widening its match to
  `Err(_) => Ok(true)` — tolerating *every* write failure after the head,
  not only `RemoteTerminate` — leaves the whole suite green. Re-measured
  after streaming landed, because a claim about which mutations a suite
  catches goes stale the moment the suite grows: 44 tests, still all green,
  including the six streaming tests that go through that function on every
  frame. The reason is unchanged and is a fact about what is observable —
  a swallowed connection error reappears immediately as the same
  `ErrorKind::Body` from the read side, so the two spellings are
  indistinguishable from outside — and a white-box test cannot close it
  either, since `StreamError` is `#[non_exhaustive]` outside h3's
  third-party-backend feature. The narrowness is right by construction and
  the construction is the only argument for it.
- **Duplex is measured with a streaming request body only.** The code path
  is one — `one_attempt` splits the stream and starts the pump whatever the
  body is — but the *observation* needs a body whose later chunks can be
  withheld until the head arrives, and only `RequestBody::Streaming` can do
  that. So "the head arrives while a `Full` body is still going out" is true
  of the same lines and is not separately pinned. What is pinned for a
  buffered body is the cancellation half:
  `dropping_a_buffered_upload_mid_write_leaves_the_connection_too`.
- **`http-ng-rt-smol` has a `OnceLock` init race, found here and not fixed
  here.** `smol_spawn` spawns its executor thread from inside
  `EXEC.get_or_init(..)`, and that thread's first act is
  `EXEC.get().expect("initialised")` — which can run before `get_or_init`
  has published the value. Observed twice in 48 runs of the `http-ng-h3`
  suite at six concurrent processes, as a panic on the `http-ng-smol`
  thread. It is that crate's to fix.
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

### The two queries, made concurrent — a follow-up, with the numbers

W2 shipped the HTTPS query **in front of** the address queries, and the
last two bullets of "what W2 has not checked" said so and priced it. This
is that item, done: `connect` now starts `lookup_ipv6`/`lookup_ipv4` at the
top and awaits the record with both of them running underneath it
(`alongside_address_lookups`), which is what RFC 9460 §10 asks for and what
a browser does.

Nothing about an address depends on the record — this connector does not
resolve a record's target name — and what does depend on it (the port, the
hints, the ALPN offer, the ECH slot) is needed when a socket is opened, not
when a name is resolved.

**`attempt` no longer takes a resolver.** That is the shape change, and it
was chosen over the two alternatives for reasons written next to the type
(`connect.rs`'s `Answers`): re-fetching costs a second query on any
resolver that does not cache, and `http-ng-dns-system` caches nothing of
its own; collecting both families into `Vec`s is the shape this module's
opening paragraph rules out, because `Scheduler` would then never be in the
state "AAAA has not arrived and the resolver is not done either" and RFC
8305's Resolution Delay would be dead code. So the answers are **replayed**:
what arrived early is handed over at once, what has not is still awaited
item by item, and the resolver's stream is polled as many times as one
attempt would have polled it.

#### The claims

| claim | proof |
|---|---|
| The HTTPS query and the address queries are outstanding at the same time | `connect::tests::the_record_and_the_addresses_are_asked_at_once` — a resolver fixture writes call/ask/answer for each of its three streams into one log; the test asserts the A query was **sent before the record was answered** *and* the record's query was **sent before the addresses were answered**. Two claims because each rules out one of the two ways to be serial, and neither is a duration: there is no clock in the fixture |
| …and a serialised connector fails rather than hangs | the fixture's pending poll wakes itself, so the wrong order is a red assertion in a millisecond rather than a watchdog kill ten seconds later. The h3 test that missed a 150 ms window by 0.22 ms is why no timing is asserted here at all |
| The retry on the origin's own terms does not resolve again | `the_retry_without_the_record_does_not_resolve_again` — one `a:call`/`a:ask` pair across two attempts, **with the attempt log asserted alongside**, so that "one lookup" cannot pass for a connector that never retried |
| A dropped connect drops all three queries | `a_dropped_connect_drops_the_record_query_with_the_address_ones` — the resolver's streams report their own `Drop`; one poll puts all three in flight and the HTTPS one never answers |
| The hints still reach Happy Eyeballs first | `a_failed_discovered_endpoint_is_retried_without_the_record`, unchanged: `[hint, origin, origin]` |

**A correction to what the W2 rows above claim.** `svcb::the_address_
hints_reach_happy_eyeballs` pins that a hint *reaches* the race; it does
**not** pin that it comes first, because its resolver answers neither
family and there is nothing for the hint to be in front of. Measured, not
reasoned: with the chain reversed (mutation N3 below) that test stays
green, and the two that go red are the `FakeRt` attempt-log ones. The
ordering claim belongs to them.

#### Mutations

Baseline before each run: `cargo nextest run -p http-ng-native
--all-features`, **141 tests, all passing** (139 before this item, plus the
two order/retry tests; the cancellation test makes 141 with the third).
Each mutation was applied at one site — the patch had to match exactly once
or it was not run — the suite run in full with `--no-fail-fast`, and the
source restored.

| # | mutation | verdict | killed by |
|---|---|---|---|
| N1 | the two queries serialised again (`pump` dropped from `alongside_address_lookups`, replay kept) | **killed**, 1 test | `connect::tests::the_record_and_the_addresses_are_asked_at_once` — and nothing else in the suite, which is exactly why it had to be written |
| N2 | the record's `port` ignored after the reorder | **killed**, 10 tests | `svcb::the_port_from_the_record_is_where_the_connection_goes`, and every other `svcb` record test whose peer is reached through that port |
| N3 | the hints no longer first (the chain reversed) | **killed**, 2 tests | `connect::tests::a_failed_discovered_endpoint_is_retried_without_the_record`, `connect::tests::the_retry_without_the_record_does_not_resolve_again`. **Not** `svcb::the_address_hints_reach_happy_eyeballs` — see the correction above |
| N4 | the retry resolves a second time (a fresh `Answers` pair before the retry) | **killed**, 1 test | `connect::tests::the_retry_without_the_record_does_not_resolve_again` |
| N5 | `Replay` starts at the end of the buffer, so the retry sees no addresses | **killed**, 21 tests | broad: every `svcb` peer test, the `connect` plumbing tests, `transport::declared_connect_timeout_is_actually_applied` |
| N6 | `Answers::pump` keeps only `Ok` items, losing a resolver error | **killed**, 1 test | `transport::resolver_cancelled_error_reaches_the_caller_through_execute_not_flattened` — which is why `seen` holds `Result`s and not addresses |
| N7 | discovery never consulted at all (`discovered_endpoint` returns `None` before the lookup) | **killed**, 14 tests | all ten `svcb` record tests plus the four `connect::tests` discovery ones, including the cancellation one — the check that it observes the *record's* query and not only the address ones |

**N7 is also why the cancellation fixture answers empty instead of
hanging.** Its first version hung all three families, and under N7
`connect` went straight into `drive` with two streams that never finish and
a `FakeSleep` that resolves at once — an infinite loop *inside a single
poll*, which no watchdog in that module can interrupt. It wedged two test
runs before being found. With the address families empty, N7 makes
`connect` return `Ready(Err)` on the first poll and the test is red in 10
ms.

#### The measurement

Same host both sides, and the host matters: x86-64 Linux, `systemd-resolved`
active in stub mode (`nameserver 127.0.0.53`), upstream `172.18.0.2` over a
VPN `tun0` holding the `~.` domain, DNSSEC off.

**Getting a cold number needed more than flushing the stub.** With
`resolvectl flush-caches` alone the same box reads 2.7–9.0 ms per query —
the stub's cache is empty, the *upstream's* is not. Every sample below
therefore asks for a **fresh random label** under the wildcard
`*.localtest.me`, which nothing on the path has seen. Cold, that way:
`lookup_svcb` median **85.4 ms**, `lookup_ipv4` median **130.0 ms** (n=15).
(The 37.5 / 39.3 ms in the bullet below was measured on this same box under
different network conditions, and is left as it was recorded.)

**Live, DNS-dominated.** `https://<fresh-label>.localtest.me/` through
`Native<Tokio, Rustls, SystemDns<Tokio>>` with pooling off. That name
resolves to 127.0.0.1/::1, so what follows the lookups is a loopback
refusal measured in microseconds and what is left in the number is the DNS
phase. 41 samples per run, three runs a side:

| | min | median |
|---|---|---|
| before (`b78af57`) | 395.93 / 398.13 / 396.87 ms | 517.79 / 456.12 / 453.03 ms |
| after | 325.23 / 323.51 / 321.47 ms | 343.21 / 340.38 / 338.29 ms |

**−73 ms on the floor, −115 ms on the median.** The cross-check is N1: with
the concurrency reverted and the replay kept, the same measurement returns
to min 396.19 / median 406.68 ms — the floor it had before the change. This
origin publishes no HTTPS record, so there is no retry here and nothing for
the replay to save; the whole difference is the concurrency.

**Synthetic, deterministic, and it separates the two effects.** A `Resolve`
whose every query takes exactly 200 ms, a record naming a loopback peer's
port and carrying **no hints** (so the race must wait for the A answer),
7 samples, medians:

| | with a record | with no record — the floor |
|---|---|---|
| before | 606.2 ms | 201.6 ms |
| N1 applied (serial again, replay kept) | 406.0 ms | 201.8 ms |
| after | 202.4 ms | 201.7 ms |

An origin's HTTPS record used to cost **404.6 ms of extra DNS time** here —
one query in front, and one more because the retry re-resolved — and now
costs **0.8 ms**. The concurrency is 203.6 ms of that and the replay 200.2
ms.

**What was not visible, and is reported rather than dropped.** A full
request to a real origin (`cloudflare.com` and four neighbours, stub cache
flushed before each, n=20) has a before-median of 252.5 ms with a
run-to-run spread of roughly ±150 ms. Against a warm upstream the DNS phase
there is 7–9 ms, which is far inside that spread; that measurement says
nothing either way and is not offered as evidence. It is the reason for the
`localtest.me` construction above rather than a second number beside it.

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
- **The extra query is still a query the default transport pays for, but it
  no longer costs a round trip of latency.** `SystemDns::supports_svcb()`
  is `true` on Linux, so `DefaultTransport` asks for an HTTPS record before
  every connect that opens a new connection. When this was first measured
  (systemd-resolved active, `res_query` through the stub) a **cold**
  `lookup_svcb` was 37.5 ms — one real round trip, the same order as the
  `A` lookup beside it (39.3 ms) — and repeats **54 µs to 1.4 ms**. The
  repeats are cheap only because the OS stub caches; `http-ng-dns-system`
  caches nothing of its own (grep it for `cache`), so on a host with no
  caching stub every request adds a full query. That query is now made
  **beside** the address lookups rather than in front of them — see "the
  two queries, made concurrent" above for the before/after numbers and for
  a cold measurement that does not depend on any cache being empty. What
  is still not measured is a second machine; what would settle it is the
  same timing with `systemd-resolved` stopped, and on a host whose
  resolver cannot answer SVCB at all (where the whole cost is zero,
  because `supports_svcb()` is `false` and nothing is asked).
- ~~**The query is serialised before the address lookups.**~~ **Done in the
  follow-up above.** The reason recorded here for the serial order was that
  "the record's `port` applies to every attempt in the race and `drive`
  takes one port for all of them", so parallelism would mean either
  abandoning started attempts or teaching Happy Eyeballs a per-attempt
  port. Both alternatives were real and both were avoided: the record is
  still awaited before the **race** starts, only not before the
  **resolution**. The addresses do not depend on it, and they were the only
  thing being blocked. `drive` still takes one port and no attempt is ever
  abandoned.
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
  GET's base64url `?dns=` makes a query cacheable by intermediaries, which
  is not obviously what a DNS-over-HTTPS deployment wants. ~~and needs a
  base64 encoder this workspace does not have~~ — **that half was wrong,
  and the live work found it.** `cargo tree -p http-ng-dns-doh -e normal -i
  base64` returns `base64 v0.22.1 <- dns-message-parser`: an encoder is
  already compiled into every build of this crate. GET costs a dependency
  line and a call site, not a crate in the graph. And it is not refused by
  anyone — both operators answer it, `the_get_form_this_crate_does_not_
  send_is_answered_by_both_operators`. The decision stands on the caching
  argument alone now, which is where it should have stood.
- **No RFC 9461 `dohpath` discovery** — see the bootstrap table.
- **No `Client`, and therefore no `total` timeout** — see above.
- **No wasm build of this crate has been attempted.** §W3 calls the wasm
  case "the one that would justify the whole crate, and the one nobody can
  test cheaply", and nothing here changes that. There is no `#[cfg]` in the
  crate and its dependencies are all wasm-capable, so it is *expected* to
  build against `http_ng_fetch::Fetch` — expected, not measured.

## What W3 leaves unverified

- ~~**No live DoH endpoint has been queried.** Every test is against a
  loopback fixture. What that leaves untested is exactly §W3's own
  unverified row: **whether a certificate with an IP SAN validates through
  `rustls-platform-verifier`** on Linux, macOS and Windows.~~ **Done for
  IPv4 on Linux, and the answer is yes** — see "Live, and the four things
  it found" below. `Doh::pinned` against Cloudflare and Google completes
  the handshake through the platform verifier and reads a DNS answer back.
  **macOS and Windows stay open**, and by more than a missing runner: both
  hand the chain to the OS (Security.framework, CryptoAPI) rather than to
  rustls's webpki path, so Linux is not evidence about them.
- ~~**The `https` requirement is enforced with one exception, and only one
  half of it is exercised by a live request.** Cleartext is refused unless
  the host is a loopback literal — the local-DoH-proxy shape, and what makes
  these tests cheap. Every test therefore runs over `http://127.0.0.1`, so
  the crate has never actually spoken to a TLS DoH endpoint in CI.~~ **The
  crate has now spoken to three TLS DoH endpoints**, though still not in
  CI, deliberately — the argument is below. The loopback-`http` half is
  unchanged and is still what every hermetic test uses.
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

## Live, and the four things it found

Written after the fact, on the terms the rest of this file uses: what was
measured, on what date, against whom, and what it cost to believe.

`crates/http-ng-dns-doh/tests/live.rs`, run by `just test-doh-live`, is nine
tests against **Cloudflare (`1.1.1.1`)** and **Google (`8.8.8.8`)**, with
**Quad9 (`9.9.9.9`)** in one of them. Two operators rather than one because
a single provider's behaviour is that provider's and not the protocol's —
and every claim below except one was made at both. All measurements here
are from **2026-08-10** on x86-64 Linux, `rustls-platform-verifier` 0.7,
rustls 0.23 with `ring`.

### 1. A certificate presented for an IP address validates — on Linux

The question this whole exercise was for, and §W3's own unverified row.
`Doh::pinned` takes an IP literal, so a pinned deployment's TLS server name
is an address and the certificate has to carry an IP SAN; nothing in this
workspace had ever made that handshake.

**It completes, at both operators, through
`Rustls::with_platform_verifier()`** — not `with_webpki_roots`, which would
have answered a different question. Both answered `200
application/dns-message` with records in it.

Two limits on that, both real. **macOS and Windows are not covered and
Linux is not evidence about them**: `rustls-platform-verifier` delegates to
Security.framework and to CryptoAPI there, and IP-SAN name matching is
theirs to implement. **An IPv6-literal endpoint did not work at all** when
this section was first written; that was finding 2, and it is now fixed.

### 2. An IPv6-literal endpoint failed at TLS — found here, fixed elsewhere

`Doh::pinned`'s own doc offered `https://[2606:4700:4700::1111]/dns-query`
as an example. Measured, it failed with **`Tls: invalid dns name`** — the
TCP connection was made, and the handshake never started.

`http::Uri::host()` returns an IPv6 literal **with its brackets** (RFC 3986
§3.2.2 puts them in the *authority*, not in the host);
`http-ng-native`'s `connect.rs` passed that string to
`TlsRequest::server_name` unchanged; `rustls_pki_types::ServerName::
try_from` tries `DnsName`, then `IpAddr`, and neither strips a bracket.
Both `IpLiteralOnly::literal` and `Doh`'s own `ip_literal` do strip them,
each with a comment naming this exact trap — the TLS name was the one place
on the path where nobody did.

**It was not DoH-specific**: every `https://[…]/` URL in this workspace met
it, and the follow-up found **two more sites** in `http-ng-h3` — the QUIC
server name handed to `Endpoint::connect_with`, and `resolve`'s
`host.parse::<IpAddr>()` shortcut, which a bracketed literal fails, so the
address went to a resolver that cannot answer it either.

All three now call `http_ng_core::bare_host`, and
`http_ng_tls::TlsRequest::server_name`'s doc states whose duty the
normalisation is (the caller's) — the gap that let three sites get it wrong
was that nobody had written that down. The tests are
`http-ng-native`'s `tests/tls_server_name.rs` and `http-ng-h3`'s
`tests/quic_server_name.rs`, each asserting a completed handshake against a
certificate with an IP SAN, plus this file's own live
`an_ipv6_literal_endpoint_resolves`, which replaces the test that used to
pin the defect and measured, against Cloudflare over IPv6, real A records
coming back.

### 3. Quad9 answers no DNS at all over HTTP/1.1

`9.9.9.9` replies to every DoH request over HTTP/1.1 with **`505 HTTP
Version Not Supported`** and an HTML body: *"This server implements RFC
8484 … and requires HTTP/2 in accordance with section 5.2 of the RFC."*
§5.2 makes HTTP/2 the minimum **RECOMMENDED** version; Quad9 reads that as
a requirement. Over HTTP/2 the same request is answered normally —
confirmed with `curl`, which negotiates `h2` by ALPN.

`http-ng-native` speaks HTTP/1.1 unless its `http2` feature is on, so **a
default build of this workspace cannot use Quad9 as a DoH resolver.** That
is a fact about a deployment choice, not a bug, and it is now written where
someone choosing an endpoint will meet it.

The crate reports it correctly and that is worth its own assertion: the
caller gets `DohError::Status { status: 505 }`, not a `ContentType` or a
`Malformed`. `Doh::exchange` checks the status *before* the content type
and the content type *before* decoding, and this is the only place in the
suite where a real server supplies the HTML error page that ordering exists
for.

### 4. `dig` and this crate spell a root TargetName differently, for a reason

The `dig` comparison failed on its first run: for `crypto.cloudflare.com`,
BIND printed `target = .` and this crate said `crypto.cloudflare.com`.

Both are right. RFC 9460 §2.5 says a ServiceMode TargetName of `.` means
the record's own owner name, and `endpoint_from_binding` substitutes it so
that no consumer has to know the convention; `dig` prints what is on the
wire. The oracle now maps one notation to the other, which turns the
comparison into a check that **the substitution happened, on a record
nobody here wrote**.

Worth recording because the first reading of it was "our parser is wrong",
and the second was a comment claiming no fixture covered the rule — which
the mutation run (L4 below) refuted immediately: `svcb.rs`'s
`a_service_mode_record_with_a_root_target_takes_its_owner_name` covers it
hermetically too.

### What else a real answer settled

| question | answer, at both operators |
|---|---|
| Does a real HTTPS record parse, field for field? | **Yes.** All seven fields of `crypto.cloudflare.com`'s record agree with `dig @<same operator>`: priority `1`, root target, `alpn=["h2"]`, no port, both hint lists, and the ECHConfigList's length |
| Does the ECHConfigList keep RFC 9460 §7.3's redundant length prefix? | **Yes**, and the prefix is right: 71 bytes, first two reading `0x0045` = 69, then `fe 0d` — the ECHConfig draft-13 version. Exactly the shape `tests/svcb.rs` builds by hand |
| Do the TTLs come back, and are they the wire's? | **Yes.** Against Cloudflare the value equals the one this test decoded by hand from its own query seconds earlier (measured pairs: 73691/73689, 73129/73129, 72912/72912, 72900/72900, 72387/72387, and one 15477/15478) |
| Does `NXDOMAIN` from a real authority look like the fixture's? | **Yes** — an empty stream, not an error, for `nothing-here.invalid` (RFC 6761 §6.4) |
| Does a real server answer the `GET` form this crate does not send? | **Yes**, `200 application/dns-message` with an A record, at Cloudflare and Google. So "POST only" stays a trade this workspace chose (no base64 encoder for one call site) rather than one a server imposed. Quad9 answers `505` to `GET` too — it is the HTTP version it objects to, not the method |
| Is the `Content-Type` this crate sends load-bearing? Is the `Accept`? | **The first yes, the second no.** Dropping `Content-Type`: `415` at Cloudflare, `400` at Google. Dropping `Accept`: `200` at both. RFC 8484 §4.1 makes the first a MUST and the second a SHOULD, and now something fails if the MUST is deleted as redundant |

**The TTL claim is made against Cloudflare only, and that is a measurement
rather than a preference.** Comparing our TTL with a second query's needs a
coherent cache behind the two. Cloudflare's gave the identical value every
time it was asked. Google's frontends do not share one: the same RRset came
back as **703 s and 18759 s within the same second** (and 847/313,
17765/5893 in an earlier probe). So the cross-query comparison is made
where it is sound, and the claim that covers *both* operators is that they
**disagree** — two independent caches serving what is left of an 86400 s
record cannot both hand back the constant a fabricating implementation
would.

### Should this be in CI? No — and the argument, not the shrug

**It is in no CI job, and `just ci` does not call it.** Three reasons, in
order of weight.

**A red build nobody can fix teaches people to ignore red builds.** What
this suite measures is a fact about somebody else's server. Findings 3 and
4 are the proof: if Quad9 starts accepting HTTP/1.1, or Cloudflare stops
publishing `ech` on `crypto.cloudflare.com`, a job goes red and the correct
response is to edit the test — which is to say the signal carried no
information about this code. Mixing that into the same signal as "your
change broke the parser" devalues the second.

**The network is a coin toss at this granularity, and mitigating it makes
the test weaker.** Measured on the host that wrote this: **17 of 240 plain
TCP connects to these three addresses were lost**, uniformly, unrelated to
rate or to DoH. That is why the suite retries four times with a 250 ms
backoff and a 5 s connect bound — and every one of those is a step away
from "one request, one answer". A GitHub runner's link is better than this
one; the class of failure is the same, and the mitigation is the same
weakening.

**The third reason is the one worth arguing with: a nightly would be
defensible.** It never gates a merge, so a red nightly does not train
anyone to ignore a red PR check, and it would catch exactly the rot that
findings 3 and 4 are made of. It is not here because a repository with no
maintainer rotation gets a permanently amber badge instead of an alert —
the same failure one clock slower — and because nothing yet reads a nightly
here. That is a fact about this project's process, not about the test, and
it is the thing to revisit first.

**What would move a real part of this into CI, and is the concrete
recommendation.** The IP-SAN question has two halves, and only one of them
needs a public resolver:

- *Does the platform verifier match a name against an IP SAN?* — This is
  the half that varies by OS and the half Linux cannot answer for macOS and
  Windows. It needs **no public server at all**: a locally issued
  certificate carrying an IP SAN for `127.0.0.1`, a test root added to the
  platform trust store, and a loopback listener would answer it on all
  three runners, hermetically, on every push.
- *Do the public resolvers actually present such certificates, and behave
  as RFC 8484 says?* — This half is irreducibly about third parties and
  belongs where it is: an opt-in recipe, and this dated record.

### The mutation table

Two groups, because the live file is a harness before it is a test and a
harness has its own way of being wrong: it can pass having done nothing.
The script is `crates/http-ng-dns-doh/live-mutations.py`; anchor counts are
checked before every edit and every edit is reverted afterwards.

**Group H — the harness cannot pass when the query never happened.** Each
row breaks the harness while leaving every assertion untouched, then runs
`just test-doh-live` with `HTTP_NG_REQUIRE_NETWORK` set.

| mutation | verdict | what killed it |
|---|---|---|
| H1 the gate always returns "skip", so no query is ever made | KILLED | the recipe's receipt count: `0 of the expected 16` |
| H2 the endpoints point at TEST-NET-1 (`192.0.2.1`) | KILLED | three tests, on this host — see the note below |
| H3 the test spells the receipt differently from the recipe | KILLED | the count reads 0, which is the `HTTP_NG_REQUIRE_WASMTIME` marker-symmetry check made by running rather than by grepping |
| H4 **control:** H1 again, with the recipe's count disabled | **SURVIVED**, as it must | nothing — which is the point: H1's kill came from the belt it is supposed to demonstrate and not from somewhere else |
| H5 the live answer is replaced with a fabricated one after the exchange, before the assertions | KILLED | `the_ttl_a_caller_gets_is_the_one_that_came_off_the_wire` |
| H6 an unreachable endpoint with the marker's panic disabled | KILLED | the tests themselves |

**Two things about H2 and H6 that are about this host rather than about the
harness**, and they are why the rows name tests instead of the count: a TCP
connect to `192.0.2.1:443` **succeeds** from here — something on the path
answers the SYN for an address in TEST-NET-1 — so the gate's probe finds a
route and the exchange fails afterwards instead. On a network where TEST-NET-1
is unroutable, both rows would be killed by the gate and the count. Either
way the harness does not go green.

**Group L — what the live suite kills, and what only the fixtures do.**
Each row is one library mutation run twice: against the hermetic suite
(`cargo nextest run -p http-ng-dns-doh` with the opt-in absent, so every
live test skips) and against the live suite alone.

| mutation | hermetic | live |
|---|---|---|
| L1 the TTL is dropped (`ttl: None`) | KILLED | KILLED |
| L2 `supports_svcb()` flipped to `false` | KILLED | **SURVIVED** |
| L3 the ECHConfigList loses RFC 9460 §7.3's length prefix | KILLED | KILLED |
| L4 RFC 9460 §2.5's owner substitution for a root TargetName is dropped | KILLED | KILLED |
| L5 the HTTP status is not checked | KILLED | KILLED |
| L6 the response is parsed but the answer section ignored | KILLED | KILLED |
| L7 the echoed question is not checked | KILLED | **SURVIVED** |
| L8 an error RCODE is treated as an empty answer | KILLED | **SURVIVED** |
| L9 the `ipv4hint` is dropped | KILLED | KILLED |
| L10 NXDOMAIN becomes an error rather than an answer | KILLED | KILLED |

**The three survivors are the finding, and none was answered by adjusting
anything.** They say precisely what a live suite cannot do, which is the
half of its value that is easy to overstate:

- **L7 and L8 survive because a real server behaves.** Cloudflare and
  Google echo the question they were asked and do not return SERVFAIL on
  request. Only a fixture can be made to misbehave on purpose, so the
  checks against a lying server are exactly the ones the loopback tests
  will always own. This is the concrete answer to "could the fixtures be
  replaced by live tests": no, and here is which fixtures.
- **L2 survives because nothing in the live suite reads the flag.**
  `a_real_https_record_parses_and_every_field_agrees_with_dig` calls
  `lookup_svcb` directly, which works whatever `supports_svcb()` says. Left
  as a survivor rather than papered over with an `assert!(…supports_svcb())`
  that no live answer could contradict: the hermetic suite kills it twice
  over, and a decorative assertion here would be the capability lie this
  project has caught four times, in miniature.

**One row in this table was scored wrong on the first run and caught by
reading it.** L2 came back `live=KILLED (the_ttl_…)` — a mutation of
`supports_svcb` killed by a test about TTLs, which is not a thing that can
be true. It was a lost packet, on the 7%-loss link measured above. Re-run
in isolation, it survives. **Any live mutation run needs its killers read
and not just counted**, which is the same discipline the anchor counts
exist for one level down.

### One harness bug, found while mutating

The two tests that *expected* an error — the IPv6-literal one (since
rewritten to expect success, finding 2) and the Quad9 one — called `doh()`
directly rather than the retrying wrapper, so a lost SYN turned "Quad9
answers 505" into a failed assertion about a connect timeout. Reproduced under `HTTP_NG_REQUIRE_NETWORK` before it was fixed.
Both now go through `lookup_v4`, whose retry predicate reads the **typed**
error — only `DohError::Transport` wrapping `ErrorKind::Connect` or
`Timeout(Phase::Connect)` is retried — so every answer, welcome or not,
arrives on the first attempt. The retry is on the network and never on an
assertion, which is the only thing that makes one admissible here.

### What the live run still leaves unverified

- **macOS and Windows.** Finding 1 is a Linux measurement. See the CI
  recommendation above for the half of it that does not need a public
  resolver.
- **HTTP/2 to a DoH endpoint.** The `http2` feature's arm of
  `an_endpoint_that_demands_http2_…` has never been exercised: the run that
  produced this section was a default build, so what is measured is the
  505. The test reads the outcome at run time and asserts the matching arm,
  so a build with the feature on will check the other one — nobody has made
  that run.
- **`Doh::bootstrapped` against a live endpoint.** Every live test here is
  `pinned`. The bootstrapped path would need a transport carrying a real
  resolver, which makes the DoH server's own name a second thing under
  test; it is covered hermetically and not live.
- **A fallback that fires against a live failure.** `with_fallback` is
  hermetic-only, and deliberately: making a public resolver fail on demand
  means blocking it at the firewall, which is a test about the firewall.

# v0.3 W4 — the WebSocket seam (steps 1 and 2)

`docs/w4-upgrade-seam.md` decided the shape; this is steps 1 and 2 of its
§5 — the trait in `http-ng-core`, and `http-ng-native` implementing it.
Steps 3 (`http-ng-fetch`) and 4 (`UpgradeSupport` deleted) are not here,
and `Capabilities::upgrade` was untouched by this change: every backend
still reported `UpgradeSupport::None`, because §3 places its removal
beside the change that also gives the browser its own answer. **Both
statements describe steps 1 and 2 as they landed, and both have since been
overtaken** — steps 3 and 4 are the section below, and there is no
`Capabilities::upgrade` in this workspace any more.

## The shape, and why

Two traits, both in `http_ng_core::unversioned` — the semver quarantine,
which is where a seam validated against exactly one backend belongs.

```rust
pub trait WebSocket: Stream<Item = Result<Message, Error>> + Sink<Message, Error = Error> {}

pub trait WebSocketConnect {
    type WebSocket: WebSocket;
    fn websocket(&self, req: http::Request<()>)
        -> impl Future<Output = Result<Self::WebSocket, Error>>;
}
```

**Two rather than one, because a backend is not a connection.** The
document says "a backend that can do WebSocket implements the trait", and
the thing a backend implements cannot itself be `Stream + Sink` — a
transport holds many connections. So the connector is what a backend
writes and the message channel is what it hands back.

**No capability field and no method returning `Unsupported`.** The seam
expresses itself by being implemented: `http_ng_native::Native` has the
`impl`, and a transport that does not have one cannot be asked. That is
`TcpAdoptStd`'s rule and `QuicTlsConnect`'s reasoning, applied a third
time.

**`Message` is `Text`, `Binary`, `Close`, and no `Ping`/`Pong`.** RFC 6455
§5.5.2 makes answering a ping the endpoint's duty rather than the
caller's, and `http-ng-native` discharges it without telling anybody. A
caller-visible `Ping` would be a variant the browser can neither send nor
ever receive — `WebSocket` in a page answers pings itself and surfaces
nothing — which is the capability lie this workspace has caught four
times. The enum is deliberately **not** `#[non_exhaustive]`: if a caller
decision ever turns on a control frame, adding the variant should be a
compile error at every backend, and nothing here is published, so that
costs a rebase inside this workspace and nothing outside it.

**The error type is concrete (`http_ng_core::Error`), unlike
`Transport::Error`.** `Transport`'s associated error plus its `to_error`
hook exist so a backend with a genuinely `!Send` error keeps its typed
source while `Client` classifies. There is no `Client` between this seam
and its caller, so the escape hatch has no subject; one concrete type also
makes `Stream::Item`'s error and `Sink::Error` the same type without an
associated-type equality every caller would have to spell.

**`http::Request<()>` in, and one duty with it**: a backend that cannot
send a header the request carries must **fail rather than drop it** — the
rule `http-ng-wasi` already follows for `wasi:http`'s request options. It
is what keeps this seam from being the place an `Authorization` silently
does not go out, and it is the whole of the answer for a browser backend,
which can send no headers at all beyond the subprotocol list.
`http-ng-native` pays it in the other direction too: the four headers the
handshake owns (`Connection`, `Upgrade`, `Sec-WebSocket-Key`,
`Sec-WebSocket-Version`) are **refused** on the way in rather than
overwritten, because overwriting is dropping.

`futures-core` and `futures-sink` are new dependencies of
`http-ng-core`, and unconditional. Measured before deciding: both are
dependency-free type-only crates, and both are already in every graph in
this workspace that has any `futures` at all — `http-ng-wasi` on
`wasm32-wasip2` and `http-ng-fetch` on `wasm32-unknown-unknown` carry them
today, so those crate counts do not move. Only `http-ng-core` built alone
changes, 11 → 13. A seam a backend author can name only in some builds
would be worse.

## What `http-ng-native` does underneath

Behind the `websocket` feature, off by default. Measured `cargo tree -e
normal`, unique crates, this tree: `http-ng-native` alone 32 → 49, and a
realistic client build (`http-ng-native` + `http-ng` + `http-ng-rt-tokio` +
`http-ng-tls-rustls` + `http-ng-dns-system`) 79 → 93. The +14 are
`tungstenite`, `log`, `data-encoding`, `rand`/`rand_core`/`chacha20`, and
`sha1` with the RustCrypto stack under it. **No `tokio`, `hyper` or `h2`
arrives with the feature**, and no runtime does.

**`poll_without_shutdown` + `into_parts`, never `hyper::upgrade`.**
`hyper::upgrade::Upgraded` holds `Rewind<Box<dyn Io + Send>>`, which would
put a `Send` bound on this crate's IO and shut out single-threaded
runtimes — the objection that disqualified `hyper/http2` in v0.2 W3.
`Parts::read_buf` carries whatever the server put in the same flight as
the `101`; hyper has already read it off the socket, and dropping it would
lose the server's first frames in exactly the case no test with a polite
server ever reaches.

**`tungstenite` 0.30, driven by us, and it needs no `unsafe`.**
`WebSocketContext` takes the stream as a *parameter*, so the shim handed to
each call borrows the poll `Context` for exactly that call —
`tokio-tungstenite`'s `AllowStd` owns the stream across calls and has to
store a `*mut Context` instead. This is the third state machine this
workspace drives by hand rather than adopting its runtime glue.

**A WebSocket is opened on a connection of its own and never returns to
the pool.** `crate::pool` is not consulted and no `CheckIn` is ever built,
which is the same conclusion `tests/switching_protocols.rs` reached from
the other side.

### §6's two open checks, answered

`docs/w4-upgrade-seam.md` §6 named two things to check when the code was
written rather than assume. Both were read out of `tungstenite 0.30.0`'s
source, and both hold.

**Does `WouldBlock` out of the shim leave `WebSocketContext` resumable on
every path?** Yes, and the interesting path is the one §6 pointed at.
`WebSocketContext::read` catches a `WouldBlock` from *its own* flush and
sets `unflushed_additional`, so a queued pong or close is retried on the
next call rather than lost — which means a `WouldBlock` that escapes
`read` can only have come from the read side, and that is what makes it
safe to report as `Poll::Pending`: the read waker has been registered by
then. `write` formats the frame into `out_buffer` before anything can
block. `close` sets `ClosedByUs` before it can block and takes its
`if let Active = state` branch only once, so a resumed close queues one
close frame, not two.

**Is a partial write lost between polls?** No.
`FrameCodec::write_out_buffer` loops `stream.write(&out_buffer)` and does
`out_buffer.drain(0..len)` for each partial write *before* the `?` on the
next one propagates: what was written leaves the buffer, what was not
stays in it, and the next flush continues from there. That is the whole
reason the shim's `write` must return the real `n` rather than `buf.len()`
— and it is a killable mutation, below, not a remark.

## The tests

`crates/http-ng-native/tests/websocket.rs`, thirteen of them, against a
fixture that speaks RFC 6455 **by hand**: it parses the opening handshake
out of raw request bytes and encodes and decodes frames itself, so what is
asserted is opcodes, mask bits and payload bytes. Using `tungstenite`'s own
server would have been a quarter of the code and a much weaker witness,
since the client under test is framed by `tungstenite` and a fixture
sharing its codec cannot tell a correct frame from a consistently wrong
one. The one function the fixture does borrow —
`handshake::derive_accept_key` — is pinned against RFC 6455 §1.3's worked
vector by `the_accept_key_derivation_matches_rfc_6455`, so the two sides
agreeing is not the same as both being right.

**Nothing is asserted by a clock.** Two tests are causal in the shape
`crates/http-ng-h3/tests/streaming.rs` established:

- the server withholds its next message until the client's **pong** has
  arrived, so a client that never answers a ping receives nothing at any
  speed;
- the partial-write test releases the server's reader only once the
  client's own `poll_flush` has actually returned `Pending`, and asserts
  that it did — so the partial write is a fact the test establishes rather
  than one it hopes for.

`BOUND` is a 30 s watchdog and never a threshold: every failure it can
produce reads "this hung".

## Mutations

Anchor counts verified before each run; each mutation applied alone,
`cargo nextest run -p http-ng-native --features websocket --test
websocket`.

| mutation | verdict | killed by |
|---|---|---|
| `Parts::read_buf` discarded (`from_partially_read` → `new`) | KILLED | `the_first_frame_may_arrive_in_the_same_flight_as_the_101`, and two others |
| the status not checked at all | KILLED | `a_200_is_an_error_rather_than_a_websocket` |
| the status checked *after* `into_parts`, before the header checks | KILLED | `a_200_is_an_error_rather_than_a_websocket` — but incidentally, on which error is reported first; see the row below |
| **all four handshake checks moved after `into_parts`** | **SURVIVED** | — see below |
| **`poll_without_shutdown` → `Connection`'s own `Future` impl** | **SURVIVED** | — see below |
| `Sec-WebSocket-Accept` not checked | KILLED | `a_101_whose_accept_key_is_wrong_is_refused` |
| `Upgrade:` not checked | KILLED | `a_101_that_is_not_upgrading_to_websocket_is_refused` |
| `Connection:` not checked | KILLED | `the_connection_header_is_read_as_a_token_list_and_is_read` |
| `Connection:` compared by equality instead of as a token list | KILLED | `the_connection_header_is_read_as_a_token_list_and_is_read` |
| the shim's `write` returns `buf.len()` instead of the real count | KILLED | `a_message_larger_than_the_socket_buffer_arrives_whole` |
| `poll_next` reads through a shim that cannot write, so the queued pong never leaves | KILLED | `a_ping_is_answered_with_a_pong_which_is_what_releases_the_next_message`, and `a_close_from_the_peer_is_echoed_and_ends_the_stream` |
| **the control-frame arm returns `Pending` instead of reading again** | **SURVIVED** | — see below |
| the four reserved handshake headers overwritten rather than refused | KILLED | `a_request_that_sets_a_handshake_header_itself_is_refused` |
| `Role::Server` instead of `Role::Client` (client frames unmasked) | KILLED | five tests |
| `WebSocketConnect::WebSocket` gains a declared `Send` bound | KILLED (compile) | `a_non_send_backend_still_satisfies_the_websocket_seam`, `http-ng-core/tests/shape.rs` |

### The three survivors, and what each one means

**All four handshake checks after `into_parts` — survived, and the reason
is worth keeping.** An earlier draft of `websocket.rs`'s module doc
asserted this would *hang*, on the grounds that `poll_without_shutdown` on
an ordinary `200` with a body never completes. It completes: `drop(body)`
finishes hyper's dispatcher whatever the response was, so the connection
comes apart on a `200` as readily as on a `101` and a refused upgrade
drops its socket either way. The checks stay where they are because
reading a response before dismantling the connection that produced it is
the order that stays correct if hyper's behaviour does not — but they are
not load-bearing today, and the module doc now says so instead of implying
otherwise. **The property that *is* pinned** is the one the trap is about:
the upgrade is recognised by the status and the three handshake headers,
never by "hyper has finished with this connection", and deleting any of
the four kills a named test.

**`poll_without_shutdown` → `Connection`'s `Future` impl — survived, and
this was predicted from hyper's source before it was run.** At hyper 1.11
`poll_inner` returns `Dispatched::Upgrade` *before* it ever looks at
`should_shutdown`, and both entry points then call `pending.manual()`, so
on a `101` the two are the same call. `poll_without_shutdown` is still the
right one: it is the API hyper documents for this, it does not require
`B::Data: Send` where the `Future` impl does, and on any completion that is
**not** an upgrade it is the one that leaves the socket alone. No test in
this workspace distinguishes them, and inventing one would mean asserting
something about hyper's internals from outside a socket.

**The control-frame arm returning `Pending` — survived, and it is a latent
hang rather than a safe alternative.** `WebSocketContext::read` returning
`Ok(Ping)` has touched the socket successfully or not at all, so nothing
has registered a waker on that path; returning `Pending` there is
returning it with no guarantee of a wake. The tests do not catch it
because a leftover tokio readiness registration from the handshake happens
to wake the task anyway — measured, by instrumenting `poll_next` and
counting entries: three, with the pong going out on the second. The loop
is kept because it makes the missing wake structurally impossible, and the
survival is recorded rather than papered over. **What is not recorded as a
gap is the pong itself**: the mutation that actually stops it (a
write-blind shim in `poll_next`) is killed at the top of the table.

## Deliberately not done

- **`Capabilities::upgrade` is untouched**, and every backend still says
  `UpgradeSupport::None` — §3's step 4, not this one. *(Done since, in
  the step 3 and 4 section below: the field and the enum are gone.)*
- **No `http-ng-fetch` implementation**, which is §5's step 3 and the one
  that makes this a seam rather than a native feature with a trait in
  front of it. Nothing here has been checked against a backend that cannot
  see bytes. *(Done since — same section.)*
- **No permessage-deflate, no subprotocol negotiation, no close-code
  semantics.** All three are on `docs/w4-upgrade-seam.md`'s own undecided
  list. A subprotocol *can* be asked for, because the request carries
  headers; nothing checks what came back, and `tungstenite`'s own
  `verify_response` rules for it are deliberately not reimplemented here.
- **No `Ping`/`Pong` a caller can send.** A keep-alive ping is a real
  need and this seam cannot express one; the answer is a variant, and a
  variant needs a caller decision that turns on it (see above).
- **`WebSocketConfig` is `tungstenite`'s default** — 128 KiB read and
  write buffers, 64 MiB maximum message, 16 MiB maximum frame — and
  nothing exposes it. A caller who needs a different message ceiling
  cannot ask for one.
- **No `wss://` test.** The scheme is mapped and the TLS path is
  `connect::connect`'s, the same call `execute` makes with the same ALPN
  list; no test in this file opens one, because every fixture here is a
  plain `TcpListener`.

## What W4 leaves unverified

- **`Timeouts::connect` is honoured through `websocket()` and is not
  tested through it.** The call is `execute`'s own `with_connect_timeout`,
  pinned for `execute` by `crates/http-ng-native/tests/timeouts.rs`, and
  no test in `tests/websocket.rs` reaches it. Nothing else in `Timeouts`
  is read at all: `first_byte` would be a bound on the `101` and is not
  applied, and `between_bytes`/`total` have no meaning for a channel with
  no response body. So **an open WebSocket has no bound of any kind**, and
  a caller who needs one wraps the future themselves. *(Superseded: it
  can have one now — RFC 6455 ping/pong, off by default, on
  `Native::websocket_keep_alive`. `Timeouts` is still not read past
  `connect`, and §7's first decision says why a gap bound in particular
  would have been the wrong answer. The last section of this document.)*
- **The `Sink` gives backpressure from the socket, and that has one
  measured test and one untested consequence.** `poll_ready` is
  `poll_flush`, so a message is accepted only when the write buffer is
  empty; `a_message_larger_than_the_socket_buffer_arrives_whole` exercises
  it once. What is not measured is the cost: `SinkExt::feed` cannot
  pipeline, and no benchmark here says what that is worth.
- **`poll_close` does not wait for the peer's close.** It queues a close
  frame and flushes; the peer's answer arrives on the `Stream`.
  `a_close_from_the_peer_is_echoed_and_ends_the_stream` covers the
  peer-initiated direction; **the caller-initiated close handshake — we
  send `Close`, the peer echoes, the stream ends — has no test.** *(It has
  one now: `the_keep_alive_stops_at_our_own_close`, written for a
  different mutation and closing this at the same time.)*
- **A queued pong can be stranded by a full write buffer.** If the flush
  inside `read` blocks and no further data arrives, the pong waits for the
  next read to be woken by *incoming* traffic, because that is the only
  waker `poll_next` registers on that path. Every async `tungstenite`
  wrapper has this shape; no test here provokes it.
- ~~**Nothing has been run against a real WebSocket server.** Every
  fixture is a loopback socket in this repository, and the Autobahn test
  suite — which is what would settle fragmentation, UTF-8 validation and
  the close code table — has not been run.~~ *(Run: 517 client cases,
  0 failures. The last section of this document has the table, the two
  things the suite tolerates that this client does anyway, and what the
  run still does not cover.)*
- ~~**Fragmented incoming messages are reassembled by `tungstenite` and
  never exercised.** The fixture sends single frames only, so
  `WebSocketConfig::max_message_size` and the continuation path have no
  test of ours.~~ *(The continuation path is exercised now — Autobahn's
  section 5 is twenty fragmentation cases and 9.4–9.6 are fragmented
  messages up to 4 MB, all OK. `max_message_size` is not: the largest
  case is 16 MiB against a 64 MiB ceiling, so nothing has ever hit it.)*


# v0.3 W4 — the WebSocket seam (steps 3 and 4)

The two halves of `docs/w4-upgrade-seam.md` §5 that steps 1 and 2 left:
`http-ng-fetch` implements `WebSocketConnect` over the browser's own
`WebSocket` global, and `UpgradeSupport` is deleted from the workspace.
They are one change because §3 says so — the field lives on every
backend's `Capabilities`, and the change that removes it is the same one
that gives the browser its own answer.

## Did the seam fit the browser?

**Yes, unchanged, and that is the result rather than a formality.** The
trait was not widened, no method was added, no bound was relaxed, and
`http-ng-fetch` implements it in one file with no `#[cfg]` in it. What the
seam asks for — `Stream<Item = Result<Message, Error>> + Sink<Message>`,
opened from `websocket(http::Request<()>)` — is what the browser's
`WebSocket` provides once its events are bridged.

Three things about the fit are worth stating precisely, because "it fit"
is the kind of claim that is easy to make and easy to make emptily.

**The seam's `!Send` allowance now has a real subject.** When the trait
landed, `http-ng-core/tests/shape.rs`'s
`a_non_send_backend_still_satisfies_the_websocket_seam` pinned "a backend
whose socket is `!Send` still satisfies this" against a *synthetic*
`LocalSocket` built for the test. `FetchWebSocket` is the real one it was
predicting: it holds `Rc<RefCell<..>>` and three `Closure`s, both `!Send`,
because a browser socket is bound to one JS event loop. Nothing had to be
relaxed to admit it — and, more usefully, **no second `unsafe impl Send`
was needed.** `http-ng-fetch` carries this project's only `unsafe` block
(`promise.rs`, so `Transport::execute`'s future can be `Send` for
`Client::execute`); this file needed none, because there is no `Client`
between this seam and its caller and therefore nothing demanding `Send`.

**The message vocabulary was the right one, and the `Ping`/`Pong`
omission is now checked rather than argued.** Step 1's module doc said a
caller-visible `Ping` "would be a variant the browser can neither send nor
ever receive". That is what a browser's API actually looks like from
inside an implementation: there is no `send(ping)` and no `onping`, and a
`Message::Ping` variant would have been a compile error here with no
honest right-hand side.

**`Message::Close(Option<CloseFrame>)`'s `Option` earns itself twice.**
`tungstenite` reports `None` for a close frame with an empty payload; a
browser reports the same event as close code **1005**, RFC 6455 §7.4.1's
*"no status code was actually present"*, which is never on the wire. Two
backends, two entirely different APIs, and one shape that both map onto
without inventing anything.

## What it cost: the headers

`WebSocketConnect::websocket`'s duty is that **a backend that cannot send
a header the request carries must fail rather than drop it**, and the
browser's constructor is `new WebSocket(url, protocols)` — a URL and a
subprotocol list, and nothing else. So this backend refuses a great deal,
and refusing precisely is the deliverable:

| what the request carries | what happens |
|---|---|
| `Sec-WebSocket-Protocol` (any number of them, each a comma-separated list) | flattened into the constructor's second argument, in order |
| **any other header at all** | `ErrorKind::Unsupported`, naming the header, **before a socket is constructed** |
| `ws://`, `wss://`, `http://`, `https://` | opened, `http`/`https` rewritten to `ws`/`wss` |
| any other scheme, or a URI with no authority | `ErrorKind::Unsupported` |

"Any other header" includes `Authorization`, `Origin`, `Cookie`, `Host`,
and the four RFC 6455 handshake headers `http-ng-native` refuses under its
own `ReservedHeader` name. That distinction has no subject here — the
browser can send none of them — and
`every_other_header_is_refused_too_including_the_handshakes_own` says so
in one test rather than letting the two backends look like they disagree.

The cost is real and is not this crate's to remove: **a bearer token in an
`Authorization` header cannot open a WebSocket from a browser.** The ways
round it — a ticket in the query string, or the credential as a
subprotocol — are the caller's decision, and a backend that dropped the
header would have made it silently.

## The bridge, and the two places it interprets

The browser's `WebSocket` is event driven, so the crossing is the one
`promise.rs` already makes for a `Promise`: a shared cell, a `Waker`
parked in it, closures that fill it and wake. Two decisions inside that
are not mechanical.

**The queue is a queue.** Messages arrive when the browser says so, not
when the caller polls. `Shared::queue` is a `VecDeque`, and
`two_messages_that_arrive_before_the_first_poll_are_both_kept` delivers
two messages inside the same microtask as `open` — before the connect
future has even resumed — and reads both back.

**There is no `onerror` handler, deliberately.** A WebSocket `error` event
is a bare `Event` carrying no information at all — that is deliberate in
the standard, so a page cannot use a WebSocket to probe its network — and
the standard fires `close` after every `error`. So everything an `onerror`
could report, `onclose` reports with a close code attached, and `onclose`
is where the one interpretation in the file lives:

- **before `open` ever fired** — the handshake failed;
  `websocket()` returns `ErrorKind::Connect`. There is no other reading: a
  server that accepted and then closed would have fired `open` first.
- **`wasClean`** — the peer's `Message::Close`, and the `Stream` ends
  after it. Code 1005 becomes `Close(None)`.
- **not `wasClean`** — code 1006, no close frame received: an *error* on
  the `Stream`, not a `Close(1006)` message. 1006 is a code RFC 6455
  forbids on the wire, and delivering it as a close message would tell a
  caller that only inspects `Message::Close` that its peer said goodbye.

## No cargo feature here, where `http-ng-native` has one

`http-ng-native` gates its implementation at `websocket`, off by default,
because `tungstenite` and its RFC 6455 codec are +17 crates for that crate
alone and +14 in a real client build. **This backend adds no crate at
all**, because the protocol implementation is the browser's. Measured,
`cargo tree -p http-ng-fetch -e normal --prefix none --target
wasm32-unknown-unknown`, unique crates: **33 before and 33 after**, zero
matches for `tokio`, `hyper` or `h2` in either. What it adds is four
`web-sys` feature names (`WebSocket`, `BinaryType`, `MessageEvent`,
`CloseEvent`) on a crate that already depends on `web-sys`, and one direct
dependency on `futures-sink` — already in the graph, since `http-ng-core`
is where the `WebSocket` trait names it.

A feature gating nothing would still cost something: the browser suite
would have to spell it in the `justfile` as well, and drift between a
`--features` list there and a crate's own gate is exactly what that
recipe's own error message warns about.

## The tests

`crates/http-ng-fetch/tests/websocket.rs`, twenty-one of them, green on
**both** engines — Chrome 151 and Firefox 153. The browser suite's minimum
count goes 78 → **99**, measured by running both engines rather than
counting attributes.

**Twenty of them run against a stand-in `globalThis.WebSocket`, and that
is a decision worth defending.** There is no WebSocket server in this test
environment — `wasm-pack test --headless` serves the harness page over
plain HTTP and nothing in this repository speaks RFC 6455 to a browser —
and a public echo server would put the whole suite on the network, which
nothing else here is. So the trick `tests/caps.rs` already uses for the
`Request` constructor: `web_sys::WebSocket::new(..)` compiles to `new
WebSocket(..)` in wasm-bindgen's glue, where `WebSocket` is a free
variable resolved through the scope chain to `globalThis` at call time.
The stand-in is a stand-in for the *browser*, not a mock of this crate:
every line under test runs against it unmodified. What it buys is the two
things a real server cannot give on demand — exact close codes with
`wasClean` in either direction, and a message delivered at a chosen moment
relative to the caller's first poll.

**The twenty-first constructs a real one.** It opens a WebSocket to the
harness's own origin, which answers the upgrade with ordinary HTTP, and
asserts the handshake fails as `ErrorKind::Connect`. That is the only live
WebSocket outcome reachable without a network, and it proves the part a
stand-in cannot: that the real global answers to the same property and
method names, that `binaryType`/`onopen`/`onclose` are where `web-sys`
says they are, and that a failed handshake reaches the caller rather than
hanging. **It asserts the `ErrorKind` and not a close code**, because
Chrome and Firefox do not agree on the number and neither is required to —
the standard hides the reason from the page on purpose. Asserting one
engine's answer is the shape that produced this crate's
`[object ReadableStream]` finding.

**The harness heals a leak rather than spreading one**, and it does so
because the first mutation run said to. That run produced three failures
where the mutation explained two: a test that panicked before its own
`restore` left the stand-in installed, and the real-global test inherited
it. The real constructor is now stashed once in `globalThis.__ws_real`,
`restore()` reads it from there, and the real-global test restores
*before* it runs. A `Drop` guard would be the usual answer and is not
available: `wasm32-unknown-unknown` does not unwind, so a panicking
`#[wasm_bindgen_test]` runs no destructor.

## Mutations

Anchor verified before each run — 21 passing in
`wasm-pack test --headless --chrome crates/http-ng-fetch --test websocket`,
and 8 passing in `cargo nextest run -p http-ng --all-features --test
facade`. Each mutation applied alone, reverted before the next.

| mutation | verdict | killed by |
|---|---|---|
| a header the browser cannot send is dropped (`continue`) instead of refused | KILLED | `a_header_the_browser_cannot_send_is_refused_and_nothing_is_opened`, `every_other_header_is_refused_too_including_the_handshakes_own` |
| the close code is not reported (`Close(None)` for every clean close) | KILLED | `the_close_code_and_reason_are_reported` |
| a message arriving before the caller polls is lost (the queue becomes a single slot) | KILLED | `two_messages_that_arrive_before_the_first_poll_are_both_kept` |
| `binaryType` left at the browser's default `"blob"` | KILLED | `binary_type_is_set_to_arraybuffer_before_anything_can_arrive` |
| `wasClean` ignored — every close is a clean close | KILLED | `a_connection_that_broke_is_an_error_and_not_a_close_message` |
| `sendable()` always `Ok`, so a send after the close is discarded silently | KILLED | `sending_after_the_peer_closed_is_an_error_rather_than_a_silent_drop` |
| the caller's close code never reaches `close()` | KILLED | `closing_the_sink_closes_the_socket_with_the_callers_code` |
| 1005 reported as a real code rather than as `Close(None)` | KILLED | `a_close_with_no_status_is_reported_as_no_frame_at_all` |
| the subprotocol list is parsed and then not passed to the constructor | KILLED | `a_subprotocol_reaches_the_constructor` |
| dropping the socket leaves it open | KILLED | `dropping_the_socket_closes_it`, `dropping_the_connect_future_closes_the_socket_it_had_opened` |
| `poll_next` reads `ended` before draining the queue | KILLED | `the_close_code_and_reason_are_reported` and two others |
| `wss`/`https` collapse to `ws` | KILLED | `http_and_https_are_opened_as_ws_and_wss` |
| **`poll_ready` stops checking `readyState`** | **SURVIVED** | — see below |
| **`on_message` drops its `ended` guard** | **SURVIVED** | — see below |
| `EarlyDataSupport` removed from `http-ng`'s `pub use` | KILLED (compile) | `crates/http-ng/tests/facade.rs`, two `E0433`s |
| the facade fixture sets `EarlyDataSupport::None` — the value `Capabilities::none()` already has | KILLED | `capability_support_types_are_reachable_from_the_facade` |
| **both `caps.early_data` lines deleted from the facade test** | **SURVIVED, and is meant to** | — see below |

### The two survivors, and the third row that is not one

**`poll_ready` stops checking `readyState` — survived, and it is a
redundancy rather than a hole.** `SinkExt::send` calls `poll_ready` and
then `start_send`, and `start_send` calls `sendable()` too, so a message
sent to a closed socket is still refused with the same error by the same
check one call later. What no test distinguishes is a caller that drives
the `Sink` by hand and treats `poll_ready`'s `Ok` as permission to
`start_send` without reading its result. The check stays because
`poll_ready`'s job *is* to answer "can this sink take an item", and
answering yes when it cannot is the lie the mutation installs; it is not
load-bearing today, and this row says so rather than letting the file
imply otherwise.

**`on_message` drops its `ended` guard — survived, and this one is a
real gap.** With the guard gone, a `message` event arriving after
`onclose` is pushed onto the queue *behind* the terminal item, and
`poll_next` drains the queue before it honours `ended` — so the `Stream`
would deliver a message after its `Close` and the seam's "a `Stream` that
has ended stays ended" would be false. No test provokes it: every
close-carrying plan ends at the close. The guard is kept because it makes
the ordering structurally impossible rather than merely unobserved, and
the missing test is recorded below rather than written now.

**The facade fixture's two lines deleted — survived, and that is the
point of the row above it.** A deleted assertion never fails, so this
tells you nothing on its own; it is here because it is the mutation
somebody will reach for when asking whether the re-pointing was real. The
guard that answers is the compile-time one: `EarlyDataSupport` removed
from `http-ng`'s `pub use` breaks `facade.rs` — an external consumer —
with two `E0433`s, and the fixture setting the value `Capabilities::none()`
already has breaks the assertion. Both were run, both killed.

## `UpgradeSupport`, and where the facade's plumbing proof went

Four variants (`None`, `H1`, `ExtendedConnect`, `Both`), every backend
assigning `None`, and no production code branching on it — re-verified
across the workspace before deleting, and the only non-test matches for
`.upgrade` that remain are `Weak::upgrade`. v0.2's rule is explicit: a
capability variant exists only if a caller decision turns on it, and with
WebSocket expressed as a trait a backend implements, none does. The field,
the enum, its `http-ng-core` and `http-ng` re-exports, and the five
backend assignments are gone; `Capabilities` is down to 19 fields.

One thing fell out of counting them. `http-ng-core`'s own
`none_is_the_conservative_base` carried the count in a comment — "every one
of the 18 fields" — against a struct that had 20 of them; the number had
drifted twice while the exhaustive destructure underneath it stayed
correct, which is the argument against writing it down at all. The comment
now says what it checks and no longer says how many.

`crates/http-ng/tests/facade.rs` was setting `UpgradeSupport::H1` on a
fixture and asserting it back. **That test pins the plumbing** — that a
`Capabilities` field's type is nameable *and writable* from a crate
depending only on `http-ng`, which is what building your own
`Capabilities` for `MockTransport::with_capabilities` needs — so it needed
a different field, not deletion.

**It now carries the proof on `early_data`/`EarlyDataSupport`**, chosen
over the alternatives on three grounds:

- **A fixture can set it to something distinguishable.**
  `Capabilities::none()` gives `EarlyDataSupport::None` and the test sets
  `Supported`, so the assertion can fail — and does, as the mutation table
  shows.
- **Nothing else reached it through the facade.**
  `DecompressionSupport` — the only other enum-typed capability re-exported
  from `http-ng` — is already named as `http_ng::DecompressionSupport` by
  `tests/compression_capability.rs`, so pointing this test at it would
  have duplicated a live guard and left `EarlyDataSupport`'s re-export
  unexercised. `tests/too_early.rs`, the one place that works with early
  data, reaches for `http_ng_core::AllowEarlyData` directly rather than
  through `http_ng::`. The early-data corner was the one with no facade
  check at all.
- **The remaining candidates would have needed a new re-export.**
  `ReuseSupport` and `CancelSupport` are enum-typed `Capabilities` fields
  and are deliberately not re-exported from `http-ng`; pointing the test at
  one of them would mean adding a re-export in order to make a test
  compile, which is the reasoning this project rejects everywhere else.

## Deliberately not done

- **No backpressure, and it is not faked.** `Sink::poll_ready` and
  `poll_flush` are `Ready(Ok(()))` whenever the socket is open;
  `bufferedAmount` is read by nothing. There is no event anywhere in the
  platform that fires when a `WebSocket`'s send buffer drains, so
  returning `Pending` on a non-empty buffer would be returning it with no
  waker anything can wake, and the alternative is a `setTimeout` poll loop
  — the busy-spin this workspace measured and rejected once already
  (`http_ng_native::testing::blocking_io`: 600 ms wall, 600 ms CPU). So
  **the two backends differ here**: `http-ng-native`'s `Sink` gives
  backpressure from the socket, this one gives none and the browser's own
  buffer is unbounded. That is a fact about the platform, recorded rather
  than smoothed over.
- **No timeout of any kind.** `Timeouts` in the request's extensions is
  not read at all — not even `connect`, which `http-ng-native`'s
  implementation does honour. `AbortSignal` does not apply to a
  `WebSocket` constructor, and there is no other bound the browser
  exposes, so a caller who needs one races the future itself.
- **Nothing checks the negotiated subprotocol.** The list goes out;
  `WebSocket.protocol` is never read back, and a server that picked none
  of the offered protocols, or one that was not offered, is not detected.
  Same position as `http-ng-native`, and on `docs/w4-upgrade-seam.md`'s
  own undecided list.
- **No permessage-deflate.** The browser negotiates its own extensions and
  exposes no control over them; `Sec-WebSocket-Extensions` is refused like
  every other header, which is the honest answer rather than a partial
  one.
- **`Capabilities` gained nothing.** The seam expresses itself by being
  implemented, so there is no field saying this backend can do WebSocket —
  which is the whole of `docs/w4-upgrade-seam.md` §2's third decision, now
  true of two backends instead of one.

## What steps 3 and 4 leave unverified

- **This backend has never spoken to a WebSocket server.** Twenty of the
  twenty-one tests run against a stand-in constructor and the
  twenty-first against a server that answers the handshake with ordinary
  HTTP. So a real frame has never been sent or received by this code, no
  subprotocol has ever actually been negotiated, and the Autobahn suite —
  which is what would settle fragmentation, UTF-8 validation and the close
  code table — has not been run against it either. The same gap
  `http-ng-native`'s section already records, reached by a different
  route: there, every fixture is a loopback socket in this repository;
  here, there is no server at all.
- **A message arriving after `onclose` would be delivered after the
  `Close`.** The `ended` guard in `on_message` is what prevents it and no
  test provokes it — the surviving mutation above. Provoking it needs a
  `message` step *after* a `close` step in the stand-in's plan, and an
  assertion that the stream ends rather than yielding it.
- **`poll_ready`'s `readyState` check is redundant with `start_send`'s**
  under every caller `SinkExt` produces, and no test drives the `Sink` by
  hand to tell them apart. Also a surviving mutation above.
- **The caller-initiated close handshake is only half tested.** The tests
  assert that `close(code, reason)` reaches the browser with the caller's
  code; what a real peer answers, and that the `Stream` then ends on the
  echo, has no test — the stand-in never echoes. Exactly the gap
  `http-ng-native`'s section records for its own `poll_close`, and for the
  same reason.
- **`wss://` is mapped and never opened.** Every stand-in URL is
  `example.invalid` and the one real handshake is `ws://` to the harness's
  own origin, so no TLS WebSocket has been opened from this code.
- **The Chrome/Firefox difference is asserted away, not measured.** The
  real-handshake test asserts an `ErrorKind` precisely because the engines
  disagree on the close code for a failed handshake. What each engine
  actually reports was not recorded, so if one of them ever started
  reporting a *clean* close for a refused upgrade, this test would still
  pass while meaning something else.
- **Nothing measures the wasm binary's growth.** The crate graph is
  unchanged at 33 crates, which is the claim made and the only one
  measured; how many kilobytes the bridge and the four `web-sys` bindings
  add to a built `.wasm` was not weighed.


# v0.3 W4 — the bound an open WebSocket has

Steps 1–4 shipped a seam with **no bound of any kind past the
handshake**: only `Timeouts::connect` was read, so a peer that vanished
without a `FIN` left a `Stream` that never yielded and never errored. The
section above records that in its own words, and
`docs/w4-upgrade-seam.md` §7 took the decisions — liveness rather than
`Timeouts`, ping/pong rather than a gap bound, on `http-ng-native`'s own
configuration, off by default, driven from `poll_next`. This is the
implementation, and the answers to the two questions §7 left open.

`Native::websocket_keep_alive(WebSocketKeepAlive { every, within })`,
behind the `websocket` feature, no counterpart on `http-ng-fetch`. A
socket silent for `every` sends a `Ping`; a peer that does not answer
within `within` ends the `Stream` with `ErrorKind::Body` whose source is
`PongNotReceived(within)`.

## The two open questions, and why each was decided that way

**§7.1 — no, an unanswered ping is not surfaced before its deadline.**
Not a judgement about how much a caller wants to know, but about what
this `Stream` can say. It yields exactly two things. `Message` has no
`Ping`/`Pong` variant and must not gain one — the seam's own doc gives
the reason, that the browser can neither send nor receive one — and an
`Err` here is *terminal* by that same seam's contract ("a `Stream` that
has ended stays ended"), so a warning delivered as an error would break
the contract for every caller rather than only for the ones who asked for
a keep-alive. A third channel — a callback, a watch handle — would be a
second vocabulary for information nobody can act on: the only action
available on "the ping has not come back **yet**" is to wait, which is
exactly what `within` already does, and a pong arriving a millisecond
inside the deadline is a perfectly healthy connection, so an early signal
would report ordinary jitter as a fault. What a caller can read is
`NativeWebSocket::keep_alive()` and the failure when it happens; the
outstanding probe appears in `Debug` and nowhere else.

**§7.2 — the interval resets on any inbound frame, the deadline only on
a pong carrying that ping's own payload.** Two clocks, two questions.

The interval measures *silence*, and any frame at all — text, binary,
ping, pong, close — is proof the peer is there, so it restarts. That is
what makes "off by default" a bound on traffic rather than a slogan: **a
busy connection sends no keep-alive traffic whatsoever**, where resetting
only on a pong would make a chatty socket ping every interval for ever —
precisely the traffic nobody asked for that the default is off to avoid.

The deadline measures *an unanswered probe*, and only the answer RFC 6455
§5.5.2 makes a MUST answers it. Data comes from the peer's application, a
pong comes from its WebSocket layer, and it is the layer that has to be
alive for anything we send to be read at all; letting any frame clear the
deadline would turn the probe back into the gap bound §7 rejected,
restricted to the window after a ping. The payload is *matched* rather
than the opcode accepted, because §5.5.3 explicitly allows unsolicited
pongs as a unidirectional heartbeat — a peer emitting one every second
would otherwise keep a probe permanently "answered" without ever having
answered it — and it is a sequence number, so a stale pong for an earlier
ping cannot answer a later one.

**A consequence worth stating rather than discovering: both clocks are
polled only when the read side has nothing.** `Pending` is the only
moment `poll_next` has to poll them in. So the deadline cannot fire in
the middle of a stream of data; it fires after `within` of *silence*
following a ping. That falls straight out of "`poll_next` is the only
driver", and it is what makes the paragraph above safe — a peer that
answers a ping with data and keeps talking is not killed, while one that
answers with data and then stops is.

**A third question §7 did not raise, which the code had to answer: the
keep-alive stops at our own close.** `tungstenite` refuses every write
once a close frame has gone out (`ProtocolError::SendAfterClosing`), so a
client that kept probing would answer its peer's ordinary goodbye with a
`Decode` error of its own making. No probe follows our close; a probe
already in flight keeps its deadline. The closing handshake is therefore
still unbounded, which is the same gap `poll_close` records for itself.

## The error is distinguishable from the peer closing, in the browser backend's own vocabulary

A missed pong is `Error::new(ErrorKind::Body, PongNotReceived(within))`.
That is not a new vocabulary: `http-ng-fetch` turns a `CloseEvent` with
`wasClean == false` into an `ErrorKind::Body` on the `Stream` rather than
a `Message::Close(1006)`, for exactly the reason that applies here — a
caller inspecting `Message::Close` must not be told its peer said goodbye
when the network went away underneath one that never did. `PongNotReceived`
is public and named, like `BetweenBytesElapsed` and `FirstByteTimedOut`,
so the distinction survives `Error::source().downcast_ref()` without
parsing a message, and it carries the bound that was in force.

It is deliberately **not** an `ErrorKind::Timeout`. No field of `Timeouts`
is in force on an open WebSocket, and `Phase::BetweenBytes` in particular
would name the gap bound §7 refused to give this seam.

## `poll_next` is the only driver, and that is stated rather than implied

Nothing is spawned; this crate has no `Spawn` bound, deliberately. So the
ping is written from `poll_next` or not at all, and **a caller that stops
polling gets no keep-alive.** It is written in the module doc, in
`Native::websocket_keep_alive`'s doc, and pinned from the server's side of
the wire by `a_socket_nobody_polls_gets_no_keep_alive`, whose two arms
carry the *same* configuration so that the only difference is whether
anything is polling them.

This is the genuine difference from `http-ng-h3`, and both halves are
written next to their own policy so that changing one does not silently
import the other's justification: there a spawned driver keeps a *pooled*
connection alive on behalf of requests nobody has made yet, because such a
connection has no caller. A WebSocket always has one, and a caller that is
not polling is not waiting for anything.

## What `tungstenite` already does, checked before anything was written

`WebSocketContext::read` answers a peer's `Ping` with a `Pong` itself —
its own doc says so, and `tests/websocket.rs` has watched that pong leave
since step 2. What it does **not** do is keep any record of pings *we*
send: `read` hands an inbound `Pong` straight back out as
`Message::Pong`, solicited or not, and `tungstenite 0.30` has no
outstanding-ping state anywhere. So the frame this code writes is a
`Ping`, and all the bookkeeping that decides whether a pong answered it is
ours — the same division HTTP/3 met with quinn's unset
`keep_alive_interval`.

One thing that is not what the shape suggests, and was read out of the
source rather than assumed: **`write` only buffers a ping.** `_write`
flushes just when it had a pong or close of its own to send, and the
default write buffer is 128 KiB, so a ping written and not flushed never
leaves. The explicit flush is not optional and the `no-flush` mutation
below kills five tests.

## The tests

Six added to `crates/http-ng-native/tests/websocket.rs`, bringing it to
**19**. Each is an A/B against the same fixture with the same
configuration, so what it measures is the difference the *server* made
rather than a number:

| test | what it pins |
|---|---|
| `keep_alive_is_off_by_default_and_pings_only_when_it_is_configured` | the default is off — read back from the socket **and** watched on the wire; a configured socket pings; a pong clears a probe |
| `a_socket_nobody_polls_gets_no_keep_alive` | the ping comes from `poll_next` and nowhere else |
| `a_missed_pong_is_an_error_and_not_the_peer_saying_goodbye` | the kind, the named source, the bound it carries, that the stream stays ended — and a control that closes cleanly and yields `Message::Close` under the same configuration |
| `only_a_pong_with_the_pings_own_payload_answers_it` | a text frame and an unsolicited pong both leave the probe standing, **and exactly one ping was ever sent** |
| `an_inbound_message_resets_the_interval_so_a_busy_socket_never_pings` | the interval half of §7.2, with a silent control proving the same configuration does ping |
| `the_keep_alive_stops_at_our_own_close` | nothing goes out past our close, and the caller-initiated close handshake completes |

**Where a duration is unavoidable it is arranged so the clock can only
make a test hang.** An interval and a deadline *are* durations, so three
of the six wait for one; the negative half of each pair is released by a
*causal* event — three completed ping/pong round trips — rather than by a
sleep. Three rather than one deliberately: `poll_next` sends a second ping
only once the first has been cleared, and only a matching pong clears one,
so a client that ignored pongs could never reach ping two at any speed.
The one ratio a slow machine could genuinely upset is
`an_inbound_message_resets_the_interval_…`'s — a 1 s interval against
messages every 150 ms, **6.7×** — and it says so where the numbers are
chosen. `the_keep_alive_stops_at_our_own_close` has the best shape of the
three: correct code writes nothing past a close *at any speed*, so its
ten-interval margin affects only whether the defect is caught.

## Mutations

Anchor verified before every run: 19 tests, all passing. Each mutation
applied alone to `crates/http-ng-native/src/`, then
`cargo nextest run -p http-ng-native --features websocket --test
websocket`, then the tree restored from git.

| mutation | verdict | killed by |
|---|---|---|
| the ping is never sent (the probe is still recorded) | KILLED | five of the six — all but `the_keep_alive_stops_at_our_own_close`, whose whole assertion is that no ping goes out |
| the ping is written but never flushed | KILLED | the same five, and for the same reason the sixth is not among them |
| the pong deadline never fires | KILLED | `a_missed_pong_is_an_error_and_not_the_peer_saying_goodbye`, `only_a_pong_with_the_pings_own_payload_answers_it` (both by hanging into `BOUND`) |
| a missed pong reported as `Ok(Message::Close(None))` rather than an error | KILLED | `a_missed_pong_…`, on the assertion that names the confusion, and `only_a_pong_…` |
| the interval resets **only** on a pong | KILLED | `an_inbound_message_resets_the_interval_so_a_busy_socket_never_pings` |
| **any inbound frame clears the probe as well as the interval** | **SURVIVED, then KILLED** | `only_a_pong_…`, after the ping count was added — see below |
| any pong answers a probe, whatever payload it carries | KILLED | `only_a_pong_…` |
| keep-alive on by default | KILLED | `keep_alive_is_off_by_default_…`, at the accessor; **and separately at the wire**, verified by re-running the mutation with the accessor assertion deleted, where the failure is "a socket with no keep-alive configured sent opcode 0x9" |
| **a ping is sent past our own close** | **SURVIVED, then KILLED** | `the_keep_alive_stops_at_our_own_close`, written for it — see below |
| the retry-flush of a ping the socket refused is removed | **SURVIVED** | — recorded below |

### The survivor that mattered, and what it says about the first version of the test

**"Any inbound frame clears the probe" passed all eighteen tests as they
first stood, and that is the interesting result of this work.**
`only_a_pong_with_the_pings_own_payload_answers_it` asserted that the
stream failed with `PongNotReceived`, at `ErrorKind::Body`, carrying
`within` — and every one of those held under the mutant, because the
frame cleared the probe, the interval simply restarted, a *second* ping
went out and *that* one died unanswered about 100 ms later. Same error,
same kind, same bound, different fact.

The fixture was **not** adjusted. The server now counts pings and the test
asserts exactly one, which the correct implementation cannot exceed: only
a matching pong clears a probe, and a stream that has ended never sends
anything again. That is the difference between "an error happened" and
"this probe was not answered", and it is the shape of assertion this
whole area needs.

**"A ping past our own close" survived for a duller reason** — no test
closed from the caller's side at all, which
`docs/v03-acceptance.md` had already recorded as a gap for `poll_close`.
`the_keep_alive_stops_at_our_own_close` was written for it and closes both
holes at once.

### The one that is still standing

**The retry-flush removed — SURVIVED, and it is recorded rather than
papered over.** After `ctx.write` has formatted a ping into
`tungstenite`'s `out_buffer`, a socket that refuses the bytes leaves it
there; `read` flushes only what *it* queued (`additional_send` /
`unflushed_additional`), never a frame this crate wrote, so without the
retry at the top of `poll_next`'s loop the ping would sit in the buffer
until something else flushed — and the deadline would then kill a
connection whose peer was perfectly healthy.

No test provokes it, because provoking it needs a caller that
`start_send`s a message larger than the socket buffer, does **not** flush
it, and then polls the `Stream` while the peer stops reading and starts
again only after a ping has been queued behind it. That is a legal caller
and an unusual one. The retry is kept because it makes the failure
structurally impossible rather than merely unobserved — the same reason
the control-frame loop in `poll_next` was kept when its mutation survived
— and this is the same shape as the already-recorded "a queued pong can be
stranded by a full write buffer".

## Deliberately not done

- **No second knob.** There is no maximum number of unanswered pings, no
  jitter on the interval, and no way to set the ping payload. Each would
  be a field whose caller decision nobody has stated, which is the
  reasoning this project rejects everywhere else.
- **Nothing on the seam, and nothing in `Capabilities`.** A knob on
  `WebSocketConnect` would be one `http-ng-fetch` could not honour, and a
  capability would be a field nothing branches on. Asking a browser for
  this does not compile, which is the whole of §7's placement argument.
- **`Timeouts` is still not read past `connect`.** `first_byte`, `total`
  and `between_bytes` have no meaning on a channel with no response body,
  and §7's first decision says why a gap bound in particular would be
  wrong here.
- **No `Ping`/`Pong` a caller can send.** Unchanged from step 2, and the
  keep-alive is deliberately not a way in: the frames it writes are never
  visible on the `Stream`.

## What this leaves unverified

- **The retry of a ping the socket refused has no test** — the surviving
  mutation above, with the caller shape that would provoke it written
  down.
- **The closing handshake is still unbounded.** The keep-alive stops at
  our own close, so a peer that vanishes *after* we have closed leaves
  `poll_close`'s successor — the `Stream` waiting for the echo — with no
  bound at all. `the_keep_alive_stops_at_our_own_close` pins that nothing
  goes out in that window; nothing pins what happens if the echo never
  comes, and a caller who needs a bound there still races the future
  itself.
- **`within` is measured against silence, not against a round trip.** The
  deadline sleep is polled only when the read side is empty, so a peer
  that keeps sending data while never answering the probe is not detected
  until it also goes quiet. That is the deliberate design above; what has
  not been measured is how long "also goes quiet" can be in practice on a
  chatty connection.
- **No `wss://` keep-alive test.** Every fixture here is a plain
  `TcpListener`, the same gap step 2 recorded; the ping goes through the
  same `Sink`/`Shim` path whatever the IO is, and no test says so.
- ~~**Nothing has been run against a real WebSocket server**~~, **but no
  real implementation's pong behaviour has ever answered one of these
  pings.** The client has met a real server now — Autobahn, 517 cases,
  last section — and that server both sends pings and answers them. What
  it has never answered is a *keep-alive* ping, because the keep-alive is
  off by default and the Autobahn driver leaves it off: turning it on
  would make the driver a thing that interprets its own run. So the
  half of this gap that was about `Ping`/`Pong` on the wire is closed and
  the half about `Liveness`'s own probes is not.
- **`u64` sequence numbers wrap at `u64::MAX` pings and nothing tests
  it.** `wrapping_add` is deliberate rather than accidental; at one ping
  a nanosecond it is 584 years, so this is recorded for completeness
  rather than as a risk.


# v0.3 W4 — the WebSocket client against the Autobahn TestSuite

Everything above about WebSocket was checked by fixtures written in this
repository, beside the implementation they observe. That is the exact
arrangement in which a fixture agrees with a bug, and the four sections
before this one say so in four different ways. This section is the
external oracle: **`crossbario/autobahn-testsuite` in `fuzzingserver`
mode — 517 client cases, none of them written here.**

`just test-autobahn` starts the container, drives every case, and reads
the suite's own `index.json`. The CI job `autobahn` runs the same recipe
with `HTTP_NG_REQUIRE_DOCKER=1`, which turns "no Docker" from a skip into
a failure — the shape `HTTP_NG_REQUIRE_WASMTIME` and
`HTTP_NG_REQUIRE_TUNTAP` already have. It is **not** in `just test`: that
is the everyday recipe and it must not need Docker.

Measured on this host, image label `25.10.1`, digest
`sha256:519915fb…fac3074`, debug build, loopback:

## The table

| section | what it tests | ok | informational | non-strict | unimplemented | **fail** |
|---|---|---|---|---|---|---|
| 1 | framing: text and binary, empty to 65535 bytes | 16 | 0 | 0 | 0 | **0** |
| 2 | ping/pong | 11 | 0 | 0 | 0 | **0** |
| 3 | reserved bits | 7 | 0 | 0 | 0 | **0** |
| 4 | reserved opcodes | 10 | 0 | 0 | 0 | **0** |
| 5 | fragmentation | 20 | 0 | 0 | 0 | **0** |
| 6 | UTF-8 handling | 143 | 0 | 2 | 0 | **0** |
| 7 | close handling | 34 | 3 | 0 | 0 | **0** |
| 9 | limits and performance, to 16 MiB | 54 | 0 | 0 | 0 | **0** |
| 10 | misc (auto-fragmentation) | 1 | 0 | 0 | 0 | **0** |
| 12 | permessage-deflate | 0 | 0 | 0 | 90 | **0** |
| 13 | permessage-deflate, parameter space | 0 | 0 | 0 | 126 | **0** |
| **total** | | **296** | **3** | **2** | **216** | **0** |

Nothing scored `FAILED`, `WRONG CODE`, `UNCLEAN` or `NO CLOSE`. The
16 MiB cases are real: the suite reports `Received text message of length
16777216` and `Received binary message of length 16777216`, echoed and
compared, so `9.1.6`/`9.2.6` move 32 MiB between them.

**The 216 UNIMPLEMENTED are permessage-deflate and nothing else**, which
is the one absence this document already recorded and
`docs/w4-upgrade-seam.md` already left open. The client never offers the
extension, so the suite marks the case rather than running it. They are
declared in `scripts/autobahn-report.py`'s `EXPECTED`, with the reason,
and the declaration is checked in *both* directions — a compressed case
that started passing would fail the run as a stale excuse.

**The 3 INFORMATIONAL are the suite's own verdict on itself.** `7.1.6`,
`7.13.1` and `7.13.2` say "actual events are undefined by the spec": a
close code of 5000, and a 256K message sent back to back with a close.
That is a fact about the case, not about this client, so it needs no
declaration.

**The 2 NON-STRICT are ours and are declared.** `6.4.3` and `6.4.4` send
one text frame in three *chops*, with invalid UTF-8 in the middle, and
score strictness by whether the client rejects at the second chop or at
the end of the frame. `tungstenite` decodes a frame when it has all of
it, so this client rejects at the end. `6.4.1` and `6.4.2` split the same
bad message across *frames* and are strict `OK`, which is what makes the
boundary exact: **validation is per frame, not per byte.** The suite's own
expectation names the outcome as acceptable ("If we timeout, we expect the
connection is failed at least then"). Declared in `NON_STRICT`, so a
regression to `OK` and a regression to `FAILED` are both red.

## Two things the suite tolerates that this client does anyway

Neither is a failure, in the suite's judgement or in this one. Both are
facts about the client that only a real oracle would have produced, and
both are written here rather than left in a report directory.

**1. When *we* detect the protocol error, we drop the TCP connection
without sending a close frame — 109 of the 517 cases.** Sections 2 (1),
3 (7), 4 (10), 5 (12), 6 (75) and 7 (4). Autobahn scores every one of
them `behaviorClose: OK`, with `resultClose: "Connection was properly
closed"` and `wasNotCleanReason: "peer dropped the TCP connection without
previous WebSocket closing handshake"` — its expectations for these cases
carry no clean-close requirement, because the case is about detecting the
violation.

RFC 6455 §7.1.7 makes it a **SHOULD**, not a MUST: "the endpoint SHOULD
send a Close frame with an appropriate status code before proceeding to
_Close the WebSocket Connection_". So this is a SHOULD not taken, and the
cost is that a server cannot tell "you sent me something illegal" from
"the network went away".

It is **not fixed here, and the reason is not effort.** The mapping it
needs — `tungstenite::Error::Protocol` to 1002, the UTF-8 error to 1007,
`Capacity` to 1009 — is close-code semantics, which
`docs/w4-upgrade-seam.md` explicitly lists as undecided, and inventing it
in the same commit that first runs an external suite would be answering a
design question with a test run. The suite is the oracle this section was
written to consult, and the oracle does not call it a failure.

Worth knowing about the boundary: the client sends close codes perfectly
well in the *other* direction. Every legal code the server closes with is
echoed back (`7.7.2`–`7.7.13`: 1001, 1003, 1007, 1008, 1009, 1010, 1011,
3000, 3999, 4000, 4999), and a code the server has no right to use is
answered with 1002 (`7.9.1`–`7.9.9`, `7.13.1`, `7.13.2`) — `tungstenite`'s
`do_close` rewrites a disallowed code rather than reflecting it. So the
gap is exactly one path: an error *we* raise while reading.

**2. `WebSocketConfig::max_message_size` is still never reached.** The
largest case is 16 MiB against a 64 MiB ceiling. `max_frame_size` (16 MiB)
is met exactly and not exceeded — `9.1.6` is a single frame of precisely
16777216 bytes, and `tungstenite` compares with `>`, so one byte more
would have been a `Capacity` error. That the frame ceiling is one byte
away from the largest case the suite has is a fact worth knowing before
anyone lowers it.

## What the run cost, and why it is on every push

**15.4 s wall for `just test-autobahn` end to end** — container start,
517 cases, and the parse. The sum of the suite's own per-case durations is
14.9 s, so essentially all of it is the cases, and `6.4.3`/`6.4.4` alone
are 2 s apiece because they are *supposed* to time out. That is a debug
build, with no release pass anywhere.

At that price it belongs on every push rather than on a schedule, which is
where it is.

## The mutations, and the one that survived

No library code was added by this work, so the harness is what was
mutated. Anchor counts were verified before each run: the parser's
`SCENARIOS` list asserts its own length, and every source patch asserts
exactly one occurrence of the text it replaces and aborts otherwise.

`scripts/autobahn-report-selftest.py` builds a 517-case report in which
everything passes, mutates one field, and requires the parser to reject
it — fifteen scenarios, including the two this work was asked for
specifically: a report with **zero cases** and a report with **a
failure**.

Then the parser itself was mutated, nine times, with the self-test run
against each:

| mutation of `scripts/autobahn-report.py` | verdict |
|---|---|
| `MINIMUM_CASES` floor removed | KILLED |
| a `FAILED` verdict counts as good | KILLED |
| `behaviorClose` is not read at all | KILLED |
| the agent key is not checked | KILLED — *after* the change below; it survived first |
| an undeclared `NON-STRICT` is tolerated | KILLED |
| an undeclared `UNIMPLEMENTED`/failure is tolerated | KILLED |
| a stale declaration is tolerated | KILLED |
| a case with no `behavior` field is tolerated | KILLED — *after* the change below; it survived first |
| a missing `index.json` is tolerated | **SURVIVED — equivalent mutant** |

**Three survived on the first run, and the answer was to make the check
stronger rather than the fixture weaker.** All three still exited
non-zero, with a `KeyError` traceback — which in CI reads as broken
infrastructure rather than as a failing check, the same distinction
`scripts/ci-mirrors-just.py` already draws about its own YAML errors. The
self-test now requires `::error::` on stderr, and two of the three die to
it.

**The third is a genuine equivalent mutant and is recorded rather than
killed.** Without the `os.path.isfile` guard, `open()` raises
`FileNotFoundError`, which is an `OSError`, which the very next `except`
catches and turns into the same `die()`. The behaviour is identical — fail
closed, with an `::error::` — and only the wording is worse. Deleting the
better message to make a mutant die would be the wrong trade.

Two mutations were also run against the code the report actually scores:

| mutation | verdict |
|---|---|
| the driver breaks out of its loop on `Message::Close` instead of polling the `Stream` to its end | KILLED — 397 cases turned `UNCLEAN` and the recipe went red with 397 undeclared |
| `Shim::write` returns `buf.len()` instead of the real `n` (the partial-write defect its own doc comment is about) | KILLED — **by a hang, not by a verdict** |

The second is why `test-autobahn` runs the driver under `timeout`. The
suite has no bound for a client that stops mid-message: it scored 222
cases and then both sides waited for ever, so the job would have been
killed by the runner with no report at all rather than going red with one.
The bound is 900 s against a 16 s run, there is no unbounded fallback (a
machine with neither `timeout` nor `gtimeout` is an error), and it was
verified by re-running that same mutation with the bound at 25 s — exit
124, with the message that says a hang is a defect and not a slow machine.

## What this run still does not cover

- **No `wss://`.** Every case is `ws://` on loopback. The TLS path is
  `connect::connect`'s, the same call `execute` makes; the gap steps 1–4
  recorded is unchanged, and now it is unchanged after 517 cases as well.
- **The keep-alive was off**, because turning it on would make the driver
  a thing that interprets its own run. So no real server has ever answered
  one of `Liveness`'s probes. Autobahn's section 2 does exercise ping/pong
  thoroughly — but in the other direction: the server pings, the client
  answers, which is `tungstenite`'s duty rather than this crate's
  bookkeeping.
- **`max_message_size` is untouched**, see above.
- **Permessage-deflate is untested rather than tested-and-absent.** 216
  cases were marked, not run. If the extension is ever implemented those
  216 become the acceptance for it, and the `EXPECTED` entries must go in
  the same commit — which is why a stale declaration is a failure here.
- **One image, one version.** The recipe uses the `latest` tag, so the
  case count can move under it. `MINIMUM_CASES = 517` is a floor: a suite
  that adds cases still passes and goes red about the new cases; a suite
  that *removes* one goes red immediately and has to be re-anchored
  deliberately. The same trade this repository already makes for the
  browser test counts.
- **The suite is an oracle for RFC 6455, not for this seam.** Nothing in
  it observes `Capabilities`, cancellation, the pool, or the fact that a
  WebSocket is never pooled. Those remain this repository's own fixtures'
  job, and the two kinds of evidence do not substitute for each other.


# v0.3 follow-up — a URI's brackets, and the three places that kept them

Found by the live DoH run above (finding 2), which could only see one of
the three sites. **Every `https://[v6-literal]/` URL in this workspace
failed**, and the HTTP/3 path failed in a second way as well.

`http::Uri::host()` returns an IPv6 literal **with its brackets**, because
RFC 3986 §3.2.2 puts `IP-literal = "[" ( IPv6address / IPvFuture ) "]"` in
the *authority*'s grammar rather than in the host's. Everything on the
far side of a URI wants them off, and three sites did not take them off:

| # | site | what it did | how it failed |
|---|---|---|---|
| 1 | `http-ng-native`'s `connect.rs` | `server_name: host` into `TlsRequest` | `Tls: invalid dns name` — `rustls_pki_types::ServerName::try_from` tries `DnsName`, then `IpAddr`, and neither strips a bracket |
| 2 | `http-ng-h3`'s `connect` | `endpoint.connect_with(cfg, addr, &key.host)` | `Connect: invalid server name: [::1]` — quinn makes the same `ServerName` |
| 3 | `http-ng-h3`'s `resolve` | `host.parse::<IpAddr>()` as the literal shortcut | the shortcut misses, the address goes to the resolver, and `getaddrinfo("[::1]")` fails: `Resolve: no address for [::1]` |

All three were measured before being fixed, each from outside the crate.
`http-ng-native` never had defect 3 because `IpLiteralOnly::literal`
strips on the way in; a shortcut in *front* of a resolver has to strip for
itself.

## The decision, which is the part that had no owner

**The caller normalises, not the TLS backend**, and until this work
`TlsRequest::server_name` had no doc comment saying so. That gap is the
whole explanation for three sites getting it wrong independently: each
author had to re-derive whose duty it was, and each guessed the same way.

Two arguments, and the second is the one that decides it:

- **A backend cannot know.** This field is a *name*, not a URI. A caller
  may have built it from a `Host` header, from configuration, or from a
  pinned identity that has nothing to do with the address dialled. A
  backend that stripped defensively would be guessing which of those it
  had.
- **It would be the second place doing it.** The resolvers already strip
  — `IpLiteralOnly::literal` in `http-ng-dns` and `ip_literal` in
  `http-ng-dns-doh`, each with a comment naming this exact trap. Two
  places normalising is how they come to disagree.

So one function: `http_ng_core::bare_host`. **`http-ng-core` is the home
because it is the only crate every consumer already has.** `http-ng-tls`,
whose doc has to state the duty, depends on `http-ng-core` and not on
`http-ng-dns`; putting the function in the DNS crate would make a TLS
server name reach through a resolver for a fact about URI syntax, and
putting it in `http-ng-tls` would do the mirror image to a resolver. It is
also the only crate in a graph that has no resolver at all —
`http-ng-fetch` and `http-ng-wasi` have `http-ng-core` and nothing else
from this list.

The rule generalises, and its other half is written next to the fix:
**what must keep its brackets.** The `Host` header and HTTP/2's
`:authority` are authority syntax (RFC 9110 §7.2), and
`http-ng-h3`'s `PoolKey.host` is matched against the URI a later request
arrives with. Only the step *out* of URI-land strips.

## The tests

Written before the fix and red against it, each observing from outside the
crate: a **real** TLS or QUIC server on a real socket, a certificate with
an IP SAN for `::1`, and the assertion that the handshake completed and
the response arrived. A test that checked `ServerName::try_from` in
isolation would pin rustls's behaviour and say nothing about ours.

- `crates/http-ng-native/tests/tls_server_name.rs` — three rows:
  `https://[::1]:p/`, `https://127.0.0.1:p/`, and `https://localhost:p/`
  through a resolver that answers every name with one address.
- `crates/http-ng-h3/tests/quic_server_name.rs` — the same three, over
  QUIC. **The two v6 rows differ only in their resolver**, and that is
  what separates sites 2 and 3: under `IpLiteralOnly` the shortcut is not
  load-bearing (the resolver strips for us), so that row can only fail on
  the server name; under a resolver that answers nothing, the shortcut is
  the only path to an address at all.
- The named rows are not decoration. A strip that fires unconditionally
  turns `localhost` into `ocalhos` — a perfectly valid DNS name and a
  certificate mismatch, which nothing else here would catch.
- `crates/http-ng-core/src/host.rs`'s own unit tests carry the degenerate
  inputs: `[`, `]`, `[::1`, `::1]`, `[]`, `[[::1]]`.

The v6 rows **skip** on a host with no IPv6 loopback, printing why, the
same shape `an_endpoint_is_bound_in_the_peers_address_family` already had.
On such a host M1-M3 below would survive; that is recorded under
"unverified" rather than worked around.

## Mutations

Seven hand-applied, anchor counts checked before each edit, the **whole
workspace** (1157 tests, ~50 s) run for each so that an unexpected killer
is visible. Script: `crates/http-ng-core/bare-host-mutations.py`. Final
state: **7 anchors matched at 1 place each, 7 killed, 0 survived.**

| mutation | verdict | killed by |
|---|---|---|
| M1 the TLS server name keeps the URI's brackets (site 1) | KILLED | `an_ipv6_literal_authority_completes_the_handshake`, alone |
| M2 the QUIC server name keeps them (site 2) | KILLED | `an_ipv6_literal_authority_reaches_quic_as_a_server_name` and `an_ipv6_literal_authority_never_reaches_the_resolver` |
| M3 the h3 literal shortcut is asked about the bracketed host (site 3) | KILLED | `an_ipv6_literal_authority_never_reaches_the_resolver`, alone — and NOT by the row above it, which is the point of there being two |
| M4 the strip fires on every host (`example.com` -> `xample.co`) | KILLED | `a_named_authority_reaches_tls_unchanged`, `a_named_authority_reaches_quic_unchanged`, `an_ipv4_literal_authority_completes_the_handshake`, `host::tests::*`, and 27 more across `http-ng-h3` |
| M5 only the opening bracket is stripped | KILLED | `host::tests::a_lone_bracket_is_a_host_like_any_other` (`[` alone), plus all three v6 rows |
| M6 brackets are trimmed repeatedly rather than one pair | KILLED | `host::tests::the_brackets_come_off_a_bracketed_host_and_nothing_else` (`[[::1]]`) and `a_lone_bracket_is_a_host_like_any_other` |
| M7 a bracketed empty host is handed back bracketed | KILLED | `host::tests::an_empty_bracketed_host_is_empty` (`[]`), alone |

M3 is the row worth reading twice. It is killed by exactly one test, and
that test exists only because `IpLiteralOnly` hides the defect: with the
obvious resolver in the fixture, site 3 could be removed and the suite
would stay green.

M4's breadth is not a weakness of the mutation but a fact about the
workspace: `127.0.0.1` becomes `27.0.0.` and every h3 fixture stops
resolving. The two `a_named_authority_reaches_*_unchanged` rows are the
ones that would have caught it on their own.

## Live acceptance

Re-run against the endpoint `Doh::pinned`'s own doc offers:
`https://[2606:4700:4700::1111]/dns-query` now answers `cloudflare.com`
with real A records (`104.16.132.229`, `104.16.133.229`, TTL 16 s).
Before the fix the same call returned `Tls: invalid dns name` with the TCP
connection made and no DNS exchanged.

The live test that **pinned the failure** is gone, replaced by
`an_ipv6_literal_endpoint_resolves`, which asserts the success at the same
endpoint. The note on `Doh::pinned` and finding 2's "does not work" text
went with it.

## What this leaves unverified

- **A host with no IPv6 loopback checks nothing here.** Three of the six
  new tests skip, and M1-M3 would survive on such a machine. The rows
  that stay are the named and v4 ones, which cover M4-M7 but say nothing
  about brackets. CI on Linux and macOS has `::1`; a container started
  with IPv6 disabled would not.
- **`http-ng-tls-native-tls` has no test for any of this.** It takes
  `server_name` through the same `TlsRequest` and would be fixed by the
  same caller-side strip, but nothing in this workspace makes it dial an
  IPv6 literal.
- **`IpvFuture` (`[v7.…]`) is stripped and then rejected downstream**
  rather than being understood. That is the same outcome `Uri::host()`
  callers had before, and no test covers it because nothing in this
  workspace can connect to one.
- **The two pre-existing strippers were not collapsed into the new
  function.** `IpLiteralOnly::literal` and `http-ng-dns-doh`'s
  `ip_literal` still carry their own copies. Both are correct and both
  are tested; folding them into `bare_host` is a change to two crates
  outside this task's boundary, and it is the obvious next step for
  whoever owns them.

---

# A v0.3-era defect, fixed in v0.4 W1: `RedirectSupport`

Recorded here rather than in the v0.4 document because the defect is this
vertical's, not the next one's: `http-ng-native` has declared
`RedirectSupport::Configurable` — *"we set the policy"* — since v0.1, while
containing no redirect handling at all. Zero matches for `Location` or a
3xx status in its `src/`; a 3xx comes back from `h1::exchange` as an
ordinary response and `Client`'s redirect stage owns the chain. It declares
`Transparent` now, and `Configurable` and `Inspectable` are deleted from
`RedirectSupport`, leaving three variants: nobody follows, `Client`
follows, the backend follows.

The deletion is `UpgradeSupport`'s precedent (W4 step 4 above, four
variants and zero branches) applied to the same shape. The extra fact that
made it a deletion rather than a documentation fix: **`Configurable` was
unimplementable, not merely unused.** `Client::run` merges the client-level
and per-request `RedirectPolicy` and deliberately does not write the result
into the request's extensions — *"no transport reads a `RedirectPolicy`"* —
so a backend claiming to set the policy could never see one set on the
client. `http-ng-fetch` had shipped the same wrong value and corrected it
to `Internal` in v0.1's audit; that audit did not reach the native crate,
which is why its `redirects_are_internal_not_configurable` still names a
variant that no longer exists. Left as written: it is the record of the
first half of this defect being found, and `http-ng-fetch` was outside the
fixing task's boundary.

## What the mutation run showed, and what it leaves unverified

Anchor 362 tests, `cargo nextest run -p http-ng-native -p http-ng
--all-features`, all green before each mutation. `Native` made to declare
each variant in turn:

| declaration | tests that fail |
|---|---|
| `None` | 1 — `http-ng-native::transport::capabilities_are_honest_about_v01_limits`, a read-back of the field |
| `Transparent` (the mutant, before the fix made it the truth) | 1 — the same read-back, and nothing else |
| `Internal` | 2 — the read-back, and `http-ng::deadline::the_deadline_spans_redirect_hops_rather_than_restarting_on_each`, which dies at `build()` with `UnsupportedCapability { what: "redirect_policy" }` |

Two more on the check rather than on the declaration, both against the
whole workspace (1156 tests):

- **`== Internal` → `!= Transparent`** (reject unless transparent): killed
  by exactly **two** tests, both in `crates/http-ng/tests/redirect.rs` —
  `enforces_the_hop_limit` and
  `redirect_limit_of_zero_sends_only_the_original_request` — because
  `MockTransport` starts from `Capabilities::none()`, whose `redirects` is
  `None`. `config.rs`'s own four unit tests cover `Internal` and
  `Transparent` and none of them catches this.
- **`== Internal` → `== Transparent`** (reject *on* transparent): **22**
  failures, including `config.rs`'s own transparent-is-fine test, because
  `portable_example.rs` and `http-ng-tower`'s round-trip build mocks that
  positively declare `Transparent` rather than leaving the default.

The gap between two and twenty-two is worth reading as a map of where the
suite's attention is: the arm that says "a policy is fine here" is watched
from many directions, the arm that says "and `None` is fine too" by two
tests that never mention redirect support at all.

**So: `Internal` versus not-`Internal` is the only distinction any
behaviour in this workspace can witness**, and `Transparent` has no
behavioural witness at all — neither the new declaration nor the assertion
that pins it. That is structural rather than a gap in the fixture:
`Client`'s redirect stage follows a 3xx whatever this field says, so no
observation from a server can separate `Transparent` from `None`. Nothing
was adjusted to hide it, and it is the reason the variant set had to
shrink: a variant nobody can catch lying must be justified by a carrier,
not by a doc comment.

Also unverified, and named because it would be easy to assume otherwise:
the claim that **`http-ng-urlsession` will not be `Configurable`'s carrier**
rests on Apple's documentation for
`urlSession(_:task:willPerformHTTPRedirection:newRequest:completionHandler:)`
— *"either the value of the `request` parameter, a modified URL request
object, or `NULL` to refuse the redirect and return the body of the
redirect response"*, and *"called only for tasks in default and ephemeral
sessions. Tasks in background sessions automatically follow redirects."* —
and on the absence of any redirect knob on `URLSessionConfiguration`. It is
read documentation, not a running Objective-C program: no Apple hardware
was involved, and W3 should confirm the `nil`-refusal path delivers the 3xx
with its `Location` intact before relying on `Transparent` there.

# v0.4 W2 — full duplex on the HTTP/2 path

`docs/v04-design.md`'s P6: *"`full_duplex: false` on `http-ng-native` is
not a declaration, it is the code."* It was. `http2::exchange` wrote the
whole request body and then awaited the response, so a caller structured
for bidirectional streaming — gRPC's client-streaming and bidirectional
modes, and nothing else in this workspace — deadlocked rather than
degraded.

It is now the h3 shape from `f70bf74`, and the differences from that
template are the interesting part.

## What changed in `exchange`

The write was three locals (`outgoing`, `send_stream`, `pending`) and a
`bool`. It is now a `Pump` value holding those three, and the loop lost
exactly one branch:

```
-                    Poll::Pending => return Poll::Pending,
+                    Poll::Pending => {}
```

That branch *was* the implementation `full_duplex: false` described. With
it gone, a write that cannot proceed falls through to `resp_fut` instead
of stopping it, and a pump still running when the head arrives moves into
`H2Body`, which drives it from `poll_frame`. Nothing is spawned: this
crate has nowhere to spawn (`pool.rs`'s module doc), and a spawned pump
would go on uploading behind a caller that walked away, with nowhere for
its errors to go.

**Not a boxed future, which is where this differs from `http_ng_h3::pump`.**
There the write is an `async fn` and had to be erased — `Pin<Box<dyn
Future + Send>>`, with amendment C2's `Send` bound and the compile-time
check that goes with it. Here the write was already a poll function, so
it stays a struct: no `dyn` lands on the `Client -> Transport` path,
`H2Body` stays generic and unboxed, and no auto trait is cut off anything
above it. There is no `an_h2_body_is_still_send` to write because nothing
could have made it stop being one.

**One branch was removed rather than moved.** The pump's `Poll::Pending
if conn_done` arm is gone: falling through reaches `resp_fut`'s own
`Poll::Pending if conn_done`, which returns the identical
`ConnectionEndedWithTheRequestQueued`. It is strictly better placed —
a response that *did* arrive before the connection ended is now handed
over instead of being replaced by that error.

**Two duties duplex created, neither of which h3 has.**

- `Pump` has a `Drop` that resets an unfinished stream. h2 resets on its
  own — `maybe_cancel`, `h2-0.4.15/src/proto/streams/streams.rs:1601` —
  but only once *every* reference to the stream is gone, and the response
  half holds one. See "what the mutations found" for the honest status of
  this guard: it is not observable today.
- A response that ends while the upload is still in flight takes the
  connection's reuse with it (`H2Body::end`). That case did not exist
  before: the response could not end before the request had been written.
  A stream that was never finished is not the evidence a check-in is made
  of, and it is deliberately easier to lose reuse than to gain it. The
  case is rarer than it sounds — a server that answers without reading
  normally drops its `RecvStream`, which schedules the
  `RST_STREAM(NO_ERROR)` the pump ends on, so reuse survives.

## The order the body polls things in, which is a decision and was measured

`H2Body::poll_frame` polls the **connection first, then the pump**, in a
loop that rounds once when the pump finishes — the same shape as
`exchange`'s.

The first draft had the pump first, on the reasoning that whatever it
queues only reaches the socket from inside `Connection::poll`, so one
`poll_frame` would both write and flush. That is true and it is the lesser
half. `Connection::poll` is also the only thing that **decodes an incoming
`RST_STREAM`**, so with the pump first it asks about a stream state one
poll out of date: a peer that stopped reading was noticed by `recv` in the
same poll that ended the response body, and `Pump::poll`'s gate was never
consulted about it at all.

Measured, and this is what the order is worth: with the pump polled first,
moving the reset tolerance from `poll_reset` to `poll_capacity` — the
placement `c56cbc9` exists to rule out — left **all 21 tests green**. With
the connection first it is killed. The `continue` keeps the flush property
the first draft was after, at a cost of at most one extra iteration.

## The two h3 defect shapes, checked here

**A cancelled upload poisoning a shared connection: structurally absent,
for two independent reasons.**

1. *There is no truncated frame to leave behind.* The h3 defect was
   `quinn::SendStream::drop` calling `finish()` on a stream carrying a
   DATA frame whose length header promised bytes that never came — RFC
   9114 §7.1 makes that `H3_FRAME_ERROR`, a **connection** error. h2
   cannot reach that state: `send_data` hands `prioritize` a complete
   `frame::Data` (`h2-0.4.15/src/proto/streams/prioritize.rs:145-222`), so
   a partly written frame is never something the peer observes.
2. *h2's drop resets rather than finishes.* `maybe_cancel` schedules a
   `RST_STREAM`, with `Reason::CANCEL` for a client
   (`.../streams.rs:1601-1620`).

And the third reason, which is the pool's rather than this file's: an h2
connection is checked out **exclusively**, so a cancelled request has no
neighbour on its connection to damage. That is `pool.rs`'s guarantee and
it is written down in both places already.
`dropping_a_streaming_request_mid_upload_stops_pulling_and_leaves_the_pool_usable`
makes the weaker claim that is checkable from outside: the server sees a
request that did not end, nothing goes on pulling from the caller's body
once the future is dropped, and the next request to the same origin
works.

**A `Rewindable` whose factory returns a `Streaming` sending nothing:
absent, and it never was present.** `body::Inner::from_request_body`
unpacks a `Rewindable` **recursively** — the same conversion the HTTP/1
path uses, written that way in v0.1 with a comment saying exactly why, and
mutation-checked there. `http-ng-h3` had a second, partial copy of the
same conversion (`if let RequestBody::Full(b) = f()`), which is how it
came to differ. `a_rewindable_body_whose_factory_streams_is_actually_sent`
pins it here anyway, because "shared code, therefore correct" is an
argument and the server's byte count is a measurement.

## Mutation testing

Anchor verified before each run: **23 tests** across
`tests/http2.rs`, `tests/http2_duplex.rs` and `tests/stream_reset.rs`,
`--all-features`, all green. Each mutation hand-applied to
`src/http2.rs`, run, reverted.

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1 | the pump writes only the first frame and calls that `Done` | KILLED | 6: `a_streamed_request_body_arrives_whole`, `a_request_body_reaches_the_server_over_http2`, `a_rewindable_body_whose_factory_streams_is_actually_sent`, `a_response_head_arrives_while_the_request_body_is_still_going_out`, `a_connection_whose_upload_never_finished_is_not_pooled`, `a_request_body_that_fails_fails_the_request_with_the_callers_own_error` |
| M2 | the response is awaited only once the body has finished (**duplex silently lost** — the code as it stood before this work) | KILLED | 4, all by hanging into the 30 s ceiling: `a_response_head_arrives_while_the_request_body_is_still_going_out`, `a_stalled_upload_does_not_stall_the_response_body`, `a_reset_while_the_body_drives_the_pump_does_not_discard_the_response`, `a_connection_whose_upload_never_finished_is_not_pooled` |
| M3 | the `poll_reset` gate deleted outright | KILLED | `a_server_that_stops_reading_the_body_still_gets_its_response_read`, `a_reset_while_the_body_drives_the_pump_does_not_discard_the_response`, `a_body_that_ends_just_after_the_peer_stopped_reading_is_not_an_error` |
| M3b | the tolerance moved from `poll_reset` to `poll_capacity` | KILLED | `a_body_that_ends_just_after_the_peer_stopped_reading_is_not_an_error`, **alone**, 5 runs out of 5 |
| M4 | the body never drives the pump | KILLED | `a_response_head_arrives_while_the_request_body_is_still_going_out` |
| M5a | `Pump`'s `Drop` reset removed | **SURVIVED** | — see below |
| M5b | a response that ends over an unfinished upload keeps its reuse | KILLED | `a_connection_whose_upload_never_finished_is_not_pooled` |
| M6 | `drive_pump`'s `Pending` returned as the body's own | KILLED | `a_stalled_upload_does_not_stall_the_response_body`, `a_reset_while_the_body_drives_the_pump_does_not_discard_the_response`, `a_connection_whose_upload_never_finished_is_not_pooled` |
| M7 | every pump error in `exchange` treated as "stop pumping" | KILLED | `a_request_body_that_fails_fails_the_request_with_the_callers_own_error` — a test that did not exist until this mutation survived |
| M8 | `poll_reset`'s `Ready(Err(_))` folded into the tolerance | **SURVIVED** | — see below |

### M3b, and the kill duplex took away

`c56cbc9` recorded that putting the tolerance at `poll_capacity` instead
of `poll_reset` was killed by `a_stalled_streaming_body_does_not_hide_a_
response_the_server_already_sent`, which **hung**: a pump parked on the
caller's body had no waker but `poll_reset`'s, so `exchange` never woke.
Duplex ends that hang — `exchange` stops waiting for the pump and takes
the head — so that test now passes either way. Measured: with the first
version of this work in place, M3b left all 21 tests green.

The discrimination moved to the site the placement is actually about. A
reset stream has **no capacity**, so every *large* body meets a reset at
`poll_capacity` and a tolerance placed there covers it; a body that simply
**ends** while the stream is reset fails at `send_data(Bytes::new(),
true)` with the `UserError::InactiveStreamId` that no public h2 API can
tell from an API misuse of ours. `/answer-and-drop` builds that moment
without a clock: it answers in full, drops the request stream — which is
what schedules the `RST_STREAM` — and only then reports, and the test
waits for that report before closing the caller's body.

### M7, and a claim in the source that was false

`http2.rs`'s doc comment said this widening was killed by
`a_connection_that_dies_mid_request_is_still_an_error`, "on the error's
kind and not on its existence". It was not, and it never was: the mutation
leaves all **14** tests green on the code as it stood *before* this branch
(`fcfd639`), and all **22** after. The cause is structural — the
connection is polled before the pump at both call sites, so a dead
connection is `Connection::poll`'s verdict and the pump's is never
reached.

The case where the pump's verdict *is* reached is a caller's error rather
than a network one: a request body that fails part-way. The widened
version resets the stream and defers to `resp_fut`, which answers with
h2's reset, so an `expect_err` passes either way; what is lost is *whose*
error comes back. The new test asserts the kind and downcasts the source
to a marker type the fixture owns.

### M5a and M8, which survived

**M5a — `Pump`'s `Drop` reset.** Removing it leaves all 23 green, and the
reason is the pool policy rather than the guard. The `RST_STREAM` it
queues can only reach the wire from inside `Connection::poll`, and an h2
connection is checked out **exclusively**, so on every path that drops a
`Pump` the connection is dropped in the same breath. The peer learns from
the socket closing instead — which is what `Capabilities::cancel_on_drop`
promises and what `tests/cancel.rs` already observes from the far end.

It is kept rather than deleted because the property it guards is not this
file's: `pool.rs` records that a build with a spawner could multiplex, and
on that day the connection outlives the stream. h2's own `maybe_cancel`
would not cover it either — the response half's reference keeps the count
above zero. Recorded here rather than claimed as tested.

**M8 — tolerating `poll_reset`'s `Ready(Err(_))`.** Survived — but the
sentence beside it, *"also survived before this branch"*, was wrong, and
the correction matters more than the survivor.

Measured on both trees during the merge review. On `b2289c4`, the commit
before duplex, folding that arm into the tolerance **is killed**, by
`a_connection_that_dies_mid_request_is_still_an_error`. On this tree the
same mutation passes all 173. So **duplex removed a second kill**, not
only the M3b one this branch found and replaced — and the cause is not
the shadowing this paragraph first claimed. Polling the response future
alongside the pump moves where a connection error surfaces: what used to
be distinguishable at the write side now arrives from `resp_fut` looking
the same.

Recorded rather than closed, and recorded with the right reason, because
a survivor explained by "no fixture could reach it" invites nobody to
try, while one explained by "this change took the reach away" says
exactly what a future test would have to restore.

## The tests

Nine new ones in `crates/http-ng-native/tests/http2_duplex.rs`, every
claim read off a real `h2::server` on a real socket.

**Causal, not timed.** `a_response_head_arrives_while_the_request_body_is_
still_going_out` has the caller's body produce its second chunk only after
`send()` has returned a head — the chunk is not in the channel until then
— so a transport that finished the body before reading the head cannot
complete the exchange at any speed. The server's own clock is the second
witness: it records when it sent the head and when the last request byte
arrived, and asserts the first is before the second. `a_stalled_upload_
does_not_stall_the_response_body` needs no clock at all: `/hold` never
reads a byte of the request, so it never enlarges either flow-control
window, and 4 MiB cannot move through 65 535 bytes of credit.

The only `Duration`s in the file are the 30 s ceilings that turn a hang
into a named failure, and the 40 ms per-frame pacing that makes
"mid-upload" a construction rather than a window.

## The capability, deliberately unchanged

`Capabilities::full_duplex` is still `false`, and
`capabilities_report_the_floor_with_the_feature_on` is still green with
the feature compiled in. The floor's reason never was this file's
implementation:

- one `Capabilities` value covers a transport that negotiates HTTP/1.1
  whenever ALPN says so, and `Transport::capabilities` returns a
  **reference** to a value fixed at construction, so there is nowhere for
  a per-connection answer to live in it;
- Cargo unifies features across a graph, so a library built on `http-ng`
  cannot know whether some other crate turned `http2` on;
- over-claiming it costs a caller a deadlock rather than a degradation.

`tests/http2.rs`'s doc comment on that test used to add "and it is not
merely a declaration here: `exchange` writes the whole request body
first". That half has expired and is corrected in place.

## What a caller could truthfully be told, and what it would cost

`docs/v04-design.md` §W2 deliverable 2 asks whether the honest answer is
per-response or per-connection. **This is an investigation, not an
implementation** — nothing below is built, and `Capabilities` is
untouched.

What the code can already offer, measured in this tree:

- **`Response::version()` already answers "was h2 negotiated", per
  response.** `version_reported` is `true` and the value is set by the
  `h2` crate while decoding a real HEADERS frame. This is the whole of
  the precedent the design document cites, and it is already shipped.
- **A per-response extension would reach the caller.** `Client::execute`
  does `resp.into_parts()` and `from_parts(parts, ..)`
  (`crates/http-ng/src/client.rs:558,579`) and wraps only the body, so a
  typed value inserted into the response's extensions by
  `Native::execute` survives the redirect loop, the deadline and the
  decompressor untouched. Nothing needs a new seam.
- **The pool key already carries the protocol.** `PoolKey { security,
  host, port, protocol }` and `Native::pooled_candidates` returning
  `[H2, Http11]` or `[Http11]` mean "a connection that speaks h2" is
  already an expressible request of the pool, with no new storage.
- **The protocol is known before the head goes out.**
  `negotiated_protocol(alpn, offered_h2)` runs after `connect::connect`
  and before `handshake_for`/`established::exchange`; for a pooled
  connection it is the key the connection was taken from. So a refusal
  could arrive before anything is sent.
- **`Capabilities::version_select` exists, is `false` everywhere, and
  nothing reads it.** `grep` across the workspace finds declarations and
  assertions only — no branch anywhere, which by v0.2's rule (*a variant
  exists only if a caller decision turns on it*) makes it the same shape
  `docs/v04-design.md`'s P5 catches `RedirectSupport::Configurable` in.

The three shapes, and what each is good for:

1. **Per-response.** A `Negotiated` value in the response extensions
   naming the protocol and what it enabled. Cheap, honest, survives
   `Client`. **And useless for `full_duplex`**, which is the capability
   that motivated the question: a caller structured for bidirectional
   streaming has to decide *before* it sends the head, and this answers
   after. It is adequate for a capability whose consumer reads it after
   the fact.
2. **Per-connection.** There is no connection handle in the public API —
   `Transport::execute` hands back a `Response`, not a connection — so
   this means either a new seam or a per-origin query answered from the
   pool. The second is answerable and **racy in a way that matters**: a
   pooled h2 entry can be evicted, idle-timed-out or closed between the
   answer and the request, and the next connection may negotiate
   HTTP/1.1. It would be a fact about the past presented as a promise
   about the next request.
3. **Per-request demand, which is what I would investigate first.** The
   caller marks a request "this needs full duplex" (or a minimum
   version); the transport checks it at the moment the protocol is known
   and before the head is written, and refuses with a typed
   `Unsupported` error otherwise. It costs no new capability field and no
   new seam: `AllowEarlyData` is exactly this shape already — a
   per-request extension read by a transport, gated on a correctness
   condition, and part of the pool key so that a marked request cannot be
   served by a connection that does not qualify. `version_select` is the
   field that would finally have a caller decision behind it.

   Its cost, stated: it turns an ALPN outcome into a request failure. For
   gRPC that is right — client-streaming against HTTP/1.1 is not a slow
   path, it is a hang. For a browser-shaped client it is wrong, which is
   why it must be per request rather than per client, which it is.

**One thing in the gRPC table needs no mechanism at all.** P7 says the h2
path already handles trailers, and `response_trailers: false` is the
HTTP/1.1 floor rather than the ceiling. Read further: `H2Body::poll_frame`
yields them as an `http_body::Frame::trailers`, and `Decompressed` passes
a non-data frame through untouched (`decompress.rs:583`, with a comment
saying so). So a caller on an h2 connection **already receives response
trailers today**, capability or no capability — `grpc-status` included.
The `false` under-promises rather than blocks, which is the safe
direction and the one the floor rule intends.

## Deliberately not done

- **No capability change of any kind.** Not `full_duplex`, not
  `response_trailers`, not `version_select`. The floor is right for a
  static answer and the honest per-request answer is a design decision
  rather than a mechanical one.
- **`request_trailers` is still `false` and still not enforced on this
  path.** `http-ng-h3` refuses a trailers frame by name
  (`RequestTrailersNotSent`) because it declares `false` and a streaming
  body can now produce one. `http2.rs` *sends* them —
  `Pump::poll`'s trailers arm calls `send_trailers` — while
  `Capabilities::request_trailers` reads `false`. That mismatch predates
  this work and is not a silent drop (the trailers do go out), but it is
  a capability that understates the code, and the h3 crate made the
  opposite choice for the same field. It wants one decision covering both.
- **No second `poll_reset` site**, and no `Display`-string matching
  anywhere. `c56cbc9`'s one-question-asked-once is unchanged; only the
  order in which the body asks its two questions moved.

## What this leaves unverified

- **M5a and M8 survive** — see the mutation section. Neither is reachable
  by a black-box fixture in this workspace, and both are recorded here
  rather than papered over.
- **Nothing exercises duplex over real TLS.** All three h2 test files use
  a `TlsConnect` stub that reports `h2` and encrypts nothing. The bytes on
  the wire are real HTTP/2 and every test asserts `Response::version()`,
  but rustls's own ALPN negotiation is `http-ng-tls-rustls`'s business and
  is not re-tested here.
- **No second h2 implementation has seen this.** The server in every test
  is the same `h2` crate as the client, so a shared misreading of RFC 9113
  would be invisible — the same gap `http-ng-h3` closed by adding a second
  UDP implementation, and it is open here.
- **The `Timeouts` interaction with a duplex upload is untested.**
  `first_byte` bounds "the request being handed to a connection to the
  response head being in hand — writing the request included", and with
  duplex the head can now arrive while the writing continues. The bound
  is unchanged and still ends at the head, so it is if anything easier to
  meet; `between_bytes`, held inside `IdleTimeout` around the response
  body, now shares its polls with a pump that can go `Pending` for
  minutes. Neither combination has a test.
- **Concurrency is untested because it is impossible here.** One stream
  per connection is the pool's policy, so "two duplex uploads on one
  connection" has no fixture and no meaning today.

---

# v0.4 W2 — a per-request version demand, and one measurement that re-opened a decision

Two pieces of `docs/v04-design.md`'s appendices. Appendix A is built;
Appendix B turned out to rest on a premise that measurement contradicted,
so it is reported rather than implemented.

## A. `RequireVersion`, and where the check lives

The type is `http_ng_core::RequireVersion(pub http::Version)`, next to
`AllowEarlyData` in `caps.rs` and for the same reason: transports read it
out of the request's `http::Extensions` and do not depend on `http-ng`. It
is that mark with the polarity reversed — an *allow* becomes a *require* —
and the reversal is why it stays per request rather than becoming a client
setting. Turning an ALPN outcome into a request failure is correct for
gRPC, whose RPC cannot proceed over HTTP/1.1 at all, and wrong for a
browser-shaped client that should degrade quietly; only the caller of one
request knows which of the two it is.

The refusal is `http_ng_core::VersionNotAvailable { required, negotiated }`
under `ErrorKind::Unsupported`, and the comparison is one function,
`http_ng_core::check_version`, so the rule cannot drift between the two
transports that enforce it.

**Exact match, in both directions, deliberately.** There is no ordering
under which "at least HTTP/2" means anything useful: a caller who needs h2
framing does not want HTTP/3 instead, and a caller who needs HTTP/1.1 —
to keep an upgrade path open — wants strictly *less* than HTTP/2, which a
minimum reading cannot express at all. The mutation that turns `!=` into
`>` is killed by four tests (M10 below).

### `http-ng-native` — three call sites, one decision

| where | what it does | why there |
|---|---|---|
| the pooled-candidate loop, first statement | **filters**: a pooled HTTP/1.1 connection under a demand for h2 is skipped, not failed | a fresh connection may still negotiate h2, and skipping is what makes the refusal rare rather than the normal outcome for any origin already spoken to |
| the `offered_h2` computation | **narrows** the ALPN offer to protocols the demand admits | without it the h1 direction of the demand is unsatisfiable against any h2-capable server: the client would propose `h2`, the server would take it, and the request would fail on a connection the client itself chose to make wrong |
| immediately after `negotiated_protocol`, before `handshake_for` | **refuses** with `VersionNotAvailable` | this is the first instant the protocol is known, and it is before `handshake_for` (which for h2 writes the connection preface) and before `established::exchange` (which writes the head) |

So a refused demand costs the server a TCP connection and a TLS handshake
and **not one byte of HTTP**. The narrowing is why the third row is
reached at all rather than dead: what survives it is the case the client
does not control — a server that selects something else, or a TLS backend
that reports one.

`spoken_version(protocol)` derives the version from `is_h2`, the same
predicate `handshake_for` branches on, so the two cannot answer
differently about one connection. An ALPN this transport does not speak
(`negotiated_protocol` → `None`) is `HTTP_11`, which is not a fallback of
convenience: v0.2 W2's standing answer to an unknown ALPN is to speak
HTTP/1.1 on that one connection and keep it out of the pool, so HTTP/1.1
is the truth about what the next bytes will be.

### What each backend does with a demand it cannot meet

| backend | `version_select` | `RequireVersion(HTTP_3)` | `RequireVersion(HTTP_2)` | `RequireVersion(HTTP_11)` |
|---|---|---|---|---|
| `http-ng-native` | **`true`** (its first `true` anywhere) | `VersionNotAvailable`, nothing written | served if ALPN gave `h2`, else `VersionNotAvailable` with nothing written | served; `h2` removed from the offer so it usually can be |
| `http-ng-h3` | **`true`** | served | `VersionNotAvailable`, **before resolution** | `VersionNotAvailable`, before resolution |
| `http-ng-fetch` | `false`, unchanged | `UnsupportedCapability` from `Client` | same | same |
| `http-ng-wasi` | `false`, unchanged | `UnsupportedCapability` from `Client` | same | same |

**`http-ng-h3` reports `true`, and that is the decision worth reading
twice.** It speaks exactly one version and *chooses* nothing — but the
field says whether a demand is **honoured**, not whether a version is
selected. `RequireVersion(HTTP_3)` is satisfied by construction, and a
`false` would have `Client`'s gate refuse the one demand this transport
meets without doing anything at all. Its check is the first statement of
`execute`, before the scheme check and before resolution, because the
answer is a pure function of the request there.

`http-ng-fetch` and `http-ng-wasi` keep `false` and neither file was
touched. Neither selects the protocol version *nor learns it* — both also
report `version_reported: false` — so there is no moment at which either
could compare a demand against anything, and refusing is the only honest
answer.

### The `Client` gate, and the two boundaries

`config::check_version_demand_supported` is `check_redirect_supported`'s
shape down to the `what` string (`"require_version"`), called from
`Client::run` beside the timeouts and the redirect policy. There is no
client-level half to merge, on purpose, and `build()` deliberately still
succeeds for a backend that cannot honour demands: a browser client must
go on working for every caller who never mentions a version.

**The demand crosses an origin, where `AllowEarlyData` does not**, and the
asymmetry is a decision rather than an omission from `next_hop`'s strip
list. `AllowEarlyData` says "replaying this is safe", a claim about what a
request does *at a server*, which a caller who addressed origin `a` never
made about `b`. `RequireVersion` is a statement about the caller's own
code — "the thing I am about to do needs this protocol" — equally true at
hop 4 as at hop 1. Dropping it would let a `302` deliver over HTTP/1.1
precisely the request that said it could not use HTTP/1.1: the failure the
mark exists to prevent, arriving through the one door left open.

**The static floor is not widened and cannot be.** `full_duplex` stays
`false`, `capabilities_report_the_floor_with_the_feature_on` is untouched
and green, and a request that does not carry the mark is unaffected by
every line of this. The demand is how a caller converts "the floor says
no" into "this connection says yes", per request.

## B. `request_trailers` — the measurement re-opened the question

Appendix B proposed making `http-ng-native` enforce its
`request_trailers: false` the way `http-ng-h3` does, **unless the h1 path
turned out to send them too**. It does.

Measured on a raw socket, plaintext `http://` so no ALPN and therefore
HTTP/1.1 with certainty — `crates/http-ng-native/tests/request_trailers.rs`.
An ordinary `Native`, a streaming body whose second frame is a trailers
frame, and a `Trailer: grpc-status` request header:

```
POST / HTTP/1.1
trailer: grpc-status
host: 127.0.0.1:36101
transfer-encoding: chunked

4
AAAA
0
grpc-status: 0

```

So the field has **two carriers, not one**, and "make native refuse what
h3 refuses" would have deleted a working HTTP/1.1 feature rather than
closed a gap. Per the brief's own condition, this is reported rather than
proceeded with, and no enforcement was added.

**The condition is RFC 9110 §6.6.1's, not ours.** hyper encodes request
trailers only for fields the request declared in a `Trailer:` header —
`proto/h1/encode.rs`'s `Kind::Chunked(Some(..))` arm, reached from
`role.rs`'s client encoder when `TRAILER` is present. That is a sensible
rule for a hop-by-hop-sensitive feature: an intermediary deciding whether
to buffer needs to know before the body starts.

**And it leaves a third state that neither `true` nor `false` describes.**
With no `Trailer:` header the same trailers are dropped silently — hyper
logs at `debug!` and returns `None` — and the request succeeds with a
`200`. That is not h3's behaviour (a typed `RequestTrailersNotSent`) and
not what `true` would promise (they go out). Both halves are pinned, so
whoever changes the declaration has to answer for this case rather than
discover it.

What the three protocols now do with a request-body trailers frame:

| path | declared | actual |
|---|---|---|
| `http-ng-native` h1 | `false` | **sent**, if declared in `Trailer:`; **dropped silently** if not |
| `http-ng-native` h2 | `false` | **sent**, unconditionally (`Pump::poll` calls `send_trailers`) |
| `http-ng-h3` | `false` | **refused**, typed `RequestTrailersNotSent` |

Three behaviours under one `false`. The decision Appendix B wanted is
still owed; it is now owed with three rows rather than two, and the
`false` is an understatement on two paths and an enforcement on the third.

## Mutation testing

Anchor verified before each run: **469 tests**,
`cargo nextest run --no-fail-fast -p http-ng-core -p http-ng-native -p http-ng-h3 -p http-ng --all-features`,
all passing on the unmutated tree. Killers read, not counted — two rows
below changed their verdict on a re-run and both changes are recorded.

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1a | `check_version` never refuses (the demand read and ignored, everywhere at once) | **killed**, 7 | `core::a_demand_the_connection_misses_is_a_typed_unsupported`, `core::a_newer_version_does_not_satisfy_a_demand_for_an_older_one`, `native::http2::a_demand_for_http1_takes_h2_off_the_alpn_offer`, `native::require_version::{a_demand_for_http2_is_refused_before_a_single_byte_is_written, a_pooled_connection_of_the_wrong_version_is_skipped_not_used}`, `h3::require_version::{a_demand_this_transport_cannot_meet_is_refused_before_the_network, a_refused_demand_reaches_the_server_as_nothing_at_all}` |
| M1b | native's fresh-connection check deleted | **killed**, 2 | `native::require_version::a_demand_for_http2_is_refused_before_a_single_byte_is_written`, `…::a_pooled_connection_of_the_wrong_version_is_skipped_not_used` |
| M1c | native's pool-candidate filter deleted | **survived, then killed** — see below | `native::require_version::a_pooled_connection_of_the_wrong_version_is_skipped_not_used` |
| M2 | the demand checked **after** `established::exchange` rather than before `handshake_for` | **killed**, 2 | `native::require_version::a_demand_for_http2_is_refused_before_a_single_byte_is_written`, `…::a_pooled_connection_of_the_wrong_version_is_skipped_not_used` |
| M3 | `version_select` left `false` on `http-ng-native` | **killed**, 6 | `native::transport::capabilities_are_honest_about_v01_limits`, `native::http2::{a_demand_for_http1_takes_h2_off_the_alpn_offer, a_demand_for_http2_is_served_by_a_connection_that_negotiated_it}`, `native::require_version::{a_demand_the_connection_meets_is_served_normally, a_demand_for_http2_is_refused_before_a_single_byte_is_written, a_pooled_connection_of_the_wrong_version_is_skipped_not_used}` |
| M3b | `version_select` left `false` on `http-ng-h3` | **killed by a read-back only, then by behaviour** — see below | `h3::require_version::{version_select_is_declared_and_the_tests_above_are_why, a_client_over_this_transport_does_not_refuse_a_demand_for_http3}` |
| M4 | the refusal raised for a demand the connection **does** satisfy | **killed**, 5 | `core::a_demand_the_connection_meets_passes`, `native::http2::a_demand_for_http2_is_served_by_a_connection_that_negotiated_it`, `native::require_version::a_demand_the_connection_meets_is_served_normally`, `h3::require_version::{a_demand_for_http3_is_served, a_client_over_this_transport_does_not_refuse_a_demand_for_http3}` |
| M6 | the ALPN narrowing removed (`h2` offered against a demand for HTTP/1.1) | **killed**, 1 | `native::http2::a_demand_for_http1_takes_h2_off_the_alpn_offer` |
| M7 | the demand stripped on a cross-origin hop, next to `AllowEarlyData` | **killed**, 1 | `http-ng::require_version::the_demand_survives_a_cross_origin_redirect` |
| M8 | the `Client` gate reads the capability without the mark | **killed**, 112 | `http-ng::require_version::an_unmarked_request_is_unaffected_by_a_backend_that_cannot_select` and every other mock-based test in `http-ng` — `Capabilities::none()` is `version_select: false`, so the mutant refuses every request in the crate |
| M9 | `http-ng-h3`'s check deleted | **killed**, 2 | `h3::require_version::{a_demand_this_transport_cannot_meet_is_refused_before_the_network, a_refused_demand_reaches_the_server_as_nothing_at_all}` |
| M10 | exact match becomes "at least" (`!=` → `>`) | **killed**, 4 | `core::a_newer_version_does_not_satisfy_a_demand_for_an_older_one`, `native::http2::a_demand_for_http1_takes_h2_off_the_alpn_offer`, `h3::require_version::{a_demand_this_transport_cannot_meet_is_refused_before_the_network, a_refused_demand_reaches_the_server_as_nothing_at_all}` |
| M11 | the trailer measurement never sends `Trailer:` | **killed**, 1 | `native::request_trailers::sends_request_trailers_on_http1_when_the_caller_declares_them` |
| M12 | the trailer measurement always sends `Trailer:` | **killed**, 1 | `native::request_trailers::drops_undeclared_request_trailers_on_http1_without_telling_anyone` |

**"Native's trailer enforcement removed" has no row, and the reason is
section B**: there is no enforcement to remove. M11 and M12 stand in its
place — they are what establishes that the two measurement tests
distinguish the two behaviours rather than both passing on one, which is
the only thing left to be wrong about once the decision is deferred.

### The two rows that changed verdict, and what they cost

**M1c survived first, and the defect was in the fixture rather than in the
mutation.** `a_pooled_connection_of_the_wrong_version_is_skipped_not_used`
passed with the pool filter and without it. The fixture answered a request
and then `break`, closing the socket — so there was never a connection in
the pool, the client's second request opened a fresh one either way, and
`accepted == 2` held for the wrong reason. The comment beside that `break`
read *"keep reading so the loop still ends on the client's own close"*,
describing something the code did not do. The server is keep-alive now
(`Content-Length` framing, no `Connection: close`, no `break`), reporting
once per connection, and M1c dies.

This is worth naming as the class rather than the instance: **a test whose
subject is the pool, which never reached the pool, passing green with the
code under test deleted.** Nothing but running the mutation would have
found it.

**M3b killed only a read-back at first.** Flipping `http-ng-h3`'s
`version_select` to `false` failed exactly one test — the one that asserts
the field and nothing about what the field does. Every other test in that
file calls `Transport::execute` directly, so `Client`'s
`UnsupportedCapability` gate never ran in any of them, and the *behaviour*
the crate's doc comments promise (`false` would refuse
`RequireVersion(HTTP_3)`) had no test under it at all.
`a_client_over_this_transport_does_not_refuse_a_demand_for_http3` is that
test, and M3b now dies twice.

## Deliberately not done

- **`request_trailers` is unchanged, in both crates**, and section B is
  why: the premise Appendix B's fix rested on is false, so the decision
  goes back rather than forward. Nothing about the declaration, the h3
  enforcement or the h2 `send_trailers` moved.
- **The silent drop of undeclared h1 trailers is left standing**, pinned
  rather than fixed. It is the same decision: three behaviours under one
  field wants one decision, and taking a third of it here would be the
  mistake this whole appendix is about.
- **No `RequestBuilder::extension` setter.** A demand goes on through
  `Client::execute` with an `http::Request`, exactly as `AllowEarlyData`
  does today. Adding an ergonomic setter for one of the two marks and not
  the other would be arbitrary, and adding it for both is a facade
  question rather than a W2 one.
- **`Transport`'s shape is untouched.** The brief made this a stopping
  condition; it was not reached, for the same reason `AllowEarlyData`
  never reached it.

## What this leaves unverified

- **The narrowing is measured against a TLS stub, not against rustls.**
  `a_demand_for_http1_takes_h2_off_the_alpn_offer` asserts on the ALPN
  list `FakeTls` records, which is exactly the input `Native`'s decision
  is made from — but that rustls sends that list unchanged and reports
  back what a real server picked is `http-ng-tls-rustls`'s business and
  is not re-tested here. The same boundary every other test in
  `tests/http2.rs` sits on.
- **No demand has been tried against a real h2 server over real TLS**, so
  the case where a server *declines* `http/1.1` and picks `h2` against a
  demand for HTTP/1.1 — the one the third native call site exists for —
  is exercised only through the stub reporting `h2`. The behaviour is the
  same either way (the stub's report is what `negotiated_protocol` reads)
  but the negotiation is simulated.
- **`http-ng-fetch` and `http-ng-wasi` are argued, not exercised.** Their
  `version_select: false` is unchanged and their refusal comes from
  `Client`'s gate, which is tested against a mock reporting the same
  `false`. No browser or `wasmtime` run has been made with a demand on a
  request. The claim being made about them is the mock's behaviour plus
  the fact that their field is `false`, and that is all.
- **Nothing checks what a demand does to a `425` replay.**
  `Client::run`'s `425` branch strips `AllowEarlyData` from the retry and
  leaves everything else; a `RequireVersion` therefore survives into the
  replay, which is believed correct for the same reason it survives a
  redirect, and has no test.
- **The interaction with `Timeouts` is untested.** A refusal happens after
  `connect::connect` has run, so a `connect` bound applies to a request
  that is then refused. That is the intended reading — the connection was
  genuinely made — but no test pins it.

---

# v0.4 W2 — event hooks, and P13 settled by construction

The original spec listed observability under v0.3 and it never happened,
so until this the answer to *"why was that request slow"* was "read the
source". `docs/v04-design.md` §W2 deliverable 3 asks for hooks, and puts
one thing before the API: **P13 — can an observability hook avoid a
`Send` bound?**

## P13: yes, and here is the construction

Every seam in this workspace manages without a declared `Send`, but the
two existing `Rc` probes (`Transport` and `WebSocketConnect`, in
`crates/http-ng-core/tests/shape.rs`) both put the `Rc` in something the
request path only ever **borrows**. A hook is a different shape, and the
difference is not cosmetic: a response body outlives
`Transport::execute`, so it cannot borrow the transport — it has to
**hold** the hook and call it from `poll_frame`. If anything on that path
declared `Send`, every single-threaded runtime would be shut out of
observability, which is the same objection that got `hyper::upgrade` and
`hyper/http2` refused here.

`a_non_send_hook_reaches_a_bodys_poll_frame_and_the_transport_still_
implements_transport` is the answer: a `Transport` whose hook counts into
an `Rc<Cell<usize>>`, a body carrying a clone of that hook past the end of
`execute`, and a `Hooks::on` call from inside `poll_frame`. It compiles,
and the test asserts the **call happened** rather than that the file
type-checks — a probe whose body is never polled would pass while proving
nothing about the site the question is about.

The complement is asserted beside it
(`a_send_hook_leaves_the_transport_and_its_body_send`), because a seam
that were unconditionally `!Send` — a `PhantomData<Rc<_>>` inside `Hooks`
itself, say — would satisfy the first test while costing every backend
`tokio::spawn` on its responses. `crates/http-ng-native/tests/shape.rs`
re-checks that direction on the real transport.

Nothing about the answer was free: `H: Clone` and `H: Unpin` are on
`Native`'s two impls, and both are consequences of the same fact. The body
needs a hook of its own rather than a borrow (`Clone`, the same bound and
the same reason `R: Clone` already carries), and `H1Body::poll_frame`
reaches its fields through the safe `Pin<&mut Self> -> &mut Self`
projection this workspace's `forbid(unsafe_code)` leaves as the only one
(`Unpin`). Every hook worth writing is both.

## The event set, derived from the code rather than from the list

The plan named six things. Four are emitted, one is not because this
transport has nothing to put in it, and the vocabulary is
`http_ng_core::unversioned::{Hooks, Event}`.

| event | what it says | why it earns its place |
|---|---|---|
| `Connected` | id, uri, remote address, negotiated version, and `dns`/`tcp`/`tls`/`total` | The connect is the thing a caller cannot see and cannot time. The version is `spoken_version(protocol)` — the same function `handshake_for` branches on, so it cannot claim HTTP/2 for a connection that got an HTTP/1 handshake |
| `Reused` | id, uri, version | **The pool already knew this and threw it away.** Emitted from the branch that took a connection out of the pool, so there is no flag anywhere that could disagree with the behaviour — the `reuse_of(&pool)` discipline applied to an event |
| `Head` | id, uri, status, version, elapsed | `Response::version()` answers after the fact and says nothing about *when*. The pair (`Head::elapsed`, `ConnectTiming::total`) is what separates "the connection was slow" from "the server was slow" |
| `Closed` | id, and one of `Ended` / `Stale` / `Failed(&Error)` | A connection going away underneath a caller is invisible today, and `Stale` in particular is the event that explains a connect the caller did nothing to deserve |

**There is no "request queued", and that is a finding rather than an
omission.** `http-ng-native` has no queue: a request that finds no live
pooled connection dials a fresh one, there is no per-origin connection
limit to wait behind, and an h2 connection is checked out of the pool
*exclusively* — one stream at a time — so `SendRequest::poll_ready` never
waits for a stream of ours. A variant no code can emit is a capability
that lies, which is the defect this workspace has caught four times.
`http-ng-urlsession` (W3) will have a queue, and that is when the variant
should arrive.

**Two of the four were facts the code already computed and discarded**,
which is why they cost no new instrumentation: `negotiated_protocol` runs
before the head is written, and `checkout`'s own control flow is the
difference between reuse and a fresh connect.

## Zero cost when nobody is watching, measured

`Hooks::WATCHING` is an associated **const**, `false` on `NoHooks`. Every
clock read whose only purpose is an event goes through `mark::<H, R>`,
which is `H::WATCHING.then(|| rt.now())` — so on a `NoHooks` build there
is no branch left for the optimiser to remove, there is nothing there at
all. The connection id is gated the same way (`connection_id::<H>`), and
`connect::Attempted` is `Option<Box<_>>` so a request that wants none of
this carries eight bytes rather than eighty through four nested `async
fn`s.

The evidence is `crates/http-ng-native/tests/hooks_cost.rs`, which counts
clock reads on a `Tokio` that keeps a tally of every `now()`:

| build | fresh connection | second request, served from the pool |
|---|---|---|
| `NoHooks` | **1** `now()` | **0** `now()` |
| a hook that does nothing but exist | 4 | 1 |

The numbers are equalities and not bounds, deliberately: a `<=` would pass
for a build that read the clock four times when it should read none. The
zero is the interesting one — a pooled request under `NoHooks` reads the
clock **not at all** — and the `1` is not the hook's either: it is
`connect::drive`'s Happy Eyeballs epoch, which RFC 8305's pacing is
measured from and which predates this feature. The four with a hook are
the four marks the reported figures are measured from (the request's
start, the connect's start, the race's epoch, the winning attempt's
launch); there is no fifth because `http://` has no handshake to time.

Only `now()` is counted, and that is not laziness: `elapsed_since` is
called once per iteration of the scheduler loop, and how many iterations
that loop takes depends on whether a loopback connect completes on its
first poll — not a number any test may pin.

`NoHooks` is also zero-sized and asserted to be, so a transport that
stores one is the same size as one with no field
(`the_no_op_hook_takes_up_no_room_in_the_transport`).

**One cost is not zero and is not the hook's**: `Native`'s future grew by
about 1.8 KB in a debug build — plumbing, not events, and it survives
removing every emission. `tests/pool.rs`'s three concurrent requests were
passing at **99%** of a 2 MiB debug test thread (measured: the fixture
needs between 2.03 and 2.125 MiB now and under 2.0 before), so that growth
turned a green test into a stack overflow. The three futures are
`Box::pin`ned now, which changes nothing the test asserts — one task, no
spawn, three requests in flight — and buys the next feature the headroom
this one did not have. The growth could not be localised: removing the
connect-side plumbing, the h1-side fields and every emission each changed
the figure by single-digit bytes, which points at rustc's debug generator
layout losing an overlap rather than at any one field.

## What a panicking hook does

**It unwinds out to whoever polled, and it is deliberately not caught.**
`std::panic::catch_unwind` needs `UnwindSafe`, which would become a bound
on the caller's own type, and it does nothing at all under
`panic = "abort"` — so catching would be a promise that holds in some
builds and not others.

What the backend owes instead is that a panic can only unwind *out*, never
leave a lock poisoned or a process aborted, and that is two structural
rules:

- **No hook is called while a lock is held.** Emission is from
  `Transport::execute` and from the response body, never from inside
  `pool.rs` — so a panicking hook cannot poison the pool's mutex, and a
  hook that blocks cannot stall a request that is not its own.
- **No hook is called from a `Drop` impl.** A panic during an unwind
  already in progress aborts the process, and an observability seam that
  can abort a program is worse than one with a hole in it.

`a_panicking_hook_leaves_the_transport_usable` tests the consequence
rather than the panic: it panics from the hook at the point nearest the
pool (`Reused`, emitted immediately after a checkout), requires the panic
to reach the caller, and then requires the **next** request on the same
transport to succeed. If the emission ever moved inside `Pool::take`, that
next request would panic with "connection pool poisoned" instead. The
recorder in every test locks a `Mutex` of its own for the same reason —
the cheapest way to have a test notice if the "no lock held" rule stops
being true.

`a_slow_hook_delays_its_own_request_and_no_other` pins the other half: a
hook is called synchronously on the task driving the request, so a hook
that sleeps holds up its own request and nothing else. There is no
internal queue and no spawned task, which is what a reader might otherwise
assume.

## It reports; it cannot steer

`Hooks::on` returns `()`. There is no verdict for the request path to
branch on, so this cannot grow into a second `Capabilities` where a
caller's declaration changes what the transport does. That is structural
rather than a rule someone has to keep.

## The timings, and what they are not

`dns`, `tcp` and `tls` are three measurements, **not a decomposition**,
and `ConnectTiming`'s own doc says so where a caller reads it:

- `dns` runs from the start of the connect to the first address the
  scheduler could try. It is a DNS figure in the honest sense — everything
  waited for is a DNS answer — including an HTTPS record (RFC 9460), which
  is looked up beside the addresses and whose hints are tried first.
- `tcp` is the **winning** attempt's own interval, stamped when *that*
  attempt was launched. RFC 8305 staggers attempts, so a winner that
  started 250 ms into the race spent that stagger in no phase at all.
- `tls` wraps the handshake alone, and is `None` — not `Duration::ZERO` —
  for a connection that has none, because a zero would read as an instant
  handshake.
- `total` is the whole connect, including attempts that failed.

`dns + tcp + tls <= total` always holds — they are disjoint intervals
inside the whole — and that is asserted, because it is the one invariant
that does not need a clock to check. The retry through an HTTPS record's
hints gets a **fresh** mark for its own `dns`: its addresses are already
in hand (`Answers` replays them), and measuring from the top would report
the whole of the failed first attempt as time spent in DNS.

Every duration is measured on the transport's `R: Timer` — the same clock
its timeouts and pool deadlines use, so a test under `tokio::time::pause()`
sees one story rather than two. There is no `SystemTime` anywhere near
this.

`crates/http-ng-native/tests/hooks_timing.rs` pins the attribution
**causally**: a resolver that takes 300 ms against a loopback socket makes
`dns > tcp` a fact about which number the code put the wait into, not a
stopwatch reading, and a handshake that takes 300 ms makes the ordering go
the other way. The pair is load-bearing — a single slow phase cannot tell
"measured correctly" from "everything is measured from the start of the
connect", because under slow DNS a `tls` stamped at the connect's start
would exceed `dns`, and that is the assertion.

## Mutation testing

Anchor: 22 tests across `binary(hooks) + binary(hooks_cost) +
binary(hooks_timing)`, verified green before each run; 204 across
`http-ng-native --all-features`.

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1 | `Connected` never emitted | killed | 13 tests, incl. `two_requests_report_one_connect_and_one_reuse` |
| M2 | `Reused` never emitted | killed | `two_requests_report_one_connect_and_one_reuse`, `the_reuse_names_the_connection_that_was_made` |
| M3 | `Head` never emitted | killed | `the_head_reports_the_status_the_server_sent`, `the_head_is_timed_from_before_the_connect_not_after_it` |
| M4 | `Closed` never emitted from the body | killed | `a_truncated_body_closes_the_connection_with_the_failure`, `a_body_polled_past_its_end_reports_one_close_and_not_two` |
| M5 | `Reused` emitted for a **fresh** connection too | killed | `without_a_pool_the_same_two_requests_report_two_connects_and_no_reuse` (+2) |
| M6 | `dns` measured from the attempt's launch, not the connect's start | killed | `a_slow_resolver_shows_up_as_dns_and_not_as_tcp` |
| M7 | `tls` measured from the connect's start | killed | `the_phases_never_add_up_to_more_than_the_connect` |
| M8 | `tcp` measured from the connect's start | killed | `a_slow_resolver_shows_up_as_dns_and_not_as_tcp` (+3) |
| M9a | a failed connection reported as `Ended` | killed | `the_three_reasons_are_not_one_reason_wearing_three_names` |
| M9b | a stale pooled connection reported as `Ended` | killed | `a_pooled_connection_the_server_closed_while_idle_is_reported_stale` |
| M10 | `NoHooks::WATCHING = true` | killed | `a_client_with_no_hook_reads_no_clock_the_hook_asked_for` |
| M11 | close reported more than once (`self.open` read, not taken) | **survived, then killed** | `a_body_polled_past_its_end_reports_one_close_and_not_two`, written for it |
| M12 | `Stale` reported for a live pooled connection | killed | `a_pooled_connection_the_server_closed_while_idle_is_reported_stale` |
| M13 | the `Reused` event mints a new id instead of the connection's | killed | `the_reuse_names_the_connection_that_was_made` |
| M14 | `Connected` emitted only after the `RequireVersion` refusal | **survived, then killed** | `a_connection_refused_by_a_version_demand_was_still_reported_made`, written for it |

**Three things this run is worth reading for beyond the verdicts.**

**M11 survived twice.** The first test written for it used a
`Connection: close` server and passed with the mutation applied, which was
the wrong conclusion for an instructive reason: on that path hyper's
`Connection` future completes *inside* `h1::exchange`, the end is reported
before the head, and the body never holds an id at all — so there was
nothing for `report_closed` to double-report. `without_pool` against a
keep-alive server is the shape in which the **body** is the reporter, and
it is deterministic rather than a race. Traced by printing the recorded
events, not by reasoning about them.

**Three rows were mis-scored by the harness, not by the code.** The
mutation script restored files with `shutil.copy2`, which preserves the
backup's mtime — so a file restored to an mtime *older* than the build
cargo had just made looked unchanged, and cargo kept the **mutated**
artifact in its cache. M11, M12 and M13 were consequently scored against a
`http-ng-core` still carrying M10's `WATCHING = true`, and all three read
as killed by a test that had nothing to do with them. The tell was a
`ConnectionId(1)` in a trace where a `NoHooks` build must produce
`ConnectionId(0)`. The restore now touches the file and every row was
re-run from a verified anchor. **Reading the killer rather than counting
the failures is what found this.**

**M9a and M4 forced a correction to the code, not to the test.** A
truncated body — five bytes of a promised hundred — reaches hyper's
`Connection` as a *clean* end, and it is `incoming` that reports the
incomplete message a poll later. Reporting `Ended` at the connection's own
poll therefore gave a failed connection the reason of one that finished,
and the failure that followed had no event left to carry: both servers in
`the_three_reasons_are_not_one_reason_wearing_three_names` answered
`Ended`. `H1Body::poll_frame` now waits for the body's verdict before
naming the reason — which is also when the check-in happens, so a
connection that went back to the pool reports nothing and one that did not
reports why.

## The facade

`http-ng` re-exports the whole vocabulary — `Hooks`, `NoHooks`, `Event`
and its four payloads, `ConnectionId`, `ConnectTiming`, `CloseReason` —
for `AllowEarlyData`'s reason: what a caller writes is an `impl Hooks for
MyType` and a `match` over `Event`, and a facade that let them *set* a
hook (through the transport) but not *name* the trait would force a
direct dependency on `http-ng-core` for the one thing this feature exists
to make easy.

`tests/facade.rs` checks it the way that file's own doc requires — by
**implementing** the trait and matching every variant rather than by
naming the types, then constructing an `Event` and calling the impl. A
`let _: Type` would say the re-export exists and nothing about whether a
consumer can write a hook with it.

## Deliberately not done

- **`Transport`'s shape is untouched.** The brief made this a stopping
  condition; it was not reached. The hook is a type parameter of
  `Native`, not a method on the seam.
- **`http-ng-h3`, `http-ng-fetch` and `http-ng-wasi` are untouched**, by
  the brief's boundary. What each would need is below.
- **No `Closed` from the h2 body.** `Connected`, `Reused` and `Head` are
  emitted by `Native::execute` and are protocol-agnostic, so the h2 path
  gets all three for free; the *end* of a connection is known inside the
  body, and h2's has three places it can arrive (the connection future,
  the response stream, the pump) where HTTP/1's has two. Guessing which
  is which without a test for each would be the kind of claim this
  document exists to prevent. `H2Body` carries the id so a later change
  has it to hand.
- **No events on the WebSocket path.** `WebSocketConnect` opens a
  connection that is never pooled, has no response head beyond the 101
  the handshake consumes, and ends when the caller's `Stream` ends —
  which the caller sees directly. A `Connected` alone would put an id in
  a log that no later event ever mentions again.
- **No `Capabilities` field.** Nothing about a hook is a promise a caller
  can act on before making a request, and the seam expresses itself by
  being *called* — the same argument the WebSocket seam rests on.

## What this leaves unverified

- **A connection dropped rather than finished reports no `Closed`.**
  Three instances: a cancelled request, a connection the pool evicts for
  age (`Pool::take` and `Reaper::reap` drop entries **under the mutex**,
  and calling a hook there is the one thing this design refuses), and a
  connection refused by a `RequireVersion` demand after `Connected` has
  been reported. This is the hole the no-`Drop` rule buys, and it is a
  decision rather than an oversight — but it means **a caller cannot
  count open connections from these events alone**, and nothing in the
  tests says so.
- **The `Stale` path in `h1::exchange` is not exercised.** Two places
  report a pooled connection found dead: `Native::checkout` (tested) and
  `h1::exchange`'s look at the connection before handing the request over
  (not). The second is the residual race `tests/pool.rs` needs its
  `LateEof` runtime to reach, and no hook test uses it.
- **"No hook is called under a lock" has no mutation.** It cannot be
  expressed as a one-line mutant without adding `H` to `pool.rs`, so what
  stands behind it is a grep (`pool.rs` contains no `hooks.on`) plus
  `a_panicking_hook_leaves_the_transport_usable`, which tests the
  consequence.
- **`dns` measured from `drive`'s start rather than `connect`'s would
  survive.** The two differ only by the HTTPS-record lookup, and that
  lookup is only made for `https://` at port 443 — which a loopback
  fixture cannot be. The mutation that swaps `dns` for the attempt's own
  mark (M6) is killed; this narrower one is not reachable from here.
- **Nothing is measured over TLS that is really TLS.** `hooks_timing.rs`
  uses a `TlsConnect` double that waits and hands the plaintext stream
  back, which is the right shape for timing a handshake seam and is not a
  handshake. The same boundary `tests/http2.rs` already sits on.
- **No h2 connection has been watched.** The three `execute`-level events
  are protocol-agnostic by construction, and `spoken_version` is shared
  with `handshake_for`, but no test runs a hook against an `h2::server`.
- **The events are untested under `smol`.** They are `R`-generic and read
  the clock only through `Timer`, so there is nothing target-specific in
  them, but `tests/dual_runtime.rs` has no hooks counterpart.
- **`Head::elapsed` is not pinned against a slow server.** It is asserted
  to contain the connect (`elapsed >= total`), which is an ordering; that
  it *tracks* a server's delay is believed and not measured.

## What the other three backends would need

- **`http-ng-h3`** is the closest, and its events would not be the same
  set. `Connected` needs a QUIC handshake rather than TCP + TLS, so
  `ConnectTiming`'s three phases do not divide the same way; it shares
  connections rather than checking them out, so `Reused` can fire while
  another request is in flight; and a spawned driver means `Closed`
  arrives on a task that is nobody's request, which is a shape neither
  `Native` nor this vocabulary has. It also has facts this enum has no
  word for — a connection migrating, 0-RTT accepted or refused. Its `H`
  would have to reach the same two places (`execute` and the body), and
  P13's answer carries over unchanged because `H3Body` already holds what
  it needs.
- **`http-ng-fetch`** can implement almost none of it honestly. The
  browser makes the connection and says nothing about it: there is no
  address, no protocol until the response arrives, no reuse signal, and
  no close. `Head` is the one event it could emit truthfully. A backend
  that emitted `Connected` with invented numbers would be the capability
  lie this workspace keeps catching, so the honest answer there is
  probably "implements `Hooks` for the events it can see, and the
  vocabulary must not force the rest" — which is an argument for keeping
  `Event` in `unversioned` until a second backend has tried.
- **`http-ng-wasi`** is the same shape as fetch and more so: `wasi:http`
  delegates the whole exchange, so the guest sees a request go out and a
  response come back. `Head` again, and nothing else without a host
  extension.

# v0.4 — request trailers: the silent drop, and the declaration it hid

`docs/v04-design.md` Appendix C, implemented. §B above ended with three
behaviours under one `request_trailers: false` and the decision owed;
this is that decision carried out, and one measurement it did not have.

## What changed

Two things, and the first is the defect.

1. **The silent drop is a typed error.** A request body that emits a
   trailer field the request never declared in `Trailer:` now fails with
   `ErrorKind::Body` over a public
   `http_ng_native::UndeclaredRequestTrailers`, which names the field(s)
   and prints the header that would have fixed it. It used to get a `200`
   with the data gone and nothing said.
2. **`http-ng-native` declares `request_trailers: true`.** It sends them
   on both protocols it speaks; the `Trailer:` header HTTP/1.1
   additionally wants is RFC 9110 §6.6.2's requirement of a sender rather
   than a limitation of ours, so a request that omits it is *malformed*
   rather than *unsupported*. `http-ng-h3` is untouched and keeps `false`
   with its `RequestTrailersNotSent`.

## Where the error is raised, and why not earlier or later

At the trailers **frame**, inside `OutgoingBody::poll_frame` — the body
hyper polls — and armed only on the HTTP/1 branch of
`established::exchange`.

- **Not before the head.** A streaming body's trailer field names are
  only known once the body ends, so nothing at `execute` time can tell a
  request that will emit trailers from one that will not. A pre-flight
  check would have to fail every undeclared streaming request, and almost
  none of them carry trailers. There is no earlier point at which the
  fact exists.
- **Not after the response**, which is `http-ng-wasi`'s
  `convert::UndeclaredTrailers` shape and is right *there* — the host owns
  the encoder and the send is raced against the body write, so that crate
  has no seat closer to the frame. Here the encoder is downstream of a
  body we own, and refusing at the frame buys the **last-chunk marker**:
  hyper's `Dispatcher::poll_write` turns a body error into
  `Error::new_user_body` and never calls `end_body`, so the message is
  aborted rather than completed. No server is ever handed a well-formed
  request whose trailers happened to be absent.

**How much of the request has already gone was measured, not assumed, and
the answer is "it depends on the caller's body".** Both shapes are pinned
in `crates/http-ng-native/tests/request_trailers.rs`:

| the caller's body | what the server had received when the guard fired |
|---|---|
| pends between its last data frame and its trailers (any real streaming producer — a gRPC client computes `grpc-status` after doing work) | the head and `4\r\nAAAA\r\n`, and **no** last-chunk marker |
| answers `Ready` for every frame | **nothing at all** — hyper drains it inside one `poll_write` and the abort takes the head with it, still in the write buffer |

The second row was a surprise and it corrected the error's own wording:
the first draft said "the head and the body chunks before the trailers
are already on the wire", which is false for exactly half the cases it
covers. It now says the request was aborted, that the server never saw a
complete one, and that how much was flushed depends on whether the body
pended — the one sentence true of both rows.

## Where the guard is armed, and why it is paired with the URI rewrite

`Rewritten::for_http1` (renamed from `to_origin_form`, because it now
does two things and a name that says one is the drift this project
hunts). It arms the guard alongside rewriting the URI into origin-form
and inserting `Host:`; `Rewritten::undo` disarms it alongside putting
those back.

The pairing is the point rather than tidiness. A request survives a
`Failed::NotSent` hand-back and may be tried again on a connection that
speaks the **other** protocol — either the next pooled bucket in the same
loop, or a fresh connection whose ALPN answers `h2` — and HTTP/2 needs no
`Trailer:` at all. A guard armed on the h1 attempt and left armed would
refuse, on HTTP/2, a request HTTP/2 would have sent. Pairing it with the
value that already exists to undo an h1-only change makes forgetting the
second half impossible rather than unlikely.

## The tests

`crates/http-ng-native/tests/request_trailers.rs`, seven, all from
outside the client against a real server:

- `sends_request_trailers_on_http1_when_the_caller_declares_them` —
  unchanged from §B, still reading `0\r\ngrpc-status: 0\r\n\r\n` off a
  raw socket over plaintext `http://`. This is the working half Appendix
  B would have deleted.
- `sends_request_trailers_on_http2_without_any_declaration` — new, and
  the second carrier: an `h2::server` decodes `grpc-status` off the
  trailing HEADERS frame with no `Trailer:` header anywhere in the
  request. Before this there was **nothing** pinning h2's `send_trailers`
  at all (mutation M4 below).
- `undeclared_request_trailers_on_http1_are_a_typed_error_naming_the_field`
  — replaces `drops_undeclared_request_trailers_on_http1_without_telling_anyone`:
  the caller gets the typed error, the error names `grpc-status`, the
  server received the flushed prefix, and the server did **not** receive
  a last-chunk marker.
- `a_body_that_never_pends_is_refused_before_any_of_it_is_flushed` — the
  other row of the table above.
- `a_trailer_header_naming_another_field_is_the_same_error` — hyper
  compares names, so a guard that only checked the header's presence
  would pass this one through to be dropped in silence.
- `an_empty_trailers_frame_is_not_an_undeclared_trailer` — `trailers_ref`
  answers `Some` for an empty map, which loses nothing on the wire.
- `a_comma_separated_and_differently_cased_declaration_still_declares` —
  `Trailer: X-Checksum, Grpc-Status` for an emitted `grpc-status`. The
  guard has to parse the header the way hyper's encoder does or it will
  refuse a request hyper would have sent.

Two capability assertions moved rather than being added:
`tests/transport.rs`'s `capabilities_are_honest_about_v01_limits` now
asserts `request_trailers` **true** (it left
`undeclared_capability_fields_match_their_conservative_defaults_today`,
where its `false` had never been the floor rule holding a line — it was a
field nobody had measured), and `tests/http2.rs`'s
`capabilities_report_the_floor_with_the_feature_on` asserts `true` for
`request_trailers` beside `false` for `response_trailers`, with the
comment corrected: `request_trailers` was never one of the fields "h2
would otherwise let us raise".

## Mutation testing

Anchor verified before **each** run: **209 tests**, `cargo nextest run -p
http-ng-native --all-features --no-fail-fast`, all passing on the
unmutated tree; every run below reported `209 tests run` and its
`Summary` line was cross-checked against the `FAIL` lines it printed.
Killers read, not counted.

| # | mutation | site | verdict | killed by |
|---|---|---|---|---|
| M1 | `check_trailers` returns `Ok(frame)` always — the error is never raised | `body.rs` | killed | `undeclared_request_trailers_on_http1_are_a_typed_error_naming_the_field`, `a_trailer_header_naming_another_field_is_the_same_error`, `a_body_that_never_pends_is_refused_before_any_of_it_is_flushed` |
| M2 | `declared_trailer_names` returns an empty set — the error is raised for a request that **did** declare | `body.rs` | killed | `sends_request_trailers_on_http1_when_the_caller_declares_them`, `a_comma_separated_and_differently_cased_declaration_still_declares` |
| M3 | `caps.request_trailers = false` | `lib.rs` | killed | `transport::capabilities_are_honest_about_v01_limits`, `http2::capabilities_report_the_floor_with_the_feature_on` |
| M4 | the h2 pump drops the trailers frame and ends the stream instead of calling `send_trailers` | `http2.rs` | killed | `over_http2::sends_request_trailers_on_http2_without_any_declaration` — and by **nothing else**, which is what that test was written for |
| M5 | the guard is armed in `Native::execute`, so both protocols enforce | `lib.rs` | killed | `over_http2::sends_request_trailers_on_http2_without_any_declaration` |
| M6 | `allow_undeclared_trailers` emptied — the guard is not disarmed on a `NotSent` hand-back | `body.rs` | **survived** | — see below |
| M7 | the names are ignored: every field in a trailers frame counts as undeclared | `body.rs` | killed | `sends_request_trailers_on_http1_when_the_caller_declares_them`, `a_comma_separated_and_differently_cased_declaration_still_declares` |
| M8 | `if undeclared.is_empty()` → always error, so an empty frame refuses too | `body.rs` | killed | `an_empty_trailers_frame_is_not_an_undeclared_trailer` and the two above |
| M9 | `ErrorKind::Body` → `ErrorKind::Unsupported` | `body.rs` | killed | `undeclared_request_trailers_on_http1_are_a_typed_error_naming_the_field`, `a_trailer_header_naming_another_field_is_the_same_error` |
| M10 | the error names the **declared** set instead of the emitted one | `body.rs` | killed | the same two as M9 |
| M11 | the trailers frame is passed through and the error deferred to the next `poll_frame` — the "raise it later" placement | `body.rs` | killed | the same three as M1, and see below |
| M12 | the arming call removed from `Rewritten::for_http1` | `established.rs` | killed | the same three as M1 |

**M11 is the placement decision, and it found more than it was aimed
at.** It was written to check that raising the error *after* the frame
had gone out would be caught by the "no last-chunk marker" assertion.
It never got that far: with the frame passed through, hyper ends the body
and stops polling it (`clear_body = true` on a trailers frame), the
response arrives, and the deferred error is **never delivered at all** —
the request completes `200`, exactly the defect this work exists to
remove. So a body-level check that fires one poll late is not a weaker
version of this design, it is the original bug with extra steps; a check
that late has to live outside the body, which is where `http-ng-wasi` put
its.

The honest limitation of that: the assertion
`!text.contains("\r\n0\r\n")` — "the request was left unterminated" — is
made but no mutation run made it the *sole* killer, because M11 died one
assertion earlier on the `200`. It is a checked claim, not an
independently mutation-pinned one.

## What this leaves unverified

- **M6 survived: nothing pins the disarm.** `Rewritten::undo` calls
  `allow_undeclared_trailers`, and removing that call leaves all 209
  tests green. The path is live rather than theoretical — a pooled HTTP/1
  connection that the server closed while idle produces
  `Failed::NotSent`, `Native::execute` puts the same request object back
  into the loop, and the next attempt can be a pooled h2 bucket or a
  fresh connection whose ALPN answers `h2`; there the stale guard would
  refuse a request HTTP/2 would have sent. Reaching it from a test needs
  a TLS stub whose reported ALPN differs between the first and second
  connect, plus a server that closes an idle connection, plus a body with
  undeclared trailers. Not built; recorded rather than papered over, and
  the fixture was **not** adjusted to make the mutation die.
- **Three more silent drops in hyper's h1 encoder are not covered**, all
  reachable in principle and none measured here. `encode_trailers` drops
  a *declared* field whose name is in `is_valid_trailer_field`'s deny
  list (`Authorization`, `Content-Length`, `Set-Cookie`, `Trailer`,
  `Transfer-Encoding`, `TE`, …), and it drops **every** trailer when the
  encoder is not chunked (`Kind::Length`, i.e. a request with a known
  `Content-Length`). The guard implemented here compares names against
  `Trailer:` and nothing else, so both of those still complete with a
  `200` and no trailers. Whether the second is reachable through
  `Native` — whether a `RequestBody::Streaming` with an exact
  `size_hint` ever gets length framing here — was not measured.
- **`response_trailers` stays `false` and was not re-examined.** The
  measurement in this section is about the request direction only. hyper
  and `h2` both surface response trailers as frames, so the same "nobody
  looked" hazard that produced the `request_trailers` understatement may
  or may not apply; nothing here checked it.
- **`declared_trailer_names` is a second implementation of the same
  parse.** `http-ng-wasi`'s `convert::declared_trailer_names` is the
  first, `pub(crate)` in its own crate. The two agree today and neither
  is generated from the other; `check_version`'s argument in
  `http-ng-core` ("a function here so the rule has one definition, two
  transports enforce it and they must not drift") applies, and unifying
  them means touching `http-ng-wasi`, which this change was not permitted
  to do.
- **`http-ng-select`'s pinned disagreement list is now wrong, and was
  left wrong deliberately.** `combine` needs no change — its conjunction
  gives the composite `true && false == false`, which is the correct
  weaker claim, and `Selecting::new` still constructs — but
  `crates/http-ng-select/tests/capabilities.rs`'s
  `the_two_stacks_disagree_on_exactly_six_fields_today` asserts the
  measured list whole, and `request_trailers` now belongs in it. That
  test fails; the fix is one string, `"request_trailers"` between
  `"full_duplex"` and `"response_trailers"`, plus the test's name and the
  neighbouring `the_stored_answer_holds_whichever_stack_serves_the_request`
  doc comment's "five take the weaker claim" becoming six. Editing that
  crate was out of scope for this change, so the red test is reported
  rather than repaired.

# v0.4 W2 — the same event set, over HTTP/3

`http-ng-h3` is the **second** backend to implement
`http_ng_core::unversioned::Hooks`, and that is the point of it. The
WebSocket seam earned its confidence exactly this way: `http-ng-fetch`
fitted `WebSocketConnect` unchanged, and that turned an argument into a
fact. This section is the same experiment for the event set.

## Did it fit unchanged?

**Yes, and nothing in `http-ng-core` was touched.** No variant added, no
field added, no bound moved. `H3<R, T, D, H = NoHooks>` carries a hook,
`H3::hooks(..)` returns the different type that makes the zero-cost claim
structural, and `http-ng-select` — which owns an `H3<R, T, D>` — compiles
unchanged, because `H` is defaulted and last.

What it cost is **one bound fewer** than the TCP backend charges.
`http-ng-native` needs `H: Hooks + Clone + Unpin`; this needs `H: Hooks +
Clone`. `Clone` is the same fact in both (the response body outlives
`execute` and holds the hook rather than borrowing it); `Unpin` falls away
because `H3Body` keeps its hook behind an `Option<Box<Watch<H>>>`, and a
`Box` is `Unpin` whatever it contains. The box is there for two reasons
anyway — a body nobody is watching carries eight bytes, and
`http-ng-select` requires `<H3<..> as Transport>::Body: Unpin`.

The predictions in the previous section were three-for-four. The phases do
not divide the same way (right); `Reused` can fire while another request is
in flight (right); `H` reaches `execute` and the body and P13 carries over
unchanged (right). **The fourth was wrong in an interesting direction**: it
predicted that "a spawned driver means `Closed` arrives on a task that is
nobody's request". It cannot arrive there at all — see below.

## What `Connected`'s timings mean here

`ConnectTiming` has four fields and QUIC has three numbers.
`crates/http-ng-h3/src/hooks.rs` carries the argument; the summary is:

| field | over TCP | over QUIC, here |
|---|---|---|
| `dns` | start of the connect to the first address that could be tried | **the same**, unchanged |
| `tcp` | the winning attempt, launch to connected | **the QUIC attempt**: `Endpoint::connect_with` to a connection that can carry a request |
| `tls` | the handshake, or `None` for a connection that has none | **always `None`** |
| `total` | the whole connect | **the same**, and it also contains h3's SETTINGS exchange |

**`tcp` holds the attempt because its name is wrong here and its definition
is exactly right.** "The winning attempt, from the moment it was launched to
the moment it connected" describes what is measured precisely; only the
field's name says TCP. The alternative was `Duration::ZERO` for a phase that
does not exist, and the brief for this work ruled that out for the right
reason — a number a caller will trust is worse than an absence.

**`tls` is `None` because it is forced, not because it is tidy.**
`Connecting::into_0rtt()` hands back a usable connection *before the
handshake completes*, so on a 0-RTT connection there is no completed
handshake to time at the moment the request goes out; reporting one would
mean waiting for the round trip 0-RTT exists to skip. A field that was
`Some` on the 1-RTT path and `None` on the 0-RTT one would mean two
different things under one name, and a `Some` duplicating `tcp` would break
the `dns + tcp + tls <= total` invariant `ConnectTiming` documents.

**The cost of that decision is a wording defect in the seam, and it is
recorded rather than fixed.** `tls`'s own doc says `None` means "a
connection that has none", which is false of every QUIC connection ever
made. `http-ng-core` is not one backend's to rewrite; the honest options if
a third backend meets the same thing are to reword the field (cheap) or to
split it (not cheap), and neither should be decided from one data point.

Everything outside the three phases — binding the endpoint, building the
crypto configuration, h3's SETTINGS exchange — is time inside `total` that
belongs to no phase, which is what `ConnectTiming` means by "three
measurements, not a decomposition".

## `Reused` on a shared connection: the event still says something true

`http-ng-native` checks an h2 connection out of the pool **exclusively**, so
its `Reused` carries an implication its wording never claimed: the previous
request on that connection has finished. Here it does not. A QUIC connection
is shared and multiplexed, and a second request joins one that is already
carrying the first.

The event's own words — *"a connection somebody else already made is being
used again"* — stay true, and the fact it exists to deliver stays exactly as
useful: **two requests to one origin either cost one connection or two, and
only the transport knows which happened.** What changes is an inference a
caller might draw and the event never licensed: two `Reused` events are not
two consecutive uses. That is written down in `crates/http-ng-h3/src/lib.rs`
beside the pool branch and pinned by
`a_second_request_joins_a_connection_that_is_still_carrying_the_first`,
which is causal rather than timed — the first response's body is
deliberately unread while the second request goes out, and both bodies are
collected afterwards.

Two mechanical consequences fell out of sharing, and both are real work
rather than bookkeeping:

- **A close can be met by several requests at once.** `http-ng-native` gets
  at-most-once for free, because one body is the only observer of an
  exclusive connection. Here `execute`, a body, and the next checkout can
  all meet the same death, so the "already told" flag lives with the
  connection (`ConnState`, an `AtomicBool` swapped outside every lock).
  Without it a caller counting connections is wrong in the direction that
  looks like a leak — mutation M12.
- **A failed request is usually not a failed connection.**
  `quinn::Connection::close_reason()` is the discriminator and there is no
  other. Announcing every stream error as a close would be the loudest
  possible lie about a transport whose whole point is that neighbours
  survive — mutation M15.

## `CloseReason::Ended` has no emitter, and refusing to invent one is the finding

This is the mirror image of the "request queued" refusal that
`http-ng-native` made. There, a variant was left out because that transport
has no queue. Here, an existing variant is left **unemitted** because
nothing in HTTP/3 has its subject: `Ended` says *"the exchange finished
it"*, and a QUIC connection outlives its streams by construction. It goes
away because it timed out, because the peer sent `CONNECTION_CLOSE`, or
because something failed. So this crate emits `Stale` and `Failed`, and
`a_clean_exchange_ends_no_connection` pins that two finished exchanges and a
dropped transport report nothing at all.

**The one place a graceful end could be observed promptly is shut by P13's
own answer, which is worth stating plainly.** h3's connection driver is
where a `GOAWAY`-then-close would surface, and it is spawned — but
`crate::QuinnTask` is `Pin<Box<dyn Future<Output = ()> + Send>>`, because
quinn declares `Runtime::spawn` that way, and `Hooks` deliberately promises
no `Send`. A future capturing an `H` does not coerce. Calling a hook from
the driver would therefore cost **every** hook a `Send` bound, which is the
thing P13 was asked to avoid.

So a close here is **discovered rather than observed**: at the next checkout
that finds the connection dead (`Stale`), or at the request or body that
fails on it (`Failed`). The previous section's prediction — "a spawned
driver means `Closed` arrives on a task that is nobody's request" — is
exactly backwards, and the reason is a seam property rather than a QUIC one.

## 0-RTT: what a caller can learn from events, and what it would cost

**Nothing.** No event says a request went out in early data, and none says
whether it was accepted. That is a deliberate answer rather than an
oversight, and it has two halves with different prices:

- **"It went out in early data"** is knowable at the moment `Connected` is
  emitted and has nowhere to go. It would need a field on `Connected` or a
  variant of its own, and `http-ng-core` is not this backend's to extend —
  a variant added for one backend is how an event set stops being a shape.
- **"It was accepted"** is not knowable then at all. The verdict resolves
  *after the response body* (8.63 ms against 8.58 ms, `docs/h3-research.md`
  §3.2), so reporting it would need either a spawned task — which this
  feature may not have, on its own rules — or making the caller wait for
  the round trip 0-RTT exists to skip.

What *is* visible without claiming anything: on a 0-RTT connection `tcp` is
the time to `into_0rtt()` returning, which is well under a round trip. That
is an observation a reader can make, not a fact the event asserts, and a
caller cannot separate it from a fast handshake without knowing the RTT.

The replay is invisible on purpose, and that is pinned:
`a_replayed_0_rtt_request_reports_one_head_and_one_connection` sends a
marked request to a second server whose ticketer refuses the first's ticket,
and asserts **one `Connected`, one `Head`, no `Closed`** — one request got
one response, and a caller told about the rejected stream would be told
about a request they never made.

## Zero cost when nobody is watching, measured

Same technique as `http-ng-native`'s, same style — a runtime whose clock
counts its own reads, and **equalities rather than bounds**, because a `<=`
would pass for a build that read the clock four times when it should read it
none. `crates/http-ng-h3/tests/hooks_cost.rs`.

| | `now()` | `elapsed_since()` |
|---|---|---|
| `NoHooks`, fresh connection | **0** | **0** |
| `NoHooks`, pooled request | **0** | **0** |
| a hook, fresh connection | 2 | 4 |
| a hook, pooled request | 1 | 2 |

**The `NoHooks` row is zero where `http-ng-native`'s is 1**, and that is a
fact about this transport rather than a virtue: `connect::drive` stamps the
start of the Happy Eyeballs race over there and RFC 8305's pacing is
measured from it, while QUIC's connect is not a SYN race and this crate had
no `Timer::now` call in it at all before this work. quinn's own timers go
through `Timer::sleep` and `std::time::Instant` (`crate::runtime`'s
`until`), not through this clock.

`elapsed_since` is counted here, which `http-ng-native`'s file deliberately
does not do — there it is called once per iteration of a scheduler loop
whose iteration count no test may pin. Here every call is at a fixed point.

The four intervals on a fresh connection are `dns`, `tcp`, `total` and
`Head::elapsed`, off two marks (the request's start; the QUIC attempt's
launch). **There is no fifth, and that is the `tls: None` decision showing
up as a number.** The pooled row's two are `dns` — computed before the pool
is consulted and thrown away on that path — and `Head::elapsed`.

`NoHooks` is zero-sized and `H3<.., NoHooks>` is the same size as `H3<..>`,
so the default type parameter is the same type rather than a second one.

## Mutation testing

Anchor: **20 tests** across `binary(hooks) + binary(hooks_cost)`, verified
green *before every single row* rather than once at the start. The complete
table was run again end to end after the last fixture change and reproduced
every verdict.

The harness restores with `git checkout` **and** `os.utime` to now, because
the failure this workspace hit four times in the previous round was a
restore that preserved the backup's mtime: cargo then kept the *mutated*
artifact and every later row was scored against the wrong binary. Killer
*names* are read rather than counted, for the same reason — a killer that
makes no sense is the only tell.

**M18 is a control and it is meant to survive**: `Ordering::SeqCst` →
`Ordering::Relaxed` in a swap no test can observe. A table of eighteen
kills with no survivor anywhere is indistinguishable from a harness that
reports "killed" unconditionally, and this row is what tells them apart.

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1 | `Connected` never emitted | killed | 11 tests, incl. `a_fresh_request_reports_the_connection_it_paid_for_and_the_head_it_got` |
| M2 | `Reused` never emitted | killed | `two_requests_report_one_connect_and_one_reuse`, `a_second_request_joins_a_connection_that_is_still_carrying_the_first`, `one_streams_failure_is_not_the_connections_end` |
| M3 | `Head` never emitted | killed | 8 tests, incl. `the_head_reports_the_status_the_server_sent` |
| M4a | `Closed` never emitted **from the body** | killed | `a_connection_that_dies_under_a_body_is_reported_by_the_body` |
| M4b | `Closed` never emitted **from `execute`** | killed | `a_connection_the_server_tore_down_is_reported_failed` (+2) |
| M5 | `Stale` never emitted | killed | `a_pooled_connection_that_died_while_idle_is_reported_stale`, `the_two_reasons_are_not_one_reason_wearing_two_names` |
| M6 | `Reused` emitted for a **fresh** connection too | killed | `two_transports_sharing_no_pool_report_two_connects_and_no_reuse` (+5) |
| M7 | `dns` measured to the wrong end — stamped before the lookup | killed | `a_slow_resolver_shows_up_as_dns_and_not_as_the_attempt` |
| M8 | `tcp` measured from the connect's start | killed | `a_slow_resolver_shows_up_as_dns_and_not_as_the_attempt`, `the_same_two_requests_with_a_hook_do_read_it` |
| M9 | `tls` reported as a duration instead of absent | killed | `a_fresh_request_reports_the_connection_it_paid_for_and_the_head_it_got` (+2) |
| M10 | the close reason is always the same value (`Failed` → `Stale`) | killed | `the_two_reasons_are_not_one_reason_wearing_two_names` (+3) |
| M11 | `NoHooks::WATCHING = true` | killed | `a_client_with_no_hook_reads_no_clock_at_all` |
| M12 | a close can be reported more than once (`load`, not `swap`) | killed | `one_connection_reports_one_close_however_many_requests_meet_its_death` |
| M13 | `Reused` mints a new id instead of the connection's | killed | `two_requests_report_one_connect_and_one_reuse` |
| M14 | `mark()` ignores `WATCHING`, so a hookless client pays | killed | `a_client_with_no_hook_reads_no_clock_at_all` |
| M15 | a request failure is a close whatever the connection says | killed | `one_streams_failure_is_not_the_connections_end` |
| M16 | `Connected` reports `HTTP_11` as the negotiated version | **survived, then killed** | `a_fresh_request_reports_the_connection_it_paid_for_and_the_head_it_got`, extended for it |
| M17 | `Reused` reports `HTTP_11` as the version spoken | killed | `two_requests_report_one_connect_and_one_reuse` |
| M18 | **control**: `SeqCst` → `Relaxed`, which no test can see | **survived, as intended** | — |

Three rows are worth reading past the verdict.

**M15 had no killer until a test was written for it, and the two obvious
fixtures both turned out to be the wrong shape** — measured, not reasoned
about. A server that drops the request stream without answering does *not*
leave the connection alive: the client's h3 connection fails too, and the
server accepts a second connection. And a **cancelled** request produces no
error at all, so no hook is ever called. The failure that works is the
request-trailers refusal, which this crate raises itself on a connection
that is provably untouched — and the proof is a second request answered on
it, one accept, the server's own count.

**M8 is killed twice over, and the second killer is the cost test.**
Measuring `tcp` from the connect's start adds an `elapsed_since` call, so
the equality in `hooks_cost.rs` fails. A test written to pin an absence
turns out to be a structural tripwire for the timing code as well.

**M16 survived the first run and the response was to add a claim, not to
adjust a fixture.** `Connected`'s `version` is `HTTP_3` by construction —
`ALPN_H3` is the only token offered, and a connection that negotiates
anything else never reaches the line — which is exactly why nothing read it
and exactly why a regression would leave it standing.

## Deliberately not done

- **`http-ng-core` is untouched.** The brief made a new variant a stopping
  condition; it was not reached, and the two places where the vocabulary
  is a poor fit (`tls`'s wording; no word for early data) are recorded
  above rather than legislated from one backend.
- **Nothing is spawned for an event's sake**, and no hook fires under a
  lock or from a `Drop`. The `Stale` event is emitted inside `checkout`
  **after** the pool's mutex guard has been dropped, so a panicking hook
  cannot poison the pool —
  `a_panicking_hook_leaves_the_transport_usable` panics at the emission
  nearest the pool and then requires the next request to succeed.
- **No `Ended`.** Argued above.
- **No event for the 0-RTT replay.** Argued above.
- **`http-ng-select` gets no hook of its own.** It owns a `Native` and an
  `H3` and would have to decide whether a hook set on the pair reaches
  both members and what a `Connected` from either means to a caller who
  asked one object. That is a design question, not plumbing, and this work
  is the second backend rather than the third.

## What this leaves unverified

- **A connection dropped rather than finished reports no `Closed`.** Same
  hole as `http-ng-native`'s, arriving here through the same rule: a
  cancelled request, and a connection this transport's `Drop` takes away,
  report nothing. **A caller cannot count open connections from these
  events alone**, and it is a decision rather than an oversight.
- **A `Stale` followed by a connect that fails has no test.** The event is
  emitted inside `checkout`, synchronously after the pool's guard is
  dropped and before the dial, with no `await` in between — so it cannot
  be lost to a cancelled `connect` future, and that ordering is the whole
  reason it is not carried out to `execute` with the rest. What has no
  test is the pair actually happening: every `Stale` in this suite is
  followed by a connect that succeeds.
- **Nothing observes a QUIC connection migrating.** The vocabulary has no
  word for it and this work did not look for one. A migrated connection
  keeps its `ConnectionId` here and its `Connected::remote` goes stale —
  that is a real inaccuracy in a field a caller may believe, and it is
  unmeasured.
- **The concurrent close is untested.** `ConnState`'s swap exists so that
  two bodies meeting one death report once; the test that kills M12 is
  the *sequential* shape (a failing `execute`, then a checkout finding the
  same entry). Two bodies failing at the same instant is not exercised,
  and making it deterministic would need a fixture this file does not
  have.
- **`Head::elapsed` is pinned against a slow server and not against a slow
  body.** A server that stalls after the head moves nothing here, because
  the head has already been reported.
- **No hook has been watched under `smol`.** The events are `R`-generic and
  read the clock only through `Timer`, and `tests/two_runtimes.rs` runs
  this transport under both — but with no hook.
- **The `remote` field is not distinguished from the address dialled.**
  It is read from `quinn::Connection::remote_address()`, which is the
  right source, and every fixture dials the address that answers — so a
  mutation replacing one with the other would survive. On loopback there
  is no way to tell them apart.
- **Nothing measures the allocation.** The cost table counts clock reads;
  `ConnState` and `Watch` are one `Arc` and one `Box` per watched
  connection and request, and a mutation that allocated them under
  `NoHooks` would be invisible to `hooks_cost.rs`. `M14` covers the clock
  half of "`WATCHING` ignored" and nothing covers this half.
