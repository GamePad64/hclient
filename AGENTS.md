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

Two things nextest does not cover. Doctests: it cannot run them, so
`just test-doc` does — `cargo test --doc --workspace --all-features`, four
of them today, and **a CI job calls that recipe**.

That job is younger than the recipe, and the gap between them is the point.
`test-doc` existed and nothing called it — not `just ci`, not any workflow
step — which is worse than no recipe at all, because it is the one people
trust before pushing. Two examples were broken the whole time it was
unwatched. `http-ng-h3`'s called `Rustls::with_webpki_roots()`, which lives
behind `http-ng-tls-rustls`'s `webpki-roots` feature while that crate's own
dev-dependency enabled `quic` alone; it compiled under `--workspace` only
because another member turned the feature on and Cargo unifies features
across the graph, so the workspace-wide run was green over an example that
did not build the way a reader would build it. The second broke the day the
WebSocket framing became its own crate — its example names `http_ng::Client`,
because borrowing a transport a `Client` already owns is the whole reason
`Tungstenite` borrows — and the job caught it rather than a reader.

Both are fixed by giving each crate the dev-dependency its own example
needs, which is what "builds the way a reader would build it" means.

Browser tests: those go through `wasm-pack test --headless
--chrome|--firefox` regardless, see the `browser` job.

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
| `http-ng-h3` (v0.3) — **measured**, and the vertical-1 prediction of 55 crates was close | `[bytes, default, io-util, sync]` plus `tokio-util`, from `h3` and `h3-quinn`; **58 crates** in total (57 until v0.4 moved `SeamRuntime` out into `http-ng-rt-quinn`, which is the one addition). Still no reactor from this crate's own dependencies — the reactor arrives with whichever `R` the caller supplies, and `R: Spawn` means it must have one |
| `http-ng-rt-quinn` — **measured**, v0.4 | `[default, sync]`, hyper's inert leaf and nothing else: **42 crates**, `quinn` + `quinn-proto` + `quinn-udp` + `ring` on top of `http-ng-rt`, and **no `h3`** — a crate that wants bare QUIC over this seam takes 42 rather than 58 and no opinion about HTTP |

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
| native | `http-ng-h3` — QUIC + HTTP/3, its own crate, TLS through a second seam | yes |
| WASI | `http-ng-wasi` — `wasi:http` 0.3 | **no** |
| browser | `http-ng-fetch` — `fetch` | **no** |

Both "no"s are machine-checked on every push rather than asserted here:
`ambient-has-no-tokio` runs `cargo tree` for `http-ng-fetch` against
`wasm32-unknown-unknown` and for `http-ng-wasi` against `wasm32-wasip2`, and
fails closed if the invocation itself breaks or returns nothing. Measured
while writing: zero matches for `tokio`, `hyper` or `h2` in either wasm
graph, four in the native one.

**Nothing here is published, and the version numbers are nominal.** Every
crate says `0.1.0` while `main` carries all of v0.2 and the start of v0.3.
That is deliberate, not drift: publishing is a promise not to break, and
this workspace is not ready to make it — `TlsConnect` changed three times in
one session (`reports_alpn`, the `TlsIdentity` extraction, the 0-RTT slots),
`Timer` gained `type Sleep`, `TcpConnect` gained `APPLIES`, and `UdpBind`
arrived from nothing.

So: **do not bump versions, do not add a CHANGELOG, do not configure
`publish`** as tidying-up work. The trigger is the owner's, and it is
"full-featured" rather than a date — what is still missing is enumerated at
the end of `docs/v01-acceptance.md`, `docs/v02-acceptance.md` and
`docs/v03-acceptance.md`, which is where to look before asking whether it
is time.

The freedom this buys is worth naming, because it is why the seams could
move as often as they did: a change to a public trait costs a rebase here
and nothing at all outside, so the right shape can be chosen on its merits
rather than on what is already promised.

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

**A second TLS seam, for QUIC, and it is not a widening of the first.**
`http-ng-tls-quic`'s `QuicTlsConnect` exists because the intersection of
`TlsConnect`'s four methods with `quinn_proto::crypto::Session`'s eleven is
**empty** — QUIC wants key schedules per encryption level and CRYPTO-frame
payloads, `TlsConnect` can only hand back a wrapped byte stream — and the
failure mode is worse than a compile error: an adapter between them
type-checks *with an empty body*. `http-ng-tls-rustls` implements it behind
a `quic` feature; `http-ng-tls-native-tls` implements nothing and using it
for HTTP/3 is a compile error, which is honest rather than harsh, because
`native-tls` binds no QUIC API at any level. It is a separate crate rather
than a feature of `http-ng-tls` because Cargo unifies features: a feature
would put `quinn-proto` in the graph of every build in which any crate
wanted h3, including `NoTls` ones. `TlsConnect` and `QuicTlsConnect` share
`TlsIdentity`, so a connector has one configuration identity rather than
two.

