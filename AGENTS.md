# http-ng

Cross-platform async HTTP client. The same application code
builds for native, browser and WASI — the transport is swapped out, not
buried under `#[cfg]`.

```rust
let client = http_ng::Client::builder(transport).build()?;
let text = client.get("https://example.com").send().await?.collect().await?.text()?;
```

On native, with the `default-transport` feature (Task 14, vertical 2) — the same
code without manually choosing a transport: `Client::new()` resolves
`DefaultTransport` (`Native` on `tokio` + `rustls` with the system trust store +
system `getaddrinfo`) itself, by target, not by a feature the user picks.

```rust
let client = http_ng::Client::new()?; // requires an ambient tokio runtime
let text = client.get("https://example.com").send().await?.collect().await?.text()?;
```

The same two lines in a browser, on `wasm32-unknown-unknown`. `Client::new()`
is infallible there, so there is no `?` on it — that is the only difference:

```rust
let client = http_ng::Client::new();
let text = client.get("https://example.com").send().await?.collect().await?.text()?;
```

End-to-end proof that this SAME generic code (not two
separate examples) actually runs over the network on two different runtimes
without a single `#[cfg]` —
[`crates/http-ng/tests/two_runtimes.rs`](crates/http-ng/tests/two_runtimes.rs):
`cargo nextest run -p http-ng --test two_runtimes` instantiates the same
`fetch_once<R>` under `http_ng_rt_tokio::Tokio` (a real `tokio::runtime::
Runtime`) and under `http_ng_rt_smol::Smol` (a bare `futures_executor::block_on`,
no spawn and no `tokio` in the smol path's graph — see the next section).

A working end-to-end example that actually builds and runs under
`wasmtime` (not just compiles) —
[`crates/http-ng-wasi/examples/fetch.rs`](crates/http-ng-wasi/examples/fetch.rs):

```
cargo build -p http-ng-wasi --example fetch --target wasm32-wasip2
wasmtime run -S http -- target/wasm32-wasip2/debug/examples/fetch.wasm
```

The acceptance for the whole `Transport` shape — a live consumer, written
against another library *before* this one existed, ported line for line and
building for all three targets from one source with no `#[cfg]` at all —
[`crates/http-ng/examples/portable.rs`](crates/http-ng/examples/portable.rs):

```
cargo build -p http-ng --example portable
cargo build -p http-ng --example portable --target wasm32-wasip2
cargo build -p http-ng --example portable --target wasm32-unknown-unknown
```

The original is `act`'s `http-client` component on `wasi-fetch`. What the
port keeps, what it fixes and the four things it changes are written down in
[`docs/porting-wasi-fetch.md`](docs/porting-wasi-fetch.md); the behaviours
the example claims to have ported are pinned by
[`crates/http-ng/tests/portable_example.rs`](crates/http-ng/tests/portable_example.rs),
because three green builds on their own would also be green for an example
that never streams and never sets a timeout.

## Running the tests

`cargo nextest run --workspace --all-features` — nextest, not `cargo test`,
and CI runs the same. Two reasons, both of which have cost this project
real time: `cargo test` abandons the remaining test binaries after the
first one fails, so a red run hides every failure but the earliest
alphabetically, and its per-binary `test result:` lines have to be summed
by hand where nextest prints one `Summary`. Nextest also runs each test in
its own process, which matters here because mutation testing is this
project's primary review technique.

Two things nextest does not cover. Doctests: it cannot run them
(`cargo test --doc` does) — the workspace currently has none, so nothing is
lost, and if that changes CI needs a `--doc` step. Browser tests: those go
through `wasm-pack test --headless --chrome|--firefox` regardless, see the
`browser` job.

## What's in the dependency graph

The first row of the table, as before, is verifiable directly in this
repository: `cargo tree -p http-ng-wasi -e normal --prefix none` contains no
`tokio` at all (28 unique crates total). The second and third rows, unlike
their counterparts in the vertical 1 report, are now measured too, not
predicted: vertical 2 (`http-ng-native`, `http-ng-rt-tokio`, `http-ng-rt-smol`,
`http-ng-tls-rustls`, `http-ng-dns-system`) is built, and as of Task 14
`http-ng` has a `DefaultTransport` (native, HTTP/1.1 only) — the
`default-transport` feature, pulling in exactly these four crates. The HTTP/2
row remains the same prior-research row it always was: `http-ng-h2` does not
exist in this repository (not merely "not built yet" — not in the v0.1 plan at
all), kept untouched for the same rationale behind the HTTP/1-first choice it
was written under in vertical 1.

| build | tokio |
|---|---|
| ambient (`http-ng` + `-wasi` / `-fetch`) — measured | **none at all** |
| `http-ng` with the `default-transport` feature (native, HTTP/1.1 only) — measured, Task 14 | real: `[default, libc, mio, net, rt, socket2, sync, time]` — the `http-ng-rt-tokio` reactor is needed for real `TcpConnect`/`Timer`, this is not "just a type dragged along", see below |
| `http-ng-rt-smol` in isolation (without `http-ng`, `async-io` gives the same capability) — measured, Task 14 | `[default, sync]` — a leaf with no reactor, only `tokio::sync::oneshot`, see below |
| `http-ng-native` with the `http2` feature (v0.2 W3) — **measured**, and the prediction below was right | `[bytes, default, io-util, sync]`, plus `tokio-util` with `[codec, default, io, libc]`. Still **no reactor**: no `rt`, `net`, `time` or `mio` come from this feature — `h2` uses tokio's IO traits and codec, not its runtime |
| native + HTTP/2 — the row above as it stood before W3: a hypothetical estimate from vertical 1, kept for the record | `h2` pulls in `tokio` with `io-util` and `tokio-util` with `codec`, and through it `libc` |

**Both middle rows are the same `hyper` fact, measured in two different places
in the graph, not two independent observations.** `hyper` depends on `tokio`
**unconditionally, not behind a feature** — `http-ng-rt`'s own `hyper = {
version = "1.11", default-features = false }` (zero feature set) still pulls
in `tokio` with the `sync` feature, verified with `cargo tree -p http-ng-rt -e
normal -i tokio` in this tree. This is the same conclusion vertical 1 drew
about the HTTP/1 path from hyper's source (`tokio::sync::oneshot::Receiver` in
`src/upgrade.rs`, the only place it's used) — now confirmed by measurement,
not just by reading the code. The `http-ng-rt-smol` crate depends on
`http-ng-rt`, and therefore on `hyper`, and therefore transitively on this
same `tokio` leaf — **regardless of the fact that `http-ng-rt-smol` itself
pulls in neither `tokio` nor `async-compat` directly** (`cargo tree -p
http-ng-rt-smol -e normal` contains neither crate among its DIRECT
dependencies — checked by the `two-runtimes` CI job). The difference between
the table rows isn't "the smol path has no tokio, native does" — it's which
REACTOR actually stands behind that leaf: for `http-ng-rt-smol` in isolation,
none (the `sync` leaf is inert, `tokio::sync::oneshot` is never driven), for
`http-ng` with `default-transport`, a real one (`http-ng-rt-tokio`, Task 3,
pulls in `mio` + `net` + `rt` + `time` for real sockets and timers) — and both
facts hold at once: the smol runtime still doesn't execute a single line of
tokio, the `tokio` crate simply sits on disk as the same leaf it would for any
other build that uses `hyper`.

Tokio can't be removed from hyper builds: [hyper#3428](https://github.com/hyperium/hyper/pull/3428)
(exactly this swap for `futures-channel`, hidden behind a feature flag) was
rejected by the maintainer not for technical reasons, but because of the
irreversibility of the decision: *"As of 1.0, we are going to be very careful
about adding new dependencies to the public API… it "exposes" a crate feature
that we could never remove"*. [hyper#3767](https://github.com/hyperium/hyper/issues/3767)
— a separate ticket with the same conclusion about the only call site — was
closed as *not planned*.

**A second fact, also measured, not assumed: the test-only busy-spin never
reaches production code.** `http_ng_native::testing::blocking_io` (Task 12) —
a `hyper::rt::Read`/`Write` wrapper over `std::net::TcpStream` for testing on
a bare `futures` executor with no reactor at all; on `WouldBlock` it calls
`cx.waker().wake_by_ref()` immediately instead of actually waiting for
readiness through the OS. Measured by CPU time (`/proc/self/stat`) around a
request to a server that responds after 600ms: under `blocking_io` — wall
600.4ms, **cpu 600ms** (an honest busy-spin for 100% of the wait time); the
same exchange code (`h1::exchange`/`NativeBody`), but with IO from
`http_ng_rt_tokio::Tokio::connect` (a real `tokio::net::TcpStream`, registered
with the reactor) — wall 601.1ms, **cpu 0ms** (Task 12 review, section B).
`two_runtimes.rs` (Task 14) confirms the same section's prediction of "won't
happen under tokio or smol" in practice: both tests run `Native` over real
`http_ng_rt_tokio::Tokio`/`http_ng_rt_smol::Smol`, never touching
`testing::blocking_io` — it exists only under `#[doc(hidden)] pub mod
testing` and is used only in this same crate's `tests/h1.rs`.

## Status

v0.1: the core (`http-ng-core`, `http-ng-proto`, `http-ng`) and three backends
— `http-ng-wasi` on top of `wasi:http` 0.3 (vertical 1), `http-ng-native` on
top of `hyper` + `rustls` + system DNS (vertical 2), and `http-ng-fetch` on the
browser's `fetch` (vertical 3).

| target | transport | tokio in the graph |
|---|---|---|
| native | `http-ng-native` — TCP + HTTP/1, HTTP/2 behind the `http2` feature, TLS pluggable | yes, on the h1 path |
| WASI | `http-ng-wasi` — `wasi:http` 0.3 | **no** |
| browser | `http-ng-fetch` — `fetch` | **no** |

Both "no"s are machine-checked on every push rather than asserted here:
`ambient-has-no-tokio` runs `cargo tree` for `http-ng-fetch` against
`wasm32-unknown-unknown` and for `http-ng-wasi` against `wasm32-wasip2`, and
fails closed if the invocation itself breaks or returns nothing. Measured
while writing: zero matches for `tokio`, `hyper` or `h2` in either wasm
graph, four in the native one.

**Minimum supported Rust: the latest stable release** — currently **1.97**,
declared once in the workspace manifest and shared by every crate. That is the
support policy, not a snapshot: the floor moves with stable, and a release that
needs a newer compiler than the one you have is expected rather than a bug.

The trade is deliberate. There is no window in which this crate builds on an
older toolchain, so if you pin an older Rust, pin an older `http-ng` with it.
In exchange nothing here carries version-shim code, and there is no MSRV
matrix to maintain.

There is also **no MSRV job in CI, deliberately**, and `rust-toolchain.toml`
pins no version — it says `channel = "stable"`. A job checking a fixed
version would be a second statement of the same promise, staler than the
first, and it is the one people would trust: the moment stable moves past
the pin, that job goes on passing while checking a toolchain nobody
supports. The whole test suite already runs on stable on three platforms,
which is the promise, so the pin would add a way to be wrong and no way to
be right.

**Two TLS backends, both behind the same `TlsConnect` seam.**
`http-ng-tls-rustls` is the default: memory-safe, and it behaves the same on
every platform. `http-ng-tls-native-tls` uses the platform's own stack —
SChannel, Security.framework, OpenSSL — and exists for deployments whose
trust decisions live in the OS store: enterprise roots pushed by policy,
smartcard client certificates, a FIPS-validated provider. That is a fact
about an environment, not a preference. It reports less back, and its own
module doc says exactly what; in particular it cannot report the negotiated
ALPN, so protocol selection driven by ALPN needs the rustls one.

`NoTls` in `http-ng-tls` is the third choice: no TLS at all, for a build
that has no room for a stack. `https://` then fails at connect with a typed
error, and `Capabilities::tls_config` reads `TlsSupport::None` rather than
claiming otherwise.

