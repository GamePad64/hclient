# v0.2 design

**Theme: from one request to a session.**

v0.1 makes a request. It opens a connection, uses it once, and closes it —
on every backend, for every request. v0.2 is about making many requests
correctly: over a connection that is reused, with a protocol that
multiplexes, and with the ability to stop.

That is one theme rather than a feature list, and the order below follows
from it: cancellation must be a contract before connections are shared,
connections must be shared before HTTP/2 means anything, and HTTP/2 forces
the capability model to answer a question it currently cannot.

## What v0.1 left, and what has since landed

The acceptance document's "deliberately not done" list has already shrunk.
Landed since it was written: `http-ng-tower` (both directions), the hickory
resolver, `http-ng-tls-native-tls`, real SVCB/HTTPS through the system
resolver, `NoTls`/`IpLiteralOnly`, `Client::query`, and a `Client` that
clones.

Still open, and the subject of this document: connection pooling, HTTP/2,
streaming request bodies, `first_byte`/`between_bytes` on native, a
whole-operation deadline, and cross-backend cancellation. Explicitly still
out: HTTP/3, WebSocket, DoH, Alt-Svc, `http-ng-rmcp`.

---

## W1 — Cancellation becomes a contract

**Why first.** A pool that returns half-cancelled connections to service is
worse than no pool, and today nothing says what dropping a future does.
`docs/v01-acceptance.md` records the asymmetry: only `http-ng-fetch` cancels
the in-flight exchange on drop, and `Transport::execute` documents nothing
for anyone.

**Deliverable.** `Transport::execute`'s contract states what dropping the
returned future must do, each backend is made to honour it, and a test per
backend proves it. Where a backend genuinely cannot cancel — a WASI host may
not expose it — that is a capability, not silence.

**Watch for.** "The future stopped being polled" and "the transfer stopped"
are different claims. A test that only checks the first is the vacuous kind
this project keeps finding.

---

## W2 — Connection reuse

**Where it lives.** In `http-ng-native`. WASI and the browser delegate
pooling to their host and cannot be given one; putting a pool above the
`Transport` seam would mean building something two of three backends must
then be told to ignore.

**Key.** `(scheme, host, port, negotiated ALPN, TLS configuration identity)`.
ALPN is in the key because an h2 connection and an h1 connection to the same
origin are not interchangeable. TLS configuration is in the key because two
clients with different roots or client certificates must not share a socket
— a pool that ignores this is a security defect, not a performance one.

**Interacts with.** W1 (a cancelled exchange must not return its connection
to the pool), idle timeouts (a new `Timeouts` field or a pool-level setting,
not both), and `Capabilities`.

**Capability.** A caller has to be able to tell "connections are reused" from
"every request is a new socket", because it changes how they batch work. The
honest shape has three answers, matching `RedirectSupport`'s precedent:
reuse is ours and configurable, reuse is the host's and not ours to control
(WASI, browser), or there is none.

---

## W3 — HTTP/2, and the question it forces

**The blocker is not hyper.** hyper does h2 already, and the ALPN plumbing
exists — `http-ng-native/src/connect.rs` proposes `h2` in a test today. The
blocker is that **`Capabilities` cannot express a per-connection fact.**

`Transport::capabilities()` returns `&Capabilities` and its own doc says the
value is "determined once — at construction — and unchanged for this object
ever since". But h1-versus-h2 is negotiated per connection: `full_duplex`
and `streaming_request_body` are false on h1 and true on h2, against the
same transport, decided at handshake time.

Three ways out, and the choice must be made before any h2 code is written:

1. **Capabilities describe the best case; the exchange reports the actual.**
   `Response` gains the negotiated protocol and what it permitted. Honest,
   but it splits a caller's decision into two places and every existing
   consumer of `capabilities()` has to learn the difference.
2. **Capabilities describe the floor.** They stay false, and h2's extra
   ability is opt-in through a separate path. Never lies; wastes h2.
3. **Capabilities become per-request.** The largest change, and it
   contradicts the reasoning already written into the trait — the signature
   returns a reference precisely because recomputing per call does not
   compile without leaking.

