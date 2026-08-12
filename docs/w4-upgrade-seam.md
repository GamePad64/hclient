# W4 — the upgrade seam, decided

`docs/v03-design.md` §W4 argued the shape. This document takes the
decisions, re-checks the facts they rest on against the versions in this
tree today, and answers a question §W4 did not have: whether the same seam
carries WebTransport.

Nothing here is code. It exists so that the implementation does not have to
re-derive any of it, and so that a later reader can tell which parts were
measured and which were argued.

## 1. The facts, re-verified today

§W4's argument rests on four claims about other people's source. All four
hold, checked against `hyper 1.11.0` and the tree as of this document.

| claim | verified |
|---|---|
| `Connection::poll_without_shutdown` and `into_parts` do not require `Send` on the IO | `src/client/conn/http1.rs`: the `impl<T, B> Connection<T, B>` carrying both is bounded by `T: Read + Write + Unpin`, `B: Body + 'static`, `B::Error: Into<Box<dyn StdError + Send + Sync>>`. `Send` appears only on the *error*, never on `T` |
| `hyper::upgrade::Upgraded` does require it | `src/upgrade.rs:66-67`: `pub struct Upgraded { io: Rewind<Box<dyn Io + Send>> }` |
| polling `Connection` to completion after a 101 destroys the upgrade and reports success | `src/client/conn/http1.rs:310-320`: `proto::Dispatched::Upgrade(pending) => { pending.manual(); Poll::Ready(Ok(())) }`, under hyper's own comment *"With no `Send` bound on `I`, we can't try to do upgrades here"* |
| nothing branches on `Capabilities::upgrade` | four variants (`None`, `H1`, `ExtendedConnect`, `Both`); every backend assigns `None`; the only reads are two assertions and one test fixture that sets `H1` to assert it back. No production code branches on it — the non-test matches for `.upgrade` in this workspace are all `Weak::upgrade` |

The third is the one to keep in mind while implementing: **"the exchange
finished" and "the upgrade was thrown away" are the same observation**, and
`crates/http-ng-native/src/h1.rs` polls `Connection` exactly that way. A
101 must be recognised by *status*, before the connection is polled to
completion. This was probed separately and is not a live defect today — a
101 does not poison the pool, because five independent places ask whether
the connection has finished — but that is an accident of the current code
rather than a guarantee, and it is written down in
`crates/http-ng-native/tests/switching_protocols.rs`.

## 2. Decision 1 — the seam is WebSocket, not "give me the socket"

**Message oriented: `Stream<Item = Result<Message>> + Sink<Message>`.**
A byte-stream seam ("hand back the socket after the 101") is implementable
by exactly one of this project's four backends, and the three it excludes
include the browser — the target whose inclusion is the whole claim.
`http-ng-fetch` says so where it sets the capability: *"`WebSocket` in the
browser is a wholly separate global, unreachable from a `fetch`-shaped
`Transport`"*. On Apple platforms `NSURLSessionWebSocketTask` is
message-framed too and hands back no bytes; `wasi:http` has an
`HTTP-upgrade-failed` error code and no mechanism at all.

So h1 upgrade is an **implementation detail underneath** the seam on
native, not the seam.

**A separate trait, not a method on `Transport`.** The reasoning is the one
already applied twice here. `QuicTlsConnect` is a separate trait from
`TlsConnect` because their method intersection is empty and an adapter
between them would type-check *with an empty body* — a failure mode worse
than a compile error. The same holds here. And `TcpAdoptStd` established
the other half: *"a backend that cannot adopt a `std::net::TcpStream`
should say so by not implementing the trait, which is already how the seam
expresses it"* (`docs/w7-embassy-research.md` §1.4).

**The seam therefore expresses itself by being implemented.** A backend
that can do WebSocket implements the trait; one that cannot does not, and
using it is a compile error rather than a runtime surprise.

## 3. Decision 2 — `UpgradeSupport` goes

