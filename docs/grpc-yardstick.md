# gRPC as a yardstick

**gRPC is not built here and must not be.** There is no framing codec in
this workspace, no status enum, no `grpc-*` helper, and no crate that
depends on one. The five-byte length prefix, the status codes, the
percent-decoding of `grpc-message` and the base64 of a `-bin` header are a
caller's job, and this document exists to make it possible for that caller
to be written — not to write it.

What gRPC *is* here is an **external specification to be audited against**,
the way the Autobahn TestSuite was used for WebSocket. `grpc/doc/
PROTOCOL-HTTP2.md` is unusually good for the purpose: it is short, it is
specific about the wire, and almost every line of it is a demand on the
HTTP client underneath rather than on the RPC layer above. If this client
can carry gRPC, it can carry the class of protocol gRPC belongs to.

The audit is `crates/http-ng-native/tests/grpc_shape.rs`: fifteen tests,
every one of them against a real `h2::server` on a real socket, with every
claim about what the client sent read from what that server decoded.

**The result is that no library code changed.** That is the headline, and
it is worth stating plainly rather than apologetically: twenty-one rows
drawn from someone else's specification, and the client already did all of
them. Three things it does *not* do are recorded below with the reason and
the place the reason already lived.

## The rows

Every row was run. "Where" names the test in
`crates/http-ng-native/tests/grpc_shape.rs`.

| # | What the specification asks | Verdict | Where |
|---|---|---|---|
| 1 | **Call-Definition** — `:method POST`, `:scheme`, `:path`, `:authority`, `content-type: application/grpc+proto`, `grpc-timeout`, `grpc-accept-encoding`, `user-agent` | all reach the wire unchanged | `the_call_definition_reaches_the_wire_te_trailers_included` |
| 2 | **`te: trailers`** — *"used to detect incompatible proxies"* | reaches the wire | same |
| 3 | RFC 9113 §8.2.2 — `connection` / `transfer-encoding` must not | removed, and **`TE` deliberately is not**, which is the whole content of the exemption | same |
| 4 | No `content-length` invented for a streaming call | none is added on either path | same |
| 5 | **Custom-Metadata** — repeated names keep both values, `-bin` values pass byte for byte padded or un-padded, `.` and `_` are legal name characters | all four, in the request head, the response head and the trailers | `custom_metadata_survives_in_the_head_the_response_and_the_trailers` |
| 6 | **Trailers-Only** — one HEADERS block, END_STREAM, no DATA | a complete response: `grpc-status` and `grpc-message` readable off the head, an empty body that ends rather than pends, and **no** trailers frame | `a_trailers_only_response_is_a_complete_response_with_no_body` |
| 7 | **Trailers** — *"Status must be sent in Trailers even if the status code is OK"* | the trailers frame reaches the caller | `response_trailers_reach_the_caller_and_the_frame_split_survives` |
| 8 | *"DATA frame boundaries have no relation to Length-Prefixed-Message boundaries"* | a message split 2 + 7 bytes across two DATA frames arrives as `[2, 7]` — neither coalesced nor lost | same |
| 9 | **EOS** — *"implementations MUST send an empty DATA frame with this flag set"* when the request stream closes with no data left | sent, unconditionally: the seventeenth frame of a sixteen-message stream is the empty one | `a_bidirectional_stream_carries_sixteen_rounds_both_ways` |
| 10 | **Bidirectional streaming** on one stream, indefinitely | 16 rounds, each request message caused by the previous response message, one stream, one connection | same |
| 11 | **Flow control, receive** | 524 328 bytes — eight 64 KiB messages over a 65 535-byte window — arrive whole, with the trailers behind them | `a_response_past_the_window_arrives_whole_with_its_trailers` |
| 12 | **Flow control, send** | back-pressured: with the server provably reading nothing, **2 of 16** 32 KiB chunks had been taken from the caller's body when the head arrived — one window's worth | `a_request_past_the_window_is_backpressured_rather_than_buffered` |
| 13 | **Cancellation** — *"immediate full-closure of the stream"* | the call ends at the server, and the next call is unaffected. **Not as `RST_STREAM`** — see limitation L2 | `cancelling_a_call_ends_it_at_the_server_and_leaves_the_next_one_alone` |
| 14 | **GOAWAY** — *"retry the call elsewhere"* | the next call opens a fresh connection and succeeds; the call the GOAWAY names is still answered | `a_goaway_costs_a_connection_and_not_the_next_call` |
| 15 | **PING** — *"the peer must respond"* | answered — by the next call. Nothing polls a pooled connection, so a ping arriving on one waits. See limitation L3 | `a_ping_to_a_pooled_connection_waits_for_the_next_call` |
| 16 | Connection reuse across calls | one connection for two sequential calls: a call's trailers do not cost it its socket | `two_calls_with_trailers_share_one_connection` |
| 17 | Multiplexing | **two** connections for two concurrent calls. See limitation L1 | `two_concurrent_calls_take_two_connections_rather_than_two_streams` |
| 18 | `Client`'s own stages must not interfere | no `Cookie` (no jar was asked for), one hop (a `200` stops the redirect stage before any counting), body and trailers byte-identical | `the_clients_own_stages_leave_a_grpc_exchange_alone` |
| 19 | …including `Accept-Encoding` | with `gzip` + `brotli` compiled in the client advertises **`gzip, br`**, and never a coding it cannot reverse | same |
| 20 | …and the caller can take it back | a caller-set `Accept-Encoding: identity` reaches the wire exactly, in every build | `a_caller_who_sets_accept_encoding_keeps_it_exactly` |
| 21 | Deadlines — a long-idle stream is normal for gRPC | a 600 ms silence mid-stream is not a failure by default, and **is** cut with `between_bytes = 150 ms`, as `Timeout(BetweenBytes)` | `an_idle_stream_survives_by_default_and_is_cut_only_when_asked` |