`NoTls` in `http-ng-tls` is the third choice: no TLS at all, for a build
that has no room for a stack. `https://` then fails at connect with a typed
error, and `Capabilities::tls_config` reads `TlsSupport::None` rather than
claiming otherwise.

**`std` is required, and that is not our decision to reverse.** `http` 1.x
forbids `no_std` outright — `src/lib.rs` carries a commented-out
`#![cfg_attr(not(feature = "std"), no_std)]` next to
`compile_error!("`std` feature currently required, support for `no_std` may
be added later")` — and `http::{Request, Response, HeaderMap, Uri, Method}`
appear in the public API of ten crates here, including the sans-io
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
  build behaves as before; `--no-default-features` removes the whole IDN
  implementation and turns a non-ASCII host into a typed
  `UriError::NonAsciiHost` naming the A-label to send instead. The
  `idn-feature-is-real` CI job checks all of that, in both directions, and
  runs the feature-off test suite that `--all-features` cannot reach.

  **The feature no longer names `idna`; it names `http-ng-idn`**, which
  chooses the implementation by target in its own `build.rs`. `uri.rs`
  calls `http_ng_idn::domain_to_ascii` and maps two error variants, and
  nothing in `http-ng-proto` mentions `idna` any more. What that changes
  is where the Unicode tables are, not what a host converts to: on Linux
  and the other ELF unixes, and on wasm, the backend *is*
  `idna::domain_to_ascii_cow(…, AsciiDenyList::URL)` — the same call
  `uri.rs` used to make itself, with the same arguments. Measured before
  believing: the 96-pair corpus is unchanged and green in both feature
  settings, and `http_ng_idn::domain_to_ascii` against `idna` directly
  over 9,739 inputs on this host gave 0 differences.

  So the crate count is now a fact about the target rather than one
  number. Measured `cargo tree -e normal`, unique crates, this tree:

  | build of `http-ng-proto` | crates | what supplies UTS 46 |
  |---|---|---|
  | default (`idn`), x86-64 Linux | **37** | `idna` + the ICU data crates |
  | default (`idn`), `--target x86_64-pc-windows-msvc` | **13** | `icuuc.dll`, through `windows-sys` |
  | default (`idn`), `--target aarch64-apple-darwin` | **15** | Foundation, through `objc2-foundation` |
  | `--no-default-features` | **10** | nothing — `NonAsciiHost` |

  The Linux row is the old **36** plus `http-ng-idn` itself and nothing
  else: `thiserror` was already there, and no new Unicode crate arrives.
  There is no `url` in any of them.

**Name resolution has a third backend, and its problem was never the wire
format.** `http-ng-dns-doh` (v0.3) puts DNS-over-HTTPS behind the same
`Resolve` seam, and the two questions worth knowing the answers to are
both about bootstrapping rather than about parsing. What makes the
request is a `Transport`, **never an `http_ng::Client`** — a cookie jar,
a redirect policy and `Authorization` belong to `Client`, which
`Transport` has never heard of, so "a resolver's client is not the
user's client" is a thing that does not typecheck rather than a thing
that is discouraged; the cost is that there is no `total` bound, because
that is `Client`'s. And what resolves the DoH server's own name is
stated by which constructor compiles: `Doh::pinned` takes an IP literal
and refuses a name, `Doh::bootstrapped` takes a name and refuses a
literal. Failing closed is the default and failing open is visible in
the type — `Doh<C>` is `Doh<C, NoFallback>` — so it travels into every
transport that holds it rather than hiding in a builder call.

SVCB parsing moved from `http-ng-dns-system` up into `http-ng-dns`
behind a `codec` feature, so an `IpLiteralOnly` build carries no DNS
decoder: 13 crates without it, 16 with, and the DoH crate itself is 22
with no `tokio`, `hyper` or `h2`.

**HTTPS/SVCB records are consulted before every new connection** on a
resolver that says it can ask (v0.3 W2), for `https://` at the default
port only — RFC 9460 §9.5, since the record fetched for a bare name is
the default-port one, and `http://` would mean upgrading the scheme,
which a connector must not do silently. The record's port, address
hints and ALPN offer are used; **its `ech_config_list` is passed on only
to a TLS backend that says it applies one** (`TlsConnect::applies_ech`,
defaulted `false` beside `reports_alpn`), and no backend in this
workspace does. That is not caution: `http-ng-tls-rustls` *refuses* a
non-`None` `ech`, so a connector that filled the field from every record
would make every ECH-publishing origin unreachable. Measured before it
was decided — zero bytes on the wire. The privacy cost of the gate is
stated where a caller will find it, including a test asserting the
origin's name goes out in the clear.

