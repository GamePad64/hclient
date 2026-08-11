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
| P1 | One value can own both stacks and still be a `Transport` | **Measured.** A probe crate compiles a two-stack type over `Native<TokioHandle, Rustls, SystemDns<TokioHandle>>` and `H3<TokioHandle, Rustls, SystemDns<TokioHandle>>` and calls `Transport::capabilities` on it. One runtime type satisfies both bound sets |
| P2 | …but only with `http-ng-rt-tokio`'s `udp` feature on | **Measured.** Without it the same probe fails `E0277: TokioHandle: UdpAdoptStd is not satisfied`. Cargo unifies features, so a build that wants both gets it and an h1-only build does not pay |
| P3 | The selecting transport must **store** its capabilities | **Measured.** `Transport::capabilities(&self) -> &Capabilities` returns a reference (`http-ng-core/src/unversioned/transport.rs:92`), so a floor computed per call cannot be returned |
| P4 | The floor is not always defined | **Measured, and the instance it was found on is closed.** It read: `Native` declares `RedirectSupport::Configurable`, `H3` declares `Transparent`, and those two have no meet. Both declare `Transparent` since W1 deliverable 1, so `redirects` has a meet today **because the members agree**, not because an order was found. The premise survives its own fix: the three remaining variants are still unordered, and `Internal` against `Transparent` — a selecting transport over `Native` and an `http-ng-urlsession` background session (W3) — has no meet either. W1's answer, *constrain the members rather than invent an order*, is unchanged |
| P5 | …and P4 has a cause worth fixing first | **Measured, and fixed — W1 deliverable 1, landed independently.** Only `RedirectSupport::Internal` was branched on anywhere (`http-ng/src/config.rs`), `Configurable`'s entire doc was one sentence — *"We set the policy."* — and `http-ng-native` contains **no redirect handling at all** (zero matches for `Location` or a 3xx status in its `src/`). The declaration was not merely unforced, it was wrong: `Native` says `Transparent` now, and `Configurable` and `Inspectable` are **deleted** — no branch, no carrier, one-sentence docs, which is what `UpgradeSupport` was deleted for in v0.3 W4. See §W1 deliverable 1 for the URLSession evidence that no carrier is arriving, and for the mutation run that says which variants are distinguishable at all |
| P6 | `full_duplex: false` on `http-ng-native` is not a declaration, it is the code | **Measured.** `http2::exchange` writes the whole request body before awaiting the response, and `tests/http2.rs` pins the floor with the feature on |
| P7 | The h2 path already *handles* trailers | **Measured.** `send_trailers` on the write side and `into_trailers` on the read side both exist in `http2.rs`; the `false` in `Capabilities` is the HTTP/1.1 floor, not the ceiling |
| P8 | `objc2-foundation` is already in this workspace's Apple graph | **Measured.** `http-ng-idn` pulls `objc2-foundation 0.3.2` on `aarch64-apple-darwin`, so W3 adds no new vendor to that target |
| P9 | The platform verifier matches a name against an **IP SAN** on Linux | **Measured** in v0.3's live-DoH run, and **only** on Linux — `rustls-platform-verifier` delegates to Security.framework and CryptoAPI elsewhere |
| P10 | NSURLSession hands bytes to a delegate that cannot be polled | **Prior research, not re-measured.** The spec cites `frakt` 0.1.0's push-based `mpsc::Receiver<Bytes>` for exactly this |
| P11 | Background transfer outlives the process | **Unverified, and W3's first task.** `Transport::execute` returns a future in our address space; a transfer that survives process death may not fit behind it at all |
| P12 | Discovery has two tiers and a race is neither of them | **Researched, not measured here.** Browsers do not race on first contact — they "only try QUIC if they know the server supports it", so an unknown origin gets TCP. Alt-Svc is the slow tier: cached from a response header, so QUIC starts from the *next* connection and the first page load is never h3. An HTTPS record is the fast tier and exists precisely to remove that penalty — it arrives at resolution time, so QUIC can be used on the first connection. Racing is a **third** thing, applied *after* the choice as a hedge against networks that block UDP: "connection racing is still needed in practice" ([Marx, Smashing Magazine](https://www.smashingmagazine.com/2021/09/http3-practical-deployment-options-part3/)) |
| P13 | An observability hook can avoid a `Send` bound | **Unverified, and W2's first task.** Every other seam here manages it, but a hook stored in a transport and called from a body is a different shape |
| P14 | Android has no crate to lean on | **Prior research, not re-measured.** Cronet is C++ with a C API; OkHttp is JVM and needs JNI, which puts a VM handle in a constructor no other backend has |

**P4 and P5 together are the finding of this document.** The selecting
transport does not merely need a floor function — computing one *surfaces*
a capability nobody had to be right about, because nothing observes the
difference. That is the shape this project has caught four times, met from
a new direction: not a capability that lies to a caller, but one that no
caller could have caught lying.

---

## W1 — A transport that chooses

> **On the name.** An earlier draft called this `Racing<A, B>`, which put a
> policy in a type name before the policy was decided — and P12 says the
> race is the *hedge*, not the chooser. The type is named for what it does
> (selects a stack per origin) and not for one of the mechanisms it uses.

**Why.** Three protocols, and the only way to pick one is to name its type
at construction. A caller who wants "HTTP/3 where the origin offers it,
HTTP/1.1 or HTTP/2 otherwise" — which is what every browser does — has
nowhere to say so.

**The shape, from P1 and P3.** One type owning one of each, with
`type Body = Either<A::Body, B::Body>` and a **stored** `Capabilities`.
Both are `Transport`s already; nothing new goes on the seam.

**The part that is not mechanical, and it is not the race.** It is P4/P5.
Two answers are possible and they are not equivalent:

- **Report the meet.** Needs a meet to exist. It does not for `redirects`
  today, and inventing an order over four variants to make one exist is
  deciding a semantic question to satisfy a helper function.
- **Constrain the members.** The selecting transport requires its two stacks
  to *agree*, and refuses to be constructed when they do not.

**Take the second.** A capability is a promise about what a caller will
observe, and under a selecting transport the caller does not know which stack
answered — so a promise that holds for one and not the other is not a
promise. Refusing at construction is the same shape as
`UnsupportedCapability` at `build()`: the error arrives where the mistake
was made.

**Deliverable, and the order is load-bearing.**

1. **Settle `RedirectSupport` first — done, and it landed independently of
   everything else here**, so a selecting transport is not the reason it
   was fixed. `http-ng-native` says `Transparent`, which is what it always
   did; `Configurable` and `Inspectable` are deleted. Three variants left:
   nobody follows, `Client` follows, the backend follows.

   **Deleted rather than documented, and the deciding evidence was not the
   missing branch.** `Configurable` was not merely unused, it was
   *unimplementable*: the merged `RedirectPolicy` never crosses the seam.
   `Client::run` merges the client-level and per-request policy and
   deliberately does not write the result back into the request's
   extensions — *"no transport reads a `RedirectPolicy`"* (`client.rs`) —
   so a backend claiming to set the policy would see only what a
   `RequestBuilder` happened to leave in the extension bag, and
   `Client::builder(..).redirect(Limited(2))` would be silently ignored by
   the one variant whose name promises to honour it.

   **URLSession was the carrier to check for, and it is not one.** W3's own
   text predicts `RedirectSupport::Internal` for `http-ng-urlsession`, and
   that prediction stands with a caveat that is now written down. Apple's
   hook is
   `urlSession(_:task:willPerformHTTPRedirection:newRequest:completionHandler:)`,
   whose completion handler takes *"either the value of the `request`
   parameter, a modified URL request object, or `NULL` to refuse the
   redirect and return the body of the redirect response"*, and which *"is
   called only for tasks in default and ephemeral sessions. Tasks in
   background sessions automatically follow redirects."*
   ([developer.apple.com](https://developer.apple.com/documentation/foundation/urlsessiontaskdelegate/urlsession(_:task:willperformhttpredirection:newrequest:completionhandler:)))
   There is no `maximumRedirects` on `URLSessionConfiguration` and no
   declarative policy of any kind — the only knobs are follow, rewrite, or
   refuse, one hop at a time. So the platform yields exactly two of the
   three surviving variants: **`Internal` for a background session**, where
   there is no hook to install and P11's transfer-outlives-the-process case
   lives, and **`Transparent` for a default or ephemeral one**, by
   answering `nil` so the 3xx becomes the task's response and `Client`'s
   stage does the chain. A third reading — follow inside the delegate and
   count hops there — is not a platform affordance, it is a second
   implementation of `http_ng_proto::redirect`, and it would silently drop
   what that stage carries per hop: `SENSITIVE_HEADERS` stripped across an
   origin, cookies re-derived rather than carried, the `AllowEarlyData`
   mark taken off. Reason enough to prefer `Transparent` even where the
   platform permits the other.

   `Inspectable` went with it rather than after it: *"we set the policy and
   see every hop"* is `Configurable` plus an increment, and a variant
   defined in terms of a deleted one is not defined.

   **What the mutation run found, and it belongs here rather than in a
   commit message.** Anchor 362 tests (`-p http-ng-native -p http-ng
   --all-features`), `Native` made to declare each variant in turn:
   `Internal` fails **two** — the capability read-back in
   `http-ng-native/tests/transport.rs`, and
   `http-ng::deadline::the_deadline_spans_redirect_hops_rather_than_restarting_on_each`,
   which dies at `build()` with `UnsupportedCapability { what:
   "redirect_policy" }` — while `None` and `Transparent` (and
   `Configurable`, before it was deleted) fail **one**, the read-back, and
   nothing else. `Internal` versus not-`Internal` is the only distinction
   any behaviour in this workspace can witness, because `Client`'s redirect
   stage follows a 3xx whatever the field says. **A variant nobody can
   catch lying has to earn its place from a carrier rather than from a doc
   comment**, which is the general form of the rule this deliverable
   applied. Recorded in `docs/v03-acceptance.md`'s unverified list as well,
   since `Transparent` has no behavioural witness and is not claimed to.
2. The type itself: a stored floor, and a constructor that refuses
   disagreement, naming the field.
3. **The fast tier first, because half of it already exists.** v0.3 W2
   already fetches the HTTPS record and reads its ALPN list, `h3` included,
   and already records that there is nowhere to act on it. So the first
   protocol choice this transport makes costs no new discovery at all —
   only the acting.
4. **Alt-Svc second**, for origins that publish no HTTPS record: the cache,
   its scope, and its negative half. This is the tier that needs storage,
   which is why it is not first.
5. **The race last, and it is a hedge rather than a chooser** (P12). It
   exists for the network that blocks UDP/443, which is also what the
   original spec's §5.6 "broken backoff" is about. Its cost is the one v0.3
   W2 left unmeasured — *"the size of the cost is unverified"* — so measure
   before choosing a policy, not after.

**Deliberately not in it.** `DefaultTransport` does **not** become this
type. Making a default that opens UDP sockets is a decision about what
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

**The redirect half of that prediction now has a condition on it**, found
while settling W1 deliverable 1 and worth carrying into this workstream
rather than rediscovering: `Internal` is forced only for a **background**
session, because the redirect delegate is not called for one. On a default
or ephemeral session the delegate *is* called and answering `nil` refuses
the hop and hands the 3xx back as the task's response — which is
`Transparent`, and which lets `Client`'s redirect stage keep doing the
per-hop work it already does (`SENSITIVE_HEADERS` across an origin,
cookies re-derived, the `AllowEarlyData` mark removed). So this backend may
well have to report *different* redirect support depending on how it was
configured, and that — not the value itself — is the interesting thing for
the `Capabilities` model: `Capabilities` is per-transport, and `Native` has
already shown the shape once, in `reuse_of(&pool)`, where the field is
asked of the thing that behaves rather than written down twice. The
citation and the reasoning are in `RedirectSupport`'s own doc comment.

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


---

## Appendix A — W2 deliverable 2, decided

The question was how a caller truthfully learns HTTP/2 is in force, given
that `capabilities()` must keep reporting the HTTP/1.1 floor. Three shapes
were investigated against the code:

- **Per response.** `Response::version()` already answers it. Honest, and
  useless for the capability that raised the question: a caller structured
  for bidirectional streaming has to decide **before** it sends, and this
  answers after.
- **Per connection.** Needs either a new seam or a pool query that is racy
  in the way that matters — the entry can be evicted between the answer and
  the request that relied on it.
- **Per request, as a demand.** The caller says *this request needs HTTP/2*,
  and a connection that negotiated something else fails it **before the head
  is written**, with a typed error.

**Take the third.** Its shape already exists here: `AllowEarlyData` is a
mark in the request's extensions that a transport reads and acts on before
sending, and `Client` knows to strip it across an origin. This is the same
mechanism with the polarity reversed — an *allow* becomes a *require* — and
the reversal is why it must be per request rather than per client. Making
an ALPN outcome a request failure is correct for gRPC, whose RPC simply
cannot proceed over HTTP/1.1, and wrong for a browser-shaped client that
should degrade quietly. Only the caller knows which of the two it is, which
is the same argument that put `AllowEarlyData` in the caller's hands.

**Built.** `http_ng_core::RequireVersion(http::Version)`, refused with
`VersionNotAvailable` under `ErrorKind::Unsupported`, compared by one
shared `check_version`. The acceptance is
`docs/v03-acceptance.md`'s "v0.4 W2 — a per-request version demand"; three
things it settled that this appendix left open:

- **The demand does not only refuse, it routes.** `Native::execute` reads
  it at three points: it filters pool candidates, it **narrows the ALPN
  offer**, and only then does it refuse. Without the middle one the h1
  direction of the demand is unsatisfiable against any h2-capable server —
  the client would propose `h2`, the server would take it, and the request
  would fail on a connection the client itself chose to make wrong.
- **Exact match, not a minimum.** There is no ordering under which "at
  least HTTP/2" is useful: a caller needing h2 framing does not want
  HTTP/3, and one needing HTTP/1.1 wants strictly less.
- **`version_select: true` means "honours a demand", not "chooses a
  version".** `http-ng-h3` speaks one protocol and reports `true`, because
  `false` would make `Client`'s gate refuse `RequireVersion(HTTP_3)` — the
  one demand it satisfies by construction. `http-ng-fetch` and
  `http-ng-wasi` keep `false`: neither selects the version *nor learns
  it*, so neither has a moment at which it could compare anything.

**And one boundary this appendix got wrong by analogy.** `AllowEarlyData`
is stripped across an origin, and the obvious reading is that its twin
should be too. It must not be. "Replaying this is safe" is a claim about
what a request does *at a server*; "my code needs HTTP/2" is a claim about
the caller's own code, equally true at hop 4. Stripping it would let a
`302` deliver over HTTP/1.1 precisely the request that said it could not
use HTTP/1.1.

**It also gives `Capabilities::version_select` a decision to turn on.**
That field is `false` in every backend and **read by nothing** — P5's shape
exactly, and the second instance found in this vertical. A demand against a
backend that cannot honour one becomes an `UnsupportedCapability`, the same
arm `RedirectPolicy`-against-`Internal` already takes. A field that was
about to be deleted for having no caller instead acquires its first.

**What it does not do**, and this must not be blurred: it does not widen
the static floor. `full_duplex` stays `false` for the same reason it always
has — Cargo unifies features across a graph, so a library cannot know
whether some other crate turned `http2` on. The demand is how a caller
converts "the floor says no" into "this connection says yes", per request,
before committing to a shape that would deadlock without it.

## Appendix B — `request_trailers`, one decision for two crates

Found while making h2 duplex: `http-ng-native` declares
`request_trailers: false` while its pump calls `send_trailers`, and
`http-ng-h3` declares the same `false` and **enforces** it with a typed
`RequestTrailersNotSent`. Two crates, one field, opposite behaviours — and
v0.2 W4's rule is that a declaration and its enforcement belong in the same
change.

`http-ng-h3` is the one that followed the rule. The gRPC case does not
argue otherwise: what gRPC needs is *response* trailers, which already
reach a caller on an h2 connection, and `grpc-status` is the server's to
send. So the fix is to make `http-ng-native` enforce what it declares
rather than to raise the declaration — unless the h1 path turns out to send
them too, in which case the field has a second carrier and the answer is
worth re-opening with that measurement in hand.

## Appendix B, re-opened — the h1 path sends them, and there is a third state

**Measured, and the conditional above fired.** On a raw socket, plaintext
`http://` so HTTP/1.1 with certainty: an ordinary `Native`, a streaming
body whose second frame is a trailers frame, and a `Trailer: grpc-status`
request header put `0\r\ngrpc-status: 0\r\n\r\n` on the wire
(`crates/http-ng-native/tests/request_trailers.rs`). So the field has two
carriers, not one, and the proposed fix would have **deleted a working
HTTP/1.1 feature** rather than closed a gap. Nothing was changed.

The condition is RFC 9110 §6.6.1's rather than ours: hyper encodes only
the fields a request declared in `Trailer:`
(`proto/h1/encode.rs`'s `Kind::Chunked(Some(..))`). Which leaves a third
state neither `true` nor `false` describes — with no `Trailer:` header the
same trailers are **dropped silently** and the request succeeds — so what
one decision has to cover is now three behaviours under one field:

| path | declared | actual |
|---|---|---|
| `http-ng-native` h1 | `false` | sent when declared in `Trailer:`; **silently dropped** when not |
| `http-ng-native` h2 | `false` | sent, unconditionally |
| `http-ng-h3` | `false` | refused, typed `RequestTrailersNotSent` |

Both h1 behaviours are pinned by tests, so whoever takes the decision
meets the silent drop rather than discovering it. The decision itself is
still owed.
