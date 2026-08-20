# v0.4 — the capsule protocol, and telling a close from a disappearance

`docs/v04-w2-webtransport.md` §6 left two things undone and described them
as separate:

> **The capsule protocol** — `CLOSE_WEBTRANSPORT_SESSION` and
> `DRAIN_WEBTRANSPORT_SESSION`. […] a real one carries an application error
> code and a reason string on the CONNECT stream, and needs the *reading*
> half below to observe the peer's.
>
> **Observing the session's end.** Nothing polls the CONNECT stream after
> the response, so a peer that closes the session is not noticed until a
> stream operation fails.

They are one thing, and that sentence — *"needs the reading half below"* —
is the join. This document is the premise taken apart first, the crate that
came out of it, and the two places where somebody else's code turned out to
be the finding.

The short version: **a capsule can leave and a capsule can arrive**, both
on the CONNECT stream this crate has held since v0.4 W2 without reading it.
`Session::close(code, reason)` writes one, `Session::closed()` reads the
peer's, and the difference between `Ok` and `Err` is the whole feature.

## 1. The premise, in three links

The datagram work broke into four links each measured separately, and one
of them — `h3-datagram`'s encoder — turned out to be wrong in a way that
reading nearly missed. The same method applies here, and it produced the
same shape of result: two of the three links are about somebody else's
code, and the third is a crate that exists, is named for exactly this, and
does not contain it.

### (a) The CONNECT stream is still ours, and it is still writable

A capsule travels on the CONNECT stream, so the first question is whether
`h3` left one to write to. It did, and the answer is visible in the struct
this task started from: `Session` held
`_connect: h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>`,
underscored because *"their whole job is to not be dropped"*.

What `h3` 0.0.8 offers on that value after `recv_response` has returned:

| method | bound | what it does |
|---|---|---|
| `send_data(B)` | `S: quic::SendStream<B>` | writes **one DATA frame** around the bytes |
| `poll_recv_data(cx)` | `S: quic::RecvStream` | hands back the next DATA frame's payload |
| `split()` | `S: quic::BidiStream<B>` | two halves that can be driven independently |
| `finish()` | `S: quic::SendStream<B>` | FIN — and a GREASE frame first, see §5 |

None of it is gated on the response not having been received, and — this
is the load-bearing part — **none of it needs the h3 connection driver.**
`connection::RequestStream::poll_recv_data` reads through its own
`FrameStream` straight off the `quinn::RecvStream`; the driver owns the
*control* stream and nothing else. So a crate that holds the driver without
polling it, which is exactly what this one does, can still read and write
the CONNECT stream. That is why observing a session's end needed no driver
and no spawn, and it is the one place this task's answer differs from
`docs/v04-w2-webtransport.md` §6's guess that it *"needs a driver — and
that is the one place a future version might have to spawn"*. It does not.

`split()` is used, because closing and being closed happen at times nobody
coordinates: the send half goes behind one lock and the receive half behind
another (§5).

### (b) The DATA framing is `h3`'s and the capsule is not

RFC 9297 §3.2 carries capsules in the payload of HTTP/3 DATA frames, so the
framing splits in two at exactly the seam `h3` already provides.
`send_data` writes the DATA frame; what goes *inside* it is a capsule type,
a capsule length — both QUIC variable-length integers — and the value.

**`h3` 0.0.8 has no capsule code at all.** Measured rather than assumed:

| crate | version | `grep -rni capsule src/` |
|---|---|---|
| `h3` | 0.0.8 | **one line**, a doc comment on the `H3_DATAGRAM_ERROR` code: *"Datagram or capsule parse error"* |
| `h3-datagram` | 0.0.2 | **nothing** |
| `h3-webtransport` | 0.1.2 | **nothing** |

The third row is the one worth stopping on. `h3-webtransport` is
`hyperium/h3`'s own WebTransport crate — the name promises precisely this
feature — and it contains `lib.rs`, `server.rs` and `stream.rs` and no
capsule of any kind. It would also have cost `h3-datagram` (the crate whose
encoder `docs/v04-w2-datagrams.md` §3 measured writing a Quarter Stream ID
of zero), plus `tokio` and `tracing`, as *direct* dependencies.