The record and the addresses are asked **at once**, which took a second
commit: the first put discovery in front of the address lookups and
roughly doubled cold DNS on the default path. Measured after: floor
396 → 322 ms, median 456 → 340 ms on a DNS-dominated request; a record
cost 404.6 ms of extra DNS time and now costs 0.8 ms.

Runtimes exercised in CI: tokio and smol. Connection reuse landed in v0.2
(W2) and `Native::new` now pools by default; **HTTP/2 landed in v0.2 (W3)**,
behind `http-ng-native`'s `http2` feature, off by default; **HTTP/3 landed
in v0.3**, in its own crate `http-ng-h3`, over QUIC. **WebSocket landed in
v0.3 (W4)**, and in v0.4 became a crate of its own,
`http-ng-ws-tungstenite` — and not as a method on `Transport`: it is its
own trait pair,
`WebSocketConnect` (what a backend implements — a backend is not a
connection) and `WebSocket` (the message channel), so a transport that
cannot do it is a **compile error** rather than a runtime `Unsupported`.
`Capabilities::upgrade` is gone with it: four variants, and nothing ever
branched on any of them.

The seam is message oriented on purpose. "Hand back the socket after the
101" is implementable by exactly one of the four backends here, and the
three it shuts out include the browser — where `WebSocket` is a separate
global that a `fetch`-shaped `Transport` cannot reach at all. That the
browser then fitted the trait **unchanged** is the evidence the shape was
right, and it settled two things the design could only argue: the seam's
`!Send` allowance has a real subject (`FetchWebSocket` is `Rc<RefCell<..>>`
plus three `Closure`s and needed no `unsafe impl Send`, because no `Client`
sits between this seam and its caller), and `Message` has no `Ping`/`Pong`
because a browser has neither `send(ping)` nor `onping` — the variant would
have had no honest right-hand side.

Framing on native is `tungstenite`, driven by us rather than through an
async wrapper, and the reason is not taste: `WebSocketContext` takes the
stream as a *parameter*, so the shim can borrow the poll `Context` for one
call — where `tokio-tungstenite`'s `AllowStd`, owning its stream across
calls, has to smuggle a `*mut Context`. `docs/w4-upgrade-seam.md` has the
measurements and the decisions.

**It is `http-ng-ws-tungstenite`, its own crate, and until v0.4 it was a
`websocket` feature of `http-ng-native` — the one pluggable thing here
that was not its own crate** (`docs/w4-upgrade-seam.md` §8). Features are
additive, so that feature put `tungstenite` into every build in any graph
that switched it on: the argument that kept `http-ng-h3` out of
`http-ng-native` and `http-ng-tls-quic` out of `http-ng-tls`, applied to
the one place it was not. A dependency in the other direction cannot be
switched on from outside, and `graph-no-framing-in-the-transport` checks
it with `--all-features` on the transport rather than asserting it.

**The seam between them is "an upgraded byte stream, plus the `read_buf`
hyper had already read past" — the shape §2 rejects as the public seam.**
Both hold at once and they are about different levels: as the public seam
it excludes three of four backends, the browser among them; between a
transport and a framing crate it is only ever asked of the one backend
that can answer it. `http_ng_native::Upgrading` is that seam, and it is
two-step on purpose — it lends out the `101`'s head and is dismantled only
by a separate `finish`, so the checks that decide whether this is *your*
`101` cannot run after the connection has been taken apart.

**What implements `WebSocketConnect` is `Tungstenite<'_, R, T, D, H>`, a
connector that borrows a `Native`**, and the losing option is worth its
line: `Native` keeping the impl and delegating the framing costs a caller
**zero** lines, against one dependency and one expression
(`Tungstenite::new(client.transport()).websocket(req)`) — and leaves the
defect exactly where it was, since the impl needs `tungstenite` and would
put the feature straight back on the transport. It borrows rather than
owns, unlike `Selecting`, because `Native` is not `Clone` and
`Client::builder` takes its transport by value: owning would cost either a
second transport with a second pool or a `Transport` impl on a type that
sends no requests. `http-ng-fetch` is untouched by all of this and needs
no connector — a browser hands back *messages* — which is the asymmetry
that says the seam is in the right place.

