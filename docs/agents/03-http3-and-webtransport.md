# HTTP/3 and WebTransport

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### HTTP/3: four things that are not obvious from the outside

**It streams request bodies and it is genuinely full duplex** (v0.3).
`streaming_request_body` and `full_duplex` are `true`, and they are the
floor rather than the ceiling because what they describe is this code
rather than the protocol: the request stream is split (RFC 9000 §2.1 —
the halves are independent), the body is written from an owned future
polled *beside* `recv_response`, and the unfinished write is handed to
`H3Body` to drive from `poll_frame`. Nothing is spawned, deliberately: a
spawned pump would keep uploading behind a caller that walked away, with
nowhere for its errors to go.

The claim is pinned **causally, not by a clock** — in
`crates/hclient-h3/tests/streaming.rs` the caller's body has no second
chunk until `execute` has returned a head, so a transport that read the
head only after finishing the body cannot complete the exchange at any
speed. That shape is worth copying: three separate timing-based
assertions in this workspace turned out to be flakes, and one of them
was hiding a real defect.

Two of those defects came out of building this, and both predate it.
Cancelling an upload used to poison the **shared** connection —
`quinn::SendStream::drop` calls `finish()` and only resets when the peer
has already stopped the stream, so a request dropped mid-DATA-frame
terminated *cleanly* carrying a truncated frame, which RFC 9114 §7.1
makes `H3_FRAME_ERROR`, a **connection** error that takes every
neighbour with it. Nothing reached it because the one cancellation test
used an empty body. And a `RequestBody::Rewindable` whose factory
returned a `Streaming` sent nothing at all: no bytes, no error, a `200`
for a body that never existed.

### HTTP/3: three more things that are not obvious from the outside

**It is its own crate, and the reason recorded here for two verticals was
wrong.** It read: this transport is bounded on `R: UdpBind + Spawn<..>` and
`T: QuicTlsConnect`, neither of which `Native<R, T, D>` has, and Cargo's
features are additive — so a `hclient-native/http3` feature would make both
unconditional for every build in the graph.

Measured, and the load-bearing line is `H3`'s own declaration:
`pub struct H3<R, T, D, H = NoHooks> {` carries **no where-clause at all**.
Every bound lives on `impl Transport for H3`, so `H3<Embassy, NoTls,
IpLiteralOnly>` is a nameable type and a feature makes the *module and the
constructor* unconditional rather than the bounds.

What is true is narrower: a field typed `Option<H3<R, T, D>>` would pull
`H3<R, T, D>: Transport` into `impl Transport for Native`'s where-clause,
because `execute` has to route to it — and *that* is unconditional. An
**erased** field is not: `Option<Box<dyn DynTransport + Send + Sync>>`,
whose blanket impl `hclient-core` already carries, leaves `execute` calling
`execute_boxed` and demanding nothing of `R` or `T`, with every bound on an
opt-in `Native::http3()` that `Native::new(Embassy, NoTls, IpLiteralOnly)`
never calls.

So the cost of the feature is not a broken build for a neighbour, it is
+18 crates of dead code in the graph — the same class as
`default-transport`, and a weaker reason for a crate boundary than the one
recorded here.

**It requires `R: Spawn`, and it shares connections.** An idle HTTP/1 socket
needs nobody; the kernel holds it. **A QUIC connection that nobody polls is
not idle, it is dying** — the PING that resets the peer's idle timer comes
from the connection's driver. So the driver is spawned, and once it is,
v0.2 W3's reason for handing out h2 connections *exclusively* has no
subject: that argument was explicitly conditional on there being no
background task, and a driver that is nobody's request future cannot be
stalled by a caller that stops polling. Both halves are written next to
their own policy — `hclient_h3`'s module doc and `hclient-native`'s
`pool.rs` — so that changing one does not silently import the other's
justification.

A second half of that, found while building rather than while planning:
**the spawned driver is necessary and not sufficient.** With a driver
running and no keep-alive configured, a 1500 ms gap under a 1000 ms idle
timeout still killed the connection — driving a connection is what lets it
*send* a PING, not what makes it *decide* to, and quinn leaves
`keep_alive_interval` unset. `H3` sets one (5 s), and the test is an A/B
with the driver spawned in both arms.

**0-RTT is admitted per request by the caller and by nothing else.**
`AllowEarlyData` in the request's extensions is the gate.
`RequestBody::retry_kind()` is checked beneath it as a **correctness**
condition — a rejected 0-RTT request is replayed and a single-pass body
cannot be — and deliberately not as a safety one: `POST /transfer` with a
buffered body is `RetryKind::Free`, trivially replayable, and exactly the
request that must never enter early data. "Can I resend this" and "may an
attacker resend this" are different questions and only the caller can
answer the second. The acceptance verdict is never a field: in QUIC it
resolves *after* the response body (8.63 ms against 8.58 ms, measured), so
it is a `Shared` future, and a a rejection is replayed by the transport
rather than surfaced.