**`std` is required, and that is not our decision to reverse.** `http` 1.x
forbids `no_std` outright — `src/lib.rs` carries a commented-out
`#![cfg_attr(not(feature = "std"), no_std)]` next to
`compile_error!("`std` feature currently required, support for `no_std` may
be added later")` — and `http::{Request, Response, HeaderMap, Uri, Method}`
appear in the public API of seven crates here, including the sans-io
`http-ng-proto`. `bytes` is a genuine `no_std` + `alloc` crate, and `url` is
gone from the graph entirely (see below), so the remaining obstacle is
`http` itself; a feature flag that claimed otherwise would not build.

For constrained targets that *do* have `std` — static musl binaries, small
containers, embedded Linux — see `NoTls` and `IpLiteralOnly`, and
`crates/http-ng-native/examples/minimal.rs`.

Microcontrollers are not reachable today, but the obstacles are
dependencies rather than design. The `Transport` seam already spans a
socket plus hyper, a delegated `wasi:http` exchange, and the browser's
`fetch`; an `embedded-nal-async` backend would be a fourth point on that
line, nearer to the native one than `fetch` is. Two things stand in the way:

- **`http` 1.x, external.** The `compile_error!` above.
- **`url`, ours — and now removed.** `http-ng-proto` used it at exactly one
  functional site — `Url::parse().join()` for RFC 3986 reference
  resolution — and that one call pulled `idna` -> `icu_normalizer` +
  `icu_properties`: measured at 1.9 MB, 1004 KB, 820 KB and 452 KB of
  vendored source, almost all Unicode tables for internationalised domain
  names. On a part with 256-512 KB of flash that is the entire budget, for
  a feature such a device rarely needs.

  `crates/http-ng-proto/src/uri.rs` now implements RFC 3986 §5.2 directly,
  and `url` has moved to `[dev-dependencies]`, where it is the oracle for
  `tests/uri_resolution.rs` — a 96-pair differential corpus (all 42 RFC
  3986 §5.4 reference examples, plus the forms a client actually meets)
  that pins both implementations' answers and enumerates every place they
  deliberately differ. IDN survives as the `idn` feature of
  `http-ng-proto`, forwarded by `http-ng` and **on by default**, so a plain
  build behaves as before; `--no-default-features` removes `idna` and the
  ICU crates altogether and turns a non-ASCII host into a typed
  `UriError::NonAsciiHost` naming the A-label to send instead. Measured
  `cargo tree -e normal` for `http-ng-proto`: **36 crates with `idn`, 10
  without**, and no `url` either way. The `idn-feature-is-real` CI job
  checks all of that, in both directions, and runs the feature-off test
  suite that `--all-features` cannot reach.