## The three limitations, and where each already lived

None of these is new, and none was found by this audit — what the audit
adds is a number and a test.

**All three have since been investigated together, and the investigation
is `docs/h2-multiplexing.md`.** Three of its findings belong here rather
than only there. This section's own framing — L2 and L3 hang off L1 — is
**confirmed by measurement**: with the connection driver spawned, the
`RST_STREAM(CANCEL)` arrives and a `PING` is answered in 0 ms, neither
needing any code of its own. L1's cost is now a count rather than a
sentence: 1, 2, 4 and 8 concurrent calls cost 1, 2, 4 and 8 TCP
connections and the same number of h2 handshakes, and over real TLS, 480
requests at a concurrency of 8 cost **480** accepts where a shared
connection needs 60. And it is **not** a pure win: at the peer's
`MAX_CONCURRENT_STREAMS` a shared connection queues where today's opens a
second socket — six concurrent calls against a limit of two finished in
three waves at 203/405/607 ms, where today the sixth finishes in ~200 ms.

### L1. No multiplexing: one concurrent call, one connection

gRPC's transport model is many concurrent calls on one HTTP/2 connection.
An h2 connection here is **checked out of the pool exclusively** and
carries one stream at a time (`crates/http-ng-native/src/pool.rs`'s module
doc, and `http2.rs`'s "One stream per connection", which is also where the
policy would have to change).

The reason is `Spawn`: without a background task the only thing driving a
connection is the in-flight request futures, so a caller that stopped
polling one stream would stall its neighbours. It is the same question
`http-ng-h3` answers the other way and for a stated reason — a QUIC
connection nobody polls is dying, so it *must* spawn a driver, and once it
has, exclusivity has no subject.

**Classification: recorded limitation.** The cost for a gRPC caller is one
TCP connection and one TLS handshake per concurrent RPC. What it is *not*
is a correctness problem: W1's rule that cancelling one stream must not
tear down the others holds here because there are never any others, which
is a property of the pool policy rather than of the h2 code, and is written
down in both places.

### L2. Cancellation closes the connection; it does not send `RST_STREAM`

A gRPC client cancelling a call sends `RST_STREAM(CANCEL)`. This client
closes the socket, and the server sees the connection go.