**An open WebSocket is bounded by liveness, not by `Timeouts`, and the
knob is on the connector rather than on the seam.** `total` is
meaningless for a connection meant to outlive its exchange and
`between_bytes` would be actively wrong, since silence is a WebSocket's
normal state — so the bound is RFC 6455's ping/pong, off by default,
because a default that pings sends traffic nobody asked for. It is not on
the trait because a browser has neither `send(ping)` nor `onping`, the same
fact that keeps `Ping`/`Pong` out of `Message`; asking `http-ng-fetch` for
it does not compile.

**Two clocks, answering to different events**, which is the part that took
measuring rather than deciding. The interval measures *silence*, so any
inbound frame restarts it — which is what makes the feature free on a busy
connection. The deadline measures *an unanswered probe*, and only a `Pong`
carrying that ping's own payload clears it — matched on payload rather than
opcode, because §5.5.3 allows unsolicited pongs, and letting any frame
clear it would turn the probe back into the gap bound that was rejected.
That distinction was found by a mutation that **survived a test asserting
the right error**: with any frame clearing the probe, the stream still
failed with the same `PongNotReceived`, the same kind and the same bound —
a second ping had simply died 100 ms later. Same error, different fact. The
fixture now counts pings and the test asserts exactly one.

The missed-pong error is `ErrorKind::Body` with a public `PongNotReceived`
source, deliberately agreeing with `http-ng-fetch`'s treatment of a
`wasClean == false` close rather than inventing a second vocabulary, and
deliberately not `ErrorKind::Timeout`, since no `Timeouts` field is in
force. Nothing is spawned: the caller's `poll_next` is the only thing
driving the socket, **so a caller that stops polling gets no keep-alive**.
That is the mirror of HTTP/3, where a spawned driver turned out to be
necessary and not sufficient; here there is no driver at all.

See [`docs/v01-acceptance.md`](docs/v01-acceptance.md) for what v0.1
deliberately does not do, and
[`docs/v03-acceptance.md`](docs/v03-acceptance.md) for what v0.3 does,
does not, and has not checked.

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
`crates/http-ng-h3/tests/streaming.rs` the caller's body has no second
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

**It is its own crate, and could not have been a feature of
`http-ng-native`.** The reason is the type system rather than the 57-crate
QUIC stack: this transport is bounded on `R: UdpBind + Spawn<..>` and
`T: QuicTlsConnect`, neither of which `Native<R, T, D>` has, and Cargo's
features are additive — so a `http-ng-native/http3` feature would make both
unconditional for every build in the graph.

**It requires `R: Spawn`, and it shares connections.** An idle HTTP/1 socket
needs nobody; the kernel holds it. **A QUIC connection that nobody polls is
not idle, it is dying** — the PING that resets the peer's idle timer comes
from the connection's driver. So the driver is spawned, and once it is,
v0.2 W3's reason for handing out h2 connections *exclusively* has no
subject: that argument was explicitly conditional on there being no
background task, and a driver that is nobody's request future cannot be
stalled by a caller that stops polling. Both halves are written next to
their own policy — `http_ng_h3`'s module doc and `http-ng-native`'s
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
the transport's: a `425` leaves `http-ng-h3` untouched, with a test pinning
it. The one line it owed back — `AllowEarlyData` removed from the replayed
request, because the mark is part of the pool key, so a marked replay would
ask for the early-data connection and, if that one had since been evicted,
would go out in early data against the very server that refused to risk it
— is paid, in `Client::run`'s `425` branch.

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

`Deadline` now **races a real sleep** rather than only stamping each frame
with the elapsed time, so `total` also cuts a body that goes completely
silent after the head — the one case an elapsed-time check structurally
cannot reach, since nothing will ever poll the wrapper again. That was
written down as impossible (`Timer::sleep` was an RPITIT), then as
possible-but-deferred, and is now done; `crates/http-ng/tests/deadline.rs`
carries the server that sends a head and then nothing, for ever.
`between_bytes` is a different promise — it bounds the gap between two
frames and restarts on each — and it landed in the same week, on
`http-ng-native`: `Native` declares and enforces `first_byte` and
`between_bytes`, the latter through `IdleTimeout<B, Tm>`, a body wrapper
holding a sleep of its own. Neither bound implies the other, and a caller
that sets only one has bounded only one shape: a body dripping a byte
every 50 ms for an hour passes `between_bytes` and is cut by `total`; a
transfer that legitimately takes an hour and stalls for ten minutes in the
middle is the reverse. Measured from outside the client against three
misbehaving servers, each with a control that must hang with the bound
unset — `crates/http-ng-native/tests/timeouts.rs`.

**A cookie jar landed in `Client`, behind the `cookies` feature** (off by
default — `http-ng-cookie`'s compiled-in public suffix list is +77 KiB, and
the browser, where paying that is certainly wrong, keeps its own jar
anyway). `ClientBuilder::cookie_jar(jar)` switches it on; the rules are
`http-ng-cookie`'s, sans-io and clockless, and what `Client` adds is *when*
(once per redirect hop, and re-derived rather than carried, so a cookie
scoped to `/one` cannot ride a same-origin 302 to `/two`), *whether*, and a
`now`.