**That was true of one of the two streams a rejection can land on, and the
other took three sightings to catch.** `h3` opens its **control stream in
early data** on a connection `into_0rtt()` hands back, so a server refusing
early data while `build()` is still writing SETTINGS gets the stream reset,
RFC 9114 §6.2.1 obliges h3 to close the connection with
`H3_CLOSED_CRITICAL_STREAM`, and `connect` surfaced that as
`ErrorKind::Connect` — the one outcome this paragraph says a caller never
sees. The replay covers a rejection on the **request** stream; on the
control stream there is no request yet to replay. `connect` now dials at
most twice, the second time without the shortcut, and that is not a retry
in the sense `RetryKind` governs: nothing was sent, so falling through to a
full handshake risks nothing.

It reached `main` as a flake — 2 failures in 277 concurrent runs of the h3
suite, 0 in 846 after — and was found the way the two before it were, by
capturing the failure rather than reasoning about it. The suspicion that
the recent `stage`/`finish` split had caused it was **wrong and checked**:
`connect` was last touched 85 commits earlier, and the 0-RTT shortcut
arrived 292 commits back. `425 Too Early` is the third failure path and is not
the transport's: a `425` leaves `hclient-h3` untouched, with a test pinning
it. The one line it owed back — `AllowEarlyData` removed from the replayed
request, because the mark is part of the pool key, so a marked replay would
ask for the early-data connection and, if that one had since been evicted,
would go out in early data against the very server that refused to risk it
— is paid, in `Client::run`'s `425` branch.

**Response decompression landed in v0.2 (W5), inside `Client` and behind
one feature per coding** — `gzip` and `brotli` then, `deflate` and `zstd`
since (see [Client behaviours](14-client-behaviours.md), where those two
landed), all off by default on `json`'s
precedent: a browser build would be linking decoders that cannot run
there. With any of them on, a client asks for the codings it can actually
reverse and reverses whatever the server chose — unless the transport says it did that already.
That is a capability of its own, `Capabilities::response_decompression`,
and deliberately NOT read off `forbidden_request_headers`: `hclient-fetch`
both forbids `Accept-Encoding` and decompresses internally, so the two
coincide there by accident, and a transport that forbids the header while
decoding nothing must still have its responses decoded.
`crates/hclient/tests/compression_capability.rs` pins both directions.
The body comes back as `ClientBody<B, Tm>` = `Decompressed<Deadline<B,
Tm>>`, and that order is load-bearing: the deadline is polled once per
COMPRESSED frame, or a slow server sending well-compressing padding would
walk around a `total_timeout`.

`Deadline` now **races a real sleep** rather than only stamping each frame
with the elapsed time, so `total` also cuts a body that goes completely
silent after the head — the one case an elapsed-time check structurally
cannot reach, since nothing will ever poll the wrapper again. That was
written down as impossible (`Timer::sleep` was an RPITIT), then as
possible-but-deferred, and is now done; `crates/hclient/tests/deadline.rs`
carries the server that sends a head and then nothing, for ever.
`between_bytes` is a different promise — it bounds the gap between two
frames and restarts on each — and it landed in the same week, on
`hclient-native`: `Native` declares and enforces `first_byte` and
`between_bytes`, the latter through `IdleTimeout<B, Tm>`, a body wrapper
holding a sleep of its own. Neither bound implies the other, and a caller
that sets only one has bounded only one shape: a body dripping a byte
every 50 ms for an hour passes `between_bytes` and is cut by `total`; a
transfer that legitimately takes an hour and stalls for ten minutes in the
middle is the reverse. Measured from outside the client against three
misbehaving servers, each with a control that must hang with the bound
unset — `crates/hclient-native/tests/timeouts.rs`.

**A cookie jar landed in `Client`, behind the `cookies` feature** (off by
default — the compiled-in public suffix list is +77 KiB, and
the browser, where paying that is certainly wrong, keeps its own jar
anyway). `ClientBuilder::cookie_jar(jar)` switches it on; the rules are
`hclient::cookie`'s, sans-io and clockless, and what `Client` adds is *when*
(once per redirect hop, and re-derived rather than carried, so a cookie
scoped to `/one` cannot ride a same-origin 302 to `/two`), *whether*, and a
`now`.

*Whether* is `Capabilities::owns_cookie_jar`, and a client-side jar against
a backend that reports it is an `UnsupportedCapability` at `build()` — the
same shape as a `RedirectPolicy` against `RedirectSupport::Internal`, and
exactly the arm that capability's own doc comment said would arrive with
the setting. `hclient-fetch` is the backend: the browser attaches and
stores cookies itself and forbids the `Cookie` header, so a second jar
there would store every `Set-Cookie` twice — including the ones the browser
refused — while the header it produced was dropped on the way out.

The *`now`* is a wall clock and deliberately **not** the client's
`Timer`: `Timer::Instant` is `Copy + PartialOrd` with an `elapsed_since` —
a stopwatch with no epoch — and `Expires` is a calendar date. Anchoring a
wall clock and advancing it with `elapsed_since` would freeze outright
under `NoClock`, whose `elapsed_since` is `Duration::ZERO` for ever.