### (c) A capsule can arrive, because the receive half is ours and something polls it

The reading half needed two things that are not the same: the receive half
of the CONNECT stream must still exist, and something must poll it.

The first is (a). The second is the caller: `Session::closed()` is the
caller's own future, and nothing is spawned — the same trade
`Session::recv_datagram` makes and the same one
`hclient-tungstenite`'s keep-alive makes. **A caller that never awaits
it never learns the session ended.** That is stated where the method is,
because it is the cost of the shape rather than a defect in it: a session
is the caller's object, and the QUIC connection is driven by the endpoint
driver `quinn` already runs.

## 2. What has to be on the wire

Two specifications stack, and the second adds three fields:

- **RFC 9297 §3** — a capsule is a *Capsule Type* (varint), a *Capsule
  Length* (varint) and a *Capsule Value*, repeated for as long as the
  message body lasts. §3.2 requires a receiver to **silently skip** a
  capsule type it does not know, which is possible only because the length
  is in front of the value.
- **draft-ietf-webtrans-http3 §5** — `CLOSE_WEBTRANSPORT_SESSION` is
  capsule type `0x2843`, and its value is a 32-bit *Application Error
  Code*, big-endian, followed by an *Application Error Message* of at most
  **1024 bytes** of UTF-8.

So the whole wire format is
`varint(0x2843) || varint(4 + reason.len()) || u32be(code) || reason`,
inside a DATA frame, followed by the CONNECT stream's FIN.

**The FIN is not decoration**, and this is the sentence the whole feature
turns on:

> Cleanly terminating a CONNECT stream without sending a
> `CLOSE_WEBTRANSPORT_SESSION` capsule SHALL be semantically equivalent to
> terminating it with a `CLOSE_WEBTRANSPORT_SESSION` capsule that has an
> error code of 0 and an empty error string.

A bare FIN is therefore a **clean close with zeroes**, not the absence of
one — which is why "did a capsule arrive" is the wrong question and
`Session::closed()` answers a different one. An *abrupt* end — the stream
reset, the connection lost — is the other side, and it is the side a
caller could previously not tell from either.

**Two independent implementations agree on the format and both were read
before anything was written.** `wtransport-proto` 0.7.2's
`CAPSULE_TYPE_CLOSE_WEBTRANSPORT_SESSION` and `web-transport-proto` 0.6.0's
`CLOSE_WEBTRANSPORT_SESSION_TYPE` are the same number, both read four
big-endian bytes and then UTF-8, and `wtransport-proto` enforces the same
1024-byte limit. Neither shares code with the other, and neither shares any
with `h3`.

## 3. Why the capsule codec is fifty-nine lines here, measured

There is exactly one crate in the ecosystem that would supply this:
**`web-transport-proto` 0.6.0**, and unlike `h3-datagram` its encoder is
**correct** — checked by running it, not by reading it (§4). Cost is the
whole argument, and it is a number:

| | crates | notes |
|---|---|---|
| `hclient-webtransport` today | **49** | `quinn` with `futures-io` alone, no `ring` |
| `web-transport-proto` 0.6.0 alone | **48** | **ten** of them `url`, `idna` and the ICU stack |

`cargo tree -p web-transport-proto -e normal --prefix none | sort -u`, in a
scratch crate outside this workspace. It would roughly double the graph of
a crate whose whole story is that it owns no endpoint, and it would bring
back the exact dependency `hclient-proto` spent a task removing —
`docs/icu-ecosystem-survey.md`. `sfv` and `tokio/io-util` come with it.

Against that: this crate already owns the QUIC varint, for the reason on
`put_varint` — *"the two bytes this crate puts in front of every stream are
the whole of its wire format"* — and RFC 9297's capsule header is that
varint twice. What it does not already own is fifty-nine lines: eleven to
encode, six for what a decoded capsule can be, twenty-five to take one off
a buffer and seventeen to read a close out of it.

## 4. Proved against peers that share no code

Three runs, all outside the workspace in `/tmp/wtinterop` for
`docs/v04-w2-webtransport.md` §10's reason unchanged: `wtransport` is 114
crates. The spike is in §10 here, to be re-run rather than believed.