Four variants, zero callers branching on them. v0.2's rule is explicit: *a
capability variant exists only if a caller decision turns on it.* With
WebSocket as a trait, no caller decision does.

The alternative was that it becomes real for a *different* reason — a raw
h1 upgrade or a `CONNECT` tunnel exposed to callers. That is a proxy
feature nobody has asked for, and inventing a user for a field in order to
keep the field is the reasoning this project rejects everywhere else.

**So: delete it — but not before the trait exists and a backend implements
it.** Deleting first would leave a window where a backend can neither
declare nor implement upgrade, and the acceptance documents would have to
describe a state that never shipped. That places it at step 4 below, beside
the `http-ng-fetch` implementation, because the field lives on every
backend's `Capabilities` and removing it touches all of them at once —
which is exactly the change that also gives the browser its own answer.

One consequence to handle rather than discover: `crates/http-ng/tests/
facade.rs` sets `UpgradeSupport::H1` on a test fixture and asserts it back.
That test is pinning the *plumbing* — that a backend's capability reaches
the facade — not upgrade itself. It needs a different field to carry the
same proof, not deletion.

## 4. WebTransport — a separate seam, and the spec's reasons are out of date

`docs/superpowers/specs/2026-08-05-http-ng-design.md` §5.7 concluded "we
don't write WebTransport", on two grounds. Both have moved.