*Whether* is `Capabilities::owns_cookie_jar`, and a client-side jar against
a backend that reports it is an `UnsupportedCapability` at `build()` — the
same shape as a `RedirectPolicy` against `RedirectSupport::Internal`, and
exactly the arm that capability's own doc comment said would arrive with
the setting. `http-ng-fetch` is the backend: the browser attaches and
stores cookies itself and forbids the `Cookie` header, so a second jar
there would store every `Set-Cookie` twice — including the ones the browser
refused — while the header it produced was dropped on the way out.

The *`now`* is `SystemTime::now()` and deliberately **not** the client's
`Timer`: `Timer::Instant` is `Copy + PartialOrd` with an `elapsed_since` —
a stopwatch with no epoch — and `Expires` is a calendar date. Anchoring a
wall clock and advancing it with `elapsed_since` would freeze outright
under `NoClock`, whose `elapsed_since` is `Duration::ZERO` for ever. The
cost of the choice is that `SystemTime::now()` panics on
`wasm32-unknown-unknown`, which is written where the setter is.

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
of the wire in `crates/http-ng/tests/too_early.rs`: the same request
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
whatever the request asks for. True — of the connection `http-ng-h3`
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
`docs/h3-research.md` §3.5 has the three-row table.

### Every backend now reports events, and two of them report one thing (v0.4 W2)

Hooks landed on `http-ng-native`, then `http-ng-h3`, then the two that own
no connections at all. **`http-ng-fetch` and `http-ng-wasi` emit exactly one
of the four events — `Head` — and they reached that answer without sharing
any reasoning.** `Connected`, `Reused` and `Closed` have no emitter in
either, and for `wasi:http` that is checkable rather than argued: `client`
is one function and there is **no connection resource anywhere in
`wasi:http@0.3.0`**. `error-code`'s eleven `connection-*` variants are how
`send` fails, not events — a `Closed::Failed` built from one would announce
the end of a connection whose beginning was never announced.

**The browser's `Performance` surface was the real question and it is
measured, not assumed**: the entry does not exist when `execute` returns
the head — 0 entries, 1 after the body drains — and nothing on it says
which request it belongs to (`requestId`, `id`, `connectionId`,
`transferId` all `undefined`). Either fact alone kills `Connected`.

**Two of `Head`'s five fields had no source, and they are the same two on
both backends — and the two answers came out different, which is the part
worth knowing.** `id` and `version` were recorded together as debts owed by
`http-ng-core`; taken up together, they separated on one question: **is the
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
demanded. `docs/v04-w2-hooks-ambient.md` §9.

The bounds went down again: `H: Hooks` alone here, one fewer than h3 and
two fewer than native, because the only event fires while `execute` still
owns everything and no body holds a hook.

**One CI gap fell out of it**, the same shape as the doctests nobody ran:
`http-ng-wasi`'s live tests sat in a file `just test-wasi` did not name, so
they printed a `NOTICE` and reported `ok` for ever — the exact defect the
`HTTP_NG_REQUIRE_WASMTIME` marker exists to prevent. Moved: the recipe runs
16 live tests where it ran 12.

### WebTransport runs on this h3, and the spec's reasons for not writing it are gone (v0.4 W2)

`http-ng-webtransport` opens a session over `http-ng-h3`'s QUIC:
`Session::connect`, `Session::id`, `Session::open_bi`. Its own crate, for
the reason `http-ng-h3` is not a feature of `http-ng-native` — features are
additive. 49 crates, `tokio` with no reactor, and `quinn` arrives with
`futures-io` alone and **no `ring`**, which is the visible consequence of
owning no endpoint.

**The premise was proved twice, and the second time is the one that counts.**
`docs/w4-upgrade-seam.md` §4 said extended CONNECT was reachable from `h3`'s
client API "verified by reading"; it is now executed — against `h3`'s own
server, and then against **`wtransport` 0.7.2**, which carries its own
HTTP/3 and depends on `h3` not at all. Two implementations sharing no code
agreed on the wire. The `wtransport` spike is **not** kept as a test — 114
crates, `url` and ICU among them — but `docs/v04-w2-webtransport.md` §10 has
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

