# The 0-RTT flake was a defect, older than the branch that recorded it

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
  is parked in the A–B gap, and stays parked: the credit it is waiting for can
  only come from a stream the server has discarded.

So the fixture takes a `stream_receive_window`
(`server::start_two_sharing_a_certificate_and_a_tiny_window`), and
`crates/http-ng-h3/tests/live.rs` carries the test, next to the sibling that
covers the same rejection arriving one stream later.

### 3.1 That was half of it, and the other half was found the same way

The window fixes the **end** of the A–B gap. It does nothing about the
**start**, and the test's own premise — that there was an early-data control
stream at all — turned out to be the same race one step back: *nothing stops
the rejection arriving before A*. There is no `await` between `into_0rtt()`
and h3's first `poll_open_send`, but there is a thread, and a kernel that can
take its core away for a millisecond; quinn's connection driver runs on
another one. When it loses that race, `zero_rtt_rejected`'s counter reset
means h3 opens ordinary 1-RTT streams, the write completes, and nothing fails
at all.

Measured, twice: the 50 ms delay of §2.1 is that ordering forced, and it makes
both 0-RTT tests pass on the **unfixed** library; and the test as first
written — window only — failed **3 times in 246 concurrent suite runs**, each
time on its premise (`dialled() == 1`), never on the property.

The second fixture is [`wire::Wire`], the UDP relay `tests/zero_rtt.rs`
already uses, holding the server's flight until the relay has seen a **0-RTT
packet from the client**. That is not a longer window; it removes the window.
While the flight is held the client's connection *cannot* learn of the
rejection, so whenever the scheduler next gives it a core it opens early-data
streams — and a 0-RTT packet exists only to carry application data sent before
the handshake completed, so seeing one *is* "A has happened and the write has
started". The release is on that event and the duration is only a backstop,
which is the rule that file's own `release` doc had already argued for a
different ordering: *"releasing on the event removes the race rather than
shrinking it."*

The premise is then asserted rather than assumed — `watcher.join()` returns
whether early data was ever seen — so a run in which the client quietly did an
ordinary handshake fails a line instead of passing.

**The fixture being wrong twice is worth its own line**, because both were
the same mistake in different places: a fact that the code makes *likely* was
asserted as though the code made it *certain*. First `accepted()`, which the
server increments only after a handshake the client tears down immediately (1
failure in 280 runs); then the premise itself. The `dialled()` counter and the
hold are each the causal form of the thing that was being hoped for.

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


## 4. The fix, and why it is at this layer

`H3::connect` now dials **at most twice**, and the second dial does not take
the 0-RTT shortcut. The dial itself moved into `H3::dial(key, addr, launched,
early)`, whose `early` is deliberately *not* `key.early_data`: the key says
what kind of connection this is and is what a later request must match to
reuse it, while the parameter says whether **this attempt** may put anything
into early data.

```rust
let launched = mark::<H, R>(&self.rt);
if key.early_data {
    match self.dial(key, addr, launched, true).await {
        Ok(made) => return Ok(made),
        Err(DialFailed::EarlyDataLost(_)) => {}
        Err(DialFailed::Fatal(e)) => return Err(e),
    }
}
self.dial(key, addr, launched, false).await.map_err(DialFailed::into_error)
```

**It is the first row of `crate::early`'s own table, one step later.** That
row — `into_0rtt()` handing the `Connecting` back for want of key material —
already falls through to a full handshake, and its sentence holds here
verbatim: *"nothing was sent, so falling through to a full handshake risks
nothing and tells the caller nothing."* Nothing had been sent. The request has
not been formed at this point, let alone written, so this is **not a retry**
and needs no `RetryKind` — the same distinction `http-ng-native` draws with
*"this is not a second request, it is the first one, which never left."*

Four decisions inside it are worth reading, because three of them are the
places a wider fix would have gone wrong.