**Ground 1 — "the client crates are server-only."** Still true of
`h3-webtransport` 0.1.2, and the working clients (`wtransport`,
`web-transport-quinn`) still carry their own HTTP/3 and are nailed to
`quinn/runtime-tokio`, which defeats this project's runtime seam. But the
implied conclusion — that WebTransport cannot be built on *our* h3 — does
not follow, because **extended CONNECT is reachable from `h3` 0.0.8's
client API today**: `Pseudo::request` reads `ext.get::<Protocol>()` when
the method is `CONNECT` (`h3-0.0.8/src/proto/headers.rs`, and the client
passes the request's extensions through at `src/client/connection.rs:163`),
`Protocol::WEB_TRANSPORT` is a public constant in `src/ext.rs`,
`SETTINGS_ENABLE_WEBTRANSPORT` is parsed in `src/config.rs`, and `h3-quinn`
has a `datagram` feature. The primitives are present.

**Ground 2 — implicitly, browser availability.** No longer an obstacle:
`WebTransport` shipped in Chrome 97 (2022-01), Firefox 114 (2023-06) and
**Safari 26.4 (2026-03-24)**. All four major engines have it.

**It is still a separate seam, and that is the durable reason.** A
WebSocket seam is one message channel: `Stream + Sink`. A WebTransport
session is a *multiplexer* — bidirectional and unidirectional streams
opened on demand in both directions, plus datagrams. The method
intersection with `Stream + Sink<Message>` is as empty as
`QuicTlsConnect`'s was with `TlsConnect`, and forcing one onto the other
would produce exactly the adapter that type-checks with an empty body.

What *is* shared is the pattern, and this is why the order matters:
WebSocket establishes that a non-request/response capability lives in its
own trait, expressed by implementation. Once that precedent is in the
tree, WebTransport is a second instance of it rather than a second
argument.

**The remaining work for WebTransport is the session layer** — streams
bound to a session id, demultiplexing incoming ones, and the capsule
protocol — and no crate provides it for a client under our runtime seam.
That is a real cost, and it is the honest reason to defer, unlike the two
above.

**Since built, in part, and the reading above was right** — v0.4 W2,
`crates/http-ng-webtransport`. The extended CONNECT leaves this stack and
is accepted both by `h3`'s own server and by `wtransport` 0.7.2, which
shares no code with it. What the reading did not say is in
[`docs/v04-w2-webtransport.md`](v04-w2-webtransport.md): `h3`'s **client**
can announce neither `SETTINGS_ENABLE_WEBTRANSPORT` nor
`SETTINGS_WT_MAX_SESSIONS`, so the announcement the draft requires of
clients does not go out; the peer's SETTINGS — which the draft makes a
precondition of sending the CONNECT — are reachable only behind `h3`'s
`i-implement-a-third-party-backend…` feature; and a session cannot share an
`http-ng-h3` pooled connection, because extended CONNECT is announced at
handshake time and `http-ng-h3` announces it nowhere.

## 5. Order of work

1. The `WebSocket` trait in `http-ng-core` — shape only, no backend.
2. `http-ng-native`: h1 upgrade underneath it, using
   `poll_without_shutdown` + `into_parts`, never `hyper::upgrade`; the 101
   detected by status before the connection is polled out.
3. `http-ng-fetch`: the browser's `WebSocket` global behind the same trait
   — the proof that the seam is the right shape, since this is the backend
   a byte-stream seam would have excluded.
4. `UpgradeSupport` deleted, `facade.rs`'s plumbing test re-pointed.

Steps 1 and 2 are one change: a trait with no implementation is a shape
nobody has tested. Step 3 is what makes it a seam rather than a native
feature with a trait in front of it.

## What this document does not decide

- ~~**Framing on native.**~~ **Decided: `tungstenite` 0.30 directly, no
  async wrapper** — see §6 below.
- **Whether the pool must change.** v0.2 W2 froze the pool without
  WebSocket, which the spec had asked to avoid. The cost is unmeasured.
- **Permessage-deflate**, subprotocol negotiation, and close-code
  semantics.


## 6. Framing on native — `tungstenite`, driven by us

§5.7 proposed `async-tungstenite` 0.35 on `futures_io`. Measured instead:

| crate, `default-features = false` | unique crates | tokio / hyper |
|---|---|---|
| `tungstenite` 0.30 | **17** | none |
| `async-tungstenite` 0.35 | 22 | none |

Neither pulls a runtime, so the five extra crates are the only cost — and
they buy glue this workspace cannot use. **`TcpConnect::Stream` is bounded
by `hyper::rt::Read + hyper::rt::Write + Unpin`**
(`crates/http-ng-rt/src/caps.rs:149`), not `futures_io`, so an adapter is
needed whichever crate is chosen. Given that, the choice is only about
which side the adapter faces.

**It faces `std::io`, and the reason is that it removes `unsafe`.**
`tungstenite::protocol::WebSocketContext` takes the stream as a *parameter*
— `read<Stream>(&mut self, stream: &mut Stream) where Stream: Read + Write`
— rather than owning it. So the persistent protocol state and the transient
IO can be separate values, and the shim handed to each call can borrow the
poll `Context` for exactly that call:

```rust
// sketch, not the implementation
ctx.read(&mut Shim { io: &mut self.io, cx })   // Shim: std::io::Read + Write,
                                               // WouldBlock on Poll::Pending
```

`tokio-tungstenite` and `async-tungstenite` cannot do this: their wrappers
*own* the stream across calls, so the `Context` has to be smuggled in as a
raw pointer stored in the wrapper. That is why `tokio-tungstenite`'s
`AllowStd` holds a `*mut Context`. Borrowing per call is the same trick
turned right way round, and it needs no `unsafe` — which matters here, where
`unsafe` is policed by CI and each use is argued in the spec's amendments.

This is also the technique this workspace already uses twice, for the same
reason both times: `h2`'s `Connection` is polled by hand because hyper's h2
executor is a sealed trait, and hyper's own h1 `Connection` is polled by
hand rather than spawned. Adopting a third party's runtime glue is the
thing this project avoids; adopting its **protocol state machine** is not.

What still has to be checked when the code is written, rather than assumed
here: that `WouldBlock` out of the shim leaves `WebSocketContext` in a
resumable state on every path (tungstenite documents `read` as never
blocking on write, which is the interesting one), and that a partial write
is not lost between polls.

## 7. The bound an open WebSocket has — decided

Steps 1–3 shipped with none. Only the handshake reads `Timeouts::connect`;
past it, a peer that vanishes without a `FIN` leaves a `Stream` that never
yields and never errors. This section decides what to do about it, before
any code.

**It is not `Timeouts`.** `total` is meaningless for a connection whose
whole point is to outlive the exchange that opened it, and `between_bytes`
would be actively wrong: silence is the normal state of a WebSocket, so a
gap bound would kill healthy connections. The need is not "bound the
transfer", it is **liveness** — is the peer still there — and RFC 6455 has
an answer for exactly that in ping/pong.

**It does not go on the seam.** `WebSocketConnect` is implemented by
`http-ng-fetch` too, and a browser has no `send(ping)` and no `onping` —
the same fact that kept `Ping`/`Pong` out of `Message`. A knob on the trait
that one backend silently could not honour would be a capability that lies,
and this project has caught that four times. Nor does it go in the
request's `extensions`: `AllowEarlyData` sets the precedent for a mark only
some transports honour, but there the fallback is a **degradation** (a
1-RTT request), while here it is **silence** — a caller who asked for
liveness detection and got none has no way to learn that.

**So it goes where the seam already puts everything else: on the backend
that can do it.** A configuration on `http-ng-native`'s WebSocket path, and
no counterpart on `http-ng-fetch`, so asking the browser for it does not
compile. Same rule as the trait itself, one level down.

**The mechanism, and the constraint that shapes it.** Nothing is spawned
here — the caller's `poll_next` is the only thing driving the socket. So
the ping can only be written when the caller polls, which means the shape
is: `poll_next` that has been `Pending` for the interval sends a `Ping`,
and a `Pong` that does not arrive within the deadline ends the stream with
an error.

That is the mirror of what HTTP/3 found and is worth stating in the same
words, because the two halves point opposite ways. There, **a spawned
driver was necessary and not sufficient**: driving a connection is what
lets it *send* a PING, not what makes it *decide* to, so `H3` had to set
`keep_alive_interval` itself. Here there is no driver at all, and the
consequence is the one this design accepts openly: **a caller that stops
polling gets no keep-alive.** That is defensible precisely because a caller
that is not polling is not waiting for anything — but it is a real
difference from h3, where a pooled connection is kept alive on behalf of
requests that have not been made yet.

**Two things must be true of the default.** It is off, because a default
that pings is a default that sends traffic nobody asked for — and on a
metered radio that is not free. And the error a missed pong produces must
be distinguishable from the peer closing, or a caller cannot tell "the
network went away" from "the server said goodbye"; `wasClean == false` is
already treated that way in the browser backend, and this should agree with
it rather than invent a second vocabulary.

**~~What this section does not decide~~ — both decided, in the writing.**
This paragraph left two questions to whoever implemented it, with tests.
Both are answered, the reasoning is next to the code
(`crates/http-ng-native/src/websocket.rs`'s module doc) and the answers
are pinned by `crates/http-ng-native/tests/websocket.rs`.

**No, an unanswered ping is not surfaced before its deadline.** The
`Stream` can yield two things and neither can carry it. `Message` has no
`Ping`/`Pong` variant and must not gain one — §2's own reason, that the
browser can neither send nor receive one — and an `Err` on this stream is
*terminal* by the seam's contract ("a `Stream` that has ended stays
ended"), so a warning delivered as one would break that contract for every
caller rather than only for those who asked for a keep-alive. A third
channel would be a second vocabulary for information nobody can act on:
the only action available on "the ping has not come back **yet**" is to
wait, which is what the deadline already does, and a pong arriving one
millisecond inside it is a healthy connection — an early signal would
report ordinary jitter as a fault. What a caller can read is the
configuration in force and the failure when it happens.

**The interval resets on any inbound frame; the deadline resets only on a
pong carrying that ping's own payload.** They are two clocks answering two
questions. The interval measures *silence*, and any frame at all is proof
the peer is there, so it restarts — which is what makes "off by default" a
bound on traffic rather than a slogan: **a busy connection sends no
keep-alive traffic at all**, where resetting only on a pong would make a
chatty socket ping for ever. The deadline measures *an unanswered probe*,
and only the answer RFC 6455 §5.5.2 makes a MUST answers it: data comes
from the peer's application, a pong comes from its WebSocket layer, and it
is the layer that must be alive for anything sent to be read. Letting any
frame clear it would turn the probe back into the gap bound this section
rejected, restricted to the window after a ping. The payload is matched
rather than the opcode accepted, because §5.5.3 allows *unsolicited* pongs
as a unidirectional heartbeat — a peer emitting one every second would
otherwise keep a probe permanently "answered" without ever answering it.

One consequence, stated rather than left to be discovered: both clocks are
polled only when the read side has nothing, since `Pending` is the only
moment `poll_next` has to poll them in. So the deadline cannot fire in the
middle of a stream of data — it fires after the deadline of *silence*
following a ping. That falls out of "`poll_next` is the only driver", and
it is what makes the paragraph above safe: a peer that answers a ping with
data and keeps talking is not killed, one that answers with data and then
stops is.

**A third thing the section did not raise and the implementation had to
answer: the keep-alive stops at our own close.** `tungstenite` refuses
every write once a close frame has gone out (`SendAfterClosing`) and RFC
6455 makes a ping after a close meaningless, so no probe follows one — a
probe already in flight keeps its deadline. The closing handshake is
therefore still unbounded, the same gap `poll_close` records for itself.

## 8. The framing belongs in its own crate — and the rejected seam is right one level down

`tungstenite` lives inside `http-ng-native` behind a `websocket` feature.
**That is the one pluggable thing in this workspace that is not its own
crate**, and it is inconsistent with the rule every other seam follows:
`http-ng-tls-rustls` and `http-ng-tls-native-tls` behind `TlsConnect`,
`http-ng-dns-system`/`-hickory`/`-doh` behind `Resolve`, `http-ng-rt-tokio`/
`-smol`/`-embassy` behind the runtime seams. Cargo's features are additive,
so a `websocket` feature on `http-ng-native` puts `tungstenite` into every
build in any graph that switches it on — which is the argument that kept
`http-ng-h3` out of `http-ng-native` and `http-ng-tls-quic` out of
`http-ng-tls`, applied to the one place it was not.

**The split is already most of the way there.** `NativeWebSocket<I, Tm>` is
generic over the IO and the clock; it names `Native` nowhere. What genuinely
needs `Native` is the other half: a connection of its own (its connector,
its TLS, `http/1.1` alone on the ALPN list, and deliberately never the
pool), and the h1 upgrade — `poll_without_shutdown` + `into_parts`, which
needs hyper.

**So the seam between them is "here is an upgraded byte stream, plus the
`read_buf` hyper had already read past" — and that is exactly the shape §2
rejected.** The rejection stands and is not weakened: as the *public* seam
it excludes three of four backends, the browser among them. But as an
**internal** seam between `http-ng-native` and a framing crate it is
correct, because it is only ever asked of the backend that can answer it.
A shape can be wrong at one level and right at the next; §2's argument was
about which level, not about the shape.

**The asymmetry with `http-ng-fetch` is what proves the arrangement.** The
browser hands back *messages*, so `http-ng-fetch` implements
`WebSocketConnect` directly and needs no adapter at all. The adapter exists
exactly where the platform hands back *bytes*. If both needed one, the seam
would be in the wrong place.

**What this does not change**: `WebSocket`, `WebSocketConnect` and `Message`
stay in `http-ng-core::unversioned` — the seam was never the problem. And
`WebSocketKeepAlive` moves with the framing, because pings and pongs are
frames; it was never `Native`'s business, and the fact that its knob is
spelled `Native::websocket_keep_alive` today is a symptom of the same
misplacement.

Open, for whoever builds it: who implements `WebSocketConnect` afterwards.
The framing crate cannot on its own — it has no connector — so it is either
a composing type over something that can upgrade (the `Selecting` shape), or
`Native` keeps the impl and delegates the framing. The second is less
machinery and the first is more honest about what depends on what; measure
the difference in what a caller has to write before choosing.
