# hclient

**`.notes/` is untracked.** This file cites it throughout — design notes,
acceptance records, research and measurements kept from building this.
None of it is in git: it is written for whoever works on this rather than
for whoever uses it, and `docs/` is for the second audience. A fresh clone
has `docs/` and not `.notes/`, so a `.notes/` link is a pointer into the
working copy, not a promise the file is there. The history still holds
everything that was moved.

**Plans and specs are the first audience, so they live in
`.notes/superpowers/`** — `plans/` and `specs/` beside each other — and
this needs saying because the skill that writes them defaults to
`docs/superpowers/plans/`. A plan is the record of an argument somebody
had while building; a reader who installed this crate has no use for it
and `docs/` is what a fresh clone ships. The freeze plan landed in
`docs/` once for exactly that reason, sat untracked for a session, and
took nine stale links with it when it moved — eight of which pointed at
the design spec and had been broken since *it* moved, which is this
file's own rule about a claim being as perishable as its subject, met
from the side where the subject is a path.

Cross-platform async HTTP client. The same application code
builds for native, browser and WASI — the transport is swapped out, not
buried under `#[cfg]`.

```rust
let client = hclient::Client::builder(transport).build()?;
let text = client.get("https://example.com").send().await?.collect().await?.text()?;
```

On native, with the `default-transport` feature — the same
code without manually choosing a transport. **`Client::new()` is one
constructor and panics on nothing**: it used to `.expect` a failure to read
the OS trust store and carry a `try_new` beside it that did not, and both
returned `Result` — so `try_` marked the one fallible about *more things*
rather than the one fallible at all, which is not what the prefix means.
`ErrorKind` already tells `Tls` from `Unsupported`, so the wide error type
stays and the panic and the prefix both go. It resolves
`DefaultTransport` (`Native` on `tokio` + `rustls` with the system trust store +
system `getaddrinfo`) itself, by target, not by a feature the user picks.

```rust
let client = hclient::Client::new()?; // requires an ambient tokio runtime
let text = client.get("https://example.com").send().await?.collect().await?.text()?;
```

The same two lines in a browser, on `wasm32-unknown-unknown`. `Client::new()`
is infallible there, so there is no `?` on it — that is the only difference:

```rust
let client = hclient::Client::new();
let text = client.get("https://example.com").send().await?.collect().await?.text()?;
```