**The condition is "we took the shortcut and the h3 client could not be built
on it", and not the 0-RTT verdict.** Awaiting `quinn::ZeroRttAccepted` looks
like the precise question and is not: it resolves `false` for a rejection
*and* for a connection lost any other way, because `terminate` sends `false`
on `on_connected` when it has not already fired
(`quinn-0.11.11/src/connection.rs:1224`). A condition whose two arms cannot be
told apart is one no test can pin, so `dial` asks the question it can answer —
`zero_rtt.is_some()`, which is exactly *this connection's streams went out in
early data*.

**Exactly one extra dial, and only for a marked request.** Every other failure
returns as it always did. The bound is not what is at stake — `stage` wraps
this whole function in `within_connect`, so `Timeouts::connect` covers both
dials and cannot be doubled by it. What a wider condition would cost is a
second attempt that is a *deterministic repeat of the first*: the same
arguments to the same `connect_with`, the same peer refusing the same
handshake, spending a caller's bound to arrive at the error it already had.
`live::a_connect_that_put_nothing_in_early_data_is_not_dialled_twice` is that
half, and it needs a fixture of its own — `Behaviour::CloseOnAccept`, the one
server here that makes `build` fail with no early data anywhere near it.

**One mark for both attempts.** `ConnectTiming::tcp` is *the attempt*, and an
attempt that spent a discarded early-data connection spent it. This is the one
decision that was expected to be untestable and turned out not to be — see M6
below.

**The discarded connection is announced to nobody.** `Connected` is emitted by
`H3::stage`, after `checkout` returns, and the fallback lives beneath it in
`connect` — so a caller counting connections still sees one per request rather
than one it can never use again. `Closed` is not emitted either, and that is
the same rule `http-ng-wasi` reaches from the far end of the workspace: the
discarded connection never had a `ConnectionId`, and *a `Closed` built from one
would announce the end of a connection whose beginning was never announced*.
`hooks::a_0_rtt_connection_lost_to_the_rejection_reports_one_connection_and_no_close`
pins both, with `b.accepted() == 2` beside them so that "one `Connected`" is
not also what a transport that never fell back would report.


## 5. Mutation testing

Anchor **213 tests**, `cargo nextest run -p http-ng-h3 -p http-ng-select
--all-features --no-fail-fast`, verified before the run and after every
restore. Each patch had to match exactly once. Restores are `git checkout`
followed by `os.utime`, and the tests were committed before the first mutation.

**Six applied: five killed, one survived and is recorded below; one control
survived as intended.**

| # | mutation | verdict | killed by |
|---|---|---|---|
| **M1** | the fallback is not taken — a 0-RTT rejection that killed the control stream is handed to the caller, i.e. **the defect itself** | **killed** (2) | `h3::live::a_0_rtt_rejection_on_the_control_stream_is_not_the_callers_either`, `h3::hooks::a_0_rtt_connection_lost_to_the_rejection_reports_one_connection_and_no_close` |
| **M2** | a build failure on a connection that **did** go out in early data is classified fatal | **killed** (2) | the same two |
| **M3** | a build failure on a connection that did **not** go out in early data is classified recoverable, so a failing connect is dialled twice | **killed** (1) | `h3::live::a_connect_that_put_nothing_in_early_data_is_not_dialled_twice` |
| **M4** | the fallback dial takes the 0-RTT shortcut again — `early` ignored, the key's flag decides | **killed** (2) | the same two as M1 |
| **M5** | every failure of a marked request's dial is recoverable (`From<Error> for DialFailed` yields `EarlyDataLost`), so a failing connect is dialled twice | **survived** | nothing — see below |
| **M6** | the fallback gets its own mark, so `ConnectTiming::tcp` reports only the second attempt | **killed** (1) | `h3::hooks_cost::the_same_two_requests_with_a_hook_do_read_it` |
| **C1** | **CONTROL** — `Staged`'s `Debug` reports nothing instead of what it holds | **survived, as intended** | nothing, and nothing should: no test asserts on that `Debug` and no code path reads it. Without a control, five kills would be indistinguishable from a harness that reports "killed" unconditionally |

Three are worth reading twice.

