# v0.4 — WebTransport datagrams, and the four links that had to hold first

`docs/v04-w2-webtransport.md` §6 listed six things left undone and gave
each what it would need. Datagrams were first on that list, and the note
under them was a fact about a crate rather than about this stack:
*"`h3-quinn` has a `datagram` feature and `h3-datagram` exists."* This
document is that note taken apart into the four things it does not say,
each answered by measurement, and the crate that came out of it.

The short version: **a datagram can leave this stack, and it does.** It is
not the same finding one feature over — `h3` 0.0.8's client *can* announce
`SETTINGS_H3_DATAGRAM`, where it cannot announce WebTransport — and
neither `h3-datagram` nor `h3-quinn`'s `datagram` feature is used, for a
reason that is measured in §3 rather than argued.

## 1. The premise, in four links

Each link is a separate question and three of them are about somebody
else's code. A green test at the end proves the chain; these are what the
chain is made of, so that a break can be located rather than bisected.

**(a) The QUIC connection carries datagrams.** RFC 9221's DATAGRAM
extension is negotiated by the `max_datagram_frame_size` transport
parameter, and quinn derives it from one config field:
`TransportParameters::new` sets `max_datagram_frame_size:
config.datagram_receive_buffer_size.map(..)`
(`quinn-proto-0.11.16/src/transport_parameters.rs`), and
`TransportConfig::default` sets that field to `Some(STREAM_RWND)`
(`config/transport.rs`). So datagrams are on unless a caller turns them
off.

That matters here because this crate does not build the connection. The
crate that would, in this workspace, is `http-ng-h3` — read-only for this
task and checked rather than assumed: it touches `TransportConfig` in
exactly one place, `keep_alive_interval`
(`crates/http-ng-h3/src/lib.rs`), and with `without_keep_alive()` it does
not construct one at all. Every other field, including both datagram
buffer sizes, is quinn's default. **A connection made by `http-ng-h3`
carries datagrams**, and nothing in this task had to ask it to.

It is still the caller's connection, so the crate reports the answer
rather than assuming it: `Session::max_datagram_size` is `None` when the
connection cannot carry one.

**(b) The client can announce the setting — and this is where datagrams
and WebTransport part company.** `h3::client::Builder::enable_datagram`
exists (`src/client/builder.rs`), writes `config.settings.enable_datagram`,
and `config.rs` puts it on the wire as `SettingId::H3_DATAGRAM`. Compare
`docs/v04-w2-webtransport.md` §3a: `enable_webtransport` is on the
**server** builder only and `Config::settings`' fields are `pub(crate)`, so
the draft's client-side MUST is unsatisfiable from outside `h3`. **The
brief asked for this to be checked and to stop if the answer were the
same. It is not the same**, which is why there is an implementation below
rather than a second finding.

**(c) The peer's answer is readable.** RFC 9297 §2.1 forbids sending an
HTTP Datagram to an endpoint that did not send `SETTINGS_H3_DATAGRAM`, so
the precondition has to be checkable. `h3::config::Settings::enable_datagram()`
is a public getter, and `ConnectionState::settings()` is reachable under
the `i-implement-a-third-party-backend-and-opt-into-breaking-changes`
feature this crate already takes. Contrast §3c of the same document, where
`max_webtransport_sessions` has no getter and the corresponding check
therefore cannot be written.

**(d) None of it has to go through `h3`.** `h3` 0.0.8 contains no datagram
send or receive path at all — `grep -rn datagram src/` finds settings, two
builder setters and one doc comment, and nothing else. The transport is
`quinn::Connection::send_datagram` / `read_datagram`, on the connection
this crate already holds, and the HTTP/3 framing is one variable-length
integer.

**Executed, not only read.** `crates/http-ng-webtransport/tests/webtransport.rs`
carries five datagram tests against `h3`'s own server on a real socket,
and §4 of this document has the run against an implementation that shares
no code with `h3`.

## 2. What has to be on the wire

Three specifications stack, and the third adds nothing:

- **RFC 9221** puts an opaque payload in a QUIC DATAGRAM frame.
- **RFC 9297 §2.1** makes that payload an HTTP/3 Datagram: a *Quarter
  Stream ID* as a QUIC variable-length integer, then the HTTP Datagram
  Payload. The Quarter Stream ID is "the value of the client-initiated
  bidirectional stream that this datagram is associated with divided by
  four".
- **draft-ietf-webtrans-http3** makes the HTTP Datagram Payload the
  WebTransport datagram, unchanged. No context ID, no length, no type
  byte.

