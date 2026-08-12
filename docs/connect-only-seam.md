# The connect-only entry point — three asks, two gaps, one already closed

Three pieces of work in this workspace reached for the same missing thing
without any of them going looking for it: the two-stack race measurement
(`docs/v04-w1-acceptance.md` §7.6), WebTransport's need for a live
`quinn::Connection` (`docs/v04-w2-webtransport.md` §4a), and the
observation in the same section that `http-ng-h3`'s quinn runtime glue is
unreachable from outside the crate. When three tasks independently hit one
gap it stops being anybody's inconvenience, so this document reads all
three, checks every premise it can against this tree, and decides.

Nothing here is code. It exists so that the implementation does not have to
re-derive any of it, and so that a later reader can tell which parts were
measured, which were read, and which were argued.

**The answer, first, because two of the three asks turn out not to want
what they asked for.** It is *two* gaps rather than one or three. One of
them — "expose a type" — is closed, by the extraction of `SeamRuntime` into
`http-ng-rt-quinn` that landed while this was being written. The other is
the race's, and what it needs is not a connect-only entry point on
`Transport`, and not on `H3` either: it is a **staged pair on each backend
that has a connector**, one phase further down the pipeline
`http_ng_native::Prefetch` already stages. WebTransport's ask is a third
thing that a connect-only entry point on `H3` would answer *wrongly*, and
§3.2 is the argument.

## 1. The three asks, in their own words

**A. The race** — `docs/v04-w1-acceptance.md` §7.6, and it is the
document's headline finding rather than an aside:

> **A race built out of two `Transport::execute` calls races requests, not
> connections.** Browsers race *connections* and send the request on the
> winner. `Transport` has no connect-only entry point — there is nowhere in
> the seam to say "open a connection and do not send this yet" — so any
> race assembled from the members as they stand duplicates the request at
> the origin whenever the loser gets far enough.

Measured, not predicted: at a zero head start the losing TCP arm delivered
a complete, well-formed HTTP request to the origin in **5 of 6 arms**
(§7.3, M3).

**B. WebTransport** — `docs/v04-w2-webtransport.md` §4a, as the second of
two things that would close its gap:

> 2. a **connect-only entry point** on `H3` handing back a live
>    `quinn::Connection` for an origin — which is a bigger decision than it
>    looks, because it is also what a two-stack *race* would need

**C. Reachability** — the same section, as the first of the two:

> `SeamRuntime`, the `quinn::Runtime` over `http_ng_rt::{Timer, Spawn,
> UdpBind}`, is therefore unreachable: 302 lines of code (494 with its
> documentation) … **What would close it** is one of: 1. `pub use
> runtime::SeamRuntime;` in `http-ng-h3` — one line …

§4a presents B and C as alternatives — "one of". §3 below finds that they
are not alternatives: C closes B, and B does not close B.

## 2. The premises, and how each is known

Everything the decisions rest on, with its evidence. "Read" means the
source in this tree or in a pinned dependency; "measured" means a number
somebody took; "in flight" means a sibling branch that had not merged when
this was written.