**The clock is `web_time::SystemTime`, and the sentence that used to end
this paragraph said the cost was a panic on `wasm32-unknown-unknown`.**
That was true and it was worse than it read. Measured rather than
reasoned, by running a `.wasm` carrying both calls: under node with a
`Date.now` supplied, `web_time::SystemTime::now()` returns the host's
clock and `std::time::SystemTime::now()` **traps** on `unreachable`
inside `<std::time::SystemTime>::now`; under `wasmtime`, with no JS at
all, the same trap. So a cookie jar or an HTTP cache in a browser was not
an undesirable configuration, it was a module that died on the first hop
that read the clock — a configuration that did not exist.

Outside `wasm32-unknown-unknown`, `web_time` is literally
`pub use std::time::*;`, so this is the same type with the same behaviour
and not one signature moved. The graph cost is **one crate** on native,
on the default features, and on a wasm build that has a transport — and
**six** on a wasm build with `cookies,cache` and no transport, where
`web-time` becomes the only parent of `js-sys` and `wasm-bindgen`. That
last row is why the manifest's own comment had to be corrected a commit
after it was written: a measurement taken on one configuration reads as a
claim about all of them.

**And the defect survived because no gate could see it.** The wasm build
compiled before the change and after it; the failure was only ever at run
time. What keeps the plain clock from coming back is
`scripts/ast-grep/rules/no-std-wall-clock-in-the-client.yml`, which bans
the **import** rather than the call — ast-grep resolves no imports, so a
bare `SystemTime::now()` is safe precisely because the only `SystemTime`
a file can have in scope is `web_time`'s. The *type* stays in public
signatures untouched, which was the constraint. Doctests keep
`use std::time::SystemTime;` and are right to: they build for the host,
where the two are one type.

**`425 Too Early` is replayed once, in `Client`** (RFC 8470 §5.2). 0-RTT
has three failure paths and a transport can close only two of them — the
handshake refusing early data, and the server rejecting the 0-RTT keys;
the third is a *status code*, and the decision to repeat belongs to
whoever owns the operation. So `Client::run` sends the request again,
once per hop, and only when `RequestBody::retry_kind()` says the body can
be sent again — reusing that vocabulary rather than inventing a second
one. A body that cannot be replayed leaves the `425` standing as the
answer: it is the server's answer, and replacing it with an error of ours
would hide a status the caller can act on.

The part that had to be built rather than declared is the budget. The
replay lives **inside the future `Client::execute` wraps in `within(..)`**
after reading the clock once, so it spends what is left of
`Timeouts.total` rather than a fresh copy of it — a bound a server can
double by answering `425` is not a bound. Watched from the server's side
of the wire in `crates/hclient/tests/too_early.rs`: the same request
arriving twice byte for byte, a server wedged on `425` getting exactly two
requests and the caller getting the second `425`, and a 600 ms bound
against 400 ms answers ending in `Timeout(Total)` with two requests on the
server.

Two things worth knowing before touching the neighbourhood. **The replay is
stripped of its early-data mark, and that duty was vacuous for exactly one
merge.** This paragraph read "no transport here can put a request into early
data yet (HTTP/3 is not in this tree)" when it was written, and both halves
were true of the branch it was written on; the two branches merged in the
other order, which turned a note for later into a live RFC 8470 §5.2 MUST
NOT on `main`. It is one line —
`retry.extensions.remove::<AllowEarlyData>()` — and it is in `Client::run`'s
`425` branch now.

**Stripped on a clone of the hop, so the mark survives to the next hop**,
and that is a decision rather than an implementation detail. A redirect
after a `425` is a different request, and the caller marked it too; the
client withdrawing that opt-in for the rest of the chain would be a silent
downgrade nothing announces, where the cost of keeping it is bounded and
self-correcting — the next hop that meets a `425` gets its own replay.
**The one boundary it does not cross is an origin.** `next_hop` takes the
mark off on the hop that strips `Authorization` — host or scheme changed —
because "replaying this is safe" is a claim about what a request does *at
a server*, and carried to another origin it is a judgement nobody made,
acted on by sending replayable data to a server the caller never vouched
for. That closes a debt `next_hop`'s own doc had recorded with the
condition for calling it in: extensions crossing an origin was harmless
"while the only type in `extensions` is `Timeouts`", and `AllowEarlyData`
is the type that ended that. It did not need an origin inside the
extension, which was the reason the gap had been recorded rather than
closed: `Follow::strip_sensitive` already answers the question.

Four tests read the mark at the transport boundary, and between them make
five claims: the first attempt carries it, the replay does not, the hop
after a replayed `425` does, a redirect chain that never sees a `425`
keeps it throughout, and a cross-origin hop drops it. The fourth exists
because the mutant that strips on every response rather than on a `425`
passed the other three; the fifth sits next to it because the pair is the
decision, and either alone reads as an accident.

