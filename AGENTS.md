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

**That story has a third act, and both halves of it are about a check
that could not fail.** `test-doc` counted five ```ignore fences as
tests — rustdoc compiles none of them, so the recipe printed `ok` over
five code blocks nothing had ever built. Four were real examples and
are `no_run` now, with hidden setup lines; writing `http-ng-rt-quinn`'s
found that `UdpAdoptStd` is `http_ng_rt`'s rather than that crate's, so
a reader copying the sketch would have imported it from the wrong
place. The fifth quotes `embassy-net`'s own `Drop for TcpSocket` as
evidence and is ```text, because someone else's code cited in an
argument was never our example. The recipe now fails closed on both
shapes — no `test result:` line at all, and any `ignored` count — so
13 doctests were checked where 9 had been — and **20 today**, which is
the number's real job: it grows with the crate, so what it pins is the
recipe's honesty rather than a figure. `just test-doc` prints it, and the
gate is the fail-closed pair rather than any value.

And `test-no-default` **ran, printed `error:`, and exited zero**, for as
long as it had existed. Its four trailing `cargo clippy` lines are
unguarded under `set -uo pipefail` with no `-e`, so the recipe's status
was the last line's and every earlier failure was invisible; the CI job
calling it was green over three real dead-code errors under
`--no-default-features`. This is worse than the missing job above,
because it is the recipe people run before pushing. Both are the same
rule: **a check that cannot fail is not a check**, and the way to know
which kind you have is to break something on purpose and watch. Doing
that here also showed how easily the wrong break proves nothing — a
syntax error in an *example* fails the nextest step first, which *is*
guarded, so both editions of the recipe scored 101 and discriminated
nothing. `fuzz-smoke` shares the missing `-e` and is unaffected: every
cargo group there is chained with `&&` inside a subshell ending
`|| exit 1`.

**One mutation is applied by CI itself, on the only platform that can
kill it.** `docs/v03-acceptance.md` recorded the single survivor of the
UDP work — a hardcoded `ecn: true` is indistinguishable from the truth on
a Linux kernel, where both answers are `true` — and named what would
settle it: one run on macOS, where `quinn-udp`'s own backend documents
`IP_RECVTOS` as unavailable on dual-stack sockets, so the honest answer
is `false`. `just ecn-mutation-dies-on-macos` is that run, on every push:
it applies the mutation and requires the test to **fail**. Every step
fails closed, because a mutation harness that quietly stops mutating
reports a kill for a test nobody changed — the same defect this file
records for `test-doc` and `test-no-default`. On Linux the recipe reports
the mutant surviving and exits non-zero, which is what makes the macOS
pass mean anything.

Browser tests: those go through `wasm-pack test --headless
--chrome|--firefox` regardless, see the `browser` job.

## What's in the dependency graph

The first row of the table, as before, is verifiable directly in this
repository: `cargo tree -p http-ng-wasi -e normal --prefix none` contains no
`tokio` at all (27 unique crates total). The second and third rows, unlike
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
| `http-ng-h3` (v0.3) — **measured**, and the vertical-1 prediction of 55 crates was close | `[bytes, default, io-util, sync]` plus `tokio-util`, from `h3` and `h3-quinn`; **56 crates** in total; it was 58 when v0.4 moved `SeamRuntime` out into `http-ng-rt-quinn`, and two have left the graph since without anyone touching this crate — see the note under this table. Still no reactor from this crate's own dependencies — the reactor arrives with whichever `R` the caller supplies, and `R: Spawn` means it must have one |
| `http-ng-rt-quinn` — **measured**, v0.4 | `[default, sync]`, hyper's inert leaf and nothing else: **41 crates**, `quinn` + `quinn-proto` + `quinn-udp` + `ring` on top of `http-ng-rt`, and **no `h3`** — a crate that wants bare QUIC over this seam takes 41 rather than 56 and no opinion about HTTP |

**Every number in this table is a fact about a dependency *resolution*,
not about this code, and they drift.** Re-measured on 2026-08-19 against
the same commands: `http-ng-wasi` 28 → 27, `http-ng-h3` 58 → 56,
`http-ng-rt-quinn` 42 → 41, `http-ng-webtransport` 49 → 48,
`http-ng-dns-doh` 22 → 23, `http-ng-proto` on Linux 37 → 36. Nothing in
this workspace moved them — the same counts come out of the commit before
the week's work as out of the commit after it — and one went **up**, which
is what says it is upstream churn rather than a systematic miscount.

The three `http-ng-proto` rows that did **not** move are the ones worth
noticing: Windows 13, macOS 15 and `--no-default-features` 10 are all
unchanged, because those graphs are this crate's own. The rows that drifted
are the ones with a large third-party subtree.

So these are colour, and the load-bearing claims beside them are the
*absences* — no `tokio` in either wasm graph, no `h3` under
`http-ng-rt-quinn`, no reactor from `http-ng-native`'s `http2` feature.
Those are asserted on every push by `just graph`, which fails closed, and
they do not go stale. **A CI check pinning the counts would be the wrong
answer**: it would fail for an upstream release that broke nothing here,
and a check that cries wolf is silenced — the mirror of this file's rule
about a check that cannot fail.

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

**The owner has pulled the trigger: 0.1.0 is the first published
version.** Nothing bumps — every crate already said `0.1.0` — but it now
says so **once**, in `[workspace.package]`, where the previous thirty
copies were thirty chances to drift with no way to see the drift until a
crate published at the wrong number.

Everything that used to be *do not do this as tidying-up work* is now
either done or the owner's to time. What has **not** changed is the reason
the rule existed, and it is worth reading before the first `cargo publish`
rather than after: publishing is a promise not to break, and this
workspace has been breaking things weekly on purpose.

**Measured rather than recalled, over the last 31 commits alone**: six
public types took a change that would have been a major bump, and **not
one of them is `#[non_exhaustive]`** — `TcpOpts` and `TcpOptsSupport`
(6 fields to 10), `Timeouts` and `TimeoutSupport` (3 to 4, the `resolve`
bound), `Phase` (a fifth variant, so an exhaustive `match` outside this
workspace stops compiling), and `Connected::remote`, which became
`Option<SocketAddr>` for the Unix-socket work. Before that week
`TlsConnect` changed three times in one session (`reports_alpn`, the
`TlsIdentity` extraction, the 0-RTT slots), `Timer` gained `type Sleep`,
`TcpConnect` gained `APPLIES`, and `UdpBind` arrived from nothing.

So the freedom that made those changes cheap is what ends here, and naming
it is the point: a change to a public trait has cost a rebase in this
repository and nothing at all outside it, which is why every seam could be
chosen on its merits rather than on what was already promised. After the
first publish it costs a major version, and the honest options are the
ordinary ones — `#[non_exhaustive]` on the structs whose whole use is
`Struct { one: .., ..Default::default() }`, or a `0.x` series where
`0.2.0` is allowed to break. `H2Opts` and `TcpOpts` already carry the
argument for *not* adding the attribute, in their own doc comments, and it
is about ergonomics rather than about semver; the two now have to be
weighed against each other rather than only one of them stated.

What is still missing is enumerated at the end of
`docs/v01-acceptance.md`, `docs/v02-acceptance.md`,
`docs/v03-acceptance.md` and `docs/v04-acceptance.md`. Those lists are
themselves the thing to check first: several entries on them were built
after they were written.

The mechanics are in place and were measured, not assumed. All 30 crates
carry `description`, `license` and `repository`; inter-crate dependencies
carry `version` beside `path`, without which nothing here could be
published at all; `http-ng-rt-pair-check` is the only `publish = false`.
`cargo publish -p http-ng-core --dry-run` packages **and verifies** clean,
and `cargo package -p http-ng` correctly refuses, because its dependencies
are not in the index — which is what the order is for: **five waves over
29 crates**, `http-ng-core`/`-cache`/`-cookie`/`-idn` first and
`http-ng`/`http-ng-select` last. Every publishable crate carries
`[package.metadata.docs.rs] all-features = true`, because docs.rs builds
`default` and `default` here is empty or near-empty by design.

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

**Bare-metal microcontrollers are not reachable today, and that sentence
used to be broader than the truth.** A device with `std` — an esp-idf
target, say — already is: `http-ng-rt-embassy` implements `TcpConnect`
over a real `embassy-net` stack, with live scenarios over a TAP device in
CI, so `Native<Embassy, NoTls, IpLiteralOnly>` is the embedded transport
and no separate backend is owed. What is still out is `no_std`, and the
obstacle there is a dependency rather than a design:

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
  | default (`idn`), x86-64 Linux | **36** | `idna` + the ICU data crates |
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
deliberately does not do,
[`docs/v03-acceptance.md`](docs/v03-acceptance.md) for what v0.3 does,
does not, and has not checked, and
[`docs/v04-acceptance.md`](docs/v04-acceptance.md) for v0.4 — which is
the shortest of the four on purpose, because v0.4's arguments were
written down one document per topic as they were made, and it indexes
them rather than copying them. What it does carry, because no per-topic
document can, is the *deliberately not done* and *not checked* lists:
each of those knows only its own half.

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
one feature per coding** — `gzip` and `brotli` then, `deflate` and `zstd`
since (see the section on those two below), all off by default on `json`'s
precedent: a browser build would be linking decoders that cannot run
there. With any of them on, a client asks for the codings it can actually
reverse and reverses whatever the server chose — unless the transport says it did that already.
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
additive. 48 crates, `tokio` with no reactor, and `quinn` arrives with
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
same shape §8 argues for the WebSocket framing, and the crate is 41 crates
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
**dial** it would be the second author of: measured at 48 → 56 crates,
`ring` among them. `docs/rt-quinn-extraction.md` §5.

A session cannot share an `http-ng-h3` pooled connection, for three reasons
in increasing hardness: a second h3 client on one QUIC connection opens a
second control stream (`H3_STREAM_CREATION_ERROR`); extended CONNECT is
announced in SETTINGS at handshake and `http-ng-h3` announces it nowhere, so
making pooled connections capable would change what **every** build puts on
the wire; and `PoolKey` has no field to tell the two apart.

**Datagrams work, and the premise broke into four links each measured
separately.** quinn derives `max_datagram_frame_size` from a
`TransportConfig` default `http-ng-h3` never touches, so the connection
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
`http-ng-fetch`'s treatment of a `wasClean == false` close rather than
inventing a second vocabulary.