| # | premise | how known |
|---|---|---|
| P1 | `Transport` has exactly one method that does I/O — `execute`. The other two are `capabilities` (a `&Capabilities` fixed at construction) and `to_error` (a pure classification hook). | read, `crates/http-ng-core/src/unversioned/transport.rs` |
| P2 | `wasi:http` 0.3's client interface is **one function**: `send: async func(request) -> result<response, error-code>`. There is no connection resource anywhere in the WIT, and `handler.handle` has the same signature by design ("the type signature of `client.send` is the same as `handler.handle`"). | read, `wasip3-0.7.0+wasi-0.3.0/wit/deps/http.wit`, `interface client` |
| P3 | `wasi:http` **can bound** the connect phase and can never observe it: `request-options` has `get-connect-timeout`/`set-connect-timeout`, and nothing else connect-shaped. | read, same file, `resource request-options` |
| P4 | `http-ng-fetch` declares `timeouts.connect = false`, with the reason written where the field is set: *"`AbortSignal` is one deadline for the whole exchange; none of the three phase timeouts (`connect`/`first_byte`/`between_bytes`) can be expressed through it individually. Declaring any of the three would be a capability that lies."* | read, `crates/http-ng-fetch/src/caps.rs` |
| P5 | The browser's only connect-shaped API is `<link rel="preconnect">`, and it is a **hint**: MDN calls it *"a hint to browsers … the browser can likely improve the user experience by preemptively initiating a connection"*. It yields no handle, and there is no way to bind a later `fetch()` to a particular connection — the browser's connection pool is not addressable from script at all. | read, MDN `Web/HTML/Reference/Attributes/rel/preconnect`, and P4 for the in-tree half |
| P6 | `http-ng-native` **already has a staged entry point**, and it is deliberately not on `Transport`: `Prefetch::prepare` → `Prepared` → `Prefetch::execute_prepared`. Its own doc gives the reason: *"`Transport` is the seam **every** backend fills in, and this is a question exactly one kind of backend can be asked: a `fetch`-shaped transport has no DNS of its own to save, and a `wasi:http` one has no connector at all. Putting it on the seam would make every other backend answer for a thing it does not have — the mistake `Capabilities::upgrade` was deleted for."* | read, `crates/http-ng-native/src/lib.rs` |
| P7 | `http-ng-native` **already has a connect-and-hand-back entry point**: `Native::upgrade(req) -> Upgrading`, then `Upgrading::head()` and `Upgrading::finish() -> (I, Bytes)`. It dials with this transport's own connector, TLS and resolver, offers `http/1.1` alone, reads `Timeouts::connect` out of the request's extensions, and **never consults the pool**. There is also `Native::runtime() -> &R`. | read, `crates/http-ng-native/src/upgrade.rs` |
| P8 | `Native`'s pool is **entirely `pub(crate)`**: `Pool`, `PoolKey`, `CheckIn` and `Established` all are. Nothing outside `http-ng-native` can put a connection in or take one out, and `Established` is the type the pool stores — a *handshaken* connection, not a socket. | read, `crates/http-ng-native/src/pool.rs`, `src/established.rs` |
| P9 | `Native::run` applies `Timeouts::connect` to the **fresh-connect path only** (`with_connect_timeout(.., timeouts.connect, connect_fut)` wraps `connect::connect`); a pooled checkout is not inside it, and does no I/O. `H3::execute` puts its checkout inside the bound, and says why in a comment: *"A pooled checkout does no I/O at all, so it is inside the bound for want of a reason to write a second path rather than because it needs one."* | read, both crates' `lib.rs` |
| P10 | A connect that stops at "a connection that can carry a request" sends **no request** and is not silent: h1 writes nothing past the TLS handshake, h2 writes the client preface and SETTINGS (`h2::client::handshake`), h3 writes its SETTINGS on the control stream. | read, `crates/http-ng-native/src/http2.rs`, `src/h1.rs`, `crates/http-ng-h3/src/lib.rs` |
| P11 | `H3` **never hands out a connection it has not already claimed**. `H3::connect` builds `h3::client::builder().build(h3_quinn::Connection::new(conn.clone()))` and spawns the driver before returning, and `checkout` inserts the result into the pool before its caller sees it. | read, `crates/http-ng-h3/src/lib.rs` |
| P12 | `http_ng_webtransport::Session::connect` builds its **own** h3 client on the connection it is handed — `h3::client::builder().enable_extended_connect(true).build(..)`. Two h3 clients on one QUIC connection open two control streams, which RFC 9114 §6.2.1 makes `H3_STREAM_CREATION_ERROR`, a **connection** error. | read, `crates/http-ng-webtransport/src/lib.rs`; the rule is `docs/v04-w2-webtransport.md` §4b's, established there |
| P13 | `SeamRuntime` has been extracted into a crate of its own, `http-ng-rt-quinn`, which exposes `SeamRuntime`, `QuinnTask` and `pub fn endpoint(&R, SocketAddr) -> io::Result<quinn::Endpoint>`. It carries `ring` unconditionally, because `quinn::EndpointConfig` has no `Default` without a crypto provider. | read, in flight — the sibling branch's `crates/http-ng-rt-quinn/{src/lib.rs,Cargo.toml}` |
| P14 | The race harness is two `execute` calls and says so about itself: *"This is **not** what a race in `http-ng-select` would look like — there is no shared budget, no capability check and no pool interaction."* | read, `crates/http-ng-select/tests/race_cost.rs`, `async fn race` |
| P15 | At a **250 ms** head start the TCP arm opened **no socket at all**, 0 of 6; at 0 ms it opened one in 6 of 6 and the origin got a complete request in 5 of 6. | measured, §7.3 M3 — not re-measured here |
| P16 | §7.4 derives the head start from the **success** side — *"a QUIC handshake that will succeed does so in ≈ 1 RTT plus the 1–3 ms of crypto measured above. The head start has to exceed that"* — and states that the honest form is an RTT observation the crate has nowhere to keep. | read, §7.4 |
| P17 | `Selecting`'s constructor already **refuses** `Native::without_pool()` against `H3`, on the `connection_reuse` field, and that is the one refusal reachable from the two members this workspace ships. | read, `crates/http-ng-select/src/caps.rs` |

