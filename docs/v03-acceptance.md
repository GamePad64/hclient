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
theirs to implement. **And an IPv6-literal endpoint does not work at all**,
which is finding 2.

### 2. An IPv6-literal endpoint fails at TLS, and the defect is not in this crate

`Doh::pinned`'s own doc offered `https://[2606:4700:4700::1111]/dns-query`
as an example. Measured, it fails with **`Tls: invalid dns name`** — the
TCP connection is made, and the handshake never starts.

`http::Uri::host()` returns an IPv6 literal **with its brackets**;
`http-ng-native`'s `connect.rs` passes that string to
`TlsRequest::server_name` unchanged; `rustls_pki_types::ServerName::
try_from` tries `DnsName`, then `IpAddr`, and neither strips a bracket.
Both `IpLiteralOnly::literal` and `Doh`'s own `ip_literal` do strip them,
each with a comment naming this exact trap — the TLS name is the one place
on the path where nobody does.

**It is not DoH-specific**: any `https://[…]/` URL through `http-ng-native`
+ `http-ng-tls-rustls` meets it. The fix is one line in one of those two
crates, both of which were out of this task's scope, so what landed instead
is the note on `Doh::pinned` and a test that pins the current behaviour —
`an_ipv6_literal_endpoint_fails_at_tls_today_and_the_defect_is_not_in_this_crate`,
whose *failure* is the signal that the note is stale.

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

The two tests that *expect* an error — the IPv6-literal one and the Quad9
one — called `doh()` directly rather than the retrying wrapper, so a lost
SYN turned "Quad9 answers 505" into a failed assertion about a connect
timeout. Reproduced under `HTTP_NG_REQUIRE_NETWORK` before it was fixed.
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
- **Nothing has been run against a real WebSocket server.** Every fixture
  is a loopback socket in this repository, and the Autobahn test suite —
  which is what would settle fragmentation, UTF-8 validation and the close
  code table — has not been run.
- **Fragmented incoming messages are reassembled by `tungstenite` and
  never exercised.** The fixture sends single frames only, so
  `WebSocketConfig::max_message_size` and the continuation path have no
  test of ours.


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
- **Nothing has been run against a real WebSocket server**, so no real
  implementation's pong behaviour has ever answered one of these pings.
  The same gap steps 1–4 record, unchanged.
- **`u64` sequence numbers wrap at `u64::MAX` pings and nothing tests
  it.** `wrapping_add` is deliberate rather than accidental; at one ping
  a nanosecond it is 584 years, so this is recorded for completeness
  rather than as a risk.
