# v0.4 W2 — WebTransport, and what it took to find out

`docs/w4-upgrade-seam.md` §4 decided that WebTransport is **a separate
seam**, not the WebSocket one, and said the primitives were present in `h3`
0.0.8 — *"verified by reading"*. This document is the reading executed, the
crate it produced, and the four things the reading did not say.

Nothing here re-litigates §4. The multiplexer-versus-message-channel
argument stands and `crates/http-ng-webtransport/src/lib.rs`'s module doc
carries it; `Message` is not reused and no adapter between the two seams
exists.

## 1. The premise holds, and it holds twice

**The claim under test.** An extended CONNECT carrying `:protocol =
webtransport` can leave this workspace's HTTP/3 stack and be accepted by a
server that speaks WebTransport.

**First proof — `h3`'s own server**, in tree, on a real socket:
`crates/http-ng-webtransport/tests/webtransport.rs`'s
`an_extended_connect_carries_the_webtransport_protocol_to_the_server`. A
`quinn` endpoint with an `rcgen` certificate, `h3::server` on top of it, and
the value asserted is what that server's **own QPACK decoder** took off the
wire (`req.extensions().get::<h3::ext::Protocol>()`), reported as a
`String` so that the two sides do not agree by sharing a type.

**Second proof — an independent implementation.** The first proof has an
obvious weakness: both ends are `h3`, and two ends of one crate can be
wrong the same way. So the same client was run, unchanged, against
**`wtransport` 0.7.2**, which carries its own HTTP/3 (`wtransport-proto`)
and depends on `h3` not at all — checked with `cargo tree -e normal -i h3`,
whose only roots are ours. Measured output of the spike:

```
server on 127.0.0.1:46400
CLIENT: QUIC up, alpn = Some(Any { .. })
SERVER: extended CONNECT arrived: authority="localhost:46400" path="/spike"
SERVER: accepted
CLIENT: session established, id 0
SERVER: read "ping"
CLIENT: echo = Ok("ping")
```

So the CONNECT, the session, the `0x41`-headed bidirectional stream and the
bytes in both directions all work against a third party's decoder.

**The spike is not kept as a test, and the reason is a number**:
`wtransport` is **114 crates** (`cargo tree -p wtransport -e normal`,
unique), including `url` and the ICU stack this workspace spent a whole
task removing from `http-ng-proto`. A dev-dependency is not a shipped
dependency, but 114 crates to re-check a fact that does not change between
runs is the wrong trade. The spike source is in §10 so it can be re-run
rather than believed.

## 2. `h3`'s opt-in feature: what it buys, and what it costs

The crate enables `h3`'s
`i-implement-a-third-party-backend-and-opt-into-breaking-changes`. Two
things are unreachable without it and neither is optional for a
WebTransport client:

- **the peer's SETTINGS** — `ConnectionState::settings()`, and with it
  `enable_webtransport()` / `enable_extended_connect()`;
- **the moment they arrived** — `ConnectionInner::poll_control` resolving
  to `Frame::Settings`.

The second matters more than it looks. `settings()` answers
`Settings::default()` before the peer's frame has arrived, and every flag
in that default is `false`, so *"the peer has not answered yet"* and *"the
peer said no"* are **the same value**. Only the arrival of the frame tells
them apart, and a client that read the flags without waiting would refuse
every peer. That is mutation **M8** in §8, and it is killed by five of the
seven tests.

**What the feature costs, counted rather than assumed.** 27 uses in `h3`
0.0.8, and `grep -rn 'cfg!(feature' src/` finds **zero** — no branch turns
on it:

| where | uses | what changes |
|---|---|---|
| `src/lib.rs`, `src/error/mod.rs` | 15 | `#[cfg(feature)] pub mod X` against `#[cfg(not(feature))] mod X`, plus `pub use shared_state::{ConnectionState, SharedState}` |
| `src/server/connection.rs` | 3 | two extra methods and their impl block, on the **server** |
| `src/error/error.rs` | 9 | `#[cfg_attr(not(feature), non_exhaustive)]` on the variants of `ConnectionError` and `StreamError` |

Only the last group touches an existing item, and it **relaxes**: removing
`non_exhaustive` lets more patterns compile and stops none. That is what
makes Cargo's feature unification harmless — `http-ng-h3` gets strictly
more surface from the same `h3` build and the same behaviour. Checked by
running it: `cargo nextest run --workspace --all-features` is **1360
passed, 0 failed** with the feature on, `http-ng-h3`'s 
suite included.