**It needed nothing spawned, and that disproves this workspace's own guess.**
`docs/v04-w2-webtransport.md` §6 said observing session end *"needs a driver
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
Six fields disagree today — measured, not taken from the design document,
whose two examples had both been fixed under it while it was being written.
It was seven until the seventh turned out not to be a disagreement at all:
`client_certs` was `true` from a constant in `http-ng-h3` and
`Capabilities::none()`'s `false` in `http-ng-native`, so **one** TLS backend
gave two answers depending on which stack was holding it, and the v0.4
table recorded the row as "same shape" as `full_duplex`. Both read
`TlsIdentity::presents_client_certs` now — a defaulted-false constant on
the seam the two connect traits share, `reports_alpn`'s shape — and the
same connector carrying a client certificate is reported by both members
and by the pair. Five take the weaker claim, `full_duplex` among them,
which is the same
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

### A response cache landed, and it is the counterpart `owns_cache` never had

`http-ng-cache` — RFC 9111 freshness, validation, `Vary` and the
directives on both sides — **sans-io and clockless**, exactly as
`http-ng-cookie` is, and a leaf: `bytes`, `http`, `thiserror`, and not
`http-ng-core`. `ClientBuilder::cache(HttpCache::new())` switches it on
behind `http-ng`'s `cache` feature, off by default. `Client` supplies the
`now` as `SystemTime::now()` for the reason the jar does — `Date`,
`Expires` and `Age` are calendar values and `Timer::Instant` is a
stopwatch with no epoch.

**It is a private cache**, a user agent's rather than a shared one, and
three rules turn on that: `private` is stored, `s-maxage` is not read at
all, and a response to an authenticated request is stored with the
credential in its `Selector` rather than refused — a narrowing of §3.5
that a private cache needs and the RFC does not require.

**`Capabilities::owns_cache` finally has a reader.** It had been a `bool`
set by one backend — `http-ng-fetch`, because the browser caches inside
`fetch()` — and branched on nowhere, which is the shape `proxy` and
`client_certs` were found in this same week. A client-side cache against
a transport reporting `true` is now an `UnsupportedCapability` at
`build()`, the arm that field's own doc comment had promised since v0.1.

Four things worth knowing before touching it. `Lookup` has **four**
answers, because *send it*, *send it with these fields added* and *do not
send it at all* are three instructions and an `Option` carries two. **A
validator alone is enough to store on**, and the absence of heuristic
freshness is load-bearing rather than a gap. **A `304` does not relabel
the stored bytes** — `Content-Encoding` is excluded from the update set,
which `http-ng`'s decompressor makes concrete. And `stale-while-
revalidate` is deliberately absent: it needs somewhere to run the
revalidation after the response has been handed over, and this client
does not spawn on a caller's behalf — the same sentence the h3 body pump
and the WebSocket keep-alive are written under.

**The wiring's own defect was found by a stack overflow three crates
away.** With the feature on, `http-ng-native`'s
`checkout_walks_past_a_dead_connection_to_a_live_one` aborted with
`SIGABRT` — a test that configures no cache at all. Measured: the
`execute` future is 4,232 bytes without the feature and 4,344 with it,
and that test needs between 1 and 2 MiB of stack without and between 2
and 2.5 MiB with, against a 2 MiB default. It is the only place in the
suite holding **two** whole client futures in one frame, through
`tokio::join!`, so it noticed first — and it noticed by aborting rather
than by failing anything. The two futures are boxed there now, and the
bound is stated where a reader will find it: `tests/future_size.rs`
asserts the future stays under 8 KiB, a ceiling with room rather than
today's number, because a test pinned to the current value gets relaxed
without thought. `cached::Cached::recorder` was already boxed for the
same reason with its own measurement recorded; this is that finding one
level up.

### `1xx` responses, and the third time hyper's `Send` shaped this crate

`Native::watching_1xx()`, and `Event::Informational` on the hooks seam —
because `Transport::execute` resolves exactly once and a `1xx` is not that
once. It carries `id`, `status` and `headers` and **no `version`**: the
connection's protocol was already reported by the `Connected` or `Reused`
that opened the exchange, and a third place to be wrong about one fact is
a third place to be wrong.

**The two protocols reach the same capability by routes that share
nothing.** HTTP/2 is `ResponseFuture::poll_informational`, a poll on the
same future that awaits the response, needing no bound at all. HTTP/1 is
`hyper::ext::on_informational`, whose callback must be
`Send + Sync + 'static` and is stored as `Arc<dyn .. + Send + Sync>` —
**the third time hyper's auto-trait requirements have shaped this
crate**, after the sealed `Http2ClientConnExec` that ruled out
`hyper/http2` in v0.2 and the `Rewind<Box<dyn Io + Send>>` inside
`hyper::upgrade::Upgraded` that ruled it out for the WebSocket work. Here
it collides with a property this workspace documents as supported: a hook
may hold an `Rc`.

What is different this time is that a pattern for absorbing it already
exists. The bound sits on the opt-in constructor and the field is a `fn`
pointer, which is `multiplexed()`'s shape exactly, so **no signature a
single-threaded hook meets gains a bound** and an `Rc`-holding hook gets
`E0277` on the line where it asked. One switch turns both protocols on,
because the capability reports the **floor** — a `true` that held on h2
alone would be a claim an HTTP/1 connection could not keep.

Three defects came out of the writing rather than the design, and the
middle one is the sharpest: **`Native::hooks` dropped the installer
pointer but carried the capability**, so `.watching_1xx().hooks(h)`
reported nothing while claiming `informational_1xx == true` — a
capability lying, which is worse than the silent downgrade it accompanies
because a caller can act on a capability. The h2 poll was also written
above the connection drive, asking for interim heads before the frames
carrying them had been read, under a comment arguing for the wrong
ordering; and the **shared** h2 path was wired and unreached, its
mutation surviving the whole suite until a fixture reached it.
`docs/informational-1xx.md`, including what is still unmeasured —
`Informational::id` is populated and never asserted.

### The pooled-reuse race has a third test sitting on it, and it was the premise

`a_pooled_connection_the_server_closed_while_idle_is_reported_stale` waits
for the *server* to have dropped its socket and then expects the second
request to find the connection dead at checkout. Under an oversubscribed
run it failed six times in sixty with `IncompleteMessage`, and the capture
says which point of the window it reached: `accepted=1`,
`closes_seen=[(1, Ended)]`, `connects=1` — no fresh connection, and the
connection's end reported from **inside the second exchange** rather than
from the checkout.

That is the far point of `docs/pooled-reuse-race.md`'s three, the one this
workspace documents as residual and deliberately unfixed — hyper wrote the
request and then read `EOF`, which is `Failed::Sent` and no retry. **The
client did exactly what is written down.**

What was wrong is the premise. `server_has_closed` establishes that the
peer dropped its socket; what the test needs is that *this client's
reactor* has processed the `FIN`'s readiness before `is_reusable` takes its
single non-suspending look, and those are different facts. Under load tokio
had not had its turn. A sleep after the wait is a guard on the premise, not
an assertion — and it is the same lever `Behaviour::close_delay` already
is, one field over.

Three flakes, three test-level causes, and one real defect (the h2 stream
limit below) — which is the ratio worth remembering before assuming a flake
is noise.

### Two more flakes in the same hunt, and neither was the client

The `-j96` hunt that found the h2 defect below turned up two more, and both
were the **test** measuring something the client does not promise. Worth
recording because the difference is only visible after capturing the
failure, and both first theories were wrong.

**`with_no_head_start_both_stacks_connect_and_exactly_one_request_is_sent`
— now `with_no_head_start_exactly_one_request_reaches_the_origin` — asserted
a clock wearing a counter's clothes.** Its `tcp_accepted >= 1`
says *the hedge ran*, and at a head start of zero the hedge is only
**started** — a QUIC arm that finishes first cancels it, possibly before
its `SYN` leaves. The captured failure carries the numbers: `body="h3"
quic_answered=1 tcp_accepted=0 elapsed=3.0ms`. The first fix was to widen
the wait from one second to ten on the theory that the fixture's thread was
starved; **measured, the rate was unchanged** — which is what proved the
connection never existed rather than arriving late. The assertion is gone
and the test is renamed for what it does claim; nothing is lost, because
the hedge running is asserted *causally* two tests down, against a QUIC
origin that cannot answer.

**`a_quic_arm_that_lost_the_race_teaches_the_memory` measured across a
boundary the client does not own.** `hop` takes a delta of the black hole's
datagram counter across `execute`, and the end of that future is not the
end of the abandoned QUIC arm's UDP. Measured: a hop whose hedge wins puts
**two** datagrams into the hole, and every captured failure showed hop 1
with one and hop 2 with the other. A `settled` guard between the hops fixes
it — 6 failures in 40 before, 0 in 40 after.

The second one also cost a wrong "decisive" reading first: the diagnostic
printed `two.quic_tried == 0` and that looked conclusive until the fixture
was read, where `quic_attempted` turns out never to move for a black hole.
**A counter that cannot move is not evidence**, and checking which counters
the fixture actually feeds is part of reading a capture.

### A stream opened on a guess, found by hunting a flake

`h2`'s `initial_max_send_streams` defaults to `usize::MAX`: until the
server's `SETTINGS` frame arrives a client may open as many streams as it
likes, and `SendRequest::poll_ready` — which *does* respect the counter
(`proto/streams/counts.rs`, `can_inc_num_send_streams`) — says yes to all
of them. So a caller firing a burst of concurrent requests at a fresh
multiplexed connection **races the frame that states the limit**, and a
server allowing fewer answers `RST_STREAM(REFUSED_STREAM)`.

**And this client could not repair it afterwards.** `send_request`
consumes the head, so `http2::exchange` can only report `Failed::Sent` and
`Native::run` will not retry — the weakness that module's own doc already
recorded against HTTP/1. RFC 9113 §8.7 says a `REFUSED_STREAM` reset means
the request was **never processed** and may be safely retried, which makes
the hard failure a wrong answer rather than a cautious one.

`Native` now asks for **one** stream until the peer has stated a number.
The cost is one round trip once per connection — the peer's SETTINGS
arrives in its first flight, and `counts.rs` overwrites the guess the
moment it does, even where the frame names no limit at all. It is
deliberately **not** an `H2Opts` field: every field there is `None` meaning
*whatever h2 chooses*, and this is a correctness choice, not a knob. A
caller who guessed high would be choosing a failure they cannot retry.