It is worth knowing *why* the strip is real rather than theoretical, because
the obvious argument says otherwise: by the time a `425` comes back the
handshake completed long ago, and streams opened afterwards are 1-RTT
whatever the request asks for. True — of the connection `hclient-h3`
happens to still have pooled. The mark is part of that pool's key, so a
marked replay asks for the early-data connection *specifically*; if that
entry has been evicted, closed by the peer or timed out, the replay opens a
**fresh** connection and `into_0rtt` puts it back into early data, against
the server that just refused to risk one. And **`RetryKind` answers only
half of what 0-RTT needs**:
"can I send this again" is the whole question for a `425` — the server
asked for the repeat — but admission into early data also asks "may an
attacker send this again", which is method safety, a notion this codebase
deliberately does not have. `POST /transfer` with `RequestBody::Full(..)`
is `RetryKind::Free` and is precisely what must never go into early data.
`.notes/h3-research.md` §3.5 has the three-row table.

### Every backend now reports events, and two of them report only what a body can say (v0.4 W2)

Hooks landed on `hclient-native`, then `hclient-h3`, then the two that own
no connections at all. **`hclient-fetch` and `hclient-wasi` emit two of the
six events — `Head` and `Progress` — and they reached the first without
sharing any reasoning.** `Connected`, `Reused` and `Closed` have no emitter
in either, and for `wasi:http` that is checkable rather than argued: `client`
is one function and there is **no connection resource anywhere in
`wasi:http@0.3.0`**. `error-code`'s eleven `connection-*` variants are how
`send` fails, not events — a `Closed::Failed` built from one would announce
the end of a connection whose beginning was never announced. `Informational` is
the fourth they do not emit, for a reason of its own: a `1xx` is a fact
about an HTTP/1 or h2 exchange, and neither backend conducts one.

**This heading said *one of the four* until `Progress` arrived**, and the
count is the only thing that moved: the argument beneath it is about
**connections**, and an octet is a fact about a body. That is why the
reasoning which leaves these two backends without `Connected` does not
also leave them without a byte counter — the two facts are about different
things, and a section written before the second one existed reads as
though they were the same.

**The browser's `Performance` surface was the real question and it is
measured, not assumed**: the entry does not exist when `execute` returns
the head — 0 entries, 1 after the body drains — and nothing on it says
which request it belongs to (`requestId`, `id`, `connectionId`,
`transferId` all `undefined`). Either fact alone kills `Connected`.

**Two of `Head`'s five fields had no source, and they are the same two on
both backends — and the two answers came out different, which is the part
worth knowing.** `id` and `version` were recorded together as debts owed by
`hclient-core`; taken up together, they separated on one question: **is the
ambiguity reachable by a reader at all?**

`id` was **not** a debt, and `ConnectionId::UNWATCHED` was not being
borrowed. Its other producer is a build with `Hooks::WATCHING == false`,
whose own documented question is *whether anything reads these events* — so
a hook can only ever meet the value in the ambient sense, *this event names
no connection*. A second value would be a distinction with one reachable
side, which is the shape `UpgradeSupport`'s four variants had when they were
deleted. What was wrong was the constant's doc comment, which named a
producer as if it were the meaning. The property it rests on — the counter
starts at `1`, so `next()` never returns it — was undocumented and untested
and now is both.

`version` **was** a debt, and `Head::version` is now
`Option<http::Version>`. The difference is that `UNWATCHED` is a sentinel no
real connection can wear, where `HTTP/1.1` is an ordinary value: a hook
counting protocol mix reported a browser's h2 and h3 traffic as HTTP/1.1, a
*wrong* answer rather than a missing one. `Capabilities::version_reported`
says the same thing and is the wrong place to say it — it is reachable from
whoever built the transport, and a `Hooks` impl is handed an `Event` and
nothing else, so a portable hook would have to know which backend it was
inside. **The rule is now a biconditional**: `Head::version` is `Some`
exactly when `version_reported`, checked on both ambient backends by tests
that read the event and the capability in one place. `Connected::version`
and `Reused::version` stay plain, which is what keeps this from being a
change made for one backend: only a transport that owns a connection emits
either, and owning one means having negotiated its protocol. The cost to
the other two backends was one line each plus the assertions the compiler
demanded. `.notes/v04-w2-hooks-ambient.md` §9.

The bounds went down again: `H: Hooks` alone here, one fewer than h3 and
two fewer than native, because the only event fires while `execute` still
owns everything and no body holds a hook.

**One CI gap fell out of it**, the same shape as the doctests nobody ran:
`hclient-wasi`'s live tests sat in a file `just test-wasi` did not name, so
they printed a `NOTICE` and reported `ok` for ever — the exact defect the
`HCLIENT_REQUIRE_WASMTIME` marker exists to prevent. Moved: the recipe runs
16 live tests where it ran 12.

### WebTransport runs on this h3, and the spec's reasons for not writing it are gone (v0.4 W2)

`hclient-webtransport` opens a session over `hclient-h3`'s QUIC:
`Session::connect`, `Session::id`, `Session::open_bi`. Its own crate, for
the reason `hclient-h3` is not a feature of `hclient-native` — features are
additive. 48 crates, `tokio` with no reactor, and `quinn` arrives with
`futures-io` alone and **no `ring`**, which is the visible consequence of
owning no endpoint.