### 4.1 The codec, against `web-transport-proto` 0.6.0

Their encoder, run on the same four inputs this crate's unit tests pin:

```
THEIRS 0x00000000 ""                     -> 68 43 04 00 00 00 00
THEIRS 0x00000001 "x"                    -> 68 43 05 00 00 00 01 78
THEIRS 0x12345678 "so long, and thanks"  -> 68 43 17 12 34 56 78 73 6f 20 6c 6f 6e 67 2c 20 61 6e 64 20 74 68 61 6e 6b 73
THEIRS 0xffffffff "the whole thing"      -> 68 43 13 ff ff ff ff 74 68 65 20 77 68 6f 6c 65 20 74 68 69 6e 67
```

Byte for byte what `close_capsule` writes. Those four rows are now the
corpus of `close_capsules_match_two_other_implementations`, **measured
first and pinned second** — the technique `hclient-proto`'s 96-pair URI
corpus uses. The first row is additionally `wtransport-proto` 0.7.2's own
unit-test vector (`Frame::new_data(vec![104, 67, 4, 0, 0, 0, 0u8])`), so
the smallest capsule is checked against two crates that do not know about
each other.

### 4.2 A close capsule, on a socket, decoded by `wtransport` 0.7.2

```
server on 127.0.0.1:35837
SERVER: extended CONNECT arrived: authority="127.0.0.1:35837" path="/spike"
SERVER: accepted, session id 0
CLIENT: session established, id 0
CLIENT: close capsule sent, code 0x12345678 reason "so long, and thanks"
SERVER: session ended: connection closed by peer: so long, and thanks (code 305419896)
```

`305419896` is `0x12345678`. The code and the reason both survived a
decoder written by somebody who has never seen this crate, and
`wtransport`'s driver classified it as `ApplicationClosed` rather than as a
transport failure — which is the classification the whole feature is
about.

### 4.3 A dropped session, decoded as zeroes

The draft's *"semantically equivalent"* clause, checked against the same
independent implementation rather than asserted from the text:

```
CLIENT: session dropped without a capsule
SERVER: session ended: connection closed by peer: 0
```

Code 0, no reason — `wtransport`'s `IoReadError::ImmediateFin` arm, which
carries the draft's sentence as a comment above it.

### 4.4 The other direction: a connection that vanishes

```
SERVER: QUIC connection closed, no capsule
CLIENT: unclean end, kind Body: Body: Connection error: Remote error: ApplicationClose: 0x9
```

`wtransport::Connection::close` closes the **QUIC connection**, not the
session, and `Session::closed()` reports it as `Err` — the distinction, from
the far side of a real socket against a peer that has no capsule to send.

### 4.5 Two findings about the peers, neither patched around

**Neither implementation measured here *sends* a `CLOSE_WEBTRANSPORT_SESSION`
capsule.** Both decode one — `wtransport` 0.7.2 in
`src/driver/streams/connect.rs`, `web-transport-quinn` 0.8.1 in
`src/session.rs` — and both close the QUIC connection when their own caller
asks to close a session. So the receive direction could not be exercised
against either over a socket, and what stands in for it is §4.1's encoder
plus the in-tree fixture, which writes its capsule headers with `h3`'s own
`VarInt` (a third implementation) and whose payloads the tests spell out
byte by byte rather than parse.

**`wtransport::Connection::closed()` does not report a session close.** It
awaits `quinn::Connection::closed()`, so a session ended by a capsule shows
up there as `LocallyClosed` — measured, and it cost a debugging pass in the
spike before `tracing` showed the driver had in fact read
`ApplicationClosed(305419896, …)` correctly all along. The end is reachable
through any driver operation (`accept_bi`) and not through the method named
for it. That is worth recording precisely because it is the confusion this
task removes on our side: a session's end and a connection's end are
different events, and an API that answers one when asked the other is how
a caller stops trusting the answer.

## 5. The shape, and six decisions

```rust
Session::close(&self, error_code: u32, reason: &str) -> Result<(), Error>
Session::closed(&self) -> Result<SessionClose, Error>
pub struct SessionClose { pub code: u32, pub reason: String }
pub enum BadCloseCapsule { NoErrorCode{..}, ReasonTooLong{..}, ReasonNotUtf8, Truncated{..} }
pub struct AlreadyClosed;
```