## 3. It is three asks, two gaps, and one of them is closed

### 3.1 Ask C wants a type exposed. That is not a seam, and it is done

Nothing about `SeamRuntime` is a question of shape. It is a `quinn::Runtime`
implemented over `http_ng_rt::{Timer, Spawn, UdpBind}`; it names no
transport, decides no policy and has no alternative implementation to
abstract over. The whole of the gap was that it sat behind `mod runtime` in
a crate whose subject is HTTP, and the whole of the fix is that it now sits
in a crate whose subject is the seam (P13).

Two things worth keeping from it, because they generalise.

**A pluggable thing that is not its own crate is unreachable, which is the
other way a shared thing stops being shared.** `docs/w4-upgrade-seam.md` §8
made this argument about `tungstenite` inside `http-ng-native`, where the
symptom was Cargo's additive features; here the symptom was the opposite
one — no feature at all, and therefore no way in. Same rule, both failure
modes.

**`Native::runtime() -> &R` is the same answer at a smaller size** (P7). A
crate that needs the transport's clock gets an accessor, not a trait. When
the thing wanted is a value the transport already holds, the ordinary
answer is the right one, and the interesting question is only which crate
it should live in.

So ask C is closed, and it is the reason ask B is closed too.

### 3.2 Ask B does not want a connect-only entry point, and one would break it

This is the finding that splits the three asks apart, and it does not
depend on any judgement about seams.

`Session::connect` builds its own h3 client on the connection it is handed,
with `enable_extended_connect(true)` (P12). `H3` builds an h3 client on
every connection it makes, before returning it, and spawns that client's
driver (P11). A second h3 client on the same QUIC connection opens a second
control stream, which is `H3_STREAM_CREATION_ERROR` — a connection error
that takes the h3 client's own requests down with it.

So a connect-only entry point on `H3` handing back a live
`quinn::Connection` would hand back a connection **WebTransport cannot
use**, and would do so whether the connection came from the pool or was
dialled a microsecond earlier. `docs/v04-w2-webtransport.md` §4b already
established this for the *pool* — three reasons in increasing hardness, of
which this is the first. What is new here is that the pool is not the
subject: `H3` has no state in which it holds an unclaimed connection, so
there is nothing for a connect-only entry point to return. Add the second
of §4b's reasons and it gets worse rather than better — `http-ng-h3`
announces `ENABLE_CONNECT_PROTOCOL = 0` in the SETTINGS of every connection
it has ever made, at handshake time, so even a connection with no h3 client
on it would be the wrong connection unless `http-ng-h3` changed what every
build puts on the wire.

**What WebTransport actually needs is a connection nobody has claimed**,
and after §3.1 that is composition rather than a seam: an endpoint from
`http_ng_rt_quinn::endpoint`, a `quinn::ClientConfig` built from
`http_ng_tls_quic::QuicTlsConnect::quic_client_config` (a public trait, and
`Rustls` implements it behind its `quic` feature), an address from a
`Resolve`, and `ALPN_H3`. That is `H3::connect` minus the h3 client and
minus the pool — a few dozen lines in `http-ng-webtransport`, owing nothing
to `http-ng-h3`.

It is not free, and the cost is countable: owning an endpoint means owning
`quinn::EndpointConfig`, which means `ring` (P13). `docs/v04-w2-webtransport.md`
§7 measured the current crate at 49 crates with *"`quinn` … with the feature
set `futures-io` alone — no `ring`"*, and named owning no endpoint as the
reason. Closing this gap spends exactly that.