Runtimes exercised in CI: tokio and smol. Connection reuse landed in v0.2
(W2) and `Native::new` now pools by default; **HTTP/2 landed in v0.2 (W3)**,
behind `http-ng-native`'s `http2` feature, off by default. HTTP/3 and
WebSocket are still later — see
[`docs/v01-acceptance.md`](docs/v01-acceptance.md) for what v0.1 deliberately
does not do, and what proves the four claims it does make.

**Response decompression landed in v0.2 (W5), inside `Client` and behind
the `gzip` and `brotli` features** (off by default, `json`'s precedent: a
browser build would be linking decoders that cannot run there). With either
on, a client asks for the codings it can actually reverse and reverses
whatever the server chose — unless the transport says it did that already.
That is a capability of its own, `Capabilities::response_decompression`,
and deliberately NOT read off `forbidden_request_headers`: `http-ng-fetch`
both forbids `Accept-Encoding` and decompresses internally, so the two
coincide there by accident, and a transport that forbids the header while
decoding nothing must still have its responses decoded.
`crates/http-ng/tests/compression_capability.rs` pins both directions.
The body comes back as `ClientBody<B, Tm>` = `Decompressed<Deadline<B,
Tm>>`, and that order is load-bearing: the deadline is polled once per
COMPRESSED frame, or a slow server sending well-compressing padding would
walk around a `total_timeout`.