**The premise was proved twice, and the second time is the one that counts.**
`.notes/w4-upgrade-seam.md` §4 said extended CONNECT was reachable from `h3`'s
client API "verified by reading"; it is now executed — against `h3`'s own
server, and then against **`wtransport` 0.7.2**, which carries its own
HTTP/3 and depends on `h3` not at all. Two implementations sharing no code
agreed on the wire. The `wtransport` spike is **not** kept as a test — 114
crates, `url` and ICU among them — but `.notes/v04-w2-webtransport.md` §10 has
it verbatim to re-run.

**The sharpest fact is a two-state answer to a three-state question.**
`h3`'s `settings()` returns `Settings::default()` before the peer's SETTINGS
frame arrives, and every flag in that default is `false` — so *"the peer has
not answered yet"* and *"the peer said no"* are the same value, and only the
frame's **arrival** separates them. That is the shape v0.4 W1 met from the
other side, where a `NoRecord`/`NotConsulted` distinction had to be added
for the same reason. The draft's *"clients MUST NOT attempt a session until
they have received the settings"* cannot be satisfied by reading the value
alone.

**Five things `h3` and `hclient-h3` do not expose, found and not patched
around.** `h3` 0.0.8's client **cannot announce WebTransport** at all —
`enable_webtransport` is on the *server* builder and `Config::settings`'
fields are `pub(crate)` — so the draft's client-side MUST is unsatisfiable
today; that is **asserted in a test** rather than described, so an `h3` that
grows the setter fails a line instead of leaving a stale paragraph.
Server-initiated unidirectional streams are consequently unreachable, since
the arm that would keep them is guarded by the flag a client cannot set. And
`hclient-h3` exposed no `quinn::Connection`, so its `SeamRuntime` — 302
lines — was unreachable and this crate takes a `quinn::Connection`
instead.

**That last one is closed: `SeamRuntime` is `crates/hclient-quinn`**, the
same shape §8 argues for the WebSocket framing, and the crate is 41 crates
with no `h3` in them against `hclient-h3`'s 58. `hclient-h3` re-exports
`QuinnTask` from it and is otherwise unchanged — one visibility change in
the whole move, `endpoint` from `pub(crate)` to `pub`. `just
graph-quinn-adapter-is-shared` checks both directions, including the one no
`absent` check can see: `hclient-h3` must still *depend* on it, or someone
has re-added a private copy.

**What that settled is that the two options recorded for closing it were
never alternatives.** A connect-only entry point on `H3` cannot serve
WebTransport at any price, because `H3::connect` builds an h3 client on the
connection and spawns its driver before it has one to hand back — and two h3
clients on one QUIC connection is `H3_STREAM_CREATION_ERROR`, the same
reason a session cannot share a *pooled* one. `hclient-webtransport` still
takes its connection from outside, and now because the remaining half is a
**dial** it would be the second author of: measured at 48 → 56 crates,
`ring` among them. `.notes/quinn-adapter-extraction.md` §5.

A session cannot share an `hclient-h3` pooled connection, for three reasons
in increasing hardness: a second h3 client on one QUIC connection opens a
second control stream (`H3_STREAM_CREATION_ERROR`); extended CONNECT is
announced in SETTINGS at handshake and `hclient-h3` announces it nowhere, so
making pooled connections capable would change what **every** build puts on
the wire; and `PoolKey` has no field to tell the two apart.

**Datagrams work, and the premise broke into four links each measured
separately.** quinn derives `max_datagram_frame_size` from a
`TransportConfig` default `hclient-h3` never touches, so the connection
already carries them; `h3::client::Builder::enable_datagram` **exists on the
client**, which is exactly where `enable_webtransport` does not, so this was
not the same finding one feature over; the peer's answer is readable through
a public getter, unlike `max_webtransport_sessions`; and none of it goes
through `h3` at all, which has no datagram path — the transport is
`quinn::Connection::{send_datagram, read_datagram}`.

The wire format is `varint(session_id >> 2) || payload`, RFC 9297's Quarter
Stream ID with nothing added, so **a stream and a datagram name the same
session differently** — the stream header carries the full id after `0x41`,
three lines away in the same file.

**`h3-datagram` 0.0.2 is not used, and the reason is a bug found by
executing it rather than reading it.** Its `Datagram::encode` writes the
quarter id into a local buffer and then constructs `EncodedDatagram {
stream_id: [0; MAX_SIZE], .. }`, discarding it — so the id on the wire is
always zero. Correct on stream 0 alone, wrong for 4, 8, 400, 1000000. A
session usually *is* stream 0, which is the trap; the interop test here runs
on **stream 4** so the shift is exercised rather than accidentally right.
Cost was never the argument — it would have been one crate.

Proved against `wtransport` 0.7.2 again, which shares no code with `h3`: our
header decoded by `wtransport-proto`, its echo decoded by us. The graph is
unchanged at 48 crates.