**`Ok` against `Err` is the distinction, and it is not a new vocabulary.**
An unclean end is `ErrorKind::Body`, deliberately agreeing with
`hclient-fetch`'s treatment of a `wasClean == false` WebSocket close and
with `hclient-tungstenite`'s `PongNotReceived`, and deliberately not
`ErrorKind::Timeout`, since no `Timeouts` field is in force on an open
session.

**A bare FIN is `Ok`, not a third state.** Draft §5 says the two are
semantically the same, so a `SessionClose { code: 0, reason: "" }` is the
honest report and a separate variant would be a distinction the wire does
not carry. This is deliberately *not* the shape
`docs/v04-w2-webtransport.md` §1 calls the sharpest fact — where a
two-state value answered a three-state question — because here the
specification is what collapses the states, not our reading of them.

**`close` does not consume the `Session`, and that was found by reading
`quinn`.** The obvious signature is `close(self)`: the session is over, so
take it away. It cannot be, and the reason is two `Drop` impls one under
the other. `h3::client::SendRequest::drop` marks the connection closed with
`H3_NO_ERROR`, and `quinn::ConnectionRef::drop` calls `implicit_close` when
the last handle goes — and quinn's own documentation is explicit that
closing *"does not ensure delivery of outstanding data"*. A consuming
`close` would therefore drop the whole connection in the same breath as
writing the capsule, and race its own bytes to the peer. So `close` leaves
the session standing, and the caller drops it when the peer has had it.

**The FIN is a `drop`, not `h3`'s `finish()`.** `quinn::SendStream::drop`
finishes the stream, so the FIN lands either way; `finish()` would
additionally write `h3`'s once-per-connection GREASE frame *after* the
capsule. Both peers measured here skip unknown frame types — `h3`'s
`FrameStream` explicitly, `wtransport`'s `ConnectStream` with a `continue`
— so it would have been harmless, and it is still a frame nobody needs to
be asked to ignore. The FIN is separately asserted, because it is a
separate line from the capsule: `a_close_capsule_carries_the_code_and_the_reason_to_the_peer`
fails if it is missing, and mutation **C5** is that line removed.

**Both methods take `&self`, and it costs two mutexes.** Every other method
on `Session` is `&self` because a `quinn::Connection` has interior
mutability; `h3`'s `RequestStream` does not. `&mut self` on either method
would stop a caller doing the one thing a session is for — waiting for the
peer's close *while* opening streams, which is `&self` and `&mut self` at
once and does not compile. Neither lock is held across an `await`:
`closed()` locks inside a `poll` and drops the guard before returning, and
`close()` **takes** the send half out from under its lock, which is also
what makes a second `close` an `AlreadyClosed` rather than a deadlock.

The two mutexes also made `Session` **`Sync`**, which it was not before —
`h3`'s `RequestStream` is not — and that is the property the `&self` is
*for* rather than a side effect: an `Arc<Session>` in two tasks is what
"wait for the peer's close while opening streams" actually looks like.
`a_session_can_be_spawned_and_shared` asserts both auto-traits, and it
lives in `tests/` rather than beside the code because
`scripts/no-send-or-sync-in-the-core-surface.sh` scans `crates/*/src` and
demands a `send-bound-exception: amendment-C…` marker on every declared
`Send` or `Sync` bound it finds. Those markers name a spec amendment that
excuses a **seam** bound; none of them excuses a test, and spending one on
this would be the marker convention lying about what it excuses. A test
directory is not the core surface.

**A second `close` is refused rather than silently `Ok`.** `close` carries
an application error code the peer acts on, so answering `Ok` to a second
call with a *different* code would tell a caller that code reached the peer
when nothing did. There is one capsule per session because the stream it
travels on is finished by the first.

**An over-long reason is refused before a byte leaves, and the limit is one
type in both directions.** `BadCloseCapsule::ReasonTooLong` is raised by
`close` about the caller's reason and by `closed` about the peer's, because
it is one sentence of the draft rather than two facts. Refusing rather than
truncating is not politeness: `wtransport` 0.7.2 answers
`ErrorCode::Datagram` to an over-long reason and its driver turns that into
a **connection** error, so sending one would turn a clean close into the one
outcome a clean close exists to avoid.