So the whole wire format is `varint(session_id >> 2) || payload`.

**A stream and a datagram name the same session differently**, and the two
encodings live three lines apart in `src/lib.rs`. A WebTransport stream
begins `varint(0x41) || varint(session_id)` — the *full* stream ID. A
datagram carries the ID shifted right by two. Getting the shift wrong
addresses the datagram to a session that is not this one, and there is no
error anywhere on the path: the peer drops it, because RFC 9297 tells it
to.

**Two independent implementations agree on this and were read before
anything was written.** `wtransport-proto` 0.7.2's `Datagram::write` emits
`put_varint(qstream_id)` then the payload, with `QStreamId::from_session_id`
defined as `session_id >> 2`. `h3-datagram` 0.0.2's `Datagram::decode`
reads a varint and multiplies by four. Neither carries a context ID.

## 3. Why not `h3-datagram`, measured

`h3-quinn`'s `datagram` feature is `dep:h3-datagram` and nothing else, so
the question is entirely about that crate. It is **not** a dependency-graph
question: `h3-datagram` 0.0.2 depends on `bytes`, `h3` and
`pin-project-lite`, all three already in this crate's graph, so taking it
would have cost **one** crate. It is a correctness question.

**`h3-datagram` 0.0.2 writes a Quarter Stream ID of zero, always.**
`Datagram::encode` encodes the varint into a local `buffer` and then builds
its `EncodedDatagram` from a **freshly zeroed array**, discarding what it
just encoded:

```rust
let mut buffer = [0; VarInt::MAX_SIZE];
let varint = VarInt::from(self.stream_id) / 4;
varint.encode(&mut buffer.as_mut_slice());
EncodedDatagram {
    stream_id: [0; VarInt::MAX_SIZE],   // <- not `buffer`
    len: varint.size(),
    ...
}
```

Read like that it is a guess; run, it is a fact. A spike outside the
workspace (`/tmp/dgspike`, `h3-datagram = "0.0.2"`) drained the `Buf` it
returns and compared it with `h3`'s own `VarInt::encode` of the same
quarter:

| session stream | quarter | `h3-datagram` wrote | RFC 9000 §16 says |
|---|---|---|---|
| 0 | 0 | `00 70 61 79…` | `00 70 61 79…` — **same** |
| 4 | 1 | `00 …` | `01 …` |
| 8 | 2 | `00 …` | `02 …` |
| 400 | 100 | `00 00 …` | `40 64 …` |
| 1000000 | 250000 | `00 00 00 00 …` | `80 03 d0 90 …` |

The first row is the trap: **a session on stream 0 is correct by
accident**, and a WebTransport session usually *is* the first
client-initiated bidirectional stream on a fresh connection. It is the
same degeneracy `docs/v04-w2-webtransport.md` §8 records for M4, met from
the other side — which is why the tests here also take stream 0 out of
circulation before establishing the session, and why the interop spike
does too.

Rows three and four are worse than a wrong session: a two-byte header
written as `00 00` decodes as a one-byte Quarter Stream ID of zero
followed by a payload with a stray `00` on the front.

A second thing, read rather than run: `DatagramSender::send_datagram`
holds the `SharedState` that carries the peer's SETTINGS and never
consults it, so RFC 9297 §2.1's "MUST NOT send unless received" is not
enforced there either.

Against that, this crate already owns the QUIC varint for the stream
header, for the reason on `put_varint` — *"the two bytes this crate puts
in front of every stream are the whole of its wire format"* — and the
datagram header is the same two lines with a shift in front.

## 4. Proved against a peer that shares no code

`docs/v04-w2-webtransport.md` §1 established the session premise twice,
and the second time is the one that counted: `wtransport` 0.7.2 carries
its own HTTP/3 and depends on `h3` not at all. The same standard applies
here, and the same spike was extended rather than replaced.

Measured output, `/tmp/wtinterop`, `wtransport = "0.7"` with
`self-signed`:

```
server on 127.0.0.1:34942
CLIENT: QUIC up
SERVER: extended CONNECT arrived: authority="127.0.0.1:34942" path="/spike"
SERVER: accepted, session id 4
SERVER: max datagram size Some(1381)
CLIENT: session established, id 4 (quarter 1)
CLIENT: max datagram payload Some(1413)
SERVER: datagram "ping" (4 bytes)
CLIENT: echo = "echo:ping"
CLIENT: ok
```

