# v0.4 design — one client that chooses, and a client you can see into

v0.3 finished the protocols. This vertical is about the two things that
having three of them made visible: **nobody can choose between them**, and
**nobody can watch what the choice cost**. A third strand — a backend on
Apple's own stack — is here because it is the only one that tests the
`Capabilities` model against a second owner of its own policy.

Written the way `docs/v03-design.md` was: **premises first, each marked
with how it is known**, then the work. A premise that says *measured* was
run in this tree today; one that says *unverified* is a thing the first
task of its workstream must settle before the rest of it means anything.

---

## 0. What changed under the roadmap

The original spec's v0.4 (`docs/superpowers/specs/2026-08-05-http-ng-design.md`)
is headed *"`http3` becomes default"*, and it was written when h3 was
expected to be a feature of `http-ng-native`. It is not, and could not have
been: the transport is bounded on `R: UdpBind + Spawn<..>` and
`T: QuicTlsConnect`, `Native<R, T, D>` has neither, and Cargo's features
are additive.

So "default" is not a flag to flip. v0.3 W2 hit the consequence while
wiring HTTPS-record discovery and wrote it down:

> `SvcbEndpoint::alpn` containing `h3` is a fact `http-ng-native` can read
> and cannot act on… **There is nowhere in this codebase for "choose
> between two protocol stacks" to live.**

That is W1 below, and it is the centre of this vertical.

Two further items move. `http-ng-nyquest` is **out** — the wrapper's own
model would sit between ours and the platform's, which is two translations
and a foreign type in the middle; W3 writes on `objc2-foundation` directly.
And gRPC arrived as a question during v0.3's close, so W2 carries what it
needs — none of which is gRPC-specific.

---

## 1. Premises

| # | premise | how it is known |
|---|---|---|
| P1 | One value can own both stacks and still be a `Transport` | **Measured.** A probe crate compiles `Racing<Native<TokioHandle, Rustls, SystemDns<TokioHandle>>, H3<TokioHandle, Rustls, SystemDns<TokioHandle>>>` and calls `Transport::capabilities` on it. One runtime type satisfies both bound sets |
| P2 | …but only with `http-ng-rt-tokio`'s `udp` feature on | **Measured.** Without it the same probe fails `E0277: TokioHandle: UdpAdoptStd is not satisfied`. Cargo unifies features, so a build that wants both gets it and an h1-only build does not pay |
| P3 | The racing transport must **store** its capabilities | **Measured.** `Transport::capabilities(&self) -> &Capabilities` returns a reference (`http-ng-core/src/unversioned/transport.rs:92`), so a floor computed per call cannot be returned |
| P4 | The floor is not always defined | **Measured.** `Native` declares `RedirectSupport::Configurable`, `H3` declares `Transparent`. The four variants are not ordered, and these two have no meet |
| P5 | …and P4 has a cause worth fixing first | **Measured.** Only `RedirectSupport::Internal` is branched on anywhere (`http-ng/src/config.rs:342`). `Configurable`'s entire doc is one sentence — *"We set the policy."* — where the others have paragraphs, and `http-ng-native` contains **no redirect handling at all** (zero matches for `Location` or `30x` in its `src/`). Its declaration is unforced at best |
| P6 | `full_duplex: false` on `http-ng-native` is not a declaration, it is the code | **Measured.** `http2::exchange` writes the whole request body before awaiting the response, and `tests/http2.rs` pins the floor with the feature on |
| P7 | The h2 path already *handles* trailers | **Measured.** `send_trailers` on the write side and `into_trailers` on the read side both exist in `http2.rs`; the `false` in `Capabilities` is the HTTP/1.1 floor, not the ceiling |
| P8 | `objc2-foundation` is already in this workspace's Apple graph | **Measured.** `http-ng-idn` pulls `objc2-foundation 0.3.2` on `aarch64-apple-darwin`, so W3 adds no new vendor to that target |
| P9 | The platform verifier matches a name against an **IP SAN** on Linux | **Measured** in v0.3's live-DoH run, and **only** on Linux — `rustls-platform-verifier` delegates to Security.framework and CryptoAPI elsewhere |
| P10 | NSURLSession hands bytes to a delegate that cannot be polled | **Prior research, not re-measured.** The spec cites `frakt` 0.1.0's push-based `mpsc::Receiver<Bytes>` for exactly this |
| P11 | Background transfer outlives the process | **Unverified, and W3's first task.** `Transport::execute` returns a future in our address space; a transfer that survives process death may not fit behind it at all |
| P12 | Alt-Svc is the only way h3 gets chosen | **Unverified.** v0.3 W2 established that HTTPS-record discovery *cannot* choose it; whether Alt-Svc can, and where its cache lives, is W1's first question |
| P13 | An observability hook can avoid a `Send` bound | **Unverified, and W2's first task.** Every other seam here manages it, but a hook stored in a transport and called from a body is a different shape |
| P14 | Android has no crate to lean on | **Prior research, not re-measured.** Cronet is C++ with a C API; OkHttp is JVM and needs JNI, which puts a VM handle in a constructor no other backend has |

