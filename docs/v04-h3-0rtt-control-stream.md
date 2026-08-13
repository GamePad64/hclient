# The 0-RTT flake was a defect, and it is older than the branch that
# recorded it

`docs/v04-staged-connect.md` §"The flake this branch may have introduced"
recorded a failure of
`http-ng-h3::hooks::a_replayed_0_rtt_request_reports_one_head_and_one_connection`
twice, could not reproduce it in 41 further runs, and said the honest thing:
*"`H3::execute` was refactored in this branch into `stage` + `finish`, so 'not
reproduced' is a weaker statement than 'not there'."*

It is there. This document is the capture, the cause, the deterministic
reproduction, and the fix.

**It is not the refactor's.** `H3::connect` — the function the defect is in —
was last touched by `8ac693a` (*"the transport says what it did"*, the hooks
work), **85 commits before** the staged-connect branch, and the 0-RTT shortcut
in it arrived with `c96a02f` (*"actually take the 0-RTT round trip, and replay
a rejection"*, v0.3), **292 commits back**. `stage`/`finish` moved the call
site and changed nothing inside it. The record was right to be suspicious and
wrong about the suspect.


## 1. The capture

Nobody had one. The recipe that produced two in an afternoon is the one the
earlier h3 campaign found and this document repeats because it is the whole
technique: **concurrency, not repetition**, and **keep every run's output**
rather than grepping for a `FAIL` line.

Eight concurrent `cargo nextest run -p http-ng-h3 --all-features` processes on
a 28-core host, each run's complete stdout+stderr written to its own file.
**2 failures in 277 runs of the whole suite.** Both failures are one test each,
and both say the same thing:

```
FAIL [   0.248s] (61/74) http-ng-h3::live a_rejected_0_rtt_request_is_replayed_and_the_caller_never_sees_it

thread 'a_rejected_0_rtt_request_is_replayed_and_the_caller_never_sees_it' panicked at
crates/http-ng-h3/tests/live.rs:275:10:
a rejected 0-RTT request is replayed, not surfaced: Error {
    kind: Connect,
    source: Custom { kind: Other, error:
        "Local error: Application { code: H3_CLOSED_CRITICAL_STREAM,
          reason: \"an error occurred on the control stream 0-RTT rejected\" }" },
}
```

The test that fails is the **sibling** of the one the record names, not the one
the record names — `tests/live.rs`'s rather than `tests/hooks.rs`'s. That is
luck rather than a distinction: the two run the same fixture over the same code
path within a second of each other, and §3 makes **both** fail, on the same
line, with the same string.

`Error { kind: Connect, source: Custom { kind: Other, .. } }` is the whole
diagnosis in one value, because exactly one site in this crate builds that
shape — `crates/http-ng-h3/src/lib.rs`, the `h3::client::builder().build(..)`
call, the only `ErrorKind::Connect` here whose source is a **string**:

```rust
.map_err(|e| Error::new(ErrorKind::Connect, std::io::Error::other(e.to_string())))?
```

Every other `ErrorKind::Connect` in the crate carries a `quinn::ConnectionError`
itself, and `"Local error: {:?}"` is `h3::error::ConnectionError`'s own
`Display` (`h3-0.0.8/src/error/error.rs:165`).


## 2. The cause

`H3::connect` takes the 0-RTT shortcut and then builds an h3 client on what it
gets back:

```rust
let (conn, zero_rtt) = if key.early_data {
    match connecting.into_0rtt() {
        Ok((conn, accepted)) => (conn, Some(accepted.shared())),
        Err(connecting) => (connecting.await?, None),   // path 1: nothing risked
    }
} else { .. };

let (mut driver, send) = h3::client::builder()
    .build(h3_quinn::Connection::new(conn.clone()))
    .await?;                                            // ← here
```

`build` is `h3::connection::ConnectionInner::new`. It opens three
unidirectional streams and then writes SETTINGS on the first of them:

```rust
let (control_send, qpack_encoder, qpack_decoder) = (
    future::poll_fn(|cx| conn.poll_open_send(cx)).await,   // A
    ..
);
..
let (control, ..) = future::join3(
    stream::write(&mut self.control_send, WriteBuf::from(UniStreamHeader::Control(settings))),
    ..                                                    // B
).await;
```

On a connection handed back by `into_0rtt()` those are **early-data streams**.
When the server refuses the early data, `quinn_proto::Connection` calls
`streams.zero_rtt_rejected()` (`quinn-proto-0.11.16/src/connection/mod.rs:2556`),
which *removes* every locally opened stream and resets the next-stream-id
counters to zero (`connection/streams/state.rs:223`). A write blocked on such a
stream is woken with `WriteError::ZeroRttRejected`, whose `Display` is
`0-RTT rejected`.

h3 is then obliged by RFC 9114 §6.2.1 — *"If either control stream is closed at
any point, this MUST be treated as a connection error of type
H3_CLOSED_CRITICAL_STREAM"* — and it obeys, **closing the QUIC connection**
(`h3-0.0.8/src/connection.rs:236`) and returning
`ConnectionError::Local { .. }` from `build`.

So the failure is: **the server rejected the early data while the h3 client was
setting itself up on it, and the caller was handed the rejection as a
`Connect` error.**

That is precisely the outcome this crate promises cannot happen.
`crates/http-ng-h3/src/early.rs` states three 0-RTT failure paths and says two
of them are handled here — *"the server rejected the 0-RTT keys → replayed on
the same connection once the handshake completes; the caller sees a normal
response"*. The replay lives in `H3::finish` and it is real; what it covers is
a rejection that lands on the **request** stream. A rejection that lands on the
**control** stream, one step earlier, has never had a handler, and there is no
request yet for `finish` to replay.

### 2.1 The window is between A and B, and that is why it is rare

Measured, not deduced. A 50 ms delay inserted between `into_0rtt()` and
`build()` — the obvious place to look — **does not reproduce it**: both 0-RTT
tests pass, every time. The reason is in `zero_rtt_rejected()` above: it sets
`next[dir] = 0`, so an h3 client that opens its streams *after* the rejection
opens ordinary 1-RTT streams and everything works.

The window is therefore the gap **between the stream being opened (A) and the
control-stream write completing (B)**, both inside `h3`, and on loopback it is
a few microseconds wide against a handshake round trip. It is reachable at all
only because quinn's connection driver runs in its own task: under load the
task calling `build` is descheduled inside that gap, the driver processes the
server's handshake flight, and the rejection lands in the middle of h3's setup.

Two failures in 277 concurrent suite runs is what that costs.


## 3. The deterministic reproduction

The window is a scheduling accident, so a test that waits for one is the flake
again. What makes it deterministic is to make the **write** in B block on
something that a rejection resolves and nothing else does: the peer's
**flow-control window for unidirectional streams**.

`quinn::TransportConfig::stream_receive_window` is
`initial_max_stream_data_uni` (`quinn-proto-0.11.16/src/transport_parameters.rs:160`),
and a client resuming with 0-RTT uses the value it remembered from the ticket.
Set it to 8 bytes on the server that issues the ticket and h3's SETTINGS write
— a stream-type varint plus a SETTINGS frame, some tens of bytes — cannot
complete in one go:

- on an **ordinary** connection the server's h3 layer reads the control stream,
  `MAX_STREAM_DATA` arrives and the write finishes. Nothing changes;
- on a **0-RTT** connection to a server that refuses the early data, the write
  is parked exactly in the A–B gap when the refusal lands. Every time.

So the fixture takes a `stream_receive_window`, and
`crates/http-ng-h3/tests/zero_rtt.rs` carries the test.

Reproduction rate, `-E` on the two 0-RTT tests, on the code as it stood:
**2 of 2, in every run** — with the identical assertion and the identical
error string as the captured flake, including on
`hooks::a_replayed_0_rtt_request_reports_one_head_and_one_connection`, the test
the record names:

```
FAIL http-ng-h3::live  a_rejected_0_rtt_request_is_replayed_and_the_caller_never_sees_it
FAIL http-ng-h3::hooks a_replayed_0_rtt_request_reports_one_head_and_one_connection

crates/http-ng-h3/tests/hooks.rs:922:37:
replayed, not surfaced: Error { kind: Connect, source: Custom { kind: Other, error:
  "Local error: Application { code: H3_CLOSED_CRITICAL_STREAM,
    reason: \"an error occurred on the control stream 0-RTT rejected\" }" } }
```

**The 8-byte window is a stand-in for a descheduling, and it is honest about
being one.** It does not invent a failure the code does not have: it holds the
write open across the instant that already breaks it, which is the same instant
the two captured failures were killed at, reached by a mechanism a test can
schedule instead of hope for.