**How it was found is the part worth copying.** It was a flake —
`beyond_the_peers_stream_limit_requests_queue_on_one_connection` failing
once in a while under a full-workspace run. Sixty runs at `-j16` and
`-j28` were clean and proved nothing; **oversubscribing to `-j96`** turned
it into 4 failures in 40, with identical captures every time:
`Reset(StreamId(5), REFUSED_STREAM, Remote)`. Every run's whole output went
to its own file — the session that lost two earlier sightings lost them to
a `grep` in the pipeline.

**The test is deterministic now, not statistical.** `DelayFirstWrite` holds
the server's *first* write for 300 ms, which is the frame carrying
`MAX_CONCURRENT_STREAMS` — the first write and no other, since delaying
every write would also delay the `RST_STREAM`s the test is about. Without
the fix it fails three times in three with the same reset on stream 5; the
assertion is the server's high-water mark, a count, so a slow machine
changes nothing.

That makes four: `docs/v03-acceptance.md` records three timing-based
assertions in this workspace that turned out to be flakes, one of them
hiding a real defect. This is the fourth, and it was hiding one too.

### Unix-domain sockets, and the sibling trait that could not exist

`Native::unix_socket(path)` — curl's `--unix-socket`, for reaching a local
daemon that speaks HTTP over a socket rather than a port. The URI still
carries a host, because HTTP needs one for `Host:` and for the pool.

**`docs/competitive-gaps.md` expected a sibling of `TcpConnect` and there
cannot be one.** `Native`'s IO type *is* `TcpConnect::Stream`, so a second
trait would have to produce the same associated type — at which point it is
`TcpConnect` with an extra method. Putting `R: UnixConnect` on `Native`
would tax every runtime with no file descriptors, and the `fn`-pointer
trick that keeps `Spawn` off `Native`'s signature does not work here:
`spawn` returns `()` where this returns a future, and boxing it drops auto
traits (amendment C1).

So it is `TcpConnect::connect_unix`, a **defaulted method** whose default
is a refusal, beside `SUPPORTS_UNIX` defaulted to `false` — `reports_alpn`
and `applies_ech`'s shape: a constant defaulted to the understating value,
read by the layer above to decide whether to *ask*. Both shipped runtimes
compute it with `cfg!(unix)`, and each holds an enum internally
(`TokioIo`'s `Socket`, `http-ng-rt-smol`'s `SmolSocket`) because one
associated type must cover both.

**It replaces the whole resolve → discovery → Happy Eyeballs → connect
block, which is `Proxy`'s slot exactly** — and a proxy and a socket
together are a **refusal**, because both answer *where does this connection
go* and a precedence rule between them would be one nobody could guess.
The two orders are not symmetrical: `unix_socket` returns a `Result` and
refuses politely, `proxy` panics, because it changes `P` and cannot hand
back a `Result<Native<.., P2>, _>` without costing every caller who never
touches a socket a `?`. Said where each is.

**`Connected::remote` became `Option<SocketAddr>` for it, and that is the
sharper half.** There is no address, and a fabricated `0.0.0.0:0` would
give a hook a *wrong* answer where the absence gives it a missing one — the
argument `Head::version` already settled one event over. Emitting no
`Connected` at all was the alternative and is worse: the `Closed` that
follows would announce the end of a connection whose beginning was never
announced, which is the defect this file records about building one out of
`wasi:http`'s error codes.

No `TcpOpts` are applied, and that is not an omission: every field of it is
a TCP or IP option `AF_UNIX` does not have. `https://` still works — the
handshake is unchanged and the server name comes from the URI.

The socket path is in the pool key, sharing `proxy`'s field since at most
one can be set, and **that correctness is unobservable** — the second
mutation control here, for the same structural reason as the proxy's:
`unix_socket` is constant within one `Native`, so two requests through one
transport cannot disagree about it.

### `Capabilities` has two kinds of field, and one of them is not a gate

`docs/competitive-gaps.md` §7 asked whether `Capabilities::proxy` "should
have a reader at all" — a producer, no reader, and the `upgrade` deletion
sitting there as a precedent. The answer is that the question was the wrong
one: `proxy` is one of **eleven** fields nothing branches on, and six of
them (`proxy`, `client_certs`, `tls_config`, `early_data`,
`connection_reuse`, `cancel_on_drop`) had no doc comment either.

**A gate** guards a setting a caller made on the `Client`, and `build()`
refuses when the transport cannot honour it — `redirects`,
`response_decompression`, `owns_cookie_jar`, `owns_cache`,
`version_select`, `timeouts`, `forbidden_request_headers`. A gate with no
branch is the *silently ignored setting* defect, and this project has
closed four of them.

**A report** states a fact about the transport, and nothing at the client
level could refuse it, because the setting it describes is configured *on
the transport*. `proxy` is a report: what it would guard is
`Native::proxy`, which is on the object that answers the question. So it
will never be a gate, and that is structural rather than an omission.

**A report is not a dead field**, which is where `upgrade` differs: those
four variants encoded a distinction with one reachable side, where a report
has both values reachable and answers a question only it can answer — *will
my requests go through a proxy* is a diagnostic's question.

`informational_1xx` is the one gate in the other direction: no `Client`
setting turns it on, and what it guards is a **claim** — `Native::hooks`
clears it, because a transport reporting `true` while reporting nothing is
a capability that lies.

The classification is enforced rather than described.
`every_capability_is_a_gate_or_a_report` lives in `http-ng-core`, because
`#[non_exhaustive]` allows an exhaustive destructure only inside the
defining crate (amendment C6) — so a field added later is a compile error
in **two** places until somebody decides which kind it is. Checked by
adding one and watching both fail.

### The response head is bounded on both protocols, and one setter can refuse

`Native::h1_opts(H1Opts { max_headers, max_buf_size })`, beside
`H2Opts::max_header_list_size`. **A response head is the one part of a
response a client must buffer whole before it can act on any of it**, so it
is the one part a hostile server can make expensive without ever sending a
body — and neither half is complete without the other, because a transport
that negotiates ALPN speaks whichever protocol the server picked.

Two bounds and not one: a count alone does not bound the bytes, since a
server can send one field with a megabyte of value and stay under any
count.

**`h1_opts` is fallible where `h2_opts` is not, and the difference is who
would refuse the value.** A `SETTINGS` frame is written by this crate and
there is nobody to say no; `max_buf_size` is handed to hyper, which
`assert!`s below 8192. A caller's number reaching a `panic!` inside a
connect is not a refusal they can act on, so it is checked at the setter
and named — and the boundary itself is accepted, which a check written
`<=` would not do.

The failures come back as `ErrorKind::Connect` rather than `Body`, and
that is hyper's classification rather than a judgement made here: a head
that cannot be parsed means nothing usable came off the connection, so
there is no response for a body error to attach to.

### More than one proxy, chosen by scheme, first match wins

`Native::and_proxy` appends to a list and `Proxy::only_for(ProxyScheme)`
restricts an entry to `http://` or `https://`. The case is the ordinary
corporate one — an `HTTP_PROXY` and an `HTTPS_PROXY` at different hosts —
which one `Option` could not hold.

**First-match-wins, not most-specific-wins.** A precedence rule has to be
learned; an ordered list is read off the builder chain that wrote it. So
an unrestricted proxy placed first shadows a narrower one after it, and
that is asserted rather than warned about — the same reason `bypass`
refuses to invent the precedence that every `NO_PROXY` implementation
disagrees about.

**One `P` per transport, stated rather than worked around.** A caller
wanting SOCKS5 for `https` and an HTTP proxy for `http` cannot say so;
lifting it means erasing `P`, and erasing `P` erases the IO with it, which
is the objection that disqualified `Box<dyn ProxyProtocol>` in the first
place. `and_proxy` therefore does not change `P`, unlike `proxy` — and it
is uncallable before `proxy` for free rather than by a check, since
`NoProxy` is an empty enum and there is no `Proxy<NoProxy>` to pass.

**A bypass belongs to the proxy that carries it**, so a bypassed host
falls through to the next proxy and goes direct only when the list runs
out. The global `NO_PROXY` reading is worse *because* the list exists: a
host bypassed on an `https`-only proxy would take an `http://` request
direct, past an `http` proxy that was never in the running and never
mentioned it. With one proxy the two rules coincide exactly, which is why
this was invisible until there could be two. The first test written for it
asserted the global rule and failed — correctly.

**The pool key asks the list rather than taking the first entry, and that
correctness is unobservable** — this change's mutation control. `choose`
is a pure function of `(use_tls, host, port)` and `PoolKey` already
carries all three, so two requests that agree on the key cannot disagree
on the proxy. It is written correctly anyway for the reason the `proxy`
field is in `PoolKey` at all: a pool shared between transports ends the
coincidence.

**SOCKS4 and SOCKS4a are one type**, and that is the wire's decision rather
than ours: 4a is 4's own extension, signalled inside a SOCKS4 request by a
`DSTIP` of `0.0.0.x` — invalid as an address, therefore meaning *a hostname
follows the userid*. There is no version byte to tell them apart and no
handshake to negotiate in, so a second type would be a choice nobody can
make. The hostname form goes out always, because that is what
`ProxyProtocol::tunnel` is handed: resolving locally would leak the DNS a
proxy user is often there to hide, which is the same reason a proxy is not
a `TcpConnect` decorator.

`Socks5` is the answer unless a server forces otherwise, for the protocol's
own reasons: **no IPv6**, the address field being four bytes, and **no
authentication**, only a `USERID` the proxy may check against an identd.
The `USERID` is deliberately not marked sensitive — marking it would claim
a secrecy the protocol does not have. Two details every implementation gets
wrong once, both pinned: the reply's version byte is **zero**, not four,
and the grant is `CD = 90`, not `0`.

### Four more socket options, and `APPLIES` stopped being a constant

`TcpOpts` gains `bind_device`, `keepalive_interval`, `keepalive_retries`
and `user_timeout`, with the matching `TcpOptsSupport` bools — the
field-per-field mirror exists precisely so the error can name the option a
caller set, so growing it is the designed-for change.