End-to-end proof that this SAME generic code (not two
separate examples) actually runs over the network on two different runtimes
without a single `#[cfg]` —
[`crates/hclient/tests/two_runtimes.rs`](crates/hclient/tests/two_runtimes.rs):
`cargo nextest run -p hclient --test two_runtimes` instantiates the same
`fetch_once<R>` under `hclient_rt_tokio::Tokio` (a real `tokio::runtime::
Runtime`) and under `hclient_rt_smol::Smol` (a bare `futures_executor::block_on`,
no spawn and no `tokio` in the smol path's graph — see the next section).

A working end-to-end example that actually builds and runs under
`wasmtime` (not just compiles) —
[`crates/hclient-wasi/examples/fetch.rs`](crates/hclient-wasi/examples/fetch.rs):

```
cargo build -p hclient-wasi --example fetch --target wasm32-wasip2
wasmtime run -S http -- target/wasm32-wasip2/debug/examples/fetch.wasm
```

The acceptance for the whole `Transport` shape — a live consumer, written
against another library *before* this one existed, ported line for line and
building for all three targets from one source with no `#[cfg]` at all —
[`crates/hclient/examples/portable.rs`](crates/hclient/examples/portable.rs):

```
cargo build -p hclient --example portable
cargo build -p hclient --example portable --target wasm32-wasip2
cargo build -p hclient --example portable --target wasm32-unknown-unknown
```

The original is `act`'s `http-client` component on `wasi-fetch`. What the
port keeps, what it fixes and the four things it changes are written down in
[`docs/porting-wasi-fetch.md`](docs/porting-wasi-fetch.md); the behaviours
the example claims to have ported are pinned by
[`crates/hclient/tests/portable_example.rs`](crates/hclient/tests/portable_example.rs),
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

**A survivor count is not a gap count, and it points the wrong way.**
`just mutants <crate>` is the entry point — never a bare `cargo
mutants`, which inherits this machine's shared `[build] build-dir` that
`CARGO_TARGET_DIR` does **not** override, so concurrent sweeps link each
other's mutated object code: three runs over one unchanged tree reported
**65, 37 and 26**. What separates a gap from an artefact is written up in
`.notes/mutation-survivors-classified.md`, including the four gating
shapes and the measurement that a mutant inside a `#[cfg] mod` this host
excludes is never even type-checked.

The ranking lesson is the one worth carrying: **rank by survivors per
test, not by survivors**, and treat even that as a pointer rather than a
verdict. `hclient-tls` sat second from the bottom on count — 12 — and had
the worst ratio in the workspace at **6 tests over 1131 lines**; **10 of
its 12 were genuine gaps**, because the only implementation it ships is
`NoTls`, which overrides or moots every defaulted member of the seam, so
nothing ever *took* a default. A small crate with a large seam produces
few mutants and hides the most.

**And the ratio never predicted the *share*, only the crate**, which is
the half worth carrying because it is the half that keeps being read as
a verdict. Measured across every cluster closed: `hclient-rt` 22 gaps of
23, `hclient-tls` 10 of 12, `hclient-tower` 8 of 10 — all at ratios the
rule calls safe. `hclient-rt-nal` was written up here as the
counter-example, *"a worse ratio still and is fine, because both its
macros expand one shared body that its tests reach four ways"*. The
macro reasoning was correct and covered two thirds of the crate;
`io.rs` is 160 lines outside it, and both of its interesting functions
were unpinned — all sixteen arms of `io_err` flattened to
`ErrorKind::Other` and `poll_shutdown`'s body deleted outright, each
leaving the suite green. **A verdict about a crate's dominant shape is
not a verdict about the crate**, and this sentence was the workspace
demonstrating that on itself.

**A subagent's report is evidence, not a result, and the difference
cost three corrections in one session.** Each of the seven clusters was
closed by an agent and re-measured by hand afterwards, and three reports
carried a claim that did not survive it: a test said to kill
`ecn_is_really_on -> true` cannot, because it asserts `ecn` *is* true
and the mutant satisfies it (what it really pins is the `!dual`
short-circuit one expression in); nine sweep timeouts attributed to
`cargo test`'s process model, where the same mutation under plain
`cargo test` also fails in 10.11 s, so the verdict came from
cargo-mutants' own per-binary bound; and a test doc claiming its
subject was *"the only reader"* while the body polls a second one three
lines down. None was a fabrication — all three are a measurement
generalised past its sample, which is this file's own recurring defect
arriving through a new door. So the rule is not *distrust the agent*,
it is **re-apply the mutation yourself before believing the kill**: it
costs one command per claim and it is the only step that distinguishes
a test that discriminates from a test that agrees.

**And re-apply it before believing a gap, which is the same command
pointed the other way.** `.notes/hclient-native-mutation-run.md` carried
two items marked as owed — `http2::shared_is_reusable`, *"the other half
of the pair, has no test either"*, and the `SeamTimer` seam, *"the
largest genuinely-unpinned surface the run has found"*. Measured: **both
are closed**, each by exactly one test, each dying to its own mutation
with the sibling's still green. The fixtures were built by later work
and the headings never followed.

That is the file's recurring defect with the subject changed, and in the
**worse** direction: a stale claim wastes a reader's trust, where a
stale *open item* sends somebody to rebuild a fixture that already
exists. So the rule is not only *verify a kill* — it is **verify a gap
before spending a day on it**, and the check is the identical one
command.

The `SeamTimer` entry is worth reading past its correction, because the
deferral was right and its success condition was too narrow. It asked
for *a test where a deadline decides the answer* and warned that a
timing assertion is what this workspace has four times found to be a
flake. What was built asserts the connection is **asleep** rather than
that anything arrived on time — a broken timer spins, a working one
parks — which is causal rather than temporal and sidesteps the flake
risk instead of managing it. Naming the wrong design did not stop
somebody finding the right one, and that is the argument for recording
the *obstacle* rather than the intended fix.

And **the output directory is part of the isolation**, which the recipe
learned the expensive way: it passed `-o` into a private scratch dir and
then copied the result back to a fixed `./mutants.out`, so two concurrent
sweeps overwrote each other and one crate's directory held 40 of another
crate's logs. That failure is silent — what you get is a well-formed
survivor list for a crate you did not ask about.

Two things nextest does not cover. Doctests: it cannot run them, so
`just test-doc` does — `cargo test --doc --workspace --all-features`, four
of them today, and **a CI job calls that recipe**.

That job is younger than the recipe, and the gap between them is the point.
`test-doc` existed and nothing called it — not `just ci`, not any workflow
step — which is worse than no recipe at all, because it is the one people
trust before pushing. Two examples were broken the whole time it was
unwatched. `hclient-h3`'s called `Rustls::with_webpki_roots()`, which lives
behind `hclient-tls-rustls`'s `webpki-roots` feature while that crate's own
dev-dependency enabled `quic` alone; it compiled under `--workspace` only
because another member turned the feature on and Cargo unifies features
across the graph, so the workspace-wide run was green over an example that
did not build the way a reader would build it. The second broke the day the
WebSocket framing became its own crate — its example names `hclient::Client`,
because borrowing a transport a `Client` already owns is the whole reason
`Tungstenite` borrows — and the job caught it rather than a reader.

Both are fixed by giving each crate the dev-dependency its own example
needs, which is what "builds the way a reader would build it" means.

**That story has a third act, and both halves of it are about a check
that could not fail.** `test-doc` counted five ```ignore fences as
tests — rustdoc compiles none of them, so the recipe printed `ok` over
five code blocks nothing had ever built. Four were real examples and
are `no_run` now, with hidden setup lines; writing `hclient-quinn`'s
found that `UdpAdoptStd` is `hclient_rt`'s rather than that crate's, so
a reader copying the sketch would have imported it from the wrong
place. The fifth quotes `embassy-net`'s own `Drop for TcpSocket` as
evidence and is ```text, because someone else's code cited in an
argument was never our example. The recipe now fails closed on both
shapes — no `test result:` line at all, and any `ignored` count — so
13 doctests were checked where 9 had been — and **45 today**, which is
the number's real job: it grows with the crate, so what it pins is the
recipe's honesty rather than a figure. (It read 22 for long enough to be
worth noticing: nothing forces a figure in prose to move, which is why
the gate is the fail-closed pair and never the value.) `just test-doc` prints it, and the
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

