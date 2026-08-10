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
- **No HTTPS/SVCB discovery, and no Alt-Svc.** h3 is chosen by constructing
  `H3`. `http_ng_dns::SvcbEndpoint` already carries `alpn` and the resolvers
  already answer, so tier 2 of the research's discovery ladder is a task
  rather than a vertical; Alt-Svc still needs a store with an eviction
  policy, a negative cache and a rule for `clear`, none of which is protocol
  work.
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