The name is a warning and it is a real one: `h3` reserves the right to
break these paths in a patch release. The mitigation is the version pin
this workspace already has and the fact that exactly two items are used.

## 3. What `h3` 0.0.8's **client** cannot do — three findings

**(a) It cannot announce WebTransport.** `h3::client::Builder` has
`enable_extended_connect` and `enable_datagram` and **no**
`enable_webtransport` or `max_webtransport_sessions` — both exist on
`h3::server::Builder` only, and `Config::settings`' fields are
`pub(crate)`. draft-ietf-webtrans-http3 says *"A client supporting
WebTransport over HTTP/3 MUST send the SETTINGS_WT_MAX_SESSIONS setting
with a value greater than 0"*, and this client sends `0`.

This is **asserted, not described**:
`an_extended_connect_carries_the_webtransport_protocol_to_the_server` reads
the client's own SETTINGS out of the fixture server's `h3` state and
asserts `webtransport == false`, so an `h3` that grows the setter fails a
test instead of leaving a stale paragraph here.

Nothing refused us over it — not `h3`'s server, which never looks at the
client's flag, and **not `wtransport` 0.7.2 either**, which is the more
interesting half since it is the draft-current implementation. So the MUST
is unenforced by both servers measured; a third might enforce it, and there
is no way to satisfy it from outside `h3`.

**(b) A server-initiated unidirectional WebTransport stream is
unreachable, not merely unimplemented.** `h3`'s client driver classifies an
incoming uni stream of type `0x54` as
`AcceptedRecvStream::WebTransportUni`, and the arm that keeps it is guarded
by `self.config.settings.enable_webtransport` — the flag a client cannot
set. With it `false` the stream falls to `_ => ()` and is **dropped
silently**. Server-initiated *bidirectional* streams are a different case
and are reachable, because the client's driver never accepts bidi streams
at all; they are simply not built (§6).

**(c) The peer's `max_webtransport_sessions` cannot be read.**
`h3::config::Settings` has public getters for `enable_webtransport`,
`enable_datagram` and `enable_extended_connect` and none for the session
limit, so a client cannot honour a server that advertises WebTransport with
a limit of zero. In practice `h3`'s own `Settings::from` defaults it to
zero and its server sends whatever it was configured with, so the check
would be meaningful — it is simply not available.

## 4. What `http-ng-h3` does not expose — and why a session cannot share a pooled connection

`crates/http-ng-h3` is read-only for this task. Two things are needed from
it and neither is reachable; this section is the finding, stated precisely
enough to act on.

**(a) There is no way to obtain a `quinn::Connection`, or an endpoint.**
`H3`'s public surface is `new`, `hooks`, `keep_alive_interval`,
`without_keep_alive` and `Transport::execute`. `endpoint`, `checkout` and
`connect` are private, and so is `mod runtime` — of which only the
`QuinnTask` type alias is re-exported. **`SeamRuntime`**, the
`quinn::Runtime` over `http_ng_rt::{Timer, Spawn, UdpBind}`, is therefore
unreachable: 302 lines of code (494 with its documentation), including the
`WakeAll` fan-out that exists because `UdpDatagrams::poll_writable` stores
one waker where `quinn` creates a `UdpPoller` per connection.

Copying it into a second crate is the thing this workspace does not do, so
`http-ng-webtransport` does not have an endpoint and takes a
`quinn::Connection` instead (§5). **What would close it** is one of:

1. `pub use runtime::SeamRuntime;` in `http-ng-h3` — one line, and it makes
   the adapter reusable by any crate that wants QUIC over the seam. The
   honest version of that is a move to a crate of its own
   (`http-ng-rt-quinn`), which is the same shape `docs/w4-upgrade-seam.md`
   §8 argues for `tungstenite` inside `http-ng-native`;
2. a **connect-only entry point** on `H3` handing back a live
   `quinn::Connection` for an origin — which is a bigger decision than it
   looks, because it is also what a two-stack *race* would need
   (`docs/v04-w1-acceptance.md` §7 records that the `Transport` seam has no
   connect-only entry point and that this is why a race races requests).