**One mutation was going to be applied by CI, and the job was withdrawn
before it ever ran — this paragraph described it for weeks afterwards
anyway.** `.notes/v03-acceptance.md` recorded the single survivor of the UDP
work: a hardcoded `ecn: true` is indistinguishable from the truth on a
Linux kernel, where both answers are `true`, and what would settle it was
said to be one run on macOS, where `quinn-udp` documents `IP_RECVTOS` as
unavailable on dual-stack sockets. `just ecn-mutation-dies-on-macos` was
written for that, with a CI step, and **both were deleted an hour later**
(`8385039` added them, `eb4b973` removed them) because the premise was
measured and was false.

Probed on macOS 27 on a `[::]` socket: `only_v6()` is `Ok(false)`,
`IPV6_RECVTCLASS` sets and reads back `true`, `IP_RECVTOS` fails `EINVAL`
— the documented limitation is real — and the kernel reports the codepoint
for v4-mapped traffic *regardless*, because `IPV6_RECVTCLASS` covers both
families there. **So the mutant is not killable on any platform**, and a
job requiring it to die would have failed on every push. The same run
found that the test could never have run there at all: it sent to a
wildcard `local_addr()`, which Linux reads as "this host" and macOS does
not.

What stands in its place is `a_dual_stack_socket_reports_ecn_for_v4_mapped_traffic_exactly_when_it_claims_to`
in `hclient-rt-tokio/tests/udp.rs`, one-directional on purpose: only a
`true` claim is a promise. And the finding that survives is about the code
rather than the harness — `ecn_is_really_on` **under-reports on macOS**,
asking for an option the kernel does not need, which is the safe direction
and the floor rule this workspace applies everywhere.

