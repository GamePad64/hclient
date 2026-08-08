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

**DONE, and the WASI guess above was wrong.** `wasi:http` cancels: dropping
the future cancels the Component Model subtask, measured on a live wasmtime
host by watching the mock server's own socket
(`crates/http-ng-wasi/tests/live_roundtrip.rs`,
`dropping_the_execute_future_closes_the_connection_the_server_sees`, with
`holding_the_execute_future_leaves_the_connection_open` as its control). All
three backends cancel, `execute`'s contract now requires it, and
`Capabilities::cancel_on_drop` (`CancelSupport::{None, Supported}`) is the
one honest way for a fourth to say it cannot. Native's pair is
`crates/http-ng-native/tests/cancel.rs`, fetch's is in
`crates/http-ng-fetch/tests/transport.rs` — each with a server, a socket or
the browser as the observer, never the client's own task.

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
"every request is a new socket", because it changes how they batch work.

**AMENDED WHILE BUILDING IT: two answers, not the three guessed here.**
`http_ng_core::ReuseSupport` has `None` and `Supported`; its doc comment
carries the argument, and the condition under which the third variant
arrives. The short version: `RedirectSupport::Internal` earns its variant
because `check_supported` *refuses* on it — there is a portable client-level
setting (`ClientBuilder::redirect`) that an internal-redirect backend would
silently ignore. Reuse has no such setting (the pool is configured on
`Native`, since an idle timeout is not a property of a request), so nothing
refuses, and "who owns the pool" turns no caller decision — the same axis
`CancelSupport` rejected one work item earlier.

**A finding from W2 that belongs to W7, recorded here because W7 needs it:**
a pool driven by a spawned task is not merely undesirable on this seam, it
does not compile. `http_ng_rt::Spawn<F>` requires `F: Send + 'static`, and
the native IO is deliberately not `Send` (`connect.rs`'s `FakeStream` holds
an `Rc<()>` to prove no path requires it). So `Spawn` is not the thing that
would have made the pool easy, and its absence on a future embassy runtime
is a smaller obstacle than it looks — what the pool actually needed was a
poll at checkout, which needs no executor at all.

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