**The consequence worth knowing is that `Tokio::APPLIES` and
`Smol::APPLIES` are no longer `TcpOptsSupport::ALL`.** `SO_BINDTODEVICE`
is Linux/Android/Fuchsia, `TCP_USER_TIMEOUT` those plus Cygwin, and
`TcpKeepalive::with_retries` is missing on three more. A constant claiming
all of them everywhere would be a capability that lies on macOS and
Windows, so `APPLIES` is `cfg!`-computed now. `ALL` still means *every
field*; it is simply no longer a value any real runtime can claim on every
target it builds for. The direction matters: an understated `APPLIES`
costs a caller a named `Unsupported` error, an overstated one costs them an
option silently not applied.

**Keepalive is one setting in three parts and the field names do not say
so**, which is why the type says it: `set_tcp_keepalive` switches
`SO_KEEPALIVE` on, so setting *any* of the three enables it and each part
left `None` keeps the OS's value. A caller who sets only the interval has
switched keepalive on with the OS's idle time — asserted, because it reads
as a surprise otherwise.

**`bind_device` is not `local_address` renamed.** An address binds the
*source address* and the kernel still routes by its table, so a request can
leave through a different interface that happens to hold the same address.
This binds the interface, which is what a caller on a multi-homed host or
inside a VRF means. Its test asserts an outcome consistent with the claim
rather than success: `SO_BINDTODEVICE` needs `CAP_NET_RAW`, so either the
socket reports the interface back or the connect fails `EPERM` — what must
never happen is a silent success with nothing bound.

**`user_timeout` is the one that catches a peer that vanished
mid-transfer**, where keepalive catches only an idle one: probes go out
when nothing is in flight, so a connection with unacknowledged data sits in
retransmission for minutes with keepalive never firing. It overlaps
`Timeouts::between_bytes` without replacing it — the kernel's, on a socket
rather than an exchange, and the only one of the two a build with no
`Client` above it can reach.

**Two test defects surfaced, both from names rather than behaviour.**
`each_unappliable_option_is_named_on_its_own` compared the error's
*message* against every other option name as a substring — which worked
while no two names shared a prefix, and reported that a withheld
`keepalive_interval` had also named `keepalive`, which the message never
did. It compares `names()` as data now, which is what its neighbour four
lines down already did. And a fixture called `all_six_set` with a
`[&str; 6]` beside it: a count in a name and a length in a type are both
things to remember when a struct grows, and it grew.

### The browser's own `fetch` members, and the one that is refused