What is asserted about loss is *what* arrives, never *that* it arrives — the
arrival bound is a hang guard rather than a claim, and the one ordering
dependence is checked by mutation instead of assumed.

**A session ends cleanly now, and telling that from a session that vanished
is the whole feature.** `Session::close(code, reason)` writes RFC 9297's
`CLOSE_WEBTRANSPORT_SESSION` capsule on the CONNECT stream and FINs;
`Session::closed()` answers `Ok` for a clean end — a capsule, or a bare FIN,
which the draft makes `{code: 0, reason: ""}` — and `Err` for a reset stream,
a lost connection or an unreadable capsule. `ErrorKind::Body`, agreeing with
`hclient-fetch`'s treatment of a `wasClean == false` close rather than
inventing a second vocabulary.

**It needed nothing spawned, and that disproves this workspace's own guess.**
`.notes/v04-w2-webtransport.md` §6 said observing session end *"needs a driver
— and that is the one place a future version might have to spawn"*. It does
not: `h3`'s `RequestStream::poll_recv_data` reads through its own
`FrameStream` straight off the `quinn::RecvStream`, and the connection driver
owns the **control** stream and nothing else.

**The capsule is ours, and the crate whose name promises it does not have
it.** Measured rather than assumed: `h3` 0.0.8 has no capsule code,
`h3-datagram` 0.0.2 has none, and **`h3-webtransport` 0.1.2** has none. The
one crate that does is `web-transport-proto` 0.6.0 — executed rather than
read, after the `h3-datagram` lesson, and it is **correct**; the reason not to
take it is cost, 48 crates with `url`, `idna` and ICU among them, against
this crate's 49 in total. Ours is 59 lines.

Two facts about the peers, found and not patched around: neither `wtransport`
0.7.2 nor `web-transport-quinn` 0.8.1 *sends* a close capsule — both close the
QUIC connection — so the receive direction has no third-party encoder to be
checked against over a socket; and `wtransport::Connection::closed()` awaits
the **QUIC** connection and reports `LocallyClosed` for a session ended by a
capsule, which is exactly the confusion this distinction removes.

Deliberately not done, each with what it needs: `GOAWAY`, server-initiated
streams, and more than one session per connection.

### A connect can be asked for on its own, and the first thing that wanted one was not the race (v0.4)

`StagedConnect` — `connect` -> an opaque handle -> `exchange` — on
`hclient-native` and on `hclient-h3`, **one trait per crate and not a method
on `Transport`**: `wasi:http` 0.3's client interface is one function with no
connection resource in the WIT, and the browser's only connect-shaped API is
a `<link rel="preconnect">` hint with no handle, so a seam on `Transport`
would be `Unsupported` for two of four backends and dishonest for one. The
nearer precedent is `Prefetch`, one phase earlier, whose own refusal reads
as if written for this: *"a `fetch`-shaped transport has no DNS of its own
to save, and a `wasi:http` one has no connector at all."*

**A handle rather than a warmed pool, and `Timeouts::connect` is the whole
reason.** Warm the pool and the second call may still connect, so it reads
the same bound off the same request and applies it again — a caller who set
`connect: Some(C)` can be made to wait `2C`. Handed a connection,
`exchange` has no connect for a bound to bound: not *ignored*, which would
need a comment and a test, but **absent**.

**The handle is not the same thing on the two stacks, and that was found by
letting `H3` answer for itself.** `hclient_native::staged::Staged` *owns* the
connection it took out of the pool, and needs a `Drop` that checks it back
in, so a connection made for a request that went elsewhere is warm rather
than closed (`without_pool()` is the control: no check-in, and the drop
closes the socket). `hclient_h3::Staged` is a **claim on a connection the
pool already holds** — `connect` builds an h3 client and spawns its driver
before it has anything to hand back, which is the same fact that makes a
connect-only entry point useless to WebTransport — so it needs no `Drop` at
all. It still needs to be a handle, because `H3::execute` resolves the
address *before* it looks in the pool, inside the bound.

`exchange` deliberately does **not** carry `Native::run`'s one retry: a
retry means another pooled candidate or a fresh dial, and the dial is the
code path the bound property requires to be absent. Half a retry would be
the rule with an exception.

**The first customer is Alt-Svc's negative half**, not the race — the
reverse of the order both were written in. the transport asks its QUIC arm to
*connect*; where that fails it records the origin in
`hclient-native`'s crate-private `H3Failures` and routes the request — untouched, unsent,
never handed to a transport — over TCP. So the fallback is not
request-level retry and needs no `retry_kind()` condition:
`hclient-native`'s own sentence is true of it verbatim, *this is not a
second request, it is the first one, which never left.*

Three things about that memory are decisions rather than mechanics. **The
veto sits after both tiers**, so a record listing `h3` at a UDP-blocked
origin is covered too; it does not overrule the record and does not remove
the advertisement, because a failed connect of ours is no evidence about
the server. **`network_changed()` clears it entirely**, where the
advertisement cache keeps `persist=1`: that flag is the origin's claim
about its own advertisement, and nothing ever claimed a failure belongs to
the origin rather than to the path. And **`Timeouts::connect` is spent
once** — the request handed to TCP carries what is left of the caller's
bound, and where nothing is left the QUIC failure stands, which is
`Client`'s `425` arithmetic one layer down.