## 6. What is not a new flag, and why

`close` deliberately does **not** stop `open_bi` or `send_datagram`
afterwards. A `closed: bool` would be true when *we* closed and false when
the **peer** did — because the peer's close is only ever noticed by a caller
who awaited `closed()`, and nothing forces them to — so it would be a guard
with one of its two cases missing. That is the shape this crate deleted
`BadSessionUri::NoAuthority` for, and the shape `Capabilities::upgrade`'s
four variants had. What actually refuses a stream opened after a close is
the peer, which is where a session's state really lives.

## 7. Deliberately not done, with what each would need

- **`DRAIN_WEBTRANSPORT_SESSION`.** Read and skipped, along with every
  other unknown capsule type, which is what RFC 9297 §3.2 requires of a
  receiver anyway. It is not surfaced because a drain is **not an end**:
  `closed()` is a future that resolves once, when the session is over, and
  a drain has no honest place in it. Surfacing it needs a second
  observation channel — a `poll_next`-shaped one, or a callback — and that
  is a shape this crate has none of, since `Session` is a multiplexer
  rather than a stream. The one thing it would buy a caller is *stop
  opening new streams*, and nothing here opens streams on its own.
- **Sending a capsule other than the close.** No producer: this crate has
  no drain to send either, for the same reason.
- **`H3_DATAGRAM_ERROR`-style escalation on a malformed capsule.**
  `closed()` reports `BadCloseCapsule` and leaves the connection alone.
  RFC 9297 §3.3 allows a receiver to treat an unparseable capsule as a
  protocol error; doing so would mean closing the QUIC connection from a
  method whose caller asked one question, and would take every other
  session on that connection with it — which is hypothetical here only
  because §7 of the WebTransport document says there is one.
- **`GOAWAY`.** Unchanged and unrelated: it arrives on the h3 **control**
  stream, which is the driver's, and the driver is held rather than
  polled. The CONNECT stream being readable says nothing about it.
- **Server-initiated streams** and **more than one session per
  connection.** Unchanged; `docs/v04-w2-webtransport.md` §3b and §4b.

## 8. The dependency graph, measured

`cargo tree -p hclient-webtransport -e normal --prefix none`, unique
crates, this tree:

| | before capsules | after |
|---|---|---|
| crates | 49 | **49** |
| `quinn` features | `[futures-io]` | `[futures-io]` |
| `ring` | absent | absent |
| `tokio` | `[bytes, default, io-util, sync]` | unchanged |

**Capsules cost nothing in the graph**, for the same reason datagrams did:
the codec is fifty-nine lines beside the varints that were already here, and the
layer under it is a method on a stream this crate already held. §3 is what
the alternative would have cost.

`cargo deny --all-features check`: **advisories ok, bans ok, licenses ok,
sources ok** — the advisory database was reachable from this environment,
unlike when `docs/v04-w2-datagrams.md` §7 was written.

## 9. Mutations

Anchor verified before the first and after the last: **34 tests, 34
passed** (14 before this work). Restore is `git checkout` plus an explicit
`os.utime`, `--no-fail-fast`, and the harness re-runs the anchor at the end
and refuses to report if it does not come back —
`crates/hclient-webtransport/mutations.py`.