**Five things `h3` and `http-ng-h3` do not expose, found and not patched
around.** `h3` 0.0.8's client **cannot announce WebTransport** at all —
`enable_webtransport` is on the *server* builder and `Config::settings`'
fields are `pub(crate)` — so the draft's client-side MUST is unsatisfiable
today; that is **asserted in a test** rather than described, so an `h3` that
grows the setter fails a line instead of leaving a stale paragraph.
Server-initiated unidirectional streams are consequently unreachable, since
the arm that would keep them is guarded by the flag a client cannot set. And
`http-ng-h3` exposed no `quinn::Connection`, so its `SeamRuntime` — 302
lines — was unreachable and this crate takes a `quinn::Connection`
instead.

**That last one is closed: `SeamRuntime` is `crates/http-ng-rt-quinn`**, the
same shape §8 argues for the WebSocket framing, and the crate is 42 crates
with no `h3` in them against `http-ng-h3`'s 58. `http-ng-h3` re-exports
`QuinnTask` from it and is otherwise unchanged — one visibility change in
the whole move, `endpoint` from `pub(crate)` to `pub`. `just
graph-quinn-adapter-is-shared` checks both directions, including the one no
`absent` check can see: `http-ng-h3` must still *depend* on it, or someone
has re-added a private copy.

**What that settled is that the two options recorded for closing it were
never alternatives.** A connect-only entry point on `H3` cannot serve
WebTransport at any price, because `H3::connect` builds an h3 client on the
connection and spawns its driver before it has one to hand back — and two h3
clients on one QUIC connection is `H3_STREAM_CREATION_ERROR`, the same
reason a session cannot share a *pooled* one. `http-ng-webtransport` still
takes its connection from outside, and now because the remaining half is a
**dial** it would be the second author of: measured at 49 → 58 crates,
`ring` among them. `docs/rt-quinn-extraction.md` §5.

A session cannot share an `http-ng-h3` pooled connection, for three reasons
in increasing hardness: a second h3 client on one QUIC connection opens a
second control stream (`H3_STREAM_CREATION_ERROR`); extended CONNECT is
announced in SETTINGS at handshake and `http-ng-h3` announces it nowhere, so
making pooled connections capable would change what **every** build puts on
the wire; and `PoolKey` has no field to tell the two apart.

Deliberately not done, each with what it needs: datagrams, the capsule
protocol, observing session end, `GOAWAY`, server-initiated streams, and
more than one session per connection.

### A connect can be asked for on its own, and the first thing that wanted one was not the race (v0.4)

`StagedConnect` — `connect` -> an opaque handle -> `exchange` — on
`http-ng-native` and on `http-ng-h3`, **one trait per crate and not a method
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
letting `H3` answer for itself.** `http_ng_native::Staged` *owns* the
connection it took out of the pool, and needs a `Drop` that checks it back
in, so a connection made for a request that went elsewhere is warm rather
than closed (`without_pool()` is the control: no check-in, and the drop
closes the socket). `http_ng_h3::Staged` is a **claim on a connection the
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
reverse of the order both were written in. `Selecting` asks `H3` to
*connect*; where that fails it records the origin in
`http_ng_select::H3Failures` and routes the request — untouched, unsent,
never handed to a transport — over TCP. So the fallback is not
request-level retry and needs no `retry_kind()` condition:
`http-ng-native`'s own sentence is true of it verbatim, *this is not a
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
how `docs/v04-w1-acceptance.md` §9.3's second blocker turned out to be the
right worry about the wrong premise: the memory records *that* the connect
failed and never reads why. The black hole is used once, where a test needs
the bound *spent*. Twenty mutations, nineteen killed, one control.
`docs/v04-staged-connect.md`.

### One transport can now choose between the two stacks (v0.4 W1)

`http-ng-select`'s `Selecting<R, T, D>` owns a `Native<R, T, D>` and an
`H3<R, T, D>` and sends each request over one of them, deciding from the
origin's **HTTPS record**: `alpn` containing `h3` chooses QUIC, anything
else chooses TCP. That closes a gap `http-ng-native`'s discovery module had
written down about itself — *"an `alpn` containing `h3` is a fact this crate
can read and cannot act on … there is nowhere in this codebase for 'choose
between two protocol stacks' to live"*. It is a crate rather than a feature
of either member for the reason `http-ng-h3` is not a feature of
`http-ng-native`: features are additive, and one on either would put the
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
That contradicted the sentence `http-ng-native` leans on for needing no
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
`SvcbEndpoint` carries no TTL and inventing a lifetime for someone else's
answer is how a resolver's cache and ours drift apart. RFC 7838 §3.1's `ma`
**is** that lifetime, given by the origin for exactly this advertisement —
so the cache that would have been dishonest for SVCB is the right shape for
Alt-Svc.

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
persisted, and `Selecting::network_changed()` is the only entry point —
public, for the caller who can see what the transport cannot. Until it is
called every entry behaves as `persist=1`, which is the unsafe direction and
is said where the setter is.