`Fetch::opts(FetchOpts { .. })` — `mode`, `credentials`, `cache` and
`referrerPolicy`, four `Option`s applied to `RequestInit`, `None` meaning
*leave it to the browser*. Until this, `to_web_request` set method,
headers, body, signal and `duplex` and nothing else, so a browser caller
could not send `credentials: "include"` — what a cross-origin
authenticated request needs. Two independent browser clients expose all of
it (reqwest's wasm build and `gloo-net`), which is what makes it an absence
rather than a knob nobody wants.

**On `Fetch`, not on `Transport`**, because three of five backends have no
such concept — the rule that put `WebSocketConnect` in its own trait. And
**not a request extension**, for the reason `Prefetch::prepare` refuses to
take an HTTPS record from a caller: an extension is a channel any code able
to build a request can write to, and `credentials: "include"` is a decision
about which origins receive the user's cookies. Four `web-sys` feature
names and no crate — the graph is 32 either way, measured.

**`redirect` is refused, and it is the interesting one, because it is a
capability that would lie.** `fetch`'s `redirect: "manual"` does not hand
back a `3xx` a caller can act on: a cross-origin response comes back
*opaque-redirect* — status `0`, empty header list, null body, no readable
`Location`. So `Capabilities::redirects` could not honestly move from
`Internal` to `Transparent`, and claiming otherwise would promise `Client`
a policy it could act on for exactly the case where redirects matter.
`http-ng-urlsession` is the backend that genuinely reports `Transparent` —
its delegate can refuse a hop — and the asymmetry is why both are worth
having. `redirect: "error"` is a third thing, *fail rather than follow*,
which is `RedirectPolicy::None` with the answer thrown away.

**Asserted without a network, deliberately.** `web_sys::Request` has a
getter for each of the four, so the browser answers whether the member
arrived. What a headless run cannot honestly arrange is what the members
are *for* — `include` against a cross-origin server that sets a cookie,
`no-cors` producing an opaque response — both needing a second origin.
What this crate is responsible for is that the member reaches the request.
The control is the default arm: without it, a test setting `Include` and
reading `Include` back would pass for a transport that hard-coded it.

**And the browser suite had not compiled since 2026-08-16.**
`Event::Informational` landed with the `1xx` work and `http-ng-fetch`'s
`tests/hooks.rs` never gained an arm, so every browser binary in that
crate failed to build through six merges that were each green on
`cargo nextest run --workspace --all-features` — which does not build for
`wasm32-unknown-unknown`, where `just test-browsers` is its own CI job.
`Event` not being `#[non_exhaustive]` is what turned a new variant into a
compile error rather than silence: **the design worked and the running of
it did not.** The cheap check that would have caught it needs no browser at
all — `cargo test -p http-ng-fetch --target wasm32-unknown-unknown
--no-run`.

### `error_for_status`, and the defect it found one line away

`Response::error_for_status()` and `Collected::error_for_status()` —
`Ok(self)` below `400`, an `ErrorKind::Status` error at or above it,
carrying an `UnexpectedStatus` with the status and the URL.

Three decisions. **On both types**, because they are used at different
moments: before the body for a caller who will not read it, after for one
who wants the server's error text and only then decides. **It takes
`self`** where nothing else on these types does — the whole point is that a
caller writing `?` is choosing to stop having a response, and a `&self`
form would leave the failed one in hand. And **`3xx` is `Ok`**, because
reaching one means the redirect policy already decided to hand it back;
`RedirectPolicy::None`'s own doc says a `3xx` is the caller's answer rather
than a failure to reach one, and erroring here would overrule that from two
layers up.

**Not a client setting**, which reqwest, ureq and curl all agree on: `404`
is a normal answer for about half the requests ever made, and a
client-wide *treat every 4xx as an error* would turn a HEAD probe or a
conditional GET into a failure. The caller knows which of their requests
has a status they can act on.

**And it found that `Response::url()` was answering the wrong question.**
It reported the URL the caller *asked for*, not the one that answered —
undocumented, untested, and different exactly when a redirect was
followed. The name reads *where did this come from*; the value said *where
did you send this*, which the caller already knows, having typed it. It is
the last hop now, pinned in `tests/redirect.rs` as well as beside the
error, with the no-redirect control that says the value is not simply *the
second request*. The defect was invisible until an error carried the value
somewhere a caller would read it.

### A separate bound on resolution, and it bounds something that is not a phase

`Timeouts::resolve`, `TimeoutSupport::resolve` and `Phase::Resolve`, in one
change — the rule that kept `connect` off `http-ng-h3` until v0.4 W1 and
`first_byte`/`between_bytes` off native until v0.2 W4.

**What it bounds took working out.** Happy Eyeballs interleaves resolution
with connecting on purpose: the resolver is a `Stream`, and `connect`
starts both families at the top precisely so `attempt` can dial the first
address while the rest are still arriving. So there is no instant at which
*resolution finished*, and a bound on one would have nothing to attach to.
What is bounded is the wait for the **first address from either family** —
which is the failure a caller cannot otherwise diagnose, since a resolver
that hangs and an origin that will not answer are the same
`Timeout(Connect)` today.

**Nothing is serialised by it.** `attempt` cannot connect before an address
exists, so the gate waits for what the next line would wait for anyway;
what changes is the error, not the schedule.

Three decisions came out of the shape:

- **It does not apply where the connection does not need the resolver.** An
  HTTPS record with address hints gives the connector somewhere to go with
  no answer at all, so waiting for one would bound a query whose result is
  off the path — the same reasoning that keeps discovery from running for
  an IP literal. That skip was first written as a mutation **control** and
  is a **test**: RFC 9460 §7.3 hints are ordinary, so it was reachable and
  untested, which this file's own rule calls a gap. It is asserted
  causally, on the fixture seeing a connection at all, because after a
  hinted attempt fails the connector legitimately falls back to the
  resolver and the exchange's *ending* is not the subject.
- **It stops waiting when both families are done**, so a name that does not
  exist stays `ErrorKind::Resolve` rather than becoming a timeout —
  `drive`'s `ResolveErrors` has per-family causes this gate does not, and
  replacing a precise diagnosis with a vague one is what the feature exists
  to undo.
- **`false` on both ambient backends, and it is the first field whose
  `TimeoutSupport` bool was ever honestly `false` on arrival.**
  `wasi:http` 0.3's `request-options` carries three timeouts and nothing
  for resolution — the host resolves — and `fetch` collapses everything
  into one `AbortController`. `http-ng-dns-doh` sets `None` for a third
  reason: it is the resolver's own client, so a `resolve` bound there would
  bound resolving the name of the thing that resolves names.

**Overlapping budgets are the caller's to reconcile.** Nothing subtracts
`resolve` from `connect`: a resolver that answered in 10 ms has not spent
any of the connect budget in any sense a connector can see, and inventing
an arithmetic between them would make one field's meaning depend on the
other's.

### Digest authentication, and it is the `425` branch with a computed header

`RequestBuilder::digest_auth(user, password)` — RFC 7616, behind the
`digest-auth` feature, off by default. No pure-Rust client ships it;
`xh`, built on reqwest, wrote its own rather than go without, which is the
evidence the absence is felt rather than theoretical.

**The shape was already here.** Digest is a challenge/response over a
`401`, and `Client::run` owns exactly that for `425 Too Early`: a
status-code test, one resend, inside the same `total` budget, gated on
`RequestBody::retry_kind()`. The branch sits beside it and differs by one
thing — the resend carries a header computed from what came back. Nothing
spawns, no clock, no `Send` bound.

**The arithmetic is checked against RFC 7616 §3.9's own printed answers**,
copied out of the document, which is why `digest::answer` takes `cnonce`
as a parameter rather than drawing it: a hash function checked against its
own output is green for any self-consistent mistake about what digest is.
Both of the RFC's examples are there, SHA-256 and MD5 over identical
inputs — the pair is what says the algorithm is *used* rather than echoed
in the header — plus RFC 2617's, for the `qop`-less form §3.4.1 keeps.

**Three decisions the building made that the plan had not asked.**
The credentials **do not cross an origin**, by the rule already stripping
`Authorization`: a password-derived secret must not reach a server the
caller never named, which is a stronger case than the one `AllowEarlyData`
was taken off a hop for. They are **not in `http::Extensions`**, because
extensions reach `Transport::execute` and a password there would be
readable by any transport, including one this workspace did not write — so
they travel as an argument to `execute_with` instead. And what goes in
`uri=` is the **request-target**, not the URL: §3.4.2 hashes what goes on
the request line, and a full URL would give the server a different `A2`
and a second `401` nobody could explain.

**Nine crates, measured, which is why they are taken rather than written.**
`md-5` + `sha2` pull `digest`, `block-buffer`, `crypto-common`,
`hybrid-array`, `typenum`, `cfg-if`, `cpufeatures` — eight net in this
graph. That is a departure from the rule that removed `url` and hand-wrote
base64, and the line between them is whether a wrong answer is *visible*:
base64 is twenty lines and fails loudly everywhere, where a hash is two
hundred whose defects are silent and whose vectors nobody re-derives.
RustCrypto's are audited, `no_std`, build-script-free and build for both
wasm targets — the same shortlist `ruzstd` was chosen from.

**MD5 is here and RFC 7616 §5.2 deprecates it**, which is not an
oversight: a client supporting only SHA-256 would fail against most servers
that speak digest at all. What this does instead is **prefer** the
strongest algorithm offered — across header lines, since a server sending
SHA-256 and MD5 as two `WWW-Authenticate` values is ordinary and a client
taking the first would answer MD5 to a server that offered better. The
client never chooses the algorithm; the server does.

Two absences with their reasons. **`auth-int`** hashes the request body
into `A2`, which cannot be done for a `Streaming` body without buffering
it — so a server offering it *alone* gets a named refusal rather than an
`auth` response it will reject with a `401` nobody can diagnose. And
**there is no nonce cache**, so every request pays one `401` round trip;
removing that needs per-origin state with a lifetime nobody states, which
is the question that made a cache dishonest for SVCB records and honest for
`Alt-Svc`.

**`cnonce` is 128 bits from the OS and its failure path is the opposite of
`sse.rs`'s.** A failed draw there degrades to un-jittered backoff, slower
and safe; here a fixed cnonce is the one value an attacker would choose, so
a failed draw falls back to a heap address — worse entropy, still not a
constant. The rule this file already records: *a degraded value is only
acceptable when the degradation has a direction.*

### HTTP/2's settings frame is tunable, and three of reqwest's eight knobs still are not

`Native::h2_opts(H2Opts { .. })` — the stream window, the connection
window, `max_frame_size` and `max_header_list_size`, four `Option`s
forwarded to `h2::client::Builder`. `None` means *whatever `h2` chooses*,
which is `TcpOpts`' rule: a value set here goes on the wire, so a default
of ours would change what a caller who asked for nothing announces to
every server.

**The window is the one that motivates the rest, and the arithmetic is not
ours.** RFC 9113 §6.9.2 fixes the default at 65 535 bytes and a peer may
have at most a window in flight, so the ceiling is `window / RTT` however
much bandwidth there is — about 5 Mbit/s over a 100 ms round trip. Raising
the stream window alone changes nothing where streams share a connection,
because the connection window is still 65 535; that is a test rather than a
sentence, since a caller reading only the field name would set one and
measure nothing.

**Infallible, unlike `tcp_opts` beside it**, and the difference is who
applies the value: a socket option is applied by a runtime that may not
have it, so `TcpOpts` needs a refusal and a per-field support mirror, where
a `SETTINGS` frame is written by this crate and there is nobody to say no.
Nothing in `Capabilities` moves either — a window size is not a capability.

**Nothing is timed, deliberately.** A throughput measurement on loopback
would say almost nothing: the window bounds bytes *in flight*, and with a
round trip near zero a sender refills it as fast as it drains — which is
exactly why the default only bites on a long fat pipe. What is observable
without a network is the setting at the peer that must obey it, so an
`h2::server` reports what capacity its `SendStream` was granted, and the
frame size is read as *behaviour* — one 512 KiB write cut into DATA frames
at the client's limit — because `h2::server::Connection` exposes no
accessor for it.

Two numbers had to be read rather than guessed. `SendStream::capacity` is
bounded by the sender's own buffer as well as by the peer's window, and
h2's `DEFAULT_MAX_SEND_BUFFER_SIZE` is 400 KiB — a first draft asked for a
megabyte and was told 409 600, which discriminates but does not measure. So
the test asks for 256 KiB, under that buffer, where the window is the only
thing binding. And `max_header_list_size` is enforced by `h2` on **receive**
as well as advertised (`codec/framed_read.rs`), which is what gives the
fourth field a local observable instead of a forwarding line nobody checks.

**Three of reqwest's eight are still absent, each for its own reason.** An
adaptive window is hyper's, computed from measured RTT; `h2` has none, so
it would be ours to write and a wrong estimator is worse than an honest
constant. Keepalive pings need somebody polling an idle connection, which
here means `multiplexed()` and its `Spawn` bound — a property of the driver
rather than of the settings frame. And `max_concurrent_streams` governs
streams the *server* opens, i.e. server push, which `h2` does not enable
and RFC 9113 §8.4 deprecates: a knob with no subject.

`H2Opts` is deliberately **not** `#[non_exhaustive]`, copying `TcpOpts`:
its whole use is `H2Opts { one: Some(n), ..Default::default() }`, which the
attribute forbids from outside the crate, leaving per-field setters that
exist only to work around it.

### A caller gets a say over each redirect hop, after the policy rather than inside it

`ClientBuilder::redirect_predicate(|hop| ..)` -> `RedirectVerdict::{Follow,
Stop, Refuse}`. `RedirectPolicy` answers *how many* and this answers
*whether this one* — no hop to a private address, none to another host,
none off `https`.

**It is not a third `RedirectPolicy` variant, and that was the obvious
shape.** Two things kill it. `RedirectPolicy` lives in `http-ng-proto`,
which is sans-io and clockless, and `redirect::decide` is a pure function
of six values — a closure variant makes it *pure except for whatever the
caller passed*. And `RedirectPolicy` is `Copy + PartialEq + Eq`, read out
of a request's extensions with `.copied()`; a boxed closure ends all three.

So `decide` is untouched and the predicate is asked **after** it, only
about a hop it already approved — which is the better order rather than a
concession, because what a predicate wants to see is `decide`'s *output*:
the resolved target (a relative `Location` is already absolute), the
method after any downgrade to `GET`, and `cross_origin`. That last is
`Follow::strip_sensitive` handed over rather than recomputed, so a
predicate refusing cross-origin hops and the client dropping
`Authorization` cannot disagree about what an origin is.

**Three verdicts, because two would lose the distinction at the one place
it matters.** `redirect.rs` already states the rule — *"do not follow" is a
`Stop`, not an error: the 3xx is the caller's answer* — and a predicate
that could only `Stop` would make an SSRF guard hand back a `3xx` the
caller must then remember to check. A caller who forgets gets a silent
success where they asked for a refusal, which is this file's *capability
that lies* one level up.

Two more decisions. **`Fn`, not `FnMut`**: the closure is shared by every
clone of the client and every request in flight, so `FnMut` means a lock
taken on every hop of every request for the sake of predicates that mostly
hold no state. And **no per-request form**, unlike `RedirectPolicy`: a
per-request setting travels in `http::Extensions`, and `AllowEarlyData` is
the type that made *may an extension cross an origin* a live question with
a real answer — where a predicate is a rule about where *this client* may
be sent.

The `Send + Sync` is amendment C12's third site and deliberately not a new
amendment: same argument, a value the caller owns reaching `Client` by
erasure rather than by a type parameter, bound on the opt-in call and
nowhere else. C10's rule against reusing an amendment by gesture is about
bounds demanded by *someone else's* trait, where the argument turns on
which external contract is being satisfied.

### `text()` learned a charset — as a second method, not a smarter first one

`Collected::text_with_charset`, behind the `charset` feature, off by
default: `encoding_rs` is over a megabyte of conversion tables and a
build that only ever meets UTF-8 has no use for them. The name is the one
reqwest and ureq both independently chose.

**What decides the shape is that `text()` must not change meaning with a
feature.** Cargo unifies features across a graph, so a charset-aware
`text()` would answer differently depending on what an unrelated crate
switched on — and the difference is silent: `windows-1251` bytes come
back as plausible mojibake rather than as the error they are today. Same
hazard as the `Capabilities` floor rule, one layer up. So it is a
separate method and the caller says so at the call site.

Four answers and each is a decision. **No `charset` parameter is UTF-8** —
RFC 7231 removed RFC 2616's ISO-8859-1 default, and content sniffing is a
browser's job done against a security model this type does not have. **An
unknown label is a typed error naming it**, never a quiet fall back to
UTF-8, which would turn *the server said something we did not understand*
into mojibake with nothing to show for it. **Malformed bytes are an error
and not U+FFFD**, because `text()` refuses invalid UTF-8 rather than
patching it, and two policies under one name is worse than either; a
caller who wants the lossy answer has `bytes()`. And **a byte order mark
overrides the declared label** — the Encoding Standard's rule, inherited
from `encoding_rs::Encoding::decode` and pinned rather than assumed.

The `charset` parameter is read here rather than by a `mime` crate, for
the reason `url` was removed and base64 is twenty lines in
`http-ng-proto`. It splits on `;` **outside quotes**, with backslash
escapes honoured, because `boundary="a;charset=utf-8;b"` is a header a
server can send and a naive `split(';')` reads a parameter out of it. An
escaped quote is reachable from a server too, so it is a test rather than
a mutation control — the rule this file recorded one section up, applied
before it had to be.

### Two more codings, and both premises for refusing them were wrong

`deflate` and `zstd`, behind features of their own, off by default like
`gzip` and `brotli` beside them. `decompress.rs` had refused both in
writing, and the reversal is worth more than the codings.

**`deflate` was refused because "a client must not advertise a coding it
may guess wrong about".** RFC 9110 §8.4.1.2 specifies zlib and its own
Note records that a long tail of servers sends the raw RFC 1951 stream
instead — so the token really is ambiguous. What was wrong is the
assumption that the guess must be made *after* a failure, which is how
curl does it: `lib/content_encoding.c` tries zlib, and on `Z_DATA_ERROR`
calls `inflateReset2(z, -MAX_WBITS)` and replays the buffer — **but only
while no output has been produced yet**, a rule with a window. The
question is answered here **from the first two bytes, before any output
exists**, and it is not probability: RFC 1950's `CM == 8` cannot open a
conformant raw stream, because RFC 1951 §3.2.3 packs `BFINAL` into bit 0
and `BTYPE` into bits 1-2, so a low nibble of 8 is a stored block with the
padding bit §3.2.4 tells encoders to zero. The `% 31` check is a second,
independent one. **It costs no crate at all** — `flate2` was already here
for `gzip`, and RFC 9110's `deflate` is the same RFC 1951 stream under a
different wrapper.

**`zstd` was refused as "a third dependency for a coding no server sends
unasked".** It is two crates, `ruzstd` + `twox-hash`, and it is a decoder
-first pure-Rust crate for the reason `flate2`'s `rust_backend` is chosen
over `zlib-ng`: this crate also builds for `wasm32-unknown-unknown` and
`wasm32-wasip2`, where a C build script is not a dependency but a wall.

Three things about zstd are ours rather than the library's. **The window
is capped at 8 MB** — RFC 8878 §3.1.1.1.2's recommended interoperability
ceiling and the number Chrome settled on, against `ruzstd`'s own 100 MB
default — because a frame declares its own `Window_Size` and `Limited`
structurally cannot reach it: `Limited` counts bytes *yielded* and a
window is allocated before the first byte is yielded. **The frame's XXH64
content checksum is compared**, which `ruzstd` does not do: it exposes
`get_checksum_from_data()` and `get_calculated_checksum()` and compares
them nowhere. And **concatenated frames are one body** (RFC 8878 §3.1),
which neither `ruzstd` entry point crosses on its own.

**The `flate2` writers could not be used, and a wire test is what found
it.** `write::ZlibDecoder::try_finish` calls `zio::finish`, which runs the
decompressor until it stops producing and returns `Ok(())` **without ever
asking whether the stream ended** (flate2 1.1, `src/zio.rs:173`) — so a
truncated body reached the caller as a complete, shorter document, the
exact defect the trailer checks on the other three codings exist against.
`flate2::Decompress` answers `Status::StreamEnd`, and `Decompress::new(
zlib_header)` is the sniff's switch in one type instead of two.

Two method notes, both this file's recurring lessons landing again.
`tests/compression.rs` already warns that a truncation under
`Content-Length` is reported by the *transport*, so the decoder's
end-of-stream check is never reached — the first draft of the new tests
was written that way and passed with `ErrorKind::Body` over a decoder that
had no check at all. And the trailing-bytes refusal was written as a
mutation **control** on the reasoning that no well-formed body has bytes
after the stream; a *server* can send them, so it was reachable and
untested — a gap rather than a control. It has a test, and the control is
now the `Sniffing` arm `push` documents as unreachable, verified by
replacing it with a `panic!` and running the suite.

**One check that could not fail was fixed with them.** `just features`
runs `cargo hack --each-feature`, which builds each feature *alone* — and
`decompress.rs`'s `#[cfg]` shape is about the **combinations**: the
wildcard arm catching "whichever codings this build has no decoder for" is
`not(all(..))` of four and the exhaustive arm is `not(any(..))` of four,
so exactly one of the sixteen sets is each one's boundary. The recipe now
runs the powerset over the four as well, and fails closed on either half.

### Four crates this file did not name, and what each is for

Recorded because `docs/competitive-gaps.md` found them missing from here
while two of them are still listed in `docs/v01-acceptance.md` as
deliberately not done — a list that was updated for its DoH half and not
for these.

- **`http-ng-mock`** — `MockTransport`, behind `http-ng`'s `test-util`
  feature. It is how every capability refusal in this workspace is tested,
  because "a jar against a jar-owning backend is refused at `build()`" is
  a fact about a type that never sends anything.
- **`http-ng-tower`** — the `tower::Service` adapter, so this client fits
  a stack that already speaks that vocabulary.
- **`http-ng-dns-hickory`** — the third `Resolve` backend, beside the
  system resolver and DoH.
- **`http-ng-rt-embassy`** — a `TcpConnect`/`Timer` runtime over
  `embassy-net`, which is what makes the embedded target reachable at all;
  see the `std` paragraph above.

### A fifth backend: Apple's `URLSession`, and the three things it refuses to take

`http-ng-urlsession` puts `URLSession` behind `Transport` — the fourth
**ambient** backend, owning no connection of its own, after `http-ng-wasi`
and `http-ng-fetch`. It exists for the list a userspace stack cannot reach
on an Apple platform: per-app VPN, the system proxy and its PAC, and
background transfer.

**That list said "enterprise roots pushed by MDM" first, and that was
wrong.** `rustls-platform-verifier` 0.7.0 — which `DefaultTransport`
already uses — reaches them: its Apple path builds the evaluation with
`SecTrust::create_with_certificates` and *deliberately avoids* narrowing
the anchors, calling `set_trust_anchor_certificates_only(false)` after any
extra roots precisely so the system's own are kept
(`src/verification/apple.rs:130-179`, read). So MDM roots were never a
reason to reach for this backend, and the sentence stood for about an
hour before `docs/competitive-gaps.md` caught it. The other three items
stand, and so does the redirect argument below, which is the stronger one
anyway. That is a fact about the
device rather than a preference, which is `http-ng-tls-native-tls`'s
argument one seam over.

**What it refuses to take from the OS is the decision worth knowing.**
`URLSession` will keep cookies, a response cache and a redirect policy for
you, and this turns all three off. None of them is in the list above; all
three are portable behaviour this workspace already implements once; and
leaving them on would make this the second backend reporting
`owns_cookie_jar` and `owns_cache`, so a caller porting from
`http-ng-native` would lose two features by changing one line.

**Redirects are the sharpest, and this backend is stronger than the
browser one.** `URLSession` lets a delegate refuse a redirect and a
browser does not — so this reports `RedirectSupport::Transparent` where
`http-ng-fetch` must report `Internal`, and `Client`'s hop limit and its
`Authorization` stripping across origins work here and cannot there.
Measured rather than read off Apple's documentation: a server that would
have answered a second request receives exactly one.

**Amendment C11 is a new kind of unsafe exemption.** The three before it
each cover a site with its own argument; this crate is an FFI boundary
where `unsafe` is the medium. It follows the policy rather than relaxing
it — a marker on every site, files listed by name — and objc2 0.6 needed
far less than expected: 23 sites became 11, and the body module has none.

**And the reason the Mac mattered.** `cargo check --target
aarch64-apple-darwin` is clean on a Linux host — which is worth knowing,
because it means the shape can be kept honest without Apple hardware — and
every network test hung on a real machine. One delegate per session was
one queue for every task, so `execute` polled a channel nothing would push
to; the signature said so, taking a `_shared` it ignored. A type-check
cannot see an argument that is merely unused. Each task carries its own
delegate now, and the four live tests are green on macOS 27.

### WebTransport: many sessions on one connection, and a `GOAWAY` nobody can see

The two items this crate had recorded as deliberately not done, taken up
together — and they came out opposite ways.

**More than one session per connection works, and the blocker recorded
for it was not the true one.** `PoolKey` is `http-ng-h3`'s problem;
`http-ng-webtransport` takes its `quinn::Connection` from outside, so
what actually binds is the peer's `SETTINGS_WEBTRANSPORT_MAX_SESSIONS`.
`Session::open_session` opens a sibling on the same connection, the limit
is **read off the SETTINGS frame** rather than assumed, a slot returns
when a session is dropped, and exceeding it is a typed `TooManySessions`.
Siblings are independent: closing one leaves the other open, and a
datagram addressed to one is handed to that one. The hard constraint is
pinned rather than described — `a_second_h3_client_on_one_connection_is_a_connection_error`
asserts the `H3_STREAM_CREATION_ERROR`, so an `h3` that stopped enforcing
it fails a line.

**`GOAWAY` is a measured impossibility**, which is a complete answer
rather than a missing feature. `h3` 0.0.8 gives a client nothing it can
observe: a session's view is unchanged across one, polling the driver
resolves nothing, and — the sharpest of the four —
**two `GOAWAY`s saying opposite things look identical**. So a session
opened after one is refused *by the peer rather than by us*, which is
what the tests assert. No state was invented for it: a variant exists
only if a caller decision turns on it, and there is nothing here for one
to turn on.

### `multipart/form-data`, and a replay contract read off the parts

`RequestBuilder::multipart(Form::new().part(Part::bytes(..).file_name(..)))`.
Its shape is decided by one requirement: **a multipart body must be able
to stream.** Concatenating every part into one `Bytes` is four lines and
is wrong for the case multipart exists for — a file large enough that a
second copy of it is the thing that fails.

**The replay contract is not a setting; it is read off the parts**, and
it is knowable before sending, which is `RetryKind`'s whole promise.
Every part resolved to bytes → a `Rewindable` body, `ViaFactory`,
`Content-Length`. Any part a stream → `Streaming`, `Impossible`, and
`Content-Length` only where every stream's own `size_hint` is exact. The
way to opt into retries is to give the parts bytes; there is no flag,
because a flag would be a promise this module could not keep for a stream
it has already handed to a transport. "Resolved to bytes" is not "written
as bytes": a `Rewindable` part whose factory hands back a `Full` counts,
one whose factory hands back a `Streaming` does not.

**The boundary is 128 bits from the OS, and there is no collision check.**
Not omitted for cost: it cannot be made whole, because a streaming part's
content is unreadable before it is sent, so a scan could only cover the
buffered parts — a guarantee for some inputs, which reads as a guarantee
for all of them and is worse than an honest probability. The probability
is the argument, and it rests on drawing **after** the caller supplied the
content, once per form: an adversary choosing a file cannot choose it to
contain a value that does not exist yet. `getrandom` was already in this
crate's graph for SSE jitter, so nothing is added.

**An entropy failure is an error and never a fixed fallback** — the
opposite resolution from `sse.rs`'s `jitter()` three files over, where a
failed draw becomes `0.0`. The two are consistent: jitter's degenerate
value is un-jittered backoff, slower and safe, where a fixed boundary is
the single string most likely to appear in someone's content and the one
an attacker could plant. **A degraded value is only acceptable when the
degradation has a direction.**

Field and file names go out as UTF-8 with three bytes escaped — LF, CR
and `"` — which is the WHATWG rule all three browser engines moved to,
and every other C0 control is **rejected**. That is a framing property
before an interoperability one: a raw CR LF in caller data would end the
header field and let the rest be read as further part headers. There is
no `filename*`, because RFC 7578 §4.2 forbids it in as many words. The
wart is stated where a caller meets it: `%` is not escaped, so a name
containing the literal text `%22` is indistinguishable from one
containing `"`.

### Request ergonomics: query, forms and auth, with no dependency added

`RequestBuilder::{query, form, basic_auth, bearer_auth}`. Small, and the
interesting part is what they are built on rather than what they do.

**`query` appends and never replaces**, and each call appends again — a
query already in the caller's own URL survives. A replacing setter fails
invisibly from the call site: the `?tenant=acme` the caller wrote is
simply gone.

**The encoding is the WHATWG serialiser, not RFC 3986 percent-encoding**,
and the two are not interchangeable. A space is `+` and only `*-._`
survive as punctuation; `uri.rs`'s `percent_encode_into` is the other set
and a query built with it reaches a form parser as different data — a `+`
sent as itself reads back as a space. Both are now written down beside
each other, in `http-ng-proto`'s `encode` module.

**That module is where `base64` moved to**, and it is one function each:
`http-ng-native`'s proxy had written its own for
`Proxy-Authorization: Basic`, and `Authorization: Basic` is the same
encoding for the same reason. Neither pulls a crate — `url` was removed
from this graph at real cost, and its `form_urlencoded` would bring it
straight back, while `base64` is a crate for twenty lines. Encode only,
both: nothing here decodes either, and a decoder is where the sharp edges
live.

**A JSON request body closes the asymmetry** the response side had left:
`Collected::json` had existed since v0.1 and `RequestBuilder::json` had
not, both behind the same feature and for the same reason — a caller who
streams bytes should not link a serialiser, and on wasm that is download
size. It serialises **in the builder**, so a value that cannot be
serialised is the first build error rather than a failure discovered
after a connection was opened.

**A colon in a Basic username is refused rather than encoded.** RFC 7617
§2 makes it the separator, so `("a:b", "")` and `("a", "b")` would
produce identical bytes and one of the two callers would be silently
wrong. Both credentials are marked sensitive, which is asserted — the
mutation that removes the marking survived every wire-level test, because
a `Debug` is the only place it shows, and an observable property with no
observer is a gap rather than a control.

### `Expect: 100-continue`, and a ceiling that was measured rather than rounded

`Native::expect_continue(after)`. A body carrying the header waits for the
`100` or for `after`, whichever is first — RFC 9110 §10.1.1 makes the
second outcome *send it anyway* rather than an error, so the gate has one
open state and not two. **Both halves are required**: the caller asks by
sending the header, the transport agrees by being configured, and either
alone leaves the body ungated.

hyper's client does **not** do this — `Expect` appears in hyper 1.11 on
the server side only — and two things read from its source are what made
it implementable. `dispatch.rs`'s `poll_loop` calls `poll_read` *before*
`poll_write` every turn, so a body answering `Pending` does not stop the
response from being read; and the `100` arrives through the same
`hyper::ext::on_informational` the `1xx` work already installs. **That
slot holds one closure**, so the gate and the hook cannot each have their
own: which one is installed depends on whether a hook is watching,
because reporting needs `H: Send + Sync + 'static` and opening a gate
needs nothing of `H`.

**The timer could not live in the body**, and that decided the shape: a
concrete `Pin<Box<Tm::Sleep>>` would give `OutgoingBody` a type parameter
a dozen signatures must carry, and a `Box<dyn Future>` drops auto traits
(amendment C1). The body holds a flag and a waker; the clock stays in
`Native`, folded into the `first_byte` race rather than wrapped around it
— written as a wrapper first, and 56 tests in this crate's hook suite
aborted with `SIGABRT` on a stack overflow.

**A default that waited would be a default that hangs**, since a server
ignoring `Expect` sends no `100`. And it is not a `Timeouts` field:
`first_byte` bounds a wait ending in **failure** where this bounds one
ending in **proceeding** — same clock, opposite outcome.

That overflow produced the more general lesson. There are now two future
-size guards, `http-ng/tests/future_size.rs` and `http-ng-native`'s, and
**neither ceiling is a round number**: `Client::execute`'s future is
4,344 bytes and `Native::execute`'s is 15,480, but the figure that sets
both is what one extra `async fn` layer costs — measured at **1.81×**, so
a ceiling at 2× would be a guard that cannot fire for the defect it
names. They are 6 KiB and 24 KiB, and both are checked in the failing
direction by reintroducing the layer. `docs/expect-continue.md` §7.

### Proxies: an HTTP one and SOCKS5, behind one seam

`Native::proxy(Proxy::new(protocol, host, port))`, behind `http-ng-native`'s
`proxy` feature, off by default. It changes `P` the way `.hooks(..)` changes
`H`. Two protocols ship and they **share no bytes** — one is HTTP, one is
RFC 1928 — which is what makes the seam evidence rather than a claim, the
same standard `Transport` and `WebSocketConnect` were held to.

**It is not a decorator over `TcpConnect`, and the reason is that seam's
signature.** `connect` takes a `SocketAddr` and nothing else, so a wrapper
could never hand the proxy the origin's *name*: the client would resolve it
locally and leak exactly the DNS a proxy user is often there to hide, and
`http://` could never take absolute-form, which is decided where the request
head is written. SOCKS5's `ATYP=0x03 DOMAINNAME` is what proves the leak is
a property of that seam rather than of proxying. So a proxy **replaces** the
resolve → Happy-Eyeballs → connect block: the resolver is not consulted for
the origin, HTTPS/SVCB discovery does not run, and Happy Eyeballs races the
*proxy's* addresses instead.

**Not `Box<dyn ProxyProtocol>` either**, and this crate's own history is the
argument: erasing the protocol erases the IO with it, and a boxed IO needs a
`Send` — the objection that disqualified `hyper::upgrade::Upgraded` for the
WebSocket work and `hyper/http2` before it. Hence a type parameter,
defaulted to `NoProxy`, which is an **empty enum**, so a transport nobody
configured holds an `Option` that cannot be `Some` rather than a stub that
exists to be absent.

**The `CONNECT` tunnel turned out to be the WebSocket upgrade seam with a
different accepted status**, and that was read in hyper 1.11 rather than
hoped: its h1 client sets `wants_upgrade` for `Method::CONNECT`
(`role.rs:240`) and skips the body for `CONNECT` + `is_success` (`:518`),
so `into_parts` yields the tunnel and the bytes read past it exactly as it
does for a `101`. `upgrade::exchange` takes the status test as a parameter,
so there is one copy of those forty delicate lines instead of two. The
request line needed nothing from hyper at all — it writes `http::Uri`'s
`Display` verbatim, so authority-form and absolute-form are both a matter
of handing it the right `Uri`.

Three things worth knowing before touching it. **A `407` refusing a tunnel
is `ErrorKind::Connect`, never a response** — it is the proxy's answer to
us, not the origin's to the caller — while a `407` answering an
absolute-form request *is* a response, from a server acting as origin for
it; both are pinned. **A non-empty `read_buf` after a tunnel is a refusal
rather than a rewind**, because nothing the origin might say can have
arrived before we wrote to it. And **the proxy in `PoolKey` is unreachable
today**, for the reason `docs/v02-acceptance.md` already gives about the
TLS identity in the same key — a constant within any one pool — and it is
in the key for the moment a pool is shared between transports. That
unreachability is this work's mutation **control**, and it survives as the
comment predicts.

**`Proxy::bypass([..])` is the half of `NO_PROXY` that is not policy**, and
the split is the point: *reading the environment* is policy — which
variables, whose matching dialect, whether a library may read the
environment at all — and belongs to whoever builds the transport, where a
list the caller wrote down is not policy at all. The rules are small
because `NO_PROXY` has no specification and every implementation disagrees
about the corners: exact host at any port, `.example.com` for a domain and
everything under it, `host:port` for one port, and an address literal —
a v6 one taking RFC 3986 brackets to carry a port. No CIDR, no wildcard,
and a pattern in no accepted shape matches **nothing** rather than
approximately something. **Nothing is bypassed by default, loopback
included**: excluding it would change what goes on the wire for a caller
who asked to proxy everything, which is what `TcpOpts`' every-field-off
default exists to avoid. The list is asked in two places — `connect`, so a
bypassed origin takes the ordinary path *in full*, and `Native::via`, so
its request is written origin-form rather than reaching an origin server
that never agreed to act as a proxy.

The feature has **no `dep:` entries**: neither protocol needs a third-party
crate, base64 included, so it buys code size back for a constrained target
and costs nobody a dependency — the WebSocket framing's argument has no
subject here. The seam itself is unconditional, because a third protocol
should not have to switch on a feature named after the two that ship.
`docs/proxy-design.md`.

### The pooled-reuse race has three points, and the middle one was ours

A server can close a pooled HTTP/1 connection between the client's last
look and its write. Every HTTP/1 pool has this; `h1.rs` has called it
"residual" since v0.2 W2 and `docs/nagle-and-nodelay.md` §6 names the two
expensive fixes it would take. Reproducing it deterministically — which
had never been done, because *"that instant cannot be hit from outside"* —
showed the window has **three** points rather than two, and that the
middle one was not a race with the network at all.

`try_send_request` puts the request into hyper's queue **eagerly**, when
the future is built. hyper's `poll_loop` then reads before it writes, and
a graceful EOF on an idle connection sets `close_read`, which makes
`can_write_head()` false — so the dispatcher **refuses to write**,
finishes with `Ok`, and says nothing about the request, which is still
sitting in its queue as a whole `http::Request`. This crate called that
`Failed::Sent`, with a comment reading *"we no longer own the request, so
there is nothing to hand back"*. The first half was true; the second was
one `drop` away from being false. hyper's `Envelope::drop` answers the
promise of every still-queued request **with the request attached**, and
that receiver lives inside the `Connection` the function is holding.

`h1::claim_back` drops the connection and asks once. **The verdict stays
hyper's** — nothing here judges what looks safe to resend, which is the
contract `Failed`'s doc states; what changed is that the question is now
asked at a moment when hyper can answer it. The far point is unchanged:
a request already taken apart and written out is `Failed::Sent`, the
caller is told, and at-most-once is intact.

**That is also why the attempt §6 records failed.** It polled the send
future on the connection's error arm *without* dropping the dispatcher —
asking a promise nothing would ever fulfil — and moved 9 failures in 20
to 6, a number that could not carry a decision. The mechanism was one
line away.

Measured twice, deterministically, and the two forms share no code.
Scripted in `h1.rs`, driven poll by poll with a noop waker: the EOF
placed at each point gives `NotSent` / **`Sent`** / `Sent` before and
`NotSent` / **`NotSent`** / `Sent` after. On a real socket through the
whole transport, with `LateEof(Tokio, n)` hiding the peer's `FIN` from
exactly `n` looks — eight sweeps of six arms each side, no disagreement
within a column:

| EOFs hidden | who finds the close | before | after |
|---|---|---|---|
| 0 | the pool's checkout poll | `200`, 2 accepts | `200`, 2 accepts |
| 1 | `exchange`'s look, request still ours | `200`, 2 accepts | `200`, 2 accepts |
| 2 | hyper's first read, request queued | **error, 1 accept** | **`200`, 2 accepts** |
| 3+ | hyper's read after writing | error, 1 accept | error, 1 accept |

**The two expensive fixes are still refused, and now for sharper reasons
than cost.** Suspending before the request is handed over buys nothing
certain — *a yield is not a fence*: it gives the reactor one more chance
to have delivered the `FIN`, moving the window rather than closing it,
and it costs a scheduler round trip on every pooled request plus the
*"exactly one poll, and it never suspends"* contract written where that
poll is. Replaying a request hyper will not hand back needs a notion of
method safety this codebase deliberately does not have, the same one
`docs/h3-research.md` §3.5 declines for 0-RTT; `RetryKind` answers only
half of it, and the `425` precedent argues the other way, because there
the **server** asked for the repeat.

Two things worth knowing before touching it. The error a caller reads is
the **connection's cause and not hyper's answer to our own drop** —
`dispatch_gone` describes the drop — and that is not decoration:
`Native::run` discards a `NotSent` error because it retries, but
`Staged::exchange` carries no retry and surfaces it. That was a mutation
that survived all 278 tests before it was an assertion. And the
`LateEof → point` mapping is exact only while the server's close is
**late**: with a prompt close the *first* exchange's own teardown read can
meet the `FIN` and spend one of the hidden EOFs, shifting every row by
one — seen once, in one arm of one sweep, and gone in the 48 since.

**The three points report `Stale`, `Ended`, `Ended` — and the first two
are one socket in one state**, now pinned rather than corrected. Both
names are true at the middle row above (the peer closed it after a
response, *and* it was handed out already closed), and which one a caller
is told is decided by which of two adjacent polls noticed. This work neither introduced it
nor changes it; what it changes is that `Stale`'s own promise — *"the
event that explains the `Connected` following it"* — now has a
`Connected` following the middle row too. Deciding the reason from what
the request did would move the emission below the request's outcome and
put the one-`Closed`-per-socket rule behind three exits where `h1.rs`
leans on two, which is a hooks change wanting its own measurement.

`docs/pooled-reuse-race.md`, including the mutation table and its
control — the connection's *error* arm, verified unreachable by replacing
it with a `panic!` and running the suite rather than by reading hyper.

### The licence was a claim with no text behind it, and the root is the wrong place for one

Every crate here has declared `license = "MIT OR Apache-2.0"` since the
workspace existed, and **there was not one licence text in the
repository** — no `LICENSE-MIT`, no `LICENSE-APACHE`, at the root or
anywhere else. An SPDX expression is a claim; the text is what makes it a
grant. `cargo package` does not check this, `cargo publish --dry-run` does
not check this, and the crate would have gone out with the claim standing
alone.

**The detail that decides where the files live is that a file at the
repository root never reaches the tarball.** `cargo package` takes only
what is inside the crate's own directory, so one pair at the root would
have looked right in every git view and shipped nothing. Each of the 29
publishable crates carries its own copy, as a symlink — cargo follows one
and packs the content, verified by extracting the `.crate` rather than by
reading the file list: 18 files where there were 16, and the first line of
`LICENSE-MIT` inside the tarball is the copyright.

A README is the same shape one step down, and it was the same absence: no
crate had one, `readme` was set nowhere, so 29 crates.io pages would have
carried a single line of `description`. Each crate has one now, and each
says the thing this workspace's own arguments turn on — **why it is its own
crate** — because that is the question a reader landing on
`http-ng-tls-quic` actually has.

`just packaging` is the check, in the `lint` job, and it asserts against
the **packaged file list** rather than the working tree, because the tree
can hold a file the tarball drops — which is the whole defect. It fails
closed on the loop not running, and it was checked in the failing direction
by removing one README and watching it name the crate.

### `#[non_exhaustive]` has three answers, and only one of them is "yes"

Publishing turns every public type into a promise, so the attribute was
decided type by type rather than swept on: **21 added**, bringing the
workspace to 35 sites. What the sweep produced is a rule, and the rule is
worth more than the list.

**It is a no-op wherever a struct has a private field**, which is most of
them — such a struct already cannot be built with a literal from outside.
So of 236 public types the question is even *live* for about 40.

**Answer 1: the caller builds it, so no.** `TcpOpts`, `Timeouts`,
`H1Opts`, `H2Opts` and `FetchOpts` exist to be written
`Struct { one: Some(n), ..Default::default() }`, and the attribute forbids
exactly that expression from outside the defining crate — functional
update included. `TcpOptsSupport` and `TimeoutSupport` are the same answer
with a different caller: a **runtime or transport implementor** writes
those, and an implementor outside this workspace is the whole point of the
seam. `WebSocketKeepAlive` looked like this group and is **not** in it,
which is what identifies the real discriminator: `::new(every, within)` is
its only construction path, so the attribute costs nobody anything. The
shape of the type is not the test; how it is built is.

**Answer 2: exhaustiveness is the mechanism, so no.** `Event` already
carried this in writing — a new variant must be a compile error for every
backend, and *the design worked and the running of it did not* is a
sentence this file records about that exact property. The capability enums
join it, because `Client::build()` refuses a setting a transport cannot
honour, and a `_` arm there is the **silently ignored setting** defect
this project has closed four times. So do `RetryKind`, `RedirectPolicy`
and `Discovered`, each of which encodes a distinction something branches
on.

**The sharpest case in this group was found by the compiler rather than by
reading, and it splits a class this workspace had treated as uniform.**
Fourteen error enums were already `#[non_exhaustive]`, so `SvcbRecordError`
looked like the fifteenth — and `http-ng-dns-system` and `http-ng-dns-doh`
both refused to compile, because both **translate** it variant by variant
into their own error types. **An error's answer depends on who stands on
the other side.** One that reaches an end caller can afford the attribute:
the caller's `_` arm says *something else went wrong*, which is true. One
that crosses a seam into a translator cannot: there the `_` arm is a
*mapping*, and a new variant would quietly acquire the wrong one.
`RawParam` is the same file's other half, and its `Other(u16)` is why — an
unknown SvcParamKey never adds a variant, so a new variant means *this
crate now parses that parameter* and every converter owes it a decision.

**Answer 3: the library hands it back and the caller only reads it —
yes**, and that is the 21. Errors with public fields (`UnexpectedStatus`,
`ResponseTooLarge`, `RedirectRefused`, `InvalidBaseUrl`, the four SOCKS and
WebTransport ones), parsed values that will grow with their RFCs
(`SetCookie`, `RequestDirectives`, `ResponseDirectives`), and reports
(`Config`, `Follow`, `Disagreement`, `RecordedRequest`).

**What the attribute does not buy is worth stating, because it is what
prompted the exercise.** Of the six types that took a semver-breaking
change in the 31 commits before the trigger, `#[non_exhaustive]` would
have saved **two** — `Phase`'s new variant, and nothing else that is now
marked. `TcpOpts`, `TcpOptsSupport`, `Timeouts` and `TimeoutSupport` are
all answer 1, and `Connected::remote` changed a field's *type*, which no
attribute has ever protected. The freedom this workspace has been spending
was never mostly about additions.

### The rendered docs had no check, and the prose is the product

`just docs` — `RUSTDOCFLAGS="-D warnings" cargo doc --workspace
--all-features --no-deps`, in the `lint` job. It was run for the first
time when publishing made the output visible, and it reported **96
warnings across 17 crates**. Four kinds, in rising order of harm, and the
order is the point: the cheapest is cosmetic and the dearest is
invisible.

**Two unclosed HTML tags, which silently delete the rest of the sentence
from the page.** `<S>` and `<usize>` escaped their code spans, and one of
them for a reason no reader would find by looking: a doc line beginning
`+ Unpin` starts a **markdown list**, which closes the code span opened
on the line above, so every backtick after it pairs one off and
`Stream<S>` lands outside any span. Counting the backticks says the line
is balanced. It is the parser that disagrees.

**Twelve unresolved links from a single shape** — a link target wrapped
across two `///` lines. rustdoc does not rejoin the path, so the target
is `crate::` followed by a newline. Searching for the *shape* rather than
working the warning list found a thirteenth the warnings had classified
differently, which is the argument for fixing a class rather than a list.

**Forty-eight links from public prose to private items**, which docs.rs
renders as literal `[`brackets`]`. These are the maintainer's cross
references — `crate::pool`, `crate::hooks`, `Native::run` — and a reader
of the published page meets them as punctuation.

The remaining fourteen were answered one at a time rather than silenced:
six named a crate that is genuinely not a dependency (one of them a
*dev*-dependency, which resolves in a test and not in a doc build), three
only needed a path, one named a private helper under a name it does not
have, and one — `[RFC5987]` — is a verbatim quotation from RFC 7578 and
is escaped so it stays the quote it is.

**This is the third shape of the rule this file keeps recording.**
`test-doc` was a recipe nothing called; `test-no-default` was a recipe
that printed `error:` and exited zero; this was **no check at all**, for
the artifact this project's whole method rests on. The recipe fails
closed twice — on rustdoc's warnings, and on there being no sign rustdoc
ran, because printing nothing and finding nothing are indistinguishable —
and it was checked in the failing direction rather than assumed: a
deliberate `[`ThisDoesNotExist`]` takes it to exit 101.

`--all-features` is not thoroughness, it is the same fact as the docs.rs
metadata beside it: `default` is empty or near-empty in every crate here
by design, so a doc build without it checks the smallest part of the
surface and publishes the same.

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
its close, and the library's race is one look narrower than it was — see
the section above on the point in it that was ours.

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