`RequireVersion(HTTP_3)` is answered before the memory as it is before the
resolver and the cache, and does not fall back.

Checked against a `quinn` server that **refuses** — an ALPN this client
will not accept, so a connect fails causally in one round trip — which is
how `.notes/v04-w1-acceptance.md` §9.3's second blocker turned out to be the
right worry about the wrong premise: the memory records *that* the connect
failed and never reads why. The black hole is used once, where a test needs
the bound *spent*. Twenty mutations, nineteen killed, one control.
`.notes/v04-staged-connect.md`.

### One transport chooses between the two stacks

`Native::http3` gives the transport a QUIC arm, and it then sends each
request over one stack or the other, deciding from the origin's
**HTTPS record**: `alpn` containing `h3` chooses QUIC, anything
else chooses TCP. That closes a gap `hclient-native`'s discovery module had
written down about itself — *"an `alpn` containing `h3` is a fact this crate
can read and cannot act on … there is nowhere in this codebase for 'choose
between two protocol stacks' to live"*. It is a crate rather than a feature
of either member for the reason `hclient-h3` is not a feature of
`hclient-native`: features are additive, and one on either would put the
whole QUIC stack into every build in any graph that switched it on.

**Discovery has two tiers and the race is neither.** Browsers do not race an
unknown origin; first contact is TCP unless something said otherwise
*before* the connection. An HTTPS record says so at resolution time — the
fast tier — and `Alt-Svc` is a response header, so it can only help the
*next* connection: the slow tier, and the one that needs storage. **Both are
built.** Racing the two stacks is a third thing, a hedge against a network
that blocks UDP/443, and it **is** built now (v0.4), off by default, because
a default that opens UDP sockets is a decision about what a plain client does
on a network that blocks them.

**The measurement that preceded it reframed it, and then the staged connect
un-framed it again.** §7 recorded the danger: a race made of two
`Transport::execute` calls races *requests*, not connections, so with no head
start the losing arm's request reached the origin — measured, 5 arms of 6.
That contradicted the sentence `hclient-native` leans on for needing no
idempotency judgement. The staged connect removed the cause rather than
mitigating it: **neither `stage` writes a request byte**, and the property is
structural rather than promised — the request is never handed to a stream
inside a `connect`, not even in the 0-RTT path, which only stores quinn's
verdict. The built race asserts the negation of that 5-of-6 row at the same
setting: at zero head start, both stacks connect and **exactly one request is
sent**.

So the head start stopped being a safety mechanism and became a cost knob:
`Duration::ZERO` is now a setting rather than a bug, and the default of 250 ms
— `HeConfig::default()`'s `attempt_delay`, this codebase's answer to the same
question one layer down — is kept because without one the hedge overrules the
chooser. Its cost is bounded by something that did not exist when it was
chosen: the race feeds `H3Failures`, so a head start is paid once per origin
per TTL rather than once per request.

**Re-measuring flipped the order of the stacks, and the reason is our own
defect.** With `nodelay` landed, TCP by name is min 1.4 / median 2.6 ms
against QUIC's 2.5 / 7.8, and with `nodelay` off the old 42 ms reproduces
exactly. §7.3's fixture had been handing QUIC a free 40 ms head start, so the
earlier row measured Nagle rather than the protocols.

Neither failure signal can shape the head start, which is the other half of
the measurement: a black hole costs **30 s**, and so does an origin with no
h3 server, because both are `quinn`'s `max_idle_timeout` rather than a
refusal — quinn contains no `ECONNREFUSED` path. The earliest honest signal
is **1.0 s**, and it too is a constant: PTO₀ off RFC 9002's guessed 333 ms
`initial_rtt`.

The slow tier is where a cache became honest, and the reason inverts the
fast tier's. There is deliberately no cache for HTTPS records here, because
inventing a lifetime for someone else's answer is how a resolver's cache
and ours drift apart. RFC 7838 §3.1's `ma` **is** that lifetime, given by
the origin for exactly this advertisement — so the cache that would have
been dishonest for SVCB is the right shape for Alt-Svc.

**Half of that reason has since been removed, and it was ours.** The
sentence above read *`SvcbEndpoint` carries no TTL*, which was true of
the type and never of the wire: every backend's decoder had the record's
TTL in hand and dropped it. The TTL is carried now —
`Option<Duration>`, where `None` is *the resolver did not say* and
`Some(ZERO)` is RFC 2181 §8's *do not cache this*, which are different
instructions and must not be one value. So a cache of HTTPS records is
no longer structurally dishonest; it is simply not written, and what it
would still need is a key, a bound and an answer to what
`network_changed()` means for an entry.