**DECIDED: the floor, and it is not blanket conservatism — it is chosen per
field by what over-claiming costs.**

The three options above treat `Capabilities` as one question. It is not. Two
of its fields fail in completely different ways when wrong, and that
difference decides the answer:

- Over-claiming `streaming_request_body` costs a **buffered copy**. The
  caller hands over a streaming body, the transport cannot stream it, and it
  is buffered or rejected. Recoverable, and visible.
- Over-claiming `full_duplex` costs a **deadlock**. A caller structured for
  bidirectional streaming writes its request body while reading the
  response; on h1 the response does not arrive until the request completes,
  and the request does not complete until the caller reads. That is a hang,
  not a degradation — and this project already documents the shape of it
  (`AGENTS.md`: "a caller that never reads the response body never finishes
  writing the request body either"). A capability whose over-claim hangs the
  program cannot be optimistic.

So: **`capabilities()` reports the value that holds on the WORST protocol
the transport might negotiate**, with the h2 feature on or off. It never
lies, it cannot hang a caller, and — the reason it must be this and not
best-case — it is the only answer a *library* can act on, since feature
unification means a library never knows whether some other crate in the
build enabled h2.

Note how narrow this actually is. Comparing what `Native` sets today
(`lib.rs`: six fields) against what h2 would change:

| field | h1 | h2 | changes? |
|---|---|---|---|
| `streaming_request_body` | `true` — h1 streams via `transfer-encoding: chunked`, and a test pins it | `true` | **no** |
| `full_duplex` | `false` | `true` | yes |
| `request_trailers` / `response_trailers` | `false` today | `true` | yes |

One field of consequence, not a category. The "capabilities cannot express a
per-connection fact" framing overstated the problem: for the field that
matters, the per-connection answer arrives *after* the caller has already
had to commit to a structure, so a per-connection answer would not help even
if the trait could carry one.

**The negotiated protocol is already observable, and no new API is needed
for it.** `Response::version()` returns `http::Version` and `Native` already
sets `version_reported: true`. A caller that wants to know what it got, gets
it — after the fact, which is the only honest time.

**What h2's extras need instead: an explicit opt-in that refuses.** A caller
who genuinely needs duplex asks for it, and gets a typed error if h1 was
negotiated. That converts the dangerous case from a silent hang into a
refusal — the same move `check_supported` already makes for a
`RedirectPolicy` against a backend that follows redirects internally. Design
the opt-in in W3; do not widen `capabilities()` to carry it.

**Both h2 and h3 sit behind a cargo feature on `http-ng-native`** — owner's
decision. There is no `[features]` section in that crate today and `hyper`
is already pulled with `default-features = false, features = ["client",
"http1"]`, so the shape is clean:

```toml
[features]
default = []
http2 = ["hyper/http2"]
```

What it buys: h2 pulls hyper's own h2 implementation, and h3 pulls a
different stack entirely (`h3` plus `quinn`, and UDP with it). Keeping both
out of a default build is the same concern `NoTls` and `IpLiteralOnly` exist
for.

**What it does to the question above, which is more than it first appears.**
With the feature OFF, every connection is h1 and `capabilities()` answers
with a fixed value — the "determined once, at construction" contract holds
exactly as it does today, and nothing needs to change. With it ON, the
per-connection problem is entirely present. So the question is not universal;
it is a question about one configuration, which is a much smaller thing to
get right.

**The catch, and it is not obvious.** Cargo features are additive across a
dependency graph. If any crate in a build enables `http-ng-native/http2`,
every crate gets it. So "the feature is off, therefore capabilities are
fixed" is a conclusion available to a **final binary** and not to a library
built on `http-ng` — for a library, the state is always "h2 may be on".
Whichever resolution is chosen for the ON case must therefore be the one a
library-facing API can live with; the OFF case cannot be treated as the
common path.

**h3's feature name is the easy part.** It is not a hyper feature but a
separate stack, and it needs UDP, which the runtime seam does not have —
`http_ng_rt` offers `TcpConnect` and nothing else. Behind that flag is a new
runtime capability, which is why h3 stays out of v0.2 (see the closing
section) even though the flag can be reserved now.

**A second constraint, already known.** `http-ng-tls-native-tls` cannot
report the negotiated ALPN — `async-native-tls` does not expose it. So h2 is
unavailable over the platform TLS backend, and that must be declared before
the work starts rather than discovered by a user whose h2 silently never
happens.

---

## W4 — Bounds on the whole operation

- **`Timeouts.total`.** The documented gap: a response that starts promptly
  and then dribbles under the `between_bytes` threshold runs unbounded.
  Implemented in `Client`, not as a tower layer — only the client knows
  where the operation begins and ends, and `tower-http`'s timeout is
  hardcoded to `tokio::time` and synthesises a 408 response instead of an
  error, which would bypass the `ErrorKind` taxonomy entirely.
- **`first_byte` and `between_bytes` on native.** Declared `false` today,
  honestly. Making them true means enforcing them, and the declaration and
  the enforcement move in the same commit.
- **A concurrency limit.** Needed the moment a pool exists, and useful
  before it: without one, in-flight requests are unbounded and so are
  sockets. `tower::limit::concurrency` fits, and reserves its permit in
  `poll_ready` — the contract `http-ng-tower`'s tests already pin.

---

## W5 — Compression

Inside `Client`, not as a tower layer. A layer wrapping the transport
changes the client's type, so `struct App { http: Client }` stops
compiling — the ergonomics just fixed by making `Client` cloneable would be
lost to the first middleware. Doing it in the client changes the response
body type only, which is already generic over the transport.

Gated on a capability: the browser decompresses already and forbids
`Accept-Encoding`, so decompressing again would corrupt every response.

---

## W6 — Streaming request bodies

`RequestBody::Streaming` passes through no transport today: native buffers
it, fetch rejects it, WASI takes only `Full`. h2 makes it natural, h1 needs
chunked, and fetch needs the `ReadableStream` half that `convert.rs`
currently refuses. Each is separately honest today, which is why this can
wait until W3 lands.

---

## Decisions needed before work starts

1. ~~**How `Capabilities` expresses a per-connection fact**~~ **Decided:
   it does not — `capabilities()` reports the floor** (W3). The framing
   overstated the problem: only `full_duplex` and trailers differ between h1
   and h2, `streaming_request_body` is already `true` and honest on both,
   and for `full_duplex` a per-connection answer would arrive after the
   caller had to commit anyway. h2's extras get an explicit opt-in that
   errors when h1 was negotiated, rather than a capability that can hang a
   caller by being optimistic.
2. **Whether the pool is configurable through `Client` or only through
   `Native`.** The first is friendlier; the second keeps the facade free of
   a concept two backends do not have.
3. ~~**Whether h2 is a feature or a default.**~~ **Decided: a feature**, on
   `http-ng-native`, and the same for h3 (W3). It keeps hyper's h2 and — for
   h3 — a whole UDP stack out of a default build. It also narrows decision 1
   to the feature-ON configuration, though not as far as it looks: cargo
   features unify across a graph, so a library on top of `http-ng` must
   assume h2 may be enabled by someone else in the build.

## IDN, and why the Unicode tables are ours rather than the system's

Support for internationalised domain names goes behind an `idn` feature on
`http-ng-proto`, **in `default`**, with the conversion at the single
boundary where a string becomes a `Uri`. In the sans-io crate rather than
the facade, so "the same on every backend" is a structural fact and not a
consequence of everyone happening to go through `Client`.

**What it replaces is an inconsistency, not a feature.** Measured before
deciding: `client.get("https://münchen.de/x")` errors with `invalid uri
character` on a client with no `base_url`, and succeeds — punycoded — on one
that has any `base_url` at all. `effective_uri` routes the first through
`Uri::parse`, which rejects non-ASCII, and the second through
`resolve_reference`, which goes via `url::Url`, which punycodes. The
`Location` header of a redirect takes the second path too. So the same URL
works or fails depending on an unrelated setting.

**Why a feature and not the system's ICU**, since the machine has
`libicuuc.so.78` sitting there:

- We do not link ICU at all. `icu_properties_data` is pure Rust — no
  `extern "C"`, no `links` key — with 1.8 MB of generated tables compiled
  in. It is ICU4X, a Rust reimplementation, not a binding.
- The system's is ICU4C: different implementation, different data format,
  C++ ABI, and symbols suffixed with the major version (`libicuuc.so.78`
  means `u_strlen_78`). Building against one and running against another
  fails at load, and distributions carry different majors.
- It does not exist on two of our three targets. There is no system ICU on
  `wasm32-unknown-unknown` or `wasm32-wasip2`, nor in a static musl binary
  or a scratch container. A system dependency for IDNA would serve one third
  of what this client is for.
- ICU4X's runtime `DataProvider` does not close the gap either: the blob is
  ICU4X's own format, so it has to be shipped alongside. That moves the
  megabytes out of the binary, it does not remove them.

Turning the feature off removes the tables entirely, on every target, with
no fallback to hunt for. That is the honest shape: this build does not deal
in internationalised domains. With it off, a non-ASCII host must produce a
typed error naming the reason — not `http`'s `invalid uri character`, from
which nobody can tell what to do.

---

## An idea recorded with its blocker: platform IDN APIs

The proposal: use the platform's own IDN conversion instead of carrying
`idna` and the ICU tables — `IdnToAscii`/`IdnToUnicode` on Windows,
something equivalent elsewhere — as an optimised variant of the `idn`
feature.

**The blocker is not size or availability. It is that the implementations
disagree about which host you connect to.**

The Rust `idna` crate implements UTS-46. Windows' `IdnToAscii` implements
IDNA2003 (RFC 3490). They differ on real inputs, and the difference is not
cosmetic — measured here against `url`, which is the UTS-46 side:

| input | UTS-46 (ours today) | IDNA2003 (Windows) |
|---|---|---|
| `straße.de` | `xn--strae-oqa.de` | `strasse.de` |
| `faß.de` | `xn--fa-hia.de` | `fass.de` |

Those are different domains. They can be registered to different people.
Under IDNA2003 the sharp s maps to `ss`, so the name resolves to plain ASCII
and no punycode is produced at all; under UTS-46 it is a character in its own
right. The same divergence exists for final sigma and for ZWJ/ZWNJ.

So a platform-backed variant is not a transparent optimisation. It changes,
per operating system, **which origin a request goes to** — in a client whose
stated mission is that the same code runs everywhere, and in the one step
that decides who you are talking to. That makes it a security-relevant
divergence rather than a size trade.

The availability picture is also thinner than it looks:

- **Windows** — `IdnToAscii` exists since Vista, and is the case above.
- **macOS** — no public IDN API. IDN handling lives inside CFNetwork, and
  `libicucore.dylib` is private with unversioned symbols that Apple
  documents as not for linking.
- **Linux** — `libidn2` is usually present and glibc's `AI_IDN` can use it,
  but neither is guaranteed, and libidn2 is IDNA2008, a third answer.
- **wasm** — nothing, though the browser's own URL parser is UTS-46 and
  therefore already agrees with us.

**If it is built anyway, it must be an explicit opt-in that says what it
costs** — not a default, and not presented as an optimisation of the same
behaviour. A build that takes it is choosing smaller binaries over
cross-platform identity of the resolved host, and only the person making
that build can weigh it. The feature name should say so; `idn-platform`
rather than `idn-fast`.

---

## Not in v0.2, and why

**HTTP/3** needs QUIC, which needs UDP, which is a runtime capability the
`http-ng-rt` seam does not have — a new trait, implemented per runtime,
before any protocol work starts. That is a vertical of its own.

**WebSocket** needs h1 upgrade, and the `UpgradeSupport` capability exists
but is `None` everywhere. Worth doing after W2, not during.

**DoH** needs an HTTP client to resolve names for an HTTP client. The
bootstrap is the design problem, not the protocol.

**Alt-Svc** wants somewhere to persist what it learns, which is a cache with
a lifetime — closer to a browser's job than a client's, and worth deferring
until a real consumer asks.