**The negative half is built now, and not by the race** (v0.4). It took
reading to establish that it was missing rather than misplaced:
`http-ng-native`'s `NegativeCache` is a different fact — a TCP connect
through a discovered endpoint failed — and it never sees an h3 attempt,
because when `Selecting` routes to `H3` the native transport is not called
at all. `http_ng_select::H3Failures` is the memory that was owed; the
**staged connect** is what unblocked it, and the section below is that.

`docs/v04-w1-acceptance.md` §7 and §9 say what the race would need and what
the slow tier does and does not check.

**The part that was not mechanical is the capability set, and it is not the
race.** `Transport::capabilities` returns a `&Capabilities`, so the pair's
answer is stored at construction, and it is decided field by field by one
rule: **the stored value must be true whichever member serves the request.**
Seven fields disagree today — measured, not taken from the design document,
whose two examples had both been fixed under it while it was being written.
Six take the weaker claim, `full_duplex` among them, which is the same
answer `http-ng-native` already gives one level down for the same reason: an
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
`http-ng` reads the field and a marked request would reach the QUIC stack
anyway. The contrast three lines below it in the same function is
`CancelSupport`, where `Supported` is a **duty owed on every dropped
future** and a member that does not owe it makes the claim false.

**What the choice costs is counted rather than argued**: **one** type-65
query per request that has a name to ask about, whichever stack answers. A
`RequireVersion` demand, `http://` and an IP literal cost none at all.

It was two on the TCP path at an origin's default port, because
`http-ng-native` fetched the record again inside its own connector, and the
fix is the part worth knowing: **the record is not handed to the connector,
it is fetched by it.** `http_ng_native::Prefetch::prepare` does the
connector's own lookup — its resolver, its rule about where discovery
applies, its negative cache — and hands back a `Prepared`, which is the
request *with* the answer; `execute_prepared` then does not look again. A
caller cannot supply a record, because there is no constructor that pairs
one with a request it was not fetched for, so the wrong-origin question
cannot be asked rather than being answered by a check. The shape that was
rejected is the obvious one: a request extension is the caller's channel,
and an HTTPS record carries a port and address hints, so an extension
carrying one would let any code that can build a request move the
connection somewhere else. `docs/v04-w1-acceptance.md` §3.1 has the
argument and §3.3 the eleven mutations behind it (ten killed, one control).

The other half of the rule is what keeps `http-ng-select` from owning a
copy of the connector's: where the member did **not** look — a non-default
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
`docs/v04-w1-acceptance.md` §5 records how the table was checked as well as
what it says.

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

**`TCP_NODELAY` is asked for when the runtime says it can apply it, and the
41 ms it saves is the head of every connection.** Measured from the server's
side of the wire, with every read stamped before TLS sees it: with Nagle on,
the client's `Finished` and its `GET` arrive **coalesced as one 137-byte
write, 41.6 ms late**; with `nodelay` they arrive separately at 0.25 ms.
Four independent confirmations that it is the *request* that waits — the gap
is inbound at the server, the byte counts show the coalescing (137 = 74 +
63), `TCP_NODELAY` on the *server* changes nothing, and plaintext never
stalls because there the request head is the connection's first write.

**The default did not change, and that is the decision.** `TcpOpts` is a
socket seam that cannot know its caller writes request/response, and in this
workspace a *set* option is a **refusal**: `nodelay: true` in the seam's
default would turn every connect on a backend that left `TcpConnect::APPLIES`
at its understating `NONE` into an `Unsupported` error for an option nobody
asked for. So `Native::new` asks for exactly what the runtime declares —
`nodelay: <R as TcpConnect>::APPLIES.nodelay` — which is `applies_ech`'s and
`reports_alpn`'s shape one seam over: a constant defaulted to the
understating value, read by the layer above to decide whether to *ask*.
Silence now costs a slow connection rather than a refused one. The cost is
that `tcp_opts` replaces the whole set, so a caller setting only `keepalive`
turns `nodelay` back off — pinned by a test.

It also made a latent defect visible, which is worth more than the
milliseconds: `http-ng-select`'s Alt-Svc fixture answered once and closed
**without `Connection: close`**, which RFC 9112 §9.6 makes a MUST. The
client pooled a connection the peer had already closed and the next request
raced the FIN — `http-ng-native`'s pooled-reuse window, recorded in `h1.rs`
as residual and still there. Nagle's 41 ms had been padding the gap; with it
gone the suite failed 7 runs in 12 under `-j16`. The fixture now announces
its close, and the library's race is unchanged and still recorded.