**M6 was expected to survive and did not, which is the best kind of surprise.**
The claim it attacks is a `Duration` — that `ConnectTiming::tcp` covers both
attempts — and this workspace has learned three times over that asserting on
one turns a one-bit question into a benchmark. What killed it is
`hooks_cost::the_same_two_requests_with_a_hook_do_read_it`, which counts
**clock reads**: a second `mark::<H, R>` is a second `rt.now()`, and a test
built to prove that a hook costs a clock read notices one that nothing asked
for. A cost test standing in for a correctness one is the same shape as
`docs/v04-staged-connect.md` §5's S6, where a DNS test caught a timeout
arithmetic error.

**M5 survived, and the honest statement is that it is close to unobservable
rather than merely untested.** The variant it changes is only ever consulted
for a request the caller marked, and the failures it would widen to are the
ones before `build`: the crypto configuration, the endpoint, `connect_with`,
and the full handshake on `into_0rtt`'s refusal path. Every one of them is a
deterministic function of arguments the second dial would repeat exactly, so
the caller gets the identical error either way; what a fixture would have to
observe is the extra dial, and a dial that fails before the handshake is one
the server never counts. The bound cannot be doubled (§4). The blast radius is
therefore one wasted dial on a connect that was already failing — recorded
rather than dressed up, and the reason the `From` impl carries a comment
saying the recoverable variant is claimed at exactly one site.

**The first scoring run of this harness was wrong, again, and differently.**
`docs/v04-staged-connect.md` §5 records a run in which every mutation came
back "survived" because `--no-fail-fast` was missing. This one had the flag
and still scored seven survivals, because the regex scraping nextest's `FAIL`
lines did not allow for the `(61/213)` progress counter — and for its
right-aligned form, `( 24/213)`, which is a *second* bug in the same regex and
was caught only after the first was fixed. The harness now cross-checks the
number of names it scraped against nextest's own `N failed` on the `Summary`
line and refuses to score a run where they disagree. **A mutation harness with
no self-check reports what it wishes were true**, and the check that catches it
has to be one it cannot satisfy by scraping nothing.


## 6. Reproduction rate, before and after

One recipe throughout: **8 concurrent `cargo nextest run -p http-ng-h3
--all-features` processes** on a 28-core host, each run's complete output kept
on disk, on the same machine and against the same suite.

| | runs | failures with `H3_CLOSED_CRITICAL_STREAM` |
|---|---|---|
| before the fix | **277** | **2** — `live::a_rejected_0_rtt_request_is_replayed_and_the_caller_never_sees_it`, both times |
| deterministic reproduction, before the fix | 2 | **2** — and both 0-RTT tests, including the one the record names |
| after the fix | **526** | **0** |

The 526 is two campaigns, 280 and 246, and neither is clean of *everything*:
the first cost 1 failure and the second 3, all four in the new tests' own
premise and none in the library — §3.1. Both fixture defects are fixed and a
third campaign follows them.

**What this is not.** 526 runs at 2-in-277 would be about four expected
failures, so the absence is evidence and not proof; what carries the claim is
§3 and §5, where the failure is produced on demand and six mutations are
scored against it. The rate is here because a fix that made the flake rarer
rather than absent would show up in it.


## 7. What is not verified

- **That this is the only place a 0-RTT rejection can be surfaced.** Three
  streams go out in early data — the control stream and QPACK's two — and h3
  tolerates a failure of the QPACK pair (`.ok()`) while treating the control
  stream as critical. A rejection landing on a *request* stream is
  `H3::finish`'s replay and is covered. Nothing here enumerates the rest of
  `h3`'s setup.
- **The 1-RTT half of the same window.** After a rejection quinn has discarded
  the SETTINGS the client thought it sent, and h3 does not resend them, so a
  connection that survives a rejection — which is exactly the sibling test's
  replay path — speaks HTTP/3 to a server that never received its SETTINGS
  frame. RFC 9114 §7.2.4.2 permits a peer to proceed without it and both
  servers here do; that this is *benign* is an observation about two
  implementations rather than a claim about the protocol, and no test pins it.
- **M5**, §5.
- **Anything outside `http-ng-h3`.** `http-ng-select` was in the mutation
  anchor because it drives `H3::connect` through `StagedConnect`, but no test
  there marks a request for early data, so the fallback is unexercised from
  that side.