**The defect worth keeping is this paragraph's own.** The job was
withdrawn in a commit that explains itself well; the prose describing it as
a live check on every push was not updated, and was then cited again in the
`quinn-udp` section of [Platform dependencies](docs/agents/16-platform-dependencies.md)
as *the other half, and it runs on CI*. A claim
about a check is exactly as perishable as the check — which is the rule
this file states three times over about `test-doc`, `test-no-default` and
the rendered docs, met here from the fourth direction: **the check was
right to disappear and the sentence about it was not.**

Browser tests: those go through `wasm-pack test --headless
--chrome|--firefox` regardless, see the `browser` job.

## What's in the dependency graph

The first row of the table, as before, is verifiable directly in this
repository: `cargo tree -p hclient-wasi -e normal --prefix none` contains no
`tokio` at all (27 unique crates total). The second and third rows, unlike
their counterparts in the vertical 1 report, are now measured too, not
predicted: vertical 2 (`hclient-native`, `hclient-rt-tokio`, `hclient-rt-smol`,
`hclient-tls-rustls`, `hclient-dns-system`) is built, and as of Task 14
`hclient` has a `DefaultTransport` (native, HTTP/1.1 only) — the
`default-transport` feature, pulling in exactly these four crates. The HTTP/2
row remains the same prior-research row it always was: `hclient-h2` does not
exist in this repository (not merely "not built yet" — not in the v0.1 plan at
all), kept untouched for the same rationale behind the HTTP/1-first choice it
was written under in vertical 1.

| build | tokio |
|---|---|
| ambient (`hclient` + `-wasi` / `-fetch`) — measured | **none at all** |
| `hclient` with the `default-transport` feature (native, HTTP/1.1 only) — measured, Task 14 | real: `[default, libc, mio, net, rt, socket2, sync, time]` — the `hclient-rt-tokio` reactor is needed for real `TcpConnect`/`Timer`, this is not "just a type dragged along", see below |
| `hclient-rt-smol` in isolation (without `hclient`, `async-io` gives the same capability) — **re-measured after the seam left `hyper::rt`** | **none at all.** It read `[default, sync]` — a leaf with no reactor — for as long as `hclient-rt` depended on `hyper`, which is where that leaf came from. The seam names `futures-io` now, `hclient-rt` names no `hyper`, and `cargo tree -p hclient-rt-smol -e normal -i tokio` answers *did not match any packages*. See below |
| `hclient-native` with the `http2` feature (v0.2 W3) — **measured**, and the prediction below was right | `[bytes, default, io-util, sync]`, plus `tokio-util` with `[codec, default, io, libc]`. Still **no reactor**: no `rt`, `net`, `time` or `mio` come from this feature — `h2` uses tokio's IO traits and codec, not its runtime |
| native + HTTP/2 — the row above as it stood before W3: a hypothetical estimate from vertical 1, kept for the record | `h2` pulls in `tokio` with `io-util` and `tokio-util` with `codec`, and through it `libc` |
| `hclient-native` **without** `http3` — re-measured 2026-09-22 | **50 crates** (was 32), and no `quinn` or `h3` among them: the QUIC stack is an optional dependency, so a build that does not ask for it does not resolve it. The absence is the claim; the count is colour and drifts |
| `hclient-native` **with** `http3` — re-measured 2026-09-22 | **84 crates** (was 63), `quinn` + `quinn-proto` + `quinn-udp` + `ring` + `h3` + `h3-quinn` on top of the 50. Still no reactor from this crate's own dependencies — it arrives with whichever `R` the caller supplies, and the arm's `Spawn` bound means that `R` must have one |

**Every number in this table is a fact about a dependency *resolution*,
not about this code, and they drift.** Re-measured on 2026-08-19 against
the same commands: `hclient-wasi` 28 → 27, `hclient-h3` 58 → 56,
`hclient-quinn` 42 → 41, `hclient-webtransport` 49 → 48,
`hclient-dns-doh` 22 → 23, `hclient-proto` on Linux 37 → 36. Nothing in
this workspace moved them — the same counts come out of the commit before
the week's work as out of the commit after it — and one went **up**, which
is what says it is upstream churn rather than a systematic miscount.