**Since done — option 1, in its honest version:
[`docs/rt-quinn-extraction.md`](rt-quinn-extraction.md).** `crates/http-ng-rt-quinn`
holds `SeamRuntime`, `SeamTimer`, `WakeAll`, `SeamSocket`, `SeamPoller` and
a now-`pub` `endpoint(&rt, local)`; `http-ng-h3` re-exports `QuinnTask` from
it, so its public API is unchanged and its graph went 57 → 58, the one
addition being the new crate. 42 crates on their own, with no `h3` in them.

**And the list above is wrong to present the two as alternatives**, which
the extraction is what established. A connect-only entry point on `H3`
**cannot serve WebTransport at any price**: `H3::connect` builds an
`h3::client` on the connection and spawns its driver before it has anything
to hand back, and `Session::connect` builds its own with
`enable_extended_connect(true)`. Two h3 clients on one QUIC connection open
two control streams — `H3_STREAM_CREATION_ERROR`, the first of the three
reasons in (b), which is about the *pool* and applies just as hard to a
fresh connection, because `H3` never hands out one it has not already
claimed. So option 1 closes this gap and option 2 answers a different
question (`docs/connect-only-seam.md`).

What remains on this crate's side is its own dialling — `http_ng_rt_quinn::
endpoint` plus a `QuicTlsConnect` and an address — and it is **not done**,
for reasons about this crate rather than about the adapter:
`Session::connect(conn, uri)` stays whatever else happens, so a dialling
constructor is an addition rather than a replacement; it costs a measured
49 → 58 crates, `ring` among them, which is the count §7 names as *"the
visible consequence of owning no endpoint"*; and it would be a second place
in this workspace where "how a QUIC connection is made" is decided, with
nothing making it agree with `http-ng-h3`'s.

**(b) A session cannot share one of `H3`'s pooled connections, and this is
not a matter of plumbing.** Three independent reasons, in increasing order
of how hard they are to change:

- **The h3 client is per QUIC connection.** A WebTransport session needs an
  `h3::client::SendRequest` to send the CONNECT on. Building a second one
  on a connection that already has one opens a **second control stream**,
  which RFC 9114 §6.2.1 makes a connection error
  (`H3_STREAM_CREATION_ERROR`). So sharing means sharing `H3`'s
  `SendRequest`, not sharing the socket.
- **SETTINGS are fixed at handshake and are connection-wide.**
  `enable_extended_connect` goes into the SETTINGS frame `h3` sends when
  the client is built. `http-ng-h3` sets it nowhere, so **every connection
  it has ever made announces `ENABLE_CONNECT_PROTOCOL = 0`**, and it cannot
  be changed afterwards. Making a pooled connection WebTransport-capable
  therefore means changing what *every* `http-ng-h3` build puts on the
  wire — which is exactly why this is a separate crate and not a feature:
  Cargo's features are additive, so a `webtransport` feature on
  `http-ng-h3` would make that wire change unconditional in any graph that
  switched it on.
- **The pool key has no room for the distinction.** `PoolKey` is `{host,
  port, tls, early_data}`. A WebTransport-capable connection and a plain
  one would be indistinguishable in it, so a plain request could be handed
  a connection whose settings differ from what the pool believes — the same
  class of mistake `early_data` is a key field to prevent.

The conclusion is not "sessions must never share": WebTransport is designed
to multiplex with ordinary requests, and a future `http-ng-h3` that
announced extended CONNECT on every connection and handed out its
`SendRequest` could do it. The conclusion is that **it is a decision about
what every connection announces**, taken at handshake time, and it is
`http-ng-h3`'s to take.

## 5. The seam's shape

```rust
Session::connect(conn: quinn::Connection, uri: &http::Uri) -> Result<Session, Error>
Session::id(&self) -> SessionId
Session::open_bi(&self) -> Result<(quinn::SendStream, quinn::RecvStream), Error>
```

Four decisions worth stating.

**It takes a QUIC connection, which is the shape §2 of the W4 document
rejected.** The rejection stands where it was made — as a *public* seam,
"hand me the transport's socket" excludes three of four backends. As an
**internal** seam between the crate that owns dialling and the crate that
owns the session, it is right, for §8's reason: *"a shape can be wrong at
one level and right at the next"*. The asymmetry that proves the placement
is the same one §8 names for WebSocket: the browser's `WebTransport` global
hands back *sessions and streams*, so a browser backend would implement the
session API directly and need no connection at all — the QUIC connection is
asked only of the backend that has one.

**There is no trait.** `WebSocketConnect` is in `http-ng-core::unversioned`
because two backends implement it and the second one is what proved the
shape. Here there is one implementation, in this crate, so a trait would be
a shape nobody has tested — the objection the W4 document raises against
declaring a seam before a backend fits it. It belongs beside
`WebSocketConnect` when the browser's own `WebTransport` is a second
implementer.

**It returns `quinn`'s stream types.** Wrapping them would be a second
vocabulary for types the caller already holds, with no method the wrapper
could add and no method it could honestly remove. The stream *header* is
written before the pair is handed back, so what the caller receives is
positioned at its own first application byte and there is no second step
whose omission would put application bytes where a header belongs.

**Nothing is spawned.** `http-ng-h3` spawns a driver because a **pooled**
QUIC connection that nobody polls is dying and between requests the pool is
the only thing holding it. Neither half applies: the QUIC connection is
driven by the endpoint driver `quinn` already runs, and a session is the
caller's own object. The h3 control stream is polled exactly once, inside
`connect`, because the draft makes the peer's SETTINGS a precondition — and
never again. §6 says what that costs.

**Three fields are held and never read**, which is unusual enough to be
worth naming: the CONNECT `RequestStream` (the session lives exactly as
long as it, and `quinn::SendStream::drop` calls `finish()`), the h3
`SendRequest` (`h3` counts them and closes the connection with
`H3_NO_ERROR` when the last drops), and the h3 connection driver (it owns
the control stream, and a control stream that ends is
`H3_CLOSED_CRITICAL_STREAM` — a *connection* error).

## 6. Deliberately not done, with what each would need

- **Datagrams — since done, and the note above was wrong about how**:
  [`docs/v04-w2-datagrams.md`](v04-w2-datagrams.md). `enable_datagram(true)`
  on the client builder was right, and it is the link that made this the
  one item on the list whose blocker is not §3a. The `h3-quinn` feature
  was not: it is `dep:h3-datagram` and nothing else, and **`h3-datagram`
  0.0.2's `Datagram::encode` writes a Quarter Stream ID of zero, always** —
  it encodes the varint into a local buffer and then builds its
  `EncodedDatagram` from a freshly zeroed array. Measured over five
  session IDs, correct on stream 0 alone. So the framing is fifteen lines
  here beside the stream header's, the graph is unchanged at 49 crates,
  and the "demultiplexer keyed by session ID" is a filter with one
  subject, because a `Session` still owns the h3 client.
- **The capsule protocol and observing the session's end — since done,
  and they were one item rather than two**:
  [`docs/v04-w2-capsules.md`](v04-w2-capsules.md). The sentence below that
  said a real `close()` *"needs the reading half"* was the join.
  `Session::close(code, reason)` writes a `CLOSE_WEBTRANSPORT_SESSION`
  capsule and `Session::closed()` reads the peer's, so a clean close is
  `Ok` and a connection that vanished is `Err` — `http-ng-fetch`'s
  `wasClean`, for a session. `DRAIN_WEBTRANSPORT_SESSION` is skipped along
  with every other unknown capsule type, which is what RFC 9297 §3.2 asks
  of a receiver; surfacing it needs a second observation channel, since a
  drain is not an end.

  **The guess below about a driver was wrong, and that is the finding.**
  Nothing has to be spawned: `h3`'s `RequestStream::poll_recv_data` reads
  the CONNECT stream straight off its own `quinn::RecvStream`, and the
  driver owns the *control* stream and nothing else. So `closed()` is the
  caller's own future, exactly like `recv_datagram`, and a caller that
  never awaits it never learns the session ended. The graph is unchanged at
  49 crates: `h3` 0.0.8 has no capsule code, nor has `h3-datagram` 0.0.2,
  nor has the crate named `h3-webtransport` 0.1.2 — and the one crate that
  does, `web-transport-proto` 0.6.0, is 48 crates with `url` and ICU among
  them.
- **`GOAWAY`.** Same cause, and unlike the two items above it really is
  the driver's: `GOAWAY` arrives on the h3 **control** stream, which the
  driver owns and nobody polls. The CONNECT stream being readable says
  nothing about it.
- **Server-initiated streams.** Unidirectional ones are unreachable
  without a change inside `h3` (§3b). Bidirectional ones are reachable —
  `quinn::Connection::accept_bi`, read the `0x41` and the session ID, match
  it — and are simply not built; the fixture in
  `tests/server.rs` does exactly this on the server side, so the shape is
  known to work.
- **More than one session per connection.** The draft allows it and
  `SETTINGS_WT_MAX_SESSIONS` bounds it; here a `Session` owns the h3
  client, so there is one.

## 7. The dependency graph, measured

`cargo tree -e normal --prefix none`, unique crates, this tree:

| crate | crates | tokio | notes |
|---|---|---|---|
| `http-ng-webtransport` | **49** | `[bytes, default, io-util, sync]` | no reactor — the same `h3`/`h3-quinn` leaf `http-ng-h3` has |
| `http-ng-h3`, for comparison | 57 | same | it also carries DNS, TLS and the runtime seam |

Both rows are unchanged by the datagram work: it added no crate and no
feature, because the transport is a method on the `quinn::Connection` this
crate already holds — `docs/v04-w2-datagrams.md` §7.

`quinn` arrives with the feature set **`futures-io` alone** — no `ring`.
That is the visible consequence of owning no endpoint: `http-ng-h3`'s
manifest records that `ring` is not optional there because a QUIC
*endpoint* needs an HMAC key for stateless resets and an AEAD for retry
tokens, and `EndpointConfig` has no `Default` without one. This crate
builds no endpoint, so it picks no crypto provider at all.

`cargo deny --all-features check`: advisories ok, bans ok, licenses ok,
sources ok.

## 8. Mutations

Anchor verified before the first and after the last: **7 tests, 7 passed**.
Restore is `git checkout` **plus an explicit `os.utime`** — a copy that
preserves mtime leaves cargo believing the mutated artifact is current, and
every run after the first would then score a stale binary. The harness is
`crates/http-ng-webtransport/mutations.py`; it re-runs the anchor at the
end and refuses to report if it does not come back.

**M11 is a control that nothing can observe**, and it is in the table for
the reason six mis-scored runs elsewhere in this session gave: a harness
that reports "killed" unconditionally cannot be told from one that works,
unless something in the table must survive.

| # | mutation | verdict | killed by |
|---|---|---|---|
| M1 | `Protocol::WEB_TRANSPORT` → `Protocol::CONNECT_UDP` | killed | `an_extended_connect_carries…`, `a_refused_session_surfaces…` |
| M2 | `Method::CONNECT` → `Method::GET` (so `h3` drops `:protocol`) | killed | the same two |
| M3 | stream signal `0x41` → `0x42` | killed | `a_bidirectional_stream_carries_bytes_both_ways` |
| M4 | session ID `stream.id().into_inner()` → `.index()` | killed | `a_bidirectional_stream_carries_bytes_both_ways` |
| M5 | varint short branch `v < 1<<6` → `v < 1<<7`, so `0x41` is one byte | killed | the bidi test **and** `varints_match_rfc_9000_a1` |
| M6 | the SETTINGS gate always passes | killed | both gate tests |
| M7 | the gate takes `\|\|` instead of `&&` | killed | both gate tests |
| M8 | the peer's SETTINGS are never awaited, so the defaults are read as the answer | killed | five of seven |
| M9 | `!resp.status().is_success()` → `false` | killed | `a_refused_session_surfaces_the_peers_status` |
| M10 | our own `enable_extended_connect(true)` → `(false)` | killed | `an_extended_connect_carries…` |
| M11 | **control** — `Vec::with_capacity(16)` → `Vec::new()` | **survived, as intended** | nothing; it is an allocation hint |

**Ten killed, one control survived.** Every mutant compiled, so no verdict
is a build failure wearing a kill's clothes.

**All eleven were re-run after datagrams landed**, and two of them gained
killers: M4 goes from one test to four and M5 from two to three, because a
datagram carries the same session ID in a second encoding and through the
same `put_varint`. The current table is `docs/v04-w2-datagrams.md` §8,
twenty-three mutations with two controls.

Two of them are worth a sentence each.

**M4 needed the fixture to be non-degenerate before it could die.** The
CONNECT is normally the first client-initiated bidirectional stream on a
fresh connection, so its ID is **0** — and `0 >> 2` is `0`, and so is a
hard-coded zero, so three different wrong answers are right. The bidi test
opens and resets one bidirectional stream first, which takes ID 0 out of
circulation and puts the CONNECT on **4**, where `index()` says `1`. The
first attempt at this held an *unwritten* stream instead of resetting one,
on the theory that an unwritten stream never reaches the peer; that is
false — QUIC opens the lower-numbered streams implicitly when a higher one
is used, so the server saw stream 0, waited for headers that never came,
and the test failed by idle timeout at 30 s. The reset version says the
same thing and says it on the wire.

**M10 could only be killed by looking at our own SETTINGS**, which nothing
else can see. The fixture reads them out of `h3`'s server-side state at the
moment the request resolves. RFC 9220 §3 says receipt of
`SETTINGS_ENABLE_CONNECT_PROTOCOL` **by a server has no impact**, so
nothing on the wire forces the line — which is precisely why it needed an
assertion rather than a comment.

## 9. One variant was deleted for having nothing that could produce it

`BadSessionUri` began as an enum with `Scheme` and `NoAuthority`. The
second is now gone, along with the check that raised it, because
**`http::Uri` cannot represent a scheme without an authority**. Measured,
in this crate's own test run, before the deletion:

| input | result |
|---|---|
| `Uri::builder().scheme("https").path_and_query("/echo").build()` | `Err(InvalidUriParts(InvalidUri(AuthorityMissing)))` |
| `"https:/echo"` | `Err(InvalidUri(InvalidFormat))` |
| `"https:///echo"` | `Err(InvalidUri(InvalidFormat))` |
| `"https://"` | `Err(InvalidUri(Empty))` |
| `"https:echo"` | `Ok` — with **no scheme**, authority `"https:echo"` |

The last row is the one that decides it: the only near-miss parses with
`scheme_str() == None`, which the `https` check above already refuses. So
the variant had no reachable right-hand side, and `h3`'s own
`HeaderError::MissingAuthority` is unreachable from this crate too. What
remains is a struct, `NotHttps`.

## 10. The interop spike, for re-running

Outside the workspace (`/tmp/wtinterop`), so that 114 crates do not enter
this one:

```toml
[dependencies]
http-ng-webtransport = { path = "…/crates/http-ng-webtransport" }
http = "1"
quinn = { version = "0.11", default-features = false, features = ["futures-io", "ring", "runtime-tokio", "rustls-ring"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
tokio = { version = "1", features = ["full"] }
wtransport = { version = "0.7", features = ["self-signed"] }
```

A `wtransport::Endpoint::server` with a self-signed identity; a plain
`quinn` client endpoint with `alpn_protocols = ["h3"]` trusting that
certificate; then `Session::connect(conn, &uri)`, `open_bi`, `write_all`,
`read_to_end`. Two things to know before repeating it: `wtransport`'s
`local_addr` reports `[::]:port`, which `quinn::Endpoint::connect` refuses
as `InvalidRemoteAddress`, so the port has to be re-paired with
`127.0.0.1`; and the spike's server panics on `finish()` after the client
has gone (`NotConnected`), which is the spike's own tidying and happens
after the echo has been received.

## 11. What is not verified

- **A browser.** All four engines ship `WebTransport`, and none of them was
  run against this. The client-side gap that would matter first is §3a —
  a browser is a client, so it is not affected, but a *server* that
  enforces the client's `SETTINGS_WT_MAX_SESSIONS` would refuse us and
  neither server measured here does.
- **draft-13's setting IDs.** `h3` 0.0.8 parses
  `SETTINGS_ENABLE_WEBTRANSPORT` (`0x2b603742`) and
  `WEBTRANSPORT_MAX_SESSIONS` (`0x2b603743`), which are the older draft's.
  `wtransport` 0.7.2 evidently still sends the first — our gate requires it
  and the session established — but that is an inference from one run, not
  a reading of `wtransport-proto`.
- **Anything under load.** One stream, four bytes, loopback — and one
  datagram, four bytes, loopback. Flow control,
  many concurrent streams and large transfers are untested, and
  `open_bi`'s eager header write in particular has never met a connection
  whose stream credit was exhausted.
- **Cancellation.** Dropping the `Session::connect` future mid-handshake is
  not tested. `http-ng-h3` has a `CancelSupport` claim to honour and this
  crate makes none, which is honest but unproven.