**Forty mutations, thirty-seven killed, three controls survived.** The
twenty-three from `docs/v04-w2-datagrams.md` §8 were re-run rather than
assumed, because the capsule code shares `put_varint`, `varint_len` and
`get_varint` with them.

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1 | `Protocol::WEB_TRANSPORT` → `CONNECT_UDP` | killed | 2 tests |
| M2 | `Method::CONNECT` → `GET` | killed | 2 tests |
| M3 | stream signal `0x41` → `0x42` | killed | the bidi stream test |
| M4 | session ID `into_inner()` → `index()` | killed | 4 tests |
| M5 | varint short branch `1<<6` → `1<<7` | killed | **4** tests — was 3 |
| M6 | the SETTINGS gate always passes | killed | both gate tests |
| M7 | the gate takes `\|\|` instead of `&&` | killed | both gate tests |
| M8 | the peer's SETTINGS are never awaited | killed | **25** of 34 — was 10 of 14 |
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
| D9 | the decoder reports a one-byte header whatever it read | killed | **10** tests — was 1 |
| D10 | `varint_len` disagrees with `put_varint` on a one-byte value | killed | `varint_len_agrees…` |
| D11 | our own `enable_datagram(true)` → `(false)` | killed | the premise test |
| D12 | **control** — the text of an error's `Display` | **survived, as intended** | nothing; the suite downcasts to types |
| C1 | the capsule type is `0x2844` | killed | 9 tests |
| C2 | RFC 9297 §3's Capsule Length is not written | killed | 7 tests |
| C3 | the error code goes out little-endian | killed | 5 tests |
| C4 | the reason is not written, only its length claimed | killed | 6 tests |
| C5 | the capsule goes out and the CONNECT stream is never finished | killed | 3 tests |
| C6 | an over-long reason is sent rather than refused | killed | the limit test |
| C7 | the limit is `>=`, so exactly 1024 is refused | killed | the limit test |
| C8 | a second `close` answers `Ok` for a capsule that never left | killed | the double-close test |
| C9 | **a reset stream and a lost connection are reported as a clean close** | killed | 3 tests |
| C10 | a bare FIN is reported as unclean, against draft §5 | killed | 2 tests |
| C11 | every capsule type is read as a close | killed | 2 tests |
| C12 | the peer's error code is read little-endian | killed | 6 tests |
| C13 | the Capsule Length is ignored on receipt | killed | `take_capsule_reads_back…` |
| C14 | the end is read again rather than remembered | killed | both memory tests |
| C15 | each DATA frame is taken for a whole capsule | killed | the split-capsule test |
| C16 | the Capsule Length leaves out the error code's four bytes | killed | 7 tests |
| C17 | **control** — `Vec::with_capacity(..)` → `Vec::new()` in `close_capsule` | **survived, as intended** | nothing; an allocation hint |

Every mutant compiled, so no verdict is a build failure wearing a kill's
clothes.

**The second-order result is the point of re-running the old rows, and it
is bigger than the datagram work's.** Three of them gained killers:

- **D9** goes from **1 test to 10.** `get_varint` reporting a one-byte
  header whatever it read used to be visible only to a unit test, because
  this session's Quarter Stream ID is 1 — one byte long — so the mutant was
  correct by accident on the wire. A capsule type is `0x2843`, a **two**-byte
  varint, so every capsule test now reads a header whose length the mutant
  gets wrong, and the payload arrives one byte short with a header byte on
  the front.
- **M8** goes from 10 of 14 to **25 of 34**, which is arithmetic rather than
  insight: almost every new test establishes a session first.
- **M5** goes from 3 to 4, for D9's reason from the encoding side: `0x2843`
  is over `1<<6`, so a short branch that took one value too many would put
  a one-byte capsule type on the wire.

Five of the new ones are worth a sentence each.

**C9 is the feature.** With a reset stream and a lost connection reported as
a clean close, every other test in the suite still passes: the session still
ends, `closed()` still resolves, and a caller counting sessions still counts
right. Three tests fail, and all three are about *which* end it was.

**C10 is C9's mirror, and it exists because the obvious fix for C9 is
wrong.** "Report an error unless a capsule arrived" kills C9 and breaks
draft §5: a bare FIN is a clean close with zeroes, and
`a_bare_fin_is_a_clean_close_with_zeroes` is the line that says so.
`a_close_capsule_carries_the_code_and_the_reason_to_the_peer` fails too,
because the fixture answers our capsule with a bare FIN of its own.

**C5 could only be killed by the fixture noticing an absence.** The capsule
is written and the stream is never finished, so every byte on the wire is
correct and only the FIN behind them is missing. What fails is the fixture
waiting for the client's half to end — which is also what makes the send
tests causal, so the mutation and the causality are the same line of the
fixture read twice.