**So §4a's two options are not alternatives.** Option 1 closes the gap;
option 2 does not close it and would introduce a defect if it were built
for this consumer. That sentence in §4a — *"which is also what a two-stack
race would need"* — is the only part of it that survives, and it survives
attached to a different consumer.

### 3.3 Ask A is the only one left, and it wants something narrower

Which leaves the race. It is the only one of the three that wants a *phase*
rather than a value, and §4 onward is about it alone. The empty-intersection
argument that split `QuicTlsConnect` from `TlsConnect` applies here in the
ordinary direction: "hand me the QUIC connection you have not made yet" and
"connect, and let me send on this one" share no method and no return type,
and a single seam covering both would be a seam with one implementation and
one caller each.

## 4. Decision 1 — not a method on `Transport`, and the reason is read rather than inherited

The shape is refused, and this is the third time. `WebSocketConnect` is not
a method on `Transport`; `Prefetch` is not a method on `Transport`;
`QuicTlsConnect` is not a widening of `TlsConnect`. But the argument in
each case was about *that* feature, and it has to be made again here rather
than cited, because a bad seam is not refuted by a family resemblance.

**What the two ambient backends could honestly answer, from their own
code and their own platform rather than from the protocol.**

`http-ng-wasi` could answer *nothing at all*. `wasi:http` 0.3's client
interface is one function (P2). There is no connection resource in the WIT,
no way to name one, and no state between two `send` calls that a guest can
observe. The phase is not merely unexposed — it is not in the ambient API's
vocabulary as a value. It **is** in it as a deadline
(`request-options.set-connect-timeout`, P3), which is the interesting half:
a backend can be able to *bound* a phase it can never *reach*, and a seam
that confuses the two would look satisfiable here and not be.