**Checked against gRPC as an external yardstick, and gRPC itself is out of
scope.** The goal is a client powerful enough that someone else could build
gRPC on it, so `grpc/doc/PROTOCOL-HTTP2.md` was used the way Autobahn was
used for WebSocket: 21 requirements, 15 tests, every claim read off what a
real `h2::server` decoded. **No library code changed** — the client already
did all of them, including `te: trailers` reaching the wire, a Trailers-Only
response (HEADERS with END_STREAM and no DATA) arriving as a complete
response, a message split across DATA frames arriving as the caller sent it,
the empty end-of-stream DATA frame, back-pressure in both directions, and
sixteen rounds of bidirectional streaming on one stream — which is the first
consumer-shaped exercise of the duplex h2 landed in v0.4.
`docs/grpc-yardstick.md` is the row-by-row report.

**Three limitations, none new, two of them one — and all three closed on
request in v0.4.** By default there is **no multiplexing**: an h2
connection is checked out exclusively, so two concurrent calls cost two
connections and two handshakes. That is v0.2 W3's decision, and its reason
is still live for the *default* — without `Spawn` there is nobody to drive
a shared connection but the in-flight request futures, so a caller that
stopped polling would stall its neighbours. **Cancellation therefore closes
the connection rather than sending `RST_STREAM(CANCEL)`**: the pump's
`Drop` does queue the reset, but the `Connection` is dropped in the same
breath. And a `PING` on a pooled connection is answered by the next call
rather than promptly. The first costs a handshake per concurrent RPC; none
of the three costs a failed call.

**`Native::multiplexed()` closes all three, and the second and third cost
no code of their own** — which is what `docs/grpc-yardstick.md` predicted
when it classified them as downstream of the first. It spawns the h2
connection's driver, so the connection outlives the stream (the queued
`RST_STREAM(CANCEL)` reaches the wire) and outlives the request (a `PING`
is answered while idle), and concurrent requests share it: eight
concurrent calls, **one** accept, eight streams open at once by the
server's own count.

**The bound sits on that constructor and nowhere else, which is the whole
of the design.** `http_ng_rt::Spawn` declares zero bounds, so
`<R as Spawn<F>>::spawn` coerces to `fn(&R, F)` and lives in a field that
demands nothing of `R` — no signature a `Spawn`-less runtime meets
changes, and `two_runtimes.rs` still runs `Native` on a bare
`futures_executor::block_on`. A runtime with no `Spawn` gets `E0277` where
it wrote `multiplexed()`, and so does a hook holding an `Rc`, because the
driver carries `H` so that a shared connection's `Closed` has an emitter
at all — the collision `http-ng-h3` met from the other side and could not
close.

**Three prices, and each is said where a caller meets them.** A spawner
nobody drives turns "sockets stay open" into "requests **hang**", which is
worse than the reaper's version of the same mistake and is cut only by
`Timeouts::first_byte`. Beyond the peer's `MAX_CONCURRENT_STREAMS`
requests **queue** — no second connection is opened, because
`SendRequest::poll_ready` is a liveness check and not a capacity one, so
the threshold would have to be ours and depends on a handshake cost that
is a network property rather than a loopback one. And `.hooks(..)` must
come **before** `.multiplexed()`: the spawner's type names the hook, so
the other order compiles and shares nothing.

Measured through the real transport in both arms, 480 requests at a
concurrency of 8: **480** TCP accepts and 480 TLS+h2 handshakes exclusive
against **60** shared, for ~3× the CPU. In steady state — a warm pool,
loopback, no handshake left to save — sharing costs *more* CPU and saves
the sockets, which is the honest shape of the trade.
`docs/h2-multiplexing.md` §11.

Also worth knowing before reading `capabilities()`: with `http2` on,
`full_duplex` and `response_trailers` still report the HTTP/1.1 **floor**, so
a caller cannot ask the capability whether duplex and trailers will work.
The honest route is `RequireVersion(HTTP_2)` before the head and
`Response::version()` after it — the floor rule behaving as designed rather
than a contradiction.

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
streaming request bodies (**since done — v0.2 W6 on `http-ng-native`, v0.3
on `http-ng-h3`, where they arrive with real full duplex**); `first_byte`/
`between_bytes` timeouts (declared unsupported via `Capabilities`, rather than
silently unimplemented — **since done in v0.2 W4**, declared and enforced in
one commit, and measured against servers that answer never, fall silent after
the head, and stall mid-body: `crates/http-ng-native/tests/timeouts.rs`); a
single `getaddrinfo` call for both address families
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
held in place by static analysis in CI (the `no-discarded-wasi-setter-result`
ast-grep rule, with the corpus it was accepted against next to it in
`scripts/ast-grep/rule-tests`) on every push.

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