**P4 and P5 together are the finding of this document.** The racing
transport does not merely need a floor function — computing one *surfaces*
a capability nobody had to be right about, because nothing observes the
difference. That is the shape this project has caught four times, met from
a new direction: not a capability that lies to a caller, but one that no
caller could have caught lying.

---

## W1 — A transport that chooses

**Why.** Three protocols, and the only way to pick one is to name its type
at construction. A caller who wants "HTTP/3 where the origin offers it,
HTTP/1.1 or HTTP/2 otherwise" — which is what every browser does — has
nowhere to say so.

**The shape, from P1 and P3.** A `Racing<A, B>` owning one of each, with
`type Body = Either<A::Body, B::Body>` and a **stored** `Capabilities`.
Both are `Transport`s already; nothing new goes on the seam.

**The part that is not mechanical, and it is not the race.** It is P4/P5.
Two answers are possible and they are not equivalent:

- **Report the meet.** Needs a meet to exist. It does not for `redirects`
  today, and inventing an order over four variants to make one exist is
  deciding a semantic question to satisfy a helper function.
- **Constrain the members.** The racing transport requires its two stacks
  to *agree*, and refuses to be constructed when they do not.

**Take the second.** A capability is a promise about what a caller will
observe, and under a racing transport the caller does not know which stack
answered — so a promise that holds for one and not the other is not a
promise. Refusing at construction is the same shape as
`UnsupportedCapability` at `build()`: the error arrives where the mistake
was made.

**Deliverable, and the order is load-bearing.**

1. **Settle `RedirectSupport` first.** Establish what `http-ng-native`
   actually does (P5 says: nothing), fix the declaration, and decide
   whether `Configurable` earns its variant at all under v0.2's rule — *a
   variant exists only if a caller decision turns on it*, and today none
   does. **This is a v0.3-era defect and lands independently of the rest**,
   so a racing transport is not the reason it is fixed.
2. `Racing<A, B>` with a stored floor, and a constructor that refuses
   disagreement, naming the field.
3. Alt-Svc: the cache, its scope, and its negative half. P12 is unverified
   — settle whether Alt-Svc can carry the choice before building the cache
   it would need.
4. The race itself, its fallback, and the "broken backoff" the original
   spec §5.6 sketched.

**Deliberately not in it.** `DefaultTransport` does **not** become
`Racing`. Making a default that opens UDP sockets is a decision about what
a plain `Client::new()` does on a network that blocks UDP/443, and it wants
the negative-cache measurement v0.3 W2 recorded as unverified — *"the size
of the cost is unverified"*. One vertical, one claim.

---

## W2 — Seeing in, and the four things gRPC needs

**Why together.** They are the same request from two directions: a caller
who cannot see which protocol was used, when the connection was made, or
why a request was slow, and a protocol layer that cannot ask the transport
for what HTTP/2 can actually do. Both are `Capabilities` and observation.

### The gRPC part

Measured against what gRPC over HTTP/2 requires:

| gRPC needs | today | what it costs |
|---|---|---|
| HTTP/2 | behind `http2`, off by default | nothing — an explicit dependency |
| response trailers (`grpc-status` lives there) | **declared `false`**, code present (P7) | a way to declare what is true when h2 is negotiated |
| streaming request body | `true` on native | nothing |
| **full duplex** | **`false`, and it is the code** (P6) | the h3 treatment applied to `http2.rs` |

So **unary and server-streaming are within reach; client-streaming and
bidirectional are not**, and the blocker is one implementation fact rather
than a missing feature.