`http2::Pump`'s `Drop` **does** queue a `RST_STREAM(CANCEL)` — but a queued
frame reaches the wire only from inside `Connection::poll`, and under L1
the connection is owned by the same future that owns the pump and is
dropped in the same breath. That impl's own doc comment already said it is
unobservable today and is kept for the day L1 changes; this audit is where
"unobservable" stopped being an assertion and became a measurement
(`Ending::ConnectionGone`, never `Ending::Reset`).

**Classification: recorded limitation, downstream of L1.** For a transport
that carries one stream per connection, the server learns the same fact at
the same instant: the call is over. It is only under multiplexing that the
difference — one stream cancelled, or all of them — becomes visible, and
that is exactly the day L1 changes.

### L3. A `PING` on a pooled connection waits for the next call

`is_reusable`'s doc comment already says that nothing polls an idle
connection and that a checkout is therefore the only moment a `GOAWAY` or a
closed socket is noticed. A `PONG` is written from the same place, so the
same sentence covers it, and the A/B in row 15 measures both halves:
nothing within 750 ms while pooled, answered by the next call, on the same
connection.

**Classification: recorded limitation with a bounded cost.** A server
enforcing a keepalive deadline against an idle pooled connection will drop
it; this client discovers that at the next checkout and opens a fresh one.
A wasted socket, never a failed call — a connection with no call on it has
no call to fail. There is deliberately no keepalive knob on `Native`; the
WebSocket seam has one for the *opposite* reason, that an open WebSocket
has no request future behind it at all and is therefore bounded by nothing
else.

## What is the caller's job, and stays the caller's job

Named here so that nobody mistakes an absence for a gap:

- **Message framing.** The compressed flag, the four-byte big-endian
  length, and reassembly across DATA frame boundaries. Row 8 exists to
  prove the boundaries arrive as the server sent them, which is the only
  thing a client can owe a reassembler.