Three things in that transcript are the point. The session is on **stream
4**, not 0 — the spike opens and resets one bidirectional stream first —
so the quarter is 1 and the shift is genuinely exercised rather than
hidden by `0 >> 2 == 0`. The payload we wrote was decoded by
`wtransport-proto`'s decoder, and the echo was encoded by
`wtransport-proto`'s encoder and decoded by ours: **both directions of the
header crossed an implementation boundary.** And `cargo tree -e normal -i
h3` in the spike shows `h3`'s only roots are ours.

**The spike stays out of the workspace**, for §10's reason unchanged: 114
crates, `url` and the ICU stack among them. It is reproducible from the
description above plus `docs/v04-w2-webtransport.md` §10's two warnings
(`wtransport`'s `local_addr` reports `[::]:port`, which
`quinn::Endpoint::connect` refuses).

## 5. The shape, and five decisions

```rust
Session::max_datagram_size(&self) -> Option<usize>
Session::send_datagram(&self, payload: Bytes) -> Result<(), Error>
Session::recv_datagram(&self) -> impl Future<Output = Result<Bytes, Error>>
```

**`send_datagram` is not `async`, and that is the shape rather than an
oversight.** There is no flush to await and no delivery to wait for; a
datagram that does not fit in the send buffer displaces an older one,
which is the trade a UDP socket makes and the reason the call can answer
immediately. It matches `quinn::Connection::send_datagram` and
`wtransport::Connection::send_datagram`, both of which are also
synchronous, for the same reason.

**Nothing is spawned, and what that costs is said rather than hidden.**
`recv_datagram` is the caller's own future and there is no demultiplexer
task, no queue of ours and no `Datagrams` stream type. A caller that stops
calling it stops receiving, and quinn's receive buffer drops the oldest —
the same shape as the WebSocket keep-alive's *"a caller that stops polling
gets no keep-alive"*, and the same reason: the session is the caller's
object, and the QUIC connection is driven by the endpoint driver quinn
already runs.

**`SETTINGS_H3_DATAGRAM` is not a third condition on the establishment
gate.** The draft does ask a WebTransport server for all three settings,
but a server can honestly send two — `h3`'s own server builder can be
configured into exactly that state — and refusing the session would charge
a caller who only ever opens streams for a feature it never used. So the
gate is unchanged at two flags, the third decides `max_datagram_size`, and
`a_peer_without_h3_datagram_gets_a_session_and_refuses_datagrams` pins it
by opening a stream on such a session and getting bytes back.

**One `None` for two causes, two variants for the same two.**
`max_datagram_size` answers `None` whether the peer's SETTINGS said no or
the QUIC connection carries no datagrams, because the question it asks —
*may I send, and how much* — has one answer in both cases. This is
deliberately not the shape `docs/v04-w2-webtransport.md` §1 calls the
sharpest fact, where two states of a three-state question were collapsed:
there is no third state here, and no *"has not answered yet"* to hide,
because SETTINGS have already arrived by the time a `Session` exists.
`DatagramsUnavailable` has the two variants, because at the point of a
send the reason is actionable — a caller who owns the endpoint can fix the
second and can do nothing about the first.

**The connection's own answer is quinn's, and is asked exactly once.** The
first version of `send_datagram` checked `max_datagram_size()` for `None`
and raised `NotOnTheConnection` itself. That branch is unkillable: quinn
answers the same connection with `UnsupportedByPeer`, which this crate
maps to the same variant, so no test can tell the two paths apart. It was
removed rather than left with a comment — the check that remains is the
size, which is the only thing this crate knows better than quinn, because
it knows the header it is about to add.

## 6. Loss, and what is honestly testable

Datagrams may be lost, reordered and duplicated. That is the feature, and
it means a test asserting one always arrives is asserting something the
protocol refuses to promise. What the suite claims is therefore narrower
than it looks, and worth saying exactly:

- **Asserted: what arrives.** The Quarter Stream ID the server read is
  this session's stream ID divided by four; the payload is the caller's
  bytes unchanged; a payload of exactly the budget is accepted by the wire
  and one byte more is refused before it. These are claims about content,
  and every one of them is checked on the far side of a real socket by a
  decoder written independently of the encoder.
- **Not asserted: delivery.** Nothing here says a datagram sent will
  arrive. The tests do wait for one, and that wait is a fact about
  loopback with a single datagram in flight and no congestion — not about
  WebTransport. The `ARRIVAL` constant is a guard against a hang, not a
  bound anything claims: a datagram is unacknowledged, so nothing the
  client can observe proves one is still in flight, and a `recv_datagram`
  that never resolved would otherwise take the whole test binary with it.