The three `hclient-proto` rows that did **not** move are the ones worth
noticing: Windows, macOS and `--no-default-features` were all unchanged,
because those graphs are this crate's own. The rows that drifted are the
ones with a large third-party subtree — and the converse held later, when
taking `base64` moved all four rows at once, which is what a change of
this crate's own looks like.

So these are colour, and the load-bearing claims beside them are the
*absences* — no `tokio` in either wasm graph, no `h3` under
`hclient-quinn`, no reactor from `hclient-native`'s `http2` feature.
Those are asserted on every push by `just graph`, which fails closed, and
they do not go stale. **A CI check pinning the counts would be the wrong
answer**: it would fail for an upstream release that broke nothing here,
and a check that cries wolf is silenced — the mirror of this file's rule
about a check that cannot fail.

**That leaf is gone, and the paragraph it replaces is kept because the
reasoning was right and the conclusion drawn from it was not.** It read:
both middle rows are the same `hyper` fact measured twice; `hyper` depends
on `tokio` **unconditionally, not behind a feature**, so `hclient-rt`'s own
`hyper = { version = "1.11", default-features = false }` still pulls in
`tokio` with the `sync` feature; `hclient-rt-smol` depends on `hclient-rt`
and therefore on `hyper` and therefore on that leaf, **regardless of
pulling in neither `tokio` nor `async-compat` directly**; and what separates
the rows is not *tokio or no tokio* but which REACTOR stands behind the
leaf.

Every clause of that is still true of `hyper`. What it never examined is
the premise underneath — **why `hclient-rt` depended on `hyper` at all** —
and the answer was: because the byte-stream seam was typed on
`hyper::rt::{Read, Write}`. The seam is `futures_io::{AsyncRead,
AsyncWrite}` plus this workspace's own [`Shutdown`] now, so `hclient-rt`
names no `hyper`, and `cargo tree -p hclient-rt -e normal -i tokio` and the
same for `hclient-rt-smol` both answer **`did not match any packages`**.