- **`grpc-status`, `grpc-message`, `grpc-status-details-bin`.** Parsing the
  integer, percent-decoding the message (*"implementations MUST NOT error
  or throw away the message"*), base64-decoding the details, and checking
  the details do not contradict the status.
- **Synthesising a status.** *"Implementations should expect broken
  deployments to send non-200 HTTP status codes … and to omit Status &
  Status-Message. Implementations must synthesize a Status."* The client
  delivers the status line and the headers faithfully; deciding what
  `502 Bad Gateway` means in gRPC's vocabulary is the RPC layer's.
- **Binary metadata.** Emitting un-padded base64, accepting padded, and
  splitting joined `-bin` values on `,`. Row 5 proves the transport carries
  whatever the caller writes, including two values under one name.
- **`grpc-timeout`.** It is a header the caller computes and the server
  enforces. `Timeouts` is a different thing with a different owner; row 21
  is there to show the two do not collide.
- **Reading the trailers.** `Response::collect()` and `Response::chunk()`
  skip trailer frames by design — `chunk`'s own doc comment says so and
  points at `into_parts()`. A gRPC caller therefore reads the body as an
  `http_body::Body`, which is what rows 7 and 10 do. Measured rather than
  assumed: row 7's second half sends the same request through `collect()`
  and confirms the `grpc-status` is not there.

## Two things the client cannot honour, and neither is ours

- **Header order.** *"Implementations should send Timeout immediately after
  the reserved headers and they should send the Call-Definition headers
  before sending Custom-Metadata."* `http::HeaderMap` does not preserve
  insertion order across different names, so this SHOULD is unreachable
  from any crate built on `http` 1.x. It is a SHOULD, no gRPC server
  depends on it, and the alternative is not owning the header type.
- **The suggested 8 KiB header limits.** This client sets no
  `SETTINGS_MAX_HEADER_LIST_SIZE`; h2's own default applies in both
  directions. Advertising a limit is a knob nobody has asked for, and the
  spec's number is a suggestion to servers about requests.

## Mutation table

Twelve mutations plus one control, applied one at a time to library code
with `tests/grpc_shape.rs` unchanged, and scored by
`cargo nextest run --workspace --all-features -E 'binary(grpc_shape)'`.
`--workspace --all-features` rather than `-p http-ng-native --all-features`
on purpose: the second leaves `http-ng` on its default features, and rows
19 and 20 are only meaningful with `gzip`/`brotli` compiled in.

**Anchor: 15 tests, 15 passing, verified before every run.** Restore is
`git checkout` followed by an explicit `os.utime` — a restore that
preserved mtime would leave cargo reusing the mutated artifact and score
the *next* mutation against the wrong binary.

| id | mutation | site | verdict | killed by |
|---|---|---|---|---|
| M1 | add `TE` to the strip list | `http2::strip_connection_headers` | **killed** | row 2 |
| M2 | remove `CONNECTION` from the strip list | same | **killed** | row 3 (h2 rejects the request as malformed) |
| M3 | swallow the trailers frame, ending the body instead | `H2Body::poll_frame` | **killed** | rows 5, 7, 10, 14, 16, 18 — 11 of 15 tests |
| M4 | write without reserving capacity (h2's own unbounded buffering) | `Pump::poll`, two sites | **killed** | row 12 |
| M5 | never release receive capacity | `H2Body::poll_frame` | **killed** | row 11 (by the ceiling, at 30 s) |
| M6 | return the pump's `Pending` as the body's | `H2Body::poll_frame` | **killed** | row 10 (by the ceiling) |
| M7 | overwrite a caller's `Accept-Encoding` | `decompress::negotiate` | **killed** | row 20 |
| M8 | never check a connection back in | `H2Body::hand_back_to_pool` | **killed** | rows 15, 16 |
| M9 | arm the idle sleep even with no `between_bytes` set | `IdleTimeout::poll_frame` | **killed** | row 21, arm A |
| M10 | drop the unfinished pump instead of moving it into `H2Body` | `http2::exchange` | **killed** | rows 10, 12 |
| M11 | collapse the request header map with `insert` | `http2::exchange` | **killed** | row 5 |
| M12 | never reuse a pooled connection | `http2::is_reusable` | **killed** | rows 15, 16 |
| **C1** | **`Pumped::Done` → `Pumped::PeerStoppedReading` at end-of-stream** | `Pump::poll` | **survived, as intended** | — |

**C1 is the control, and it is a real one.** Both call sites match
`Poll::Ready(Ok(_))`, so the variant is never read; the enum's own doc
comment says as much — *"the distinction buys no branch there — it is here
because the request is complete in one case and truncated in the other"*.
It looks behavioural, compiles, changes a value that flows through two
functions, and cannot be observed. A harness that reported it killed would
be reporting the twelve above without having run them.

## What this did not check

- **Real TLS.** The fixture's `TlsConnect` reports `h2` and encrypts
  nothing, so what is pinned is the transport's behaviour *given* a
  negotiated ALPN, not rustls's ability to negotiate one. That half is
  `http-ng-tls-rustls`'s, where it belongs, and `tests/http2.rs`'s module
  doc argues the technique at length.
- **A real gRPC server.** Everything here is measured against frames
  written by hand, which is what every other fixture in this workspace
  does and is the reason no dependency was added. An interop run against
  `tonic` or `grpc-go` would be a different kind of evidence and is not
  reachable from this test suite.
- **HTTP/3.** `http-ng-h3` speaks a protocol gRPC has a separate mapping
  for, and none of this transfers. `Capabilities::response_trailers` is
  `false` there for a real reason — that crate sends no request trailers at
  all — where on `http-ng-native` the same `false` is the HTTP/1.1 floor.
- **The `Capabilities` question a gRPC caller would actually ask.** With
  `http2` compiled in, `full_duplex` and `response_trailers` still report
  the floor — the value that holds on HTTP/1.1, because Cargo unifies
  features and a library cannot know who turned h2 on. So a caller cannot
  ask `capabilities()` whether trailers and duplex will work; the honest
  route is `RequireVersion(HTTP_2)` (`version_select` is `true`, and the
  demand is enforced before the head) plus `Response::version()` after the
  fact. Rows 7 and 10 pass through the same client that reports `false` for
  both fields, which is the floor rule behaving exactly as designed and is
  worth knowing before reading the declaration as a contradiction.