**C13 and C15 are the two halves of "a DATA frame is not a capsule".** C13
ignores the Capsule Length and reads to the end of the buffer; C15 throws
the buffer away between frames. Neither is visible in a session whose peer
sends exactly one capsule in exactly one frame, which is what a first test
would have written — so `a_capsule_split_across_two_data_frames_is_one_capsule`
cuts at **two bytes**, inside the capsule type itself, and
`take_capsule_reads_back_what_close_capsule_wrote` appends a second
capsule's worth of bytes behind the first.

**C7 is the boundary, and it needed the test to close at exactly 1024.**
A limit test that only sends 1025 bytes passes with `>` and with `>=`.

## 10. The interop spike, for re-running

Outside the workspace (`/tmp/wtinterop`), so that 114 crates and 48 more do
not enter this one:

```toml
[dependencies]
hclient-webtransport = { path = "…/crates/hclient-webtransport" }
http = "1"
quinn = { version = "0.11", default-features = false, features = ["futures-io", "ring", "runtime-tokio", "rustls-ring"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
tokio = { version = "1", features = ["full"] }
wtransport = { version = "0.7", features = ["self-signed"] }
web-transport-proto = "0.6"
bytes = "1"
```

Four things it does, in order: encode the four capsules of §4.1 with
`web_transport_proto::Capsule::encode` and print them; run
`Session::connect` + `Session::close(0x12345678, "so long, and thanks")`
against a `wtransport::Endpoint::server` and print what the server's driver
says; do the same with the session **dropped** instead of closed; and close
the `wtransport` connection out from under a session and print what
`Session::closed()` answers.

Four things to know before repeating it, three of which cost time here:

1. `wtransport`'s `local_addr` reports `[::]:port`, which
   `quinn::Endpoint::connect` refuses as `InvalidRemoteAddress`, so the
   port has to be re-paired with `127.0.0.1`
   (`docs/v04-w2-webtransport.md` §10's first warning, unchanged).
2. **`wtransport::Connection::closed()` is the wrong method**, §4.5: it
   awaits the QUIC connection and reports `LocallyClosed` for a session
   ended by a capsule. Use any driver operation — `accept_bi()` — whose
   error is `ConnectionError::ApplicationClosed`.
3. `wtransport::Identity` is not `Clone` in 0.7.2, so the certificate has
   to be taken out of it before the `ServerConfig` builder consumes it.
4. When in doubt, `tracing_subscriber::fmt().with_env_filter("wtransport=trace")`
   — its driver logs `Ended with error: ApplicationClosed(ApplicationClose
   { code: …, reason: … })` at `DEBUG`, which is how (2) was found.

## 11. What is not verified

- **A browser.** Unchanged from `docs/v04-w2-webtransport.md` §11 and
  still the first thing that would test this against an implementation
  with opinions. All four engines' `WebTransport` has a `close({closeCode,
  reason})` and a `closed` promise, which is this pair of methods with
  different names, and none of them was run against this.
- **Receiving a capsule from an implementation that is not `h3`.** §4.5:
  neither `wtransport` 0.7.2 nor `web-transport-quinn` 0.8.1 *sends* a
  `CLOSE_WEBTRANSPORT_SESSION` capsule — both close the QUIC connection
  instead — so there was no third-party encoder to receive one from over a
  socket. What stands in for it is §4.1's encoder run offline, and the
  in-tree fixture, whose capsule headers come from `h3`'s `VarInt` and
  whose payloads the tests write out byte by byte.
- **A capsule larger than one DATA frame's worth of anything.** The split
  test cuts a capsule in two on purpose, but the largest capsule any test
  sends has a 1028-byte value — four of error code and the limit's 1024 —
  and no test sends one across more than two frames.
- **Anything at volume, and any interleaving.** No test sends a capsule
  while a stream or a datagram is in flight, so the claim that `close` and
  `closed` take `&self` *so that* they can run beside `open_bi` is
  supported by the type system and not by a test that does it.
- **Cancellation.** Dropping the `close` future after `send_data` has
  started is untested; the send half is already taken by then, so the
  session ends by the drop's FIN — a clean close with zeroes rather than
  the code the caller asked for. That is a consequence of the shape rather
  than a measured behaviour.
- **`Session::closed()` from two callers at once.** The lock makes it
  sound, and the memory in `CloseWatch` makes it consistent, but nothing
  exercises two futures on one session.