- **Not asserted: ordering** — with one exception, named where it is
  taken. `a_datagram_for_another_session_is_discarded` needs the two
  rejects to arrive before the echo, and gets that because the fixture
  sends all three from one task in immediate succession, so quinn packs
  them into a single QUIC packet on loopback. That is a property of the
  fixture's path, not of the protocol. It was checked rather than assumed:
  mutations D7 and D8 both die on the *content* assertion in 0.03 s, not
  on the arrival timeout, which is what a lost race would have looked
  like.

`Server::wait_for_datagrams` is the one helper in this fixture with no
causal alternative, and its doc says so. Every other ordering the suite
relies on is causal: the echo exists only because the server read the
datagram, and the server records it before echoing.

## 7. The dependency graph, measured

`cargo tree -p http-ng-webtransport -e normal --prefix none`, unique
crates, this tree:

| | before datagrams | after |
|---|---|---|
| crates | 49 | **49** |
| `quinn` features | `[futures-io]` | `[futures-io]` |
| `ring` | absent | absent |
| `tokio` | `[bytes, default, io-util, sync]` | unchanged |

**Datagrams cost nothing in the graph**, which is a consequence of §3
rather than a separate virtue: the transport is a method on a
`quinn::Connection` this crate already holds, and the framing is fifteen
lines it already had. Taking `h3-quinn/datagram` would have added one
crate and the encoder of §3.

`cargo deny --all-features check`: **bans ok, licenses ok, sources ok**.
The advisories check could not run in this environment — `github.com` is
unreachable from it, so `cargo-deny` could not fetch the RustSec database;
nothing in this change adds a dependency, so the advisory set is the one
`main` already carries.

## 8. Mutations

Anchor verified before the first and after the last: **14 tests, 14
passed** (7 before this work). Restore is `git checkout` plus an explicit
`os.utime`, `--no-fail-fast`, and the harness re-runs the anchor at the end
and refuses to report if it does not come back —
`crates/http-ng-webtransport/mutations.py`.

**Twenty-three mutations, twenty-one killed, two controls survived.** The
eleven from `docs/v04-w2-webtransport.md` §8 were re-run rather than
assumed, because the datagram code shares `put_varint` and the session ID
with them.

**All twenty-three were re-run again after the capsule protocol landed**,
and three gained killers — D9 most of all, from **one** test to ten,
because a capsule type is a *two*-byte varint where this session's Quarter
Stream ID is one. The current table is
[`docs/v04-w2-capsules.md`](v04-w2-capsules.md) §9, forty mutations with
three controls.

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1 | `Protocol::WEB_TRANSPORT` → `CONNECT_UDP` | killed | 2 tests |
| M2 | `Method::CONNECT` → `GET` | killed | 2 tests |
| M3 | stream signal `0x41` → `0x42` | killed | the bidi stream test |
| M4 | session ID `into_inner()` → `index()` | killed | **4** tests — was 1 |
| M5 | varint short branch `1<<6` → `1<<7` | killed | 3 tests — was 2 |
| M6 | the SETTINGS gate always passes | killed | both gate tests |
| M7 | the gate takes `\|\|` instead of `&&` | killed | both gate tests |
| M8 | the peer's SETTINGS are never awaited | killed | **10** of 14 |
| M9 | `!resp.status().is_success()` → `false` | killed | the refusal test |
| M10 | our own `enable_extended_connect(true)` → `(false)` | killed | the premise test |
| M11 | **control** — `Vec::with_capacity(16)` → `Vec::new()` | **survived, as intended** | nothing; an allocation hint |
| D1 | the Quarter Stream ID is the stream ID, unshifted | killed | 3 datagram tests |
| D2 | the Quarter Stream ID is hard-coded zero | killed | 3 datagram tests |
| D3 | the peer is taken to support datagrams whatever it announced | killed | `a_peer_without_h3_datagram…` |
| D4 | the peer is taken to support none, whatever it announced | killed | 4 tests |
| D5 | the budget check never fires | killed | the budget test |
| D6 | the header is not subtracted from the budget | killed | the budget test |
| D7 | another session's datagram is delivered as this one's | killed | the discard test |
| D8 | a frame too short to name a session is delivered as payload | killed | the discard test |
| D9 | the decoder reports a one-byte header whatever it read | killed | `get_varint_reads_back…` |
| D10 | `varint_len` disagrees with `put_varint` on a one-byte value | killed | `varint_len_agrees…` |
| D11 | our own `enable_datagram(true)` → `(false)` | killed | the premise test |
| D12 | **control** — the text of an error's `Display` | **survived, as intended** | nothing; the suite downcasts to types, never matches strings |