The order between them is a rule rather than an accident: **the record
first, the cache only where there is no record.** So an origin that
publishes an HTTPS record never touches the cache, and the slow tier adds no
query and no lock to the fast tier's path — measured, the DNS cost table is
unchanged. A record saying `h3` is absent is also not overruled by an
advertisement, which is the mutation that rule exists to fail.

**Scope is a correctness question and the RFC says so.** §2.2 conditions its
own SHOULD on *"when information about network state is available"*, and to
a `Transport` it is not: a cache surviving a laptop's move between networks
advertises an alt-authority that was reachable somewhere else. So nothing is
persisted, and `Native::network_changed()` is the only entry point —
public, for the caller who can see what the transport cannot. Until it is
called every entry behaves as `persist=1`, which is the unsafe direction and
is said where the setter is.

**The negative half is built now, and not by the race** (v0.4). It took
reading to establish that it was missing rather than misplaced:
`hclient-native`'s `NegativeCache` is a different fact — a TCP connect
through a discovered endpoint failed — and it never sees an h3 attempt,
because when the transport routes to its QUIC arm the TCP path is not called
at all. `H3Failures`, private to `hclient-native`, is the memory that was owed; the
**staged connect** is what unblocked it, and the section below is that.

`.notes/v04-w1-acceptance.md` §7 and §9 say what the race would need and what
the slow tier does and does not check.

**The part that was not mechanical is the capability set, and it is not the
race.** `Transport::capabilities` returns a `&Capabilities`, so the pair's
answer is stored at construction, and it is decided field by field by one
rule: **the stored value must be true whichever member serves the request.**
Six fields disagree today — measured, not taken from the design document,
whose two examples had both been fixed under it while it was being written.
It was seven until the seventh turned out not to be a disagreement at all:
`client_certs` was `true` from a constant in `hclient-h3` and
`Capabilities::default()`'s `false` in `hclient-native`, so **one** TLS backend
gave two answers depending on which stack was holding it, and the v0.4
table recorded the row as "same shape" as `full_duplex`. Both read
`TlsIdentity::presents_client_certs` now — a defaulted-false constant on
the seam the two connect traits share, `reports_alpn`'s shape — and the
same connector carrying a client certificate is reported by both members
and by the pair. Five take the weaker claim, `full_duplex` among them,
which is the same
answer `hclient-native` already gives one level down for the same reason: an
over-claimed `full_duplex` deadlocks a caller and an under-claimed one costs
a buffered copy. Where the two values are *different claims* rather than a
stronger and a weaker one — every remaining enum, and the two *the transport
already does this itself* flags — there is no true value and the constructor
**refuses, naming the field**. `Native::without_pool()` against `H3` is the
one refusal reachable from the two members this workspace ships, and it is
an ordinary mistake rather than a contrived one.

`early_data` is the single field whose *stronger* value is the true one, and
that is about what the variant says rather than an exception to the rule:
`Supported` means the transport *can* place a marked request into early
data, which stays true of the pair, where `None` — "never offers early data"
— is false of it, and false in the direction that matters, since nothing in
`hclient` reads the field and a marked request would reach the QUIC stack
anyway. The contrast three lines below it in the same function is
`CancelSupport`, where `Supported` is a **duty owed on every dropped
future** and a member that does not owe it makes the claim false.

**What the choice costs is counted rather than argued**: **one** type-65
query per request that has a name to ask about, whichever stack answers. A
`RequireVersion` demand, `http://` and an IP literal cost none at all.

It was two on the TCP path at an origin's default port, because
`hclient-native` fetched the record again inside its own connector, and the
fix is the part worth knowing: **the record is not handed to the connector,
it is fetched by it.** `Native::prepare` — a public `Prefetch` trait until
the routing moved inside the crate, a private method since — does the
connector's own lookup — its resolver, its rule about where discovery
applies, its negative cache — and hands back a `Prepared`, which is the
request *with* the answer; `run` then does not look again. A
caller cannot supply a record, because there is no constructor that pairs
one with a request it was not fetched for, so the wrong-origin question
cannot be asked rather than being answered by a check. The shape that was
rejected is the obvious one: a request extension is the caller's channel,
and an HTTPS record carries a port and address hints, so an extension
carrying one would let any code that can build a request move the
connection somewhere else. `.notes/v04-w1-acceptance.md` §3.1 has the
argument and §3.3 the eleven mutations behind it (ten killed, one control).

The other half of the rule is what keeps the routing from owning a copy of
the connector's: where the member did **not** look — a non-default
port, whose record lives under a name only the selecting transport
constructs — it answers `Discovered::NotConsulted`, which is not an answer,
and the caller asks its own resolver exactly as it always did. `NoRecord`
*is* an answer and stops the second query, which is the half a plain
`Option` gets wrong.

Checked against **two real servers behind one authority** — a `quinn`
endpoint on UDP and a `tokio-rustls` listener on TCP, on the same port
number, both alive in every test — so a request reaching one is a choice and
not the only possibility. Nineteen mutations applied, nineteen killed; the
first run of them scored every one as survived and was wrong, which is why
`.notes/v04-w1-acceptance.md` §5 records how the table was checked as well as
what it says.