`http-ng-fetch` could answer *less than nothing*, in the sense that it
cannot even do the part WASI can. It declares `timeouts.connect = false`
and gives the reason where the field is set: one `AbortSignal` for the
whole exchange, and *"declaring any of the three would be a capability that
lies"* (P4). The browser's one connect-shaped API is a `<link
rel="preconnect">` hint (P5) — fire and forget, no handle, no readiness
signal, no failure signal, and no way to bind a subsequent `fetch()` to the
connection it may or may not have opened. A backend that implemented a
connect-only method through it would be implementing "ask the browser
nicely, then return `Ok(())` and hope", which is the capability lie this
workspace has caught four times, wearing a `Result`.

So a `Transport::connect` would be `Unsupported` for two of four backends
and would be *dishonest* rather than merely unimplemented for one of them.
That is `docs/w4-upgrade-seam.md` §2's shape exactly, and it is refused for
§2's reason.

**And there is a nearer precedent than §2's, in the same crate and on the
same pipeline.** `Prefetch` (P6) staged the phase one step earlier — name
resolution — and refused the seam with an argument that reads as if it were
written for this document: *"a `fetch`-shaped transport has no DNS of its
own to save, and a `wasi:http` one has no connector at all."* The second
clause is this decision, already taken, about a phase that had not come up
yet. So this is the **second instance of a rule**, not a second argument for
it, which is the relationship `docs/w4-upgrade-seam.md` §4 names between
WebSocket and WebTransport.

**Decision.** A staged connect lives on the backends that have a connector,
as `Prefetch` does: a trait declared by the crate that implements it, not
by `http-ng-core`. A trait rather than inherent methods for `Prefetch`'s own
mechanical reason — *"a caller generic over `Native<R, T, D>` reaches it
through a `where` bound, and an inherent method would make that caller
repeat every structural bound `Native`'s exchange impl declares"* — and one
per crate rather than one shared, because `http-ng-select` owns both members
concretely and needs no polymorphism between them.

## 5. Decision 2 — "connect-only" is a misnomer; the shape is `connect → handle → exchange`

### 5.1 Exactly what has to be true for the losing arm to send nothing

The brief for this document asked whether a connect-only entry point would
even fix §7.6's finding, since a connection made and then handed to
`execute` still races two *requests*. Worked out precisely, three conditions
have to hold, and only one of them is about connecting:

1. **The request is handed to a transport exactly once, after the race
   resolves.** This is the whole of the fix, and it is structural rather
   than a property of the connect call: the race becomes
   `select(connect_a, connect_b)` followed by *one* exchange, where today
   it is `select(execute_a, execute_b)`. Any entry point that lets the
   connect happen without the request satisfies it.
2. **The losing arm's connection is disposed of without sending
   anything.** Free on both stacks and already measured on one: §7.6 found
   the QUIC arm dropped mid-handshake emits *"exactly one further datagram,
   1.3–2.4 ms after the drop, and then silence for the whole 5 s watched"*,
   and a dropped TCP/TLS handle is a closed socket. There is a caveat in
   §7 for the case where the loser *finishes* connecting — see §7 below.
3. **The winner's exchange uses the connection the race produced.** Not
   needed for correctness, and needed for the race to mean anything: a
   race whose winner then opens a second connection has measured which
   stack connects faster and paid for the answer twice.

Condition 1 also fixes something the finding does not name. §7.6 observes
that a race *"is the second thing to break the sentence `http-ng-native`
leans on for needing no idempotency judgement"* — *"this is not a second
request, it is the first one, which never left."* With the request handed
over once, after the race, that sentence is **true again**: nothing left,
because nothing was given to a transport to send. The race stops needing
`RetryKind`, and stops needing the method-safety notion this codebase has
twice declined to invent.

**What is *not* fixed, and should not be claimed.** The losing arm still
opens a socket and completes a handshake (P10) — TCP + TLS, or QUIC plus
h3's SETTINGS. An origin racing at zero head start sees two connections. So
the honest claim is "the loser sends no **request**", never "the loser
sends nothing", and the head start remains worth having for **cost** even
once it is no longer needed for **safety**.

### 5.2 The two shapes, and what decides between them

Condition 1 is satisfied by two quite different APIs, and it is worth being
explicit that both fix the finding, because the weaker one is the one a
reader will reach for.

**Shape W — warm the pool.** `fn connect(&self, uri) -> Result<()>`, which
dials and leaves the connection in this transport's pool; the winner is
then served by an ordinary `Transport::execute` that happens to find it
there. New public types: none.

**Shape H — a handle.** `fn connect(&self, req) -> Result<Connected>` and
`fn execute_on(&self, conn: Connected, req) -> Result<Response<..>>`, where
`Connected` is opaque, is produced only by this crate and consumed only by
it. This is `Prepared`'s shape one phase later, and `Upgrading`'s shape with
the exchange put back (P6, P7).

Three things decide it, and the third is decisive.

- **Shape W is silent where it does nothing.** Against
  `Native::without_pool()` it connects, throws the connection away, and
  returns `Ok(())`; the request then opens a second one. That is silent
  degradation, which this workspace refuses by name. `Selecting` happens to
  be safe from it — its constructor already refuses `without_pool()`
  against `H3` (P17) — but the entry point would live on `Native`, whose
  other callers are not so constrained, and a seam that is correct only
  because of a neighbour's capability check is a seam with an undeclared
  precondition.
- **Shape W leaves condition 3 to coincidence.** A pool is shared; the
  raced connection can be taken by a concurrent request, reaped, or
  expired between the connect and the exchange. Nothing is wrong when that
  happens — but nothing is guaranteed either, and the measurement the race
  exists to act on becomes noise.
- **Shape H is the only one under which `Timeouts::connect` cannot be
  spent twice** — §6.

**Decision: shape H.** `Connected` is opaque, carries its own `PoolKey`
(so `execute_on` can still mint the `CheckIn` that returns the connection
to the pool afterwards), and is never constructible from outside the crate
that produced it. The wrong-connection question is then not answered — it
cannot be asked, which is precisely `Prepared`'s own argument for pairing a
record with the request it was fetched for.

**What `Connected` must not be.** It must not be `Established` made public
(P8): that is hyper's `SendRequest` and h2's, and exporting them would put
two third-party public APIs into this crate's surface for the benefit of one
caller inside the workspace. `Upgrading` is the model — it holds a
`hyper::client::conn::http1::Connection` and hands back an `I`, naming
neither in what a caller has to write.

## 6. Decision 3 — `Timeouts::connect` keeps its meaning exactly when the second call cannot connect

The brief asked whether `Timeouts::connect` still means anything once
connecting is a separate call. It does, and the condition under which it
does is sharp enough to be a design constraint rather than a note.

**What it means today.** It is a per-request extension read out of
`req.extensions()` by each backend, and it bounds "from a URI to a
connection that can carry a request" — DNS included, deliberately, on both
stacks. On `Native` it wraps the fresh-connect path only; a pooled checkout
is outside it and does no I/O (P9). On `H3` the checkout is inside the
bound, with the reason recorded as "for want of a reason to write a second
path". `TimeoutSupport::connect` is the capability that declares it, and
`http-ng-fetch` sets it `false` (P4).

**What a staged pair does to it.** The connect call spends it — and it must
be *able* to, which is why the connect call takes a request rather than a
bare URI: `Timeouts` is a request extension, and `Native::upgrade` already
reads it out of an `http::Request<()>` for exactly this reason (P7). The
question is then what the second call does with the same extension, still
sitting on the same request.

Under **shape W** the answer is bad: the exchange call may still connect —
that is the whole of its failure mode — so it reads `Timeouts::connect` and
applies it again. The caller who set `connect: Some(C)` can be made to wait
`2C`, which is §7.5's rule broken in the plainest way, and it is the same
defect `Client`'s `425` replay had to be built around: *"a bound a server
can double by answering `425` is not a bound."*

Under **shape H** the question dissolves. `execute_on` is handed a
connection; there is no connect for a bound to bound. `Timeouts::connect`
is read by the connect call and is structurally unspendable by the exchange
call — not "ignored", which would need a comment and a test, but absent,
because the code path is absent.

**Decision.** `Timeouts::connect` keeps its meaning unchanged, and the
staged pair is arranged so that it can only be spent once. The capability
`TimeoutSupport::connect` is untouched: it is a claim about
`Transport::execute`, and `execute` is unchanged.

**One thing this does not settle**, and it is `http-ng-select`'s rather than
the seam's: with a head start `H` and a connect bound `C`, the second arm
gets `C − H` and not `C` (§7.5). That is the caller's arithmetic, it is
already written down, and a staged pair neither helps nor hinders it.

## 7. What the pool says, and it has already said most of it

The brief noted that a connection produced outside the pool and then used
by `execute` is a question the pool has opinions about. Read rather than
assumed, it has four, and three of them are already satisfied by the shape
§5.2 chose.

**It cannot be asked from outside, and must not become askable.** `Pool`,
`PoolKey`, `CheckIn` and `Established` are all `pub(crate)` (P8). So
"a connection produced outside `http-ng-native` and handed to `execute`" is
not expressible today, and shape H keeps it that way: `Connected` is
produced by `Native` and consumed by `Native`, and the fact that a caller
holds it in between changes nothing about who made it.

**A connection nobody polls is the pool's normal state, so holding one
across a race window costs nothing new.** `pool.rs`'s module doc opens with
*"Nobody polls an idle connection, and that is the design"*, and enumerates
what that costs: an idle connection reads nothing, so neither a `FIN` nor
anything the server sends unbidden is noticed until checkout. A `Connected`
held for the length of a head start is in exactly that state, and the
existing check-at-checkout (`h1::is_reusable`, `http2::is_reusable`) is the
existing answer. The residual window the same doc records — *"a server may
close between our check and our write"* — widens by the head start and is
otherwise the same window.

**QUIC is the exception, and it is already handled.** *"A QUIC connection
that nobody polls is not idle, it is dying"* — but `H3` spawns the driver
inside `connect`, before the connection is returned (P11), so an `H3`
`Connected` is driven for as long as it is held. That is the one place
where the two stacks differ and the one place where the difference is
already paid for.

**The check-in must survive the split.** `Native::run` mints its `CheckIn`
from a key derived from the request's URI and the *negotiated* protocol,
after the connect and before the exchange. Split in two, the protocol is
known at the end of the connect call, so the key can be computed there and
carried in `Connected` — which is the concrete reason §5.2 says the handle
carries its own `PoolKey` rather than having `execute_on` recompute one.
Recomputing would be two places holding one fact, which is the class of
invariant this crate says it tries not to have.

**And one thing the pool wants of the connect call itself.** It must be
allowed to answer "I already had one". `Native::run` looks in the pool
before it dials, and a staged connect that always dialled would cost a
connection at every origin the pool was already serving — and would make
the race's answer wrong in the common case, since a warm origin's arm would
lose to a cold one. So the connect call is `run`'s steps 1 and 2 without
the exchange, not `connect::connect` on its own. `Native::upgrade` is the
counter-example that proves it is a choice: it *does* refuse the pool, and
says why — a socket that stops speaking HTTP is not a connection any later
request could use. A raced connection is one, so the reasoning does not
carry over.

## 8. The first customer is not the race

The strongest reason to build this is not deliverable 5, and finding that
out is the part of this investigation that changed the answer.

`docs/v04-w1-acceptance.md` §9.3 records the Alt-Svc negative half as
unbuilt, and gives two blockers. The first:

> **Without a fallback it degrades the caller rather than protecting
> them.** A windowed suppression on `http-ng-native`'s model would cost one
> *failed* request per window per origin — where native's own costs none,
> because native falls back to the origin's addresses inside the same
> connect. The equivalent here is falling back from QUIC to TCP inside
> `execute`, which is request-level retry with a
> `RequestBody::retry_kind()` condition on it, and is the same mechanism
> deliverable 5 is about.

That blocker is **entirely a consequence of the same gap**, and a staged
connect removes it without any race at all. Sequentially: `Selecting` asks
`H3` to connect; if the connect fails or hits `Timeouts::connect`, it routes
the request — untouched, unsent, never handed to a transport — over TCP. No
retry, no `retry_kind()`, no idempotence judgement, because there is no
second request. `http-ng-native`'s own sentence applies verbatim: it is the
first request, which never left.

This is cheaper than a race in every dimension. It opens one connection in
the common case instead of two; it needs no head start, so §7.7's items 1
(an origin-keyed RTT store) and 2 (a budget rule that subtracts) do not
arise; and it is what an Alt-Svc-routed request needs in order to survive a
network that blocks UDP/443, which is the case §9.3 says is unprotected
today.

What it costs is a serial connect attempt before the fallback, and that
cost is a number this workspace already has: unbounded it is **30 s**
(§7.3, M1 — quinn's `max_idle_timeout`, the same on a black hole and on an
origin with no h3 server at all), and bounded it is whatever
`Timeouts::connect` says, honoured to within **0.5–2.0 ms** at 100 ms,
300 ms, 1 s and 3 s (§7.3, M1b). So the sequential fallback is usable
exactly when a connect bound is set, and unusable when it is not — which is
a precondition to state and check, in §7.5's spirit, rather than a
degradation to discover.

**The race is then what it was always described as: a hedge, applied after
the choice, for the case where even a bounded serial attempt is too slow.**
And the relationship between the two mechanisms and the head start comes
out clean:

- **Today**, the head start is the safety mechanism, and §7.4 says so —
  *"the head start is not a latency knob. It is the safety mechanism."*
- **It is a probabilistic one.** §7.4 derives it from the success side: it
  must exceed one RTT plus 1–3 ms of crypto (P16). At 250 ms on loopback
  the loser opened no socket in 6 of 6 (P15); on a path with a 300 ms RTT
  the same constant would put a socket — and, today, a request — at the
  origin every time. The constant is safe on fast paths and on no others,
  and the document already says the honest form is an RTT observation the
  crate has nowhere to keep.
- **With a staged connect the head start stops being a safety mechanism
  and becomes a cost knob**, which is what it should have been all along:
  set it to zero and the race is a real race that duplicates no request;
  set it to 250 ms and it is a delayed hedge that usually costs nothing.

## 9. What this document does not decide

**Since built, and three of these are now answered** —
[`docs/v04-staged-connect.md`](v04-staged-connect.md). The seam is
`StagedConnect` on `http-ng-native` and on `http-ng-h3` (one trait per
crate, §4's decision, and building it produced the concrete reason: the two
do not agree on what `connect` takes). The first customer is §8's, and is
built: Alt-Svc's negative half in `http-ng-select`. Read the answers below
against the bullets they belong to.


- **Whether to build any of it.** The race's other three needs (§7.7 items
  1, 2 and 4 — an RTT store, a budget rule, a failure memory) are
  untouched by this and are unbuilt. The sequential fallback of §8 needs
  none of them, which is an argument for its order and not for its
  scheduling.
- **The name.** "Connect-only entry point" is what all three asks called
  it and is what §5 finds it is not. `Prefetch`/`Prepared` is the naming
  precedent for the phase before; whether the pair is a second method on
  `Prefetch` or a trait beside it is an implementation choice with a real
  argument on each side (one trait keeps one `where` bound at the caller;
  two keep a backend free to stage DNS without staging connects).
- **Whether `H3` gets one at all.** The race needs both stacks staged;
  the sequential fallback of §8 needs only the QUIC side, and needs it in
  its weakest form — *"did this origin's QUIC connect succeed"* — for which
  a handle may be more than is required. That is worth measuring against a
  real fallback before deciding, and it is not measurable from here.

  **Answered: it needs one, and the reason is the bound rather than the
  connection.** `H3::execute` resolves the origin's address *before* it
  looks in the pool, inside `Timeouts::connect`, so the weakest form leaves
  a second call able to spend the bound again — §6's criterion, applied to
  a stack §6 was not looking at. What the handle *is* differs from
  `Native`'s and could not have been predicted from `Native`'s side: since
  `connect` builds an h3 client and spawns its driver before it has
  anything to hand back, the handle is a **claim on a connection the pool
  also holds**, not an unclaimed connection. Both satisfy the property,
  because the property is about the second call's code path.
- **What happens to the loser's connection when it finishes rather than
  being dropped mid-handshake.** On `H3`, `checkout` inserts before it
  returns and the driver keeps a 5 s keep-alive running (P11), so a losing
  QUIC arm that completes leaves a pooled, pinging connection at an origin
  the caller declined. Whether that is a leak or a warm connection for the
  next request is a policy question with a measurable answer, and neither
  §7.6 (which only measured the mid-handshake drop) nor this document has
  it.

  **Answered as "a warm connection", on both stacks, by different means to
  the same observable end.** `http_ng_native::Staged` has a `Drop` that
  checks the connection in — nothing was spoken on it, so it is exactly the
  connection the pool would have held had the request never been staged;
  with `without_pool()` there is no check-in and the drop closes the
  socket, which is the control. `http_ng_h3::Staged` needs no `Drop`,
  because `checkout` pooled the connection before the caller saw it. The
  pinging is real and is stated rather than fixed: it matters to a race,
  and the one consumer connects on the arm it intends to use.
- **Anything about `http-ng-select`'s capability set.** `Selecting`'s
  `Capabilities` is combined at construction and its rule — *"the stored
  value must be true whichever member serves the request"* — is unaffected
  by how a member is asked to connect. If a race is ever built,
  `CancelSupport` is the field to re-read first, for §7.6's reason.
- **Permessage-anything about WebTransport.** §3.2 says what closes ask B;
  `docs/v04-w2-webtransport.md` §6 owns the list of what it still does not
  do.

## 10. What could not be verified

- **That `<link rel="preconnect">` fires no `load`/`error` event.** MDN
  documents it as a hint and documents no event (P5); the HTML standard's
  §4.6.8 processing model for the link type was not reachable through the
  fetch this document had available. The decision does not rest on it:
  even an observable preconnect gives no handle and no way to bind a
  `fetch()` to a connection, so `http-ng-fetch` could satisfy neither
  condition 3 nor the "tell me when it can carry a request" half.
- **Anything measured.** No number in this document was taken here. §7's
  and §4b's were re-read, not re-run, and P15/P16 are cited to their
  source.
- **The sibling branch.** P13 describes `http-ng-rt-quinn` as it stood on
  another agent's worktree while this was written; §3.1 and §3.2 depend on
  the fact of the extraction rather than on its API, but the signature
  quoted may have moved.
- **That shape H compiles.** `Connected` carrying a `PoolKey` and an
  `Established<NativeIo<R, T>>` while remaining opaque is an ordinary
  private-field struct, and nothing in it looked doubtful; but no probe
  crate was built for it, and the projection problems `Prefetch`'s own doc
  records for `<Native<..> as Transport>::Body` are the kind of thing that
  only shows up at the caller.

  **It does**, and the projection was the one place it showed: the caller
  (`http-ng-select`) names the two member bodies through
  `<Native<..> as Transport>::Body` in a `where` clause, exactly as it
  already did, and the staged pair adds no new one. The handle is named
  `Staged` rather than `Connected` for a duller reason than any of this:
  `Connected` is already the hook event both crates emit three lines after
  making a connection.