> **CORRECTION, written while implementing W3: `hyper/http2` is not
> available to this crate at any price, and the reason is that the trait
> is `sealed`.**
>
> `hyper::client::conn::http2::handshake` takes its executor as a *type
> parameter*, `E: Http2ClientConnExec<B, T>` (hyper 1.11.0,
> `src/client/conn/http2.rs:75-84`) — not as an option that can be left
> unset. And `Http2ClientConnExec` is declared `pub trait
> Http2ClientConnExec<B, T>: … + sealed_client::Sealed<(B, T)>`
> (`src/rt/bounds.rs:51-52`): **sealed**, with one blanket impl over
> `hyper::rt::Executor`. So an executor of our own — one that queues
> futures and lets the request future poll them, which is what a crate
> with no `Spawn` would need — cannot be written. Not "written at a cost":
> the impl does not exist to be written.
>
> What that executor receives is not an optional extra either: the
> handshake hands it the h2 connection itself,
> `exec.execute_h2_future(H2ClientFuture::Task { .. })`
> (`src/proto/h2/client.rs:192`). The `Connection` value handshake returns
> to the caller is a *different* future, the request dispatcher. Polling
> only that one moves no bytes.
>
> Even setting the sealing aside, the shape it forces costs something this
> vertical will not pay. `H2ClientFuture<B, T, E>` lives in hyper's
> private `proto` module and cannot be named, so any queue of ours is
> `Box<dyn Future>` — which is either not `Send` (taking `NativeBody:
> Send` with it, the property v0.2 W2 restored) or `dyn Future + Send`
> (adding a `Send` bound on this crate's IO, which is "single-threaded
> runtimes shut out", the thing the crate exists to avoid). `handshake`
> also requires `B::Data: Send`, a third bound of the same kind. Under
> Cargo's feature unification all of these are paid by builds that never
> asked for h2.
>
> **What shipped instead: the `h2` crate directly**, the same one hyper
> uses. `h2::client::Connection<T, B>` is a concrete future this crate
> polls by hand, exactly as it already polls hyper's HTTP/1 `Connection` —
> no executor, no `dyn`, no new bound anywhere in the public shape. The
> feature reads `http2 = ["dep:h2", "dep:tokio"]`, `tokio` being present
> for its `AsyncRead`/`AsyncWrite` traits alone (no features, so no
> runtime and no reactor); `http2::TokioIo` bridges them to
> `hyper::rt::{Read, Write}`, `unsafe`-free. See
> `crates/http-ng-native/src/http2.rs`'s module doc.

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

  **Correction, measured while doing it: the layer bounds requests, not
  sockets.** `tower`'s permit is dropped when its response future
  completes, i.e. at the response HEAD; the body streams on afterwards
  holding its connection, so with a limit of N there can be more than N
  connections open. Pinned by
  `crates/http-ng-tower/tests/concurrency.rs`'s
  `the_permit_is_released_at_the_response_head_so_bodies_are_not_bounded`.
  Bounding sockets needs a limiter that carries its permit into the
  response body — the shape `http-ng-wasi`'s `Body` and `http_ng::Deadline`
  both use — which `http-ng-tower` would have to own rather than borrow
  from `tower`. Not written; it matters most to W2, which is where a
  connection count becomes a real resource rather than an incidental one.

**Status: the first and third bullets are done; the middle one is not.**
`Timeouts.total` shipped as a bound in `Client` — `ClientBuilder::
total_timeout(clock, d)` for any transport, `Client::total_timeout(d)` for
a client already carrying the target's default clock — expiring as
`ErrorKind::Timeout(Phase::Total)` and dropping the exchange rather than
only reporting on it. It is deliberately NOT a fourth field of `Timeouts`:
that struct is what transports read out of `http::Extensions` and enforce,
and no transport can enforce a bound on an operation whose redirect loop it
does not own — a `TimeoutSupport::total` would be a capability describing
the client. What could be missing instead is a clock, and that is settled
in the type system (`http_ng::NoClock`), not by a runtime refusal. See
`crates/http-ng/src/deadline.rs`, and `tests/deadline.rs` for the server
that dribbles for ever.

Two limits of it, both stated in the code rather than only here: a body
that goes **completely silent** after the head is not cut (nothing polls
the wrapper again, and the deadline holds no sleep of its own — see
`Deadline`'s doc comment for why it cannot without making every response
body `!Send`), which is `between_bytes`'s job, i.e. the middle bullet; and
there is no per-request override yet, only per-client — a `Client` handle
with a different bound costs one `Arc` bump, which covers most of the same
ground.

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

**Two thirds of this section were already stale when W3 landed, and the
correction shrinks the task rather than growing it.** It said native buffers
a streaming body and WASI takes only `Full`. Neither is true: `Native`
declares `streaming_request_body = true` and pins it with
`streaming_request_body_is_actually_streamed_not_buffered`, and
`http-ng-wasi` declares it `true` because `RequestBody::Streaming` goes
straight through to the host. h2 changes nothing here — h1 already streams
via `transfer-encoding: chunked`.

What is left is **fetch alone**: `streaming_request_body` is hardcoded
`false` in `crates/http-ng-fetch/src/caps.rs`, and `convert.rs` refuses the
`ReadableStream` half. So W6 is a one-backend task in a crate nothing else
touches — and the capability there must stop being a constant and start
being derived, the way `ReuseSupport` and `DecompressionSupport` already
are.

---

## W7 — Embassy as a third runtime

**Why it belongs here.** The runtime seam (`http-ng-rt`) exists precisely so
that a third answer is possible, and it has been proved exactly twice — by
`http-ng-rt-tokio` and `http-ng-rt-smol`, both of which run on a desktop
OS. Two implementations that share a libc are weak evidence for a seam whose
whole claim is portability. Embassy is the case that tests it: a different
executor model, on hardware, but — and this is what makes it tractable —
**with `std`**.

**The target is esp32 with esp-idf, and that is a `std` target.** The owner
has run this stack. `esp-idf-svc` gives `std`, real sockets, threads and a
`getaddrinfo`; embassy supplies the executor, the timer and the async I/O
around it. So W7 is not a `no_std` project — `no_std` is a separate,
larger question about `http-ng-core` and `http-ng-proto` that this item
deliberately does not open. Everything below assumes `std` is present and
nothing in the workspace needs `#![no_std]` to make it work.

**Deliverable.** `http-ng-rt-embassy` implementing the same four traits the
other two do, and the `two_runtimes` acceptance grown to three:

- `Timer` — `embassy_time::Timer::after`. The one piece embassy gives
  directly.
- `TcpConnect` — `embassy_net::tcp::TcpSocket`, adapted to `hyper::rt::Read`
  and `Write`. Note `TcpOpts`: embassy-net is not a socket API with
  `setsockopt`, so several options have no counterpart. That is a
  capability, not a silent no-op — the same rule W1 applies to cancellation.
- `TcpAdoptStd` — likely **not implemented**. It exists for "platforms with
  file descriptors", and an embassy-net socket has none. A backend that
  cannot adopt a `std::net::TcpStream` should say so by not implementing the
  trait, which is already how the seam expresses it.
- `Spawn` — embassy's executor takes `'static` non-allocating tasks bound at
  compile time (`#[embassy_executor::task]`), which does not accept an
  arbitrary future the way `tokio::spawn` does. This is the interesting one,
  and the reason this item is worth doing: `Spawn` is the seam most likely
  to be shaped around desktop assumptions, and the smol path already proves
  the client works with no spawn at all.
- `Blocking` — there is no blocking pool. Same treatment as `Spawn`.

**Watch for.** Two traps, both of which this project has already met in
another guise.

1. **A capability that describes the environment rather than the
   transport.** "Embassy has no blocking pool" is a fact about the executor;
   what `Capabilities` must say is what a caller can rely on from *this*
   transport. Caught four times in v0.1 — do not add a fifth.
2. **A test that compiles.** The two-runtime acceptance is worth what it is
   because it *runs over a network* on both. A third runtime that only
   builds for `xtensa-esp32-espidf` proves the trait bounds line up and
   nothing else. Either the acceptance runs on hardware or under an emulator
   (QEMU has an esp32 target), or the deliverable says plainly that it is a
   compile-only claim — the pattern `portable-example-three-targets` already
   uses, and which is honest exactly because it says so.

**Ordering.** Independent of W1..W6 — it touches no protocol code. Best
after W1, so the cancellation contract is something the new backend is
written against rather than retrofitted into.

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

**Not by switching backends.** `idna_adapter` is a supported seam and
pinning it to 1.1.0 moves `idna` onto the unicode-rs backend, taking
`http-ng-proto`'s graph from 35 crates to 20 for +126 KiB of binary
(measured; see `docs/icu-ecosystem-survey.md`). **Rejected, 2026-08-08:**
that backend is the stale one — it is what `idna` used before ICU4X — so
the fifteen crates are bought with Unicode tables that lag. IDN decides
*which host we connect to*, and a mapping table a Unicode version behind is
a difference in destination rather than in polish. It is the same failure
mode as `IdnToAscii`, further down: an older standard that answers almost
the same question.

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

## Platform IDN: the right Windows API is ICU, not `IdnToAscii`

The proposal: use the platform's own IDN conversion instead of carrying
`idna` and the ICU tables, as an alternative backing for the `idn` feature.

**An earlier version of this section said the idea could not work because
implementations disagree. That was right about the wrong API.**

`IdnToAscii`/`IdnToUnicode` implement IDNA2003 (RFC 3490), and they do
diverge from what this project produces. Measured here against `url`, which
is UTS-46:

| input | UTS-46 (ours) | IDNA2003 (`IdnToAscii`) |
|---|---|---|
| `straße.de` | `xn--strae-oqa.de` | `strasse.de` |
| `faß.de` | `xn--fa-hia.de` | `fass.de` |

Different domains, registrable by different people. Under IDNA2003 the sharp
s maps to `ss` and no punycode is produced at all. Final sigma and ZWJ/ZWNJ
diverge the same way.

**Windows ships ICU, and its ICU can produce exactly what we produce — but
only with the right flags.** Measured on a `windows-latest` runner, not
inferred, by calling both APIs on the same inputs:

| input | `IdnToAscii` | `uidna_openUTS46(0)` | `uidna_openUTS46(NONTRANSITIONAL)` | Rust `idna` (ours) |
|---|---|---|---|---|
| `straße.de` | `strasse.de` | `strasse.de` | `xn--strae-oqa.de` | `xn--strae-oqa.de` |
| `faß.de` | `fass.de` | `fass.de` | `xn--fa-hia.de` | `xn--fa-hia.de` |
| `münchen.de` | `xn--mnchen-3ya.de` | same | same | same |

Three things fall out of that table, and the middle one is the reason the
measurement was worth making.

`IdnToAscii` is IDNA2003, confirmed — Microsoft documents it as RFC 3490,
and it behaves that way.

**ICU's UTS-46 with default options is transitional, and therefore agrees
with IDNA2003 rather than with us.** `UIDNA_DEFAULT` is 0; the behaviour
this project needs requires `UIDNA_NONTRANSITIONAL_TO_ASCII |
UIDNA_NONTRANSITIONAL_TO_UNICODE` (0x10 | 0x20). An implementation that
reached for ICU because "it is UTS-46, so it matches" and passed 0 would
produce a different origin from every other target, silently, for exactly
the inputs where it matters.

With the non-transitional flags it agrees with the bundled Rust
implementation on every input tried. The UTF-8 entry point
(`uidna_nameToASCII_UTF8`) also avoids a UTF-16 round trip.

What the Windows path actually costs:

- **A version floor.** `icuuc.dll` + `icuin.dll` from 1703; the combined
  `icu.dll` only from 1903. Resolving the symbol at runtime rather than
  linking gives a clean answer on older systems: fall back to the bundled
  Rust implementation instead of failing to start.
- **`CoInitializeEx` first**, for Win32 apps — except on 1903+ with the
  combined library, where it may be omitted.
- **C only.** No C++ APIs are exposed and never will be, which suits an FFI
  boundary.
- Microsoft notes that not all ICU-returned data aligns with the rest of
  Windows yet. Irrelevant for IDNA, worth knowing before reaching for other
  ICU functions from the same handle.

The other platforms are still thin: **macOS** has no public IDN API
(CFNetwork does it internally; `libicucore.dylib` is private with symbols
Apple documents as not for linking), **Linux**'s libidn2 is neither
guaranteed nor the same standard (IDNA2008, a third answer), and **wasm**
has none — though browsers are UTS-46 and therefore already agree.

So the shape is: a per-platform backing for `idn` where the platform offers
the *same standard*, with the bundled Rust implementation as the fallback
everywhere else and on older Windows. That is an implementation detail
rather than a behaviour change, which is what makes it worth doing —
and it is the opposite of what a naive `IdnToAscii` binding would be.

**Whatever is built must have a differential test against the bundled
implementation on a shared corpus.** The whole claim is that the two agree;
an untested claim of agreement between two IDNA implementations is exactly
the kind of thing that is false in the tail.

---

## 0-RTT must stay reachable

Not v0.2 work, but a constraint on every seam v0.2 touches, recorded here
because it is cheap now and a breaking change later.

`TlsRequest::ech` already establishes the rule in this codebase: the slot
was reserved before any implementation needed it, because *adding a field to
a request struct later breaks every `TlsConnect` implementation already
written*. Early data gets the same treatment — an "offer early data" slot on
the request and an "early data was accepted" answer on `TlsInfo`, reserved
without a backend behind them.

Three things a future implementer should not have to re-derive:

- **0-RTT is replayable, so which requests may use it is a policy, not a
  crypto detail.** `RequestBody::retry_kind()` and W2's reasoning about
  `is_canceled()` are where that policy already half exists.
- **The floor rule applies with unusual force.** Over-claiming
  `streaming_request_body` costs a buffered copy and over-claiming
  `full_duplex` costs a deadlock; over-claiming 0-RTT costs replay exposure.
- **Half the machinery assembled itself.** rustls keeps session tickets in
  `ClientConfig`'s `ClientSessionStore`, `Rustls::from_config` already holds
  exactly that, and W2's `TlsConfigId` identifies that value and is already
  in the pool key. A session cache does not need designing from scratch —
  it needs noticing.

`native-tls` will not be able to do this, in the same shape it cannot report
the negotiated ALPN.

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