**Deliverable.**

1. **Duplex on the h2 path.** Split the exchange the way v0.3 did for h3:
   the body written from a future polled *beside* the response, not before
   it. h3's `pump.rs` and its three found defects are the template —
   especially that a cancelled upload must not poison a shared connection.
2. **A capability that can say "h2 was negotiated".** The floor is right
   for a *static* answer and wrong for a caller holding an open connection.
   `version_reported` is the precedent: the honest time to answer is after
   the fact. Decide whether this is a per-response fact or a per-connection
   one — **and do not widen the static floor**, which exists because Cargo
   unifies features across a graph.
3. **Event hooks.** Connection established, protocol negotiated, request
   queued, first byte, connection reused, connection closed and why. P13 is
   unverified and comes first: a hook is stored in a transport and called
   from a body, which is not the shape any existing seam has.

**Deliberately not in it.** No gRPC crate. The frame codec, `grpc-timeout`,
status codes and metadata are a layer over `Client`, exactly where the
cookie jar and WebSocket went, and building it here would make a transport
concern out of one that is not.

---

## W3 — `http-ng-urlsession`, and what it is really for

**Why, and it is not speed.** App Transport Security, the system trust
store, CAs pushed by MDM, per-app VPN, the system proxy and its PAC, and
background transfer. Every one is a fact about an environment rather than a
preference, which is the same argument that justifies
`http-ng-tls-native-tls`.

**The reason it is worth more than a fourth point on a line.** URLSession
decides redirects, cookies, caching and proxying itself — like the browser.
So it will declare `RedirectSupport::Internal` and `owns_cookie_jar = true`,
and **today exactly one backend holds those variants**. A variant with one
carrier is indistinguishable from a variant shaped around that carrier;
a second, on a different platform and for a different reason, is what
tests the model. That is also a 1.0 condition — *plugin traits validated
against ≥3 backends*.

**Deliverable.**

1. **Settle P11 before anything else.** If background transfer cannot live
   behind `Transport::execute`, that is a finding about the seam, not an
   obstacle: it would be the same shape as *"the browser cannot implement
   upgrade at all"*, which is how the WebSocket seam got decided. Report it
   rather than working around it.
2. The push→poll bridge for the delegate (P10). The technique is in this
   tree twice already — `http-ng-fetch`'s body streaming and its
   `FetchWebSocket`, which needed no `unsafe impl Send` because no `Client`
   sits between that seam and its caller.
3. The refusals. URLSession will not carry everything a request can, and
   naming precisely what it drops is the deliverable, not an afterthought —
   `http-ng-fetch`'s WebSocket does this and it is the model.

**Deliberately not in it: Android.** Not on size but on shape. Apple has a
crate and it is our ordinary kind; Android has none (P14), and the answer
puts `jni` in the graph and a VM handle in a constructor no other backend
asks for. It gets its own research before it gets a task, and binding it to
iOS in one plan item would hide its cost.

---

## W4 — WebTransport, if the vertical has room

`docs/w4-upgrade-seam.md` §4 already decided this: a separate seam, for the
durable reason rather than the two the original spec gave, both of which
have expired. Extended CONNECT **is** reachable from `h3` 0.0.8's client
API, and `WebTransport` now ships in all four browser engines (Safari 26.4,
2026-03-24). What remains is the session layer — streams bound to a session
id, demultiplexing, the capsule protocol — and no crate provides it for a
client under our runtime seam.

It is last because it depends on nothing here and nothing here depends on
it, which makes it the cheapest thing to drop.

---

## What this document does not decide

- **Whether `http3` becomes the default.** W1 builds the thing that would
  make it possible; making it the default is a separate claim needing the
  UDP-blocked-network measurement.
- **The compio backend**, which the original spec listed under v0.3 and
  which no one has asked for since.
- **`no_std`.** Unchanged and still external: `http` 1.x carries a
  `compile_error!`, and the answer stays no until that moves.
- **The 1.0 condition "not a single foreign type in the public API."** It
  is in tension with a deliberate decision — `http::{Request, Response,
  HeaderMap, Uri, Method}` and `bytes::Bytes` are across ten crates here,
  and that is what makes porting a consumer line-for-line possible. The
  condition needs rewriting or the decision needs reversing, and neither
  belongs in a feature vertical.