The upstream facts that made it look permanent are unchanged and are worth
keeping, because they are why nobody looked again:
[hyper#3428](https://github.com/hyperium/hyper/pull/3428) (exactly this swap
for `futures-channel`, behind a feature flag) was rejected not for technical
reasons but for the irreversibility of the decision — *"As of 1.0, we are
going to be very careful about adding new dependencies to the public API… it
"exposes" a crate feature that we could never remove"* — and
[hyper#3767](https://github.com/hyperium/hyper/issues/3767), a separate
ticket with the same conclusion about the only call site, was closed as *not
planned*. Both still stand. **Tokio still cannot be removed from a hyper
build; what changed is that these crates are no longer hyper builds.**

So the sentence *"the `tokio` crate simply sits on disk as the same leaf it
would for any other build that uses `hyper`"* was a correct description of
a consequence, written as though it were a constraint. That is this file's
own recurring defect — a measurement generalised past its sample — met from
the direction where the sample was somebody else's dependency graph.

**Measured after the change**, `cargo tree -e normal`, unique crates:
`hclient-rt` **15** with no `hyper`, no `tokio` and no `http-body-util`;
`hclient-tls` 16, `hclient-tls-rustls` 31, `hclient-tls-native-tls` 29,
`hclient-rt-tokio` 21, `hclient-rt-smol` 40 — **zero `hyper` in every one
of them**. `hyper` is a `[dependencies]` entry of exactly **one** crate in
this workspace, `hclient-native`, which is the crate that drives hyper's
client; `hclient-tls-rustls` keeps it as a dev-dependency for the
third-party acceptance test its own manifest argues for, and
`hclient-tungstenite` meets it only through the `Native` it borrows.

**A second fact, also measured, not assumed: the test-only busy-spin never
reaches production code.** `hclient_native::testing::blocking_io` (Task 12) —
a wrapper over `std::net::TcpStream` on this workspace's byte-stream seam,
for testing on a bare `futures` executor with no reactor at all; on `WouldBlock` it calls
`cx.waker().wake_by_ref()` immediately instead of actually waiting for
readiness through the OS. Measured by CPU time (`/proc/self/stat`) around a
request to a server that responds after 600ms: under `blocking_io` — wall
600.4ms, **cpu 600ms** (an honest busy-spin for 100% of the wait time); the
same exchange code (`h1::exchange`/`NativeBody`), but with IO from
`hclient_rt_tokio::Tokio::connect` (a real `tokio::net::TcpStream`, registered
with the reactor) — wall 601.1ms, **cpu 0ms** (Task 12 review, section B).
`two_runtimes.rs` (Task 14) confirms the same section's prediction of "won't
happen under tokio or smol" in practice: both tests run `Native` over real
`hclient_rt_tokio::Tokio`/`hclient_rt_smol::Smol`, never touching
`testing::blocking_io` — it exists only under `#[doc(hidden)] pub mod
testing` and is used only in this same crate's `tests/h1.rs`.

## The record, in parts

This file held the whole record and grew past ten thousand lines, which is
past what a session should read to find one rule. It is the operational
rules and the index now. The parts live in `docs/agents/`, cut verbatim,
section titles unchanged:

| part | what it holds |
|---|---|
| [Releases and versions](docs/agents/01-releases-and-versions.md) | the index, the pre-release series, who is stable and why, the semver gates, the MSRV policy, the publish order |
| [The TLS backends](docs/agents/02-tls-backends.md) | native-tls as a non-wrapper, the second `unsafe`, mTLS identities |
| [HTTP/3 and WebTransport](docs/agents/03-http3-and-webtransport.md) | the QUIC arm, 0-RTT, sessions, datagrams, GOAWAY |
| [Egress filters](docs/agents/04-egress-filters.md) | `Dial`, `EgressFilter`, TLS through a filter, datagrams through a filter |
| [The seams](docs/agents/05-seams.md) | the trait review, `Resolve`, the byte-stream seam, the QUIC TLS seam, `embedded-nal-async` |
| [Send, and embassy](docs/agents/06-send-and-embassy.md) | the `!Send` counterexample, `SendTransport`, channels |
| [Mock, CLI, auth, OpenTelemetry](docs/agents/07-mock-cli-auth-otel.md) | `hclient-mock`, `hc`, the auth seam, `hclient-otel` |
| [The client surface](docs/agents/08-client-surface.md) | tower, retry, redirect, the consumer report, the CLI's backend refusal |
| [hclient-idn](docs/agents/09-idn.md) | UTS 46 by target, the backends, the corpus, the cost |
| [DNS and its decoders](docs/agents/10-dns-decoders.md) | `domain` over RDATA, the two granularities |
| [The state stores](docs/agents/11-state-stores.md) | cache, cookies, the kv seam, HSTS, alt-svc, the one-backend question |
| [The parsers](docs/agents/12-parsers.md) | the date parsers, the header grammars |
| [Native's internals](docs/agents/13-native-internals.md) | 1xx, the flake hunts, Unix sockets, `Capabilities`, socket options |
| [Client behaviours](docs/agents/14-client-behaviours.md) | fetch members, `error_for_status`, digest, h2 settings, charset, codings |
| [Proxies](docs/agents/15-proxies.md) | sans-io protocols, the machine's own settings, the pooled-reuse race |
| [Platform dependencies](docs/agents/16-platform-dependencies.md) | the floor moving under a green tree, quinn-udp, jni |
| [Surface, naming, audits](docs/agents/17-surface-and-naming.md) | crate names, the rendered surface, the four proxy audits, `hclient-proto` internal |
| [Client erasure and Send](docs/agents/18-client-erasure-and-send.md) | `Client` with no type parameters, the `Send` story, RTN |
| [Language and packaging](docs/agents/19-language-and-packaging.md) | features as a floor, the licence, `#[non_exhaustive]`, pedantic, erasure naming |
| [The verticals](docs/agents/20-verticals.md) | what vertical 1 and vertical 2 proved |

## Status

The version numbers, the release order and the argument for each one:
[Releases and versions](docs/agents/01-releases-and-versions.md).