Every mutant compiled, so no verdict is a build failure wearing a kill's
clothes.

Five of them are worth a sentence.

**M4 got three new killers, and that is the datagram work paying back into
the session work.** Before, exactly one test could tell `into_inner()` from
`index()`. Now every datagram test can, because a datagram addressed with
`index() >> 2` reaches nobody — the session ID is load-bearing twice over
and in two different encodings.

**D1 and D2 die three times each and take ten seconds doing it**, which is
the arrival guard working as designed. With the shift wrong, the datagram
we send is recorded under the wrong Quarter Stream ID *and* the echo comes
back addressed to a session we are not listening for, so the content
assertion and the arrival timeout both fire. A test that only sent would
have caught the first; a test that only received would have caught the
second; the round trip catches both and says which.

**D6 could only be killed by the wire.** The budget is our own number, so
a budget one byte too generous is self-consistent — `send_datagram` would
accept the payload it computed, and `DatagramTooLarge` would carry the
number it computed. What refuses it is quinn, one layer down, when the
header pushes the frame past the real limit. That is why
`the_datagram_budget_is_the_payload_the_wire_accepts` asserts the
**success** of a payload of exactly the budget and treats the refusal
above it as a bracket.

**D9 and D10 are killed only by unit tests, and that is the point of
having them.** Both mutants are invisible over the wire for this session:
its Quarter Stream ID is 1, one byte long, so a decoder that always
reports one byte and a length function that says two are both harmless
until a session lands past stream 252. The corpus tests fail on RFC 9000
§A.1's own examples instead of waiting for that connection.

**D11 could only be killed by looking at our own SETTINGS**, exactly like
M10, and for a sharper reason. The fixture sends datagrams from raw `quinn`
and consults no HTTP/3 setting before doing so — as a real peer would not,
either, since nothing on the wire punishes a server that ignores the
client's flag. So the announcement RFC 9297 §2.1 requires has no
observable consequence in this suite at all, and the only thing standing
between it and silent deletion is the assertion that reads it out of
`h3`'s server-side state.

## 9. What is not done, and what is not verified

**`H3_DATAGRAM_ERROR` is not raised.** RFC 9297 §2.1 makes a received
Quarter Stream ID above `2^60 - 1` a *connection* error of type
`H3_DATAGRAM_ERROR`. `recv_datagram` drops it instead, along with every
other ID that is not this session's, because it cannot be this session's
and the RFC's other arm — "SHALL either drop that datagram silently or
buffer it temporarily" — covers the case this crate can actually
distinguish. Escalating would mean `quinn::Connection::close` with an
HTTP/3 error code from a method whose caller asked to receive one
datagram, and it would tear down a session over a peer's malformed
unreliable packet.

**RFC 9297 §2.1's other MUST cannot be honoured from here, and this is a
finding rather than a shortcut.** An endpoint that sends
`SETTINGS_H3_DATAGRAM = 1` MUST also have sent the `max_datagram_frame_size`
transport parameter. This crate sends the setting unconditionally in
`Session::connect`, and it **cannot check the second half**: the connection
belongs to the caller, and quinn exposes no accessor for the *local*
transport parameters — `Connection::max_datagram_size` reports the peer's
limit, not what we advertised. A caller who built an endpoint with
`datagram_receive_buffer_size(None)` and then opened a session here would
announce a setting it cannot honour. quinn's default satisfies it, so the
common case conforms; closing the gap needs either an accessor in quinn or
an argument this crate has no honest default for.

**Untested, each with what it would take:**

- **Loss, reordering and duplication.** One datagram at a time on
  loopback. Nothing here has met a lossy path, and `recv_datagram` has no
  behaviour that depends on one — but neither has it been shown to
  survive a duplicate.
- **Anything at volume.** No test sends more than one datagram at once,
  and quinn's receive buffer dropping the oldest — the consequence of
  spawning nothing — has never actually dropped anything.
- **Payloads at the MTU boundary over time.** The budget is read once and
  moves with the path MTU estimate; the test that uses it reads it and
  sends immediately.
- **A browser.** Still true from §11 of the WebTransport document, and
  still the first thing that would test the client's SETTINGS against an
  implementation that has opinions about them.
- **More than one session per connection.** Unchanged: a `Session` owns
  the h3 client, so there is one, and the mismatch arm of `recv_datagram`
  therefore has exactly one subject. A second session on one connection is
  what would make that arm a demultiplexer rather than a filter.