### Vertical 2 (native): what's proven

**The runtime seam is real, not decorative.** The same generic code
(`fetch_once<R>` in `crates/http-ng/tests/two_runtimes.rs`, bounded by
`http_ng_rt::{TcpConnect, Timer, Blocking} + Clone`, with no `#[cfg]` anywhere
in the test code — the file's only conditional is the `#![cfg(not(target_family
= "wasm"))]` gate excluding it from wasm targets, where its native
dev-dependencies do not build) actually drives an HTTP/1.1 request over real TCP to a real
server on loopback — once under `http_ng_rt_tokio::Tokio` inside a
`tokio::runtime::Runtime`, once under `http_ng_rt_smol::Smol` on a bare
`futures_executor::block_on`. The property is confirmed by more than a green
run: adding `R::Instant: PartialEq<std::time::Instant>` to `fetch_once`'s
bound (the same mutation trick `http-ng-rt-pair-check`'s `pair_property.rs`
already applied to runtime capabilities individually) breaks instantiation on
`Tokio` (`Instant = tokio::time::Instant`, a wrapper, `E0277: can't compare
tokio::time::Instant with std::time::Instant`) and does not break `Smol`
(`Instant = std::time::Instant` directly) — the test is sensitive to a
regression of the seam, not just to whether the file compiles at all.

**The HTTP/1 exchange runs without spawn and without a reactor where there
isn't one.** `http-ng-native/tests/h1.rs`'s
`works_on_a_bare_futures_executor_with_no_spawn` checks this on IO with no
reactor at all (Task 12); `two_runtimes.rs` above checks the same property of
the transport (`Native`), now under real runtime backends, not just under the
test busy-spin.

**`DefaultTransport`/`Client<T = DefaultTransport>`/`Client::new()`** — the
`default-transport` feature (not in `http-ng`'s `default`, as for every
crate in the vertical — opt in explicitly; `default` carries `idn` alone,
see the `url` bullet above). On any non-wasm target it resolves to
`Native<Tokio, Rustls, SystemDns<Tokio>>` with the system trust store
(`rustls-platform-verifier`, not `webpki-roots` — a client that "just works",
not one with explicitly chosen roots). On `wasm32-unknown-unknown` it resolves
to `http_ng_fetch::Fetch`, and `Client::new()` there returns `Self` rather than
a `Result`, because fetch's constructor cannot fail. Without the feature, or on
`wasm32-wasip2` (`target_os = "wasi"`), the type doesn't exist at all — an
ordinary compile error, not a silently weaker transport; on wasip2/wasip1 there's deliberately no branch that reuses the
already-built `http_ng_wasi::WasiHttp` through this mechanism — `http-ng`
doesn't depend on `http-ng-wasi` (an invariant recorded in
`http-ng-wasi/Cargo.toml`), and adding that dependency here would mean a path
that no CI job in this repository builds (the `wasip2` job runs `http-ng-wasi`
directly). The direct path on WASI remains `Client::builder(http_ng_wasi::
WasiHttp::new())`, same as before this task. Resolution details are in the
`DefaultTransport` doc comment in `crates/http-ng/src/lib.rs`.

**HTTP/2 is negotiated and spoken, not merely compiled in (v0.2 W3).** The
`http2` feature is off by default and, when on, changes nothing a caller can
observe except speed and `Response::version()`: `capabilities()` still report
the **floor** — the value that holds on the worst protocol the transport
might negotiate — because over-claiming `full_duplex` costs a caller a
deadlock rather than a degradation, and because Cargo unifies features across
a graph, so a library can never know whether some other crate turned h2 on.
`crates/http-ng-native/tests/http2.rs` pins both halves: an `h2::server` on a
real socket answers the client (an HTTP/1.1 request would get nothing at all
from it) and `Response::version()` reads `HTTP_2`, while
`capabilities_report_the_floor_with_the_feature_on` asserts `full_duplex ==
false` with the feature compiled in.

Two things behind that are worth knowing before reading the code.
**`hyper/http2` is unusable here** — its executor bound
`Http2ClientConnExec` is a sealed trait and the executor is handed the h2
connection itself, so a crate with no `Spawn` cannot supply one; the `h2`
crate underneath it is used directly instead, its `Connection` polled by hand
exactly as hyper's HTTP/1 one already is (`src/http2.rs`'s module doc, and
the correction in `docs/v02-design.md` §W3). And **h2 is offered only over a
TLS backend that can report the negotiated ALPN** — `TlsConnect::reports_alpn`,
defaulting to `false`, overridden to `true` by `http-ng-tls-rustls`: a backend
that sends the ALPN list and cannot read the answer back (which is exactly
`http-ng-tls-native-tls`) would otherwise leave the client speaking HTTP/1
into a connection the server had switched to HTTP/2.

An h2 connection is **checked out of the pool exclusively, one stream at a
time**: without `Spawn` there is nobody to drive a shared connection but the
in-flight request futures, so a caller that stopped polling would stall its
neighbours. W1's "cancelling one stream must not tear down the others" then
holds because there are no others — a property of that pool policy, not of
the h2 code, and written down in both places so that lifting the exclusivity
does not lose it silently.

**What's still unverified live and carries over into vertical 3** (a boundary
from the vertical's brief, not narrowed by this task): the `Capabilities`
runtime model for fetch with its Chrome/Safari difference; `SseStream`
reconnection; `act` acceptance.

**Deliberately not done in v0.1** (recorded, not hidden): connection pooling
(one connection per request — **since done in v0.2 W2**, and `Native::new`
pools by default; `Native::without_pool()` restores this v0.1 behaviour);
streaming request bodies; `first_byte`/
`between_bytes` timeouts (declared unsupported via `Capabilities`, rather than
silently unimplemented); a single `getaddrinfo` call for both address families
instead of separate v4/v6 slots; h1 upgrade.

### Vertical 1 (WASI): what's proven

**Proven.** The `Transport` shape actually works against an ambient backend
with no socket of its own on the guest side — not in theory, but under a real
`wasmtime` host (`crates/http-ng-wasi/tests/live_roundtrip.rs`). A setting the
transport doesn't support becomes a typed `UnsupportedCapability` error
already at `ClientBuilder::build()`, rather than being silently ignored; the
same holds one level down — the `wasi:http` host rejecting a request-option
value (timeout, method, scheme) also becomes an error rather than being
dropped, and this isn't only verified by hand during implementation — it's
held in place by static analysis in CI (`no-discarded-wasi-setters`) on every
push.

**`full_duplex` is declared `false` — and that's about the `http-ng-wasi`
implementation, not about the shape of the seam.** The `wasi:http` 0.3
protocol itself supports duplex request bodies: body data can flow while the
host hasn't yet returned a response. The shipped `WasiHttp::execute` doesn't
give you that — `convert::race_send_with_body` waits for both `send` and the
full body write (except on an early `send` failure). Measured on a live
`wasmtime` host (host-specific behavior, not pinned down by `wasi:http`): the
response already existed on the server at t≈0.10s, but the caller saw it only
at t≈2.00s, once the body finished writing; for a body with no end, it would
never see it.

The limitation is lifted **inside `http-ng-wasi`, without touching
`Transport`.** `Transport::execute` returns `http::Response<Self::Body>`, and
`Self::Body` is `http_ng_wasi::Body`, a type from that same crate: the
unfinished write future is carried into it and polled further from
`poll_frame`, and a transfer failure becomes a terminal body error. The
branch's final review implemented this as a proof of concept — around forty
lines, one new `Inner` variant, the `Transport::execute` signature untouched —
and measured it on the same host and server: the branch as it stands hangs
until killed at 25s; the variant with the future in `Body` delivers
`RESPONSE_HEAD_RECEIVED status=200 OK` in 0.094s. The technique isn't new: the
same `convert::resolve_send` doc comment proposes exactly this for a
*different* discarded future (`transmitted`).

Deferred not because of the seam, but because of three real costs it would
have to pay: (1) the guard against undeclared trailers can't run before
`execute` returns — trailer names are only known once the body has ended, so
the guard moves into `Body` and becomes a terminal body error; (2)
`resolve_send`'s policy that "a response arriving on top of a failed body
write is not a success" moves from an `execute`-level error to a body-level
error, i.e. gets weaker; (3) a caller that never reads the response body never
finishes writing the request body either — that's inherent to duplex without
`spawn` and needs documenting. Vertical 2's work, entirely inside
`http-ng-wasi`.

Design: [`docs/superpowers/specs/2026-08-05-http-ng-design.md`](docs/superpowers/specs/2026-08-05-http-ng-design.md).
