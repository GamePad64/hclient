# hclient

**`.notes/` is untracked.** This file cites it throughout — design notes,
acceptance records, research and measurements kept from building this.
None of it is in git: it is written for whoever works on this rather than
for whoever uses it, and `docs/` is for the second audience. A fresh clone
has `docs/` and not `.notes/`, so a `.notes/` link is a pointer into the
working copy, not a promise the file is there. The history still holds
everything that was moved.

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
`quinn-udp` section below as *the other half, and it runs on CI*. A claim
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
| `hclient-rt-smol` in isolation (without `hclient`, `async-io` gives the same capability) — measured, Task 14 | `[default, sync]` — a leaf with no reactor, only `tokio::sync::oneshot`, see below |
| `hclient-native` with the `http2` feature (v0.2 W3) — **measured**, and the prediction below was right | `[bytes, default, io-util, sync]`, plus `tokio-util` with `[codec, default, io, libc]`. Still **no reactor**: no `rt`, `net`, `time` or `mio` come from this feature — `h2` uses tokio's IO traits and codec, not its runtime |
| native + HTTP/2 — the row above as it stood before W3: a hypothetical estimate from vertical 1, kept for the record | `h2` pulls in `tokio` with `io-util` and `tokio-util` with `codec`, and through it `libc` |
| `hclient-native` **without** `http3` — measured | **32 crates**, and no `quinn` or `h3` among them: the QUIC stack is an optional dependency, so a build that does not ask for it does not resolve it |
| `hclient-native` **with** `http3` — measured | **63 crates**, `quinn` + `quinn-proto` + `quinn-udp` + `ring` + `h3` + `h3-quinn` on top of the 32. Still no reactor from this crate's own dependencies — it arrives with whichever `R` the caller supplies, and the arm's `Spawn` bound means that `R` must have one |

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

**Both middle rows are the same `hyper` fact, measured in two different places
in the graph, not two independent observations.** `hyper` depends on `tokio`
**unconditionally, not behind a feature** — `hclient-rt`'s own `hyper = {
version = "1.11", default-features = false }` (zero feature set) still pulls
in `tokio` with the `sync` feature, verified with `cargo tree -p hclient-rt -e
normal -i tokio` in this tree. This is the same conclusion vertical 1 drew
about the HTTP/1 path from hyper's source (`tokio::sync::oneshot::Receiver` in
`src/upgrade.rs`, the only place it's used) — now confirmed by measurement,
not just by reading the code. The `hclient-rt-smol` crate depends on
`hclient-rt`, and therefore on `hyper`, and therefore transitively on this
same `tokio` leaf — **regardless of the fact that `hclient-rt-smol` itself
pulls in neither `tokio` nor `async-compat` directly** (`cargo tree -p
hclient-rt-smol -e normal` contains neither crate among its DIRECT
dependencies — checked by the `two-runtimes` CI job). The difference between
the table rows isn't "the smol path has no tokio, native does" — it's which
REACTOR actually stands behind that leaf: for `hclient-rt-smol` in isolation,
none (the `sync` leaf is inert, `tokio::sync::oneshot` is never driven), for
`hclient` with `default-transport`, a real one (`hclient-rt-tokio`, Task 3,
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
reaches production code.** `hclient_native::testing::blocking_io` (Task 12) —
a `hyper::rt::Read`/`Write` wrapper over `std::net::TcpStream` for testing on
a bare `futures` executor with no reactor at all; on `WouldBlock` it calls
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

## Status

v0.1: the core (`hclient-core`, `hclient-proto`, `hclient`) and three backends
— `hclient-wasi` on top of `wasi:http` 0.3 (vertical 1), `hclient-native` on
top of `hyper` + `rustls` + system DNS (vertical 2), and `hclient-fetch` on the
browser's `fetch` (vertical 3).

| target | transport | tokio in the graph |
|---|---|---|
| native | `hclient-native` — TCP + HTTP/1, HTTP/2 behind the `http2` feature, TLS pluggable | yes, on the h1 path |
| native | `hclient_native::H3` — QUIC + HTTP/3, a module rather than a crate since `f4dfe48`, TLS through a second seam | yes |
| WASI | `hclient-wasi` — `wasi:http` 0.3 | **no** |
| browser | `hclient-fetch` — `fetch` | **no** |

Both "no"s are machine-checked on every push rather than asserted here:
`ambient-has-no-tokio` runs `cargo tree` for `hclient-fetch` against
`wasm32-unknown-unknown` and for `hclient-wasi` against `wasm32-wasip2`, and
fails closed if the invocation itself breaks or returns nothing. Measured
while writing: zero matches for `tokio`, `hyper` or `h2` in either wasm
graph, four in the native one.

**The owner has pulled the trigger, and the first published version is
`0.1.0-alpha.1` rather than `0.1.0`.** The reason is not doubt about the
code — 19 CI jobs across three platforms are green — it is that the last
week moved six public surfaces: `Client` from generic to erased,
`Client::transport()` removed, `Client::new`'s error type widened and
`try_new` folded into it, `Response<B>`'s parameter defaulted,
`hclient-idn`'s answer changed three times, and `poll_shutdown`'s treatment
of `ENOTCONN`. Publishing that as `0.1.0` would freeze twenty-nine public
surfaces at the moment they were last seen moving.

A pre-release claims the names and promises nothing, so the next week of
changes costs `-alpha.2` rather than a major version across the family.

**The guard is real and it is not yet in force, which this sentence used
to get wrong.** It read that `cargo add hclient` will not select a
pre-release without being asked. Measured on 2026-08-29, from a fresh
crate against the registry: `cargo add hclient` selected
`0.1.0-alpha.2` — because there is nothing stable for it to prefer. The
moment `0.1.0` exists, `cargo add` takes that and a pre-release needs
asking for. So the protection begins at the first stable release, not at
the first publication. `0.1.0` follows
when the seams have stopped moving on their own.

The version now says itself **once**, in `[workspace.package]`, where the
previous thirty copies were thirty chances to drift with no way to see the
drift until a crate published at the wrong number.

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
`.notes/v01-acceptance.md`, `.notes/v02-acceptance.md`,
`.notes/v03-acceptance.md` and `.notes/v04-acceptance.md`. Those lists are
themselves the thing to check first: several entries on them were built
after they were written.

The mechanics are in place and were measured, not assumed. All **27**
publishable crates carry `description`, `license` and `repository`; inter-crate dependencies
carry `version` beside `path`, without which nothing here could be
published at all. Three crates are `publish = false` —
`hclient-rt-pair-check`, `hclient-rt-nal` and `hclient-rt-embassy` — and
this sentence said there was one for as long as there had been two, which
is the count-in-prose defect this file records about itself elsewhere:
`just packaging` derives the figure from `cargo metadata` and does not go
stale, and a number written here does.
`cargo publish -p hclient-core --dry-run` packages **and verifies** clean,
and `cargo package -p hclient` correctly refuses, because its dependencies
are not in the index — which is what the order is for.

**That order was recorded here as "five waves over 26 crates" and it is
six over 25**, which is the difference between two questions rather than a
miscount. Five is the *normal* dependency graph; `cargo publish` also has
to satisfy **dev-dependencies that carry a version**, of which there are 32
here, and they add three waves. `hclient-core`/`-cache`/`-cookie`/`-idn`
are still first and the terminal backends still last, and the chokepoints
in between are one crate wide: `hclient-tls-rustls`, then
`hclient-native` — two, since `hclient-h3` folded into the second.
**Nothing follows that order by hand any more**: `cargo publish
--workspace` is native since cargo 1.90 and computes it — measured on this
tree, 29 packaged, 29 verified, and its ordering identical to the one
derived here from `cargo metadata` before either tool was consulted. The
table is kept because that agreement is what makes the count a fact about
the graph rather than a guess. `cargo-release` does the half cargo does not:
the bump, including the literal version requirements — dozens of them,
counted by `just versions-agree` rather than written down here — that must
move with `[workspace.package].version` and that cargo gives no way to
centralise.

**The policy is one shared version and every crate published on every
release** — `cargo release <level>`, no `-p`. Its argument is that it
removes a question rather than answering one: selecting means knowing
which crates changed, and knowing means a step that can be forgotten,
where publishing everything cannot forget. The cost is 23 uploads for a
one-line fix, which for crates this size is cosmetic.

**One crate is outside that policy now, and what buys the exception is a
measurement rather than a preference.** `system-resolver` carries its own
version and its own `cargo-release` group — `shared-version` takes a
string, so a named subset is the mechanism rather than something invented
for it. The reason is that a shared version cannot leave pre-release while
any member still needs to, and **inside a pre-release `cargo
semver-checks` checks nothing**: measured against 0.50.0, both
`0.1.0-alpha.2 -> 0.1.0-alpha.2` and `0.1.0-alpha.2 -> 0.1.0` execute **0
of its 254 lints**, because a major step permits breaking — while `0.1.0
-> 0.1.1` executes 196 and caught a breaking change on the first try. So a
compatibility job over the family would be green for a tree in which every
promise had been broken, which is *a check that cannot fail* with the
subject changed. `just semver` is the gate, and it fails closed on a run
that executed nothing as well as on one that failed, because
cargo-semver-checks prints `0 checks` beside `no semver update required`
and exits zero.

**It is a step in the `lint` job as of the first publish**, and the thing
that made that possible is narrower than *the crate is stable*: a working
tree at the **published** number is `no change; assume minor` and runs
**196** checks, where the pre-release pair above is `assume major` and
runs none. So the vacuum belongs to the pre-release rather than to equal
version numbers, and the gate asks *has anything breaking landed since
the last release* on every push rather than only at release time.
Verified against the published crate: marking `Error` `#[non_exhaustive]`
exits 100 naming the lint and the item.

The cost is the question the policy existed to remove, coming back for one
crate: nothing here publishes it, and nothing will notice if a bump is
forgotten. It is the crate that can afford the trade — five public names,
none of which moved in the week that moved six of the family's, and a use
outside HTTP entirely. `just versions-agree` is what keeps the split
honest: it compared every requirement against `[workspace.package]`'s
version, which was one statement only while every crate shared it, and it
resolves each requirement to the crate it names now.

**What guarantees the set resolves is the requirement, not the matching
numbers** — and that was written here as a reassurance, which is exactly
where it went wrong. The published `hclient` 0.1.0-alpha.2 asks
`^0.1.0-alpha.1` of each neighbour; semver is indeed the mechanism, and
the mechanism was **making a promise nobody had made**. Measured on the
published crates, not reasoned about: a caller who pins `hclient-core =
"=0.1.0-alpha.1"` beside `hclient = "=0.1.0-alpha.2"` gets a resolution
cargo accepts and rustc refuses — *cannot find `Reduced` in
`hclient_core`*, because that type arrived in alpha.2. A pre-release
promises nothing between alphas, which is what this file says a
pre-release is **for**; a requirement spanning two of them says the
opposite.

**The cause was `dependent-version = "fix"`, and it was configured for a
policy this workspace does not have.** `fix` rewrites a requirement only
when the new version stops satisfying it, and `^0.1.0-alpha.1` goes on
satisfying alpha.2 for ever — so after the alpha.2 release every one of
the requirements still read alpha.1, and nothing said so. Its argument is
sound and its subject is `cargo release -p <crate>`: sparing a publish for
a neighbour that did not change. The policy two paragraphs up forbids
exactly that — **every crate is published on every release** — so there was
no publish to spare and only the lie to pay for.

It is `upgrade` now. `^0.1.0-alpha.2` excludes alpha.1 outright, so this
needs no `=` pins, and `just versions-agree` checks the **result** rather
than the setting: a release run from an older checkout, a hand-edited
manifest and a merge all bypass a setting, and none of them bypass a gate.
Two things still read as mistakes and are not — an unpublished crate's
version runs ahead of the index, and published versions go sparse per
crate — and **publishing everything removes both**, which is the second
argument for the policy. cargo-release does **not** work out which
crates changed — measured, with a tag one commit back: a plain `cargo
release patch` still planned all 23 uploads, which under this policy is
the wanted behaviour. `just release-pending` answers it for the day the
policy changes back. `docs/publishing.md` has the table, the script that derives it,
and the reason the waves are **not** collapsed back to five — a version-carrying
dev-dependency is what lets a downloaded `.crate` run its own tests, which
distribution packagers do.

**No local check can catch a wrong order**, which is why it is a document
rather than a recipe: `cargo package --workspace` makes every member
available to every other through a local overlay, so `just package-build`
is green for an order that a real sequential publish would refuse. The
refusal is benign — the verify step names the missing crate and nothing is
uploaded. Every publishable crate carries
`[package.metadata.docs.rs] all-features = true`, because docs.rs builds
`default` and `default` here is empty or near-empty in 29 of the 30 —
`hclient` itself is the exception since `default-transport` joined its
default, and even there `json`, `gzip`, `cookies` and `cache` are opt-in.

**Minimum supported Rust: the latest stable release** — currently **1.98**,
declared once in the workspace manifest and shared by every crate. That is the
support policy, not a snapshot: the floor moves with stable, and a release that
needs a newer compiler than the one you have is expected rather than a bug.

The trade is deliberate. There is no window in which this crate builds on an
older toolchain, so if you pin an older Rust, pin an older `hclient` with it.
In exchange nothing here carries version-shim code, and there is no MSRV
matrix to maintain.

**One crate is outside that too, and it is the same crate and the same
argument as the shared version.** `system-resolver` declares
`rust-version = "1.85.0"` literally. The policy above buys the *family* no
shim code and no matrix, and it costs a crate meant to be used from
outside thirteen releases of reach for nothing — measured rather than
assumed: on 1.85.0 it builds `--all-targets` and passes its 62 unit tests
and its doctest, and every dependency's own floor is below that
(`cfg-if` 1.32, `thiserror` 1.71, `windows-sys` 1.71, `windows-strings`
1.82). What binds it is **edition 2024**, which is 1.85 and nothing else
in that crate.

**And the three-platform promise was not being kept, which is worth
knowing before the next argument leans on it.** For twelve days — every
`ci.yml` run the API still holds, back to 2026-08-09 — `test
(macos-latest)` and `test (windows-latest)` **never finished a single
run**. Each sat until GitHub's default six-hour job limit and was killed
or cancelled, while the Linux leg finished the same suite in two to four
minutes. Nothing said which test was stuck, because **three separate
causes had to line up for that silence**: no per-test bound (there was no
`.config/nextest.toml` at all), no `timeout-minutes` on the job (so the
six-hour default applied), and `just test-workspace` capturing nextest's
output in a shell variable it never lived to print. Remove any two and
the third still gives a six-hour blank.

All three are fixed together — `slow-timeout = { period = "30s",
terminate-after = 10 }`, `timeout-minutes: 60`, and `tee` — with the
numbers measured rather than picked: the slowest test in this workspace
is 7.8 s, so the per-test kill is 38x it and the job bound is 10x the
Linux wall time. **What the fix does is make the next red run say which
test hangs**; it does not fix the hang, which is still unknown and is
nobody's guess worth recording. Both halves were checked in the failing
direction — a red nextest exits non-zero through the new pipe, and a run
printing no `Summary` exits 1 naming itself.

The claim below is stated as it stands, and it is the one to re-read once
that is diagnosed. One data point has since arrived and it is not a
diagnosis: the whole suite runs on a real Mac — macOS 27, arm64 — in
**12.4 s**, so whatever wedges the runner is not something this tree does
on every macOS. See the Apple resolver section for the run and for the
one candidate it produced.

There is also **no MSRV job in CI for the family, deliberately**, and
`rust-toolchain.toml` pins no version — it says `channel = "stable"`. A job
checking a fixed version would be a second statement of the same promise,
staler than the first, and it is the one people would trust: the moment
stable moves past the pin, that job goes on passing while checking a
toolchain nobody supports. The whole test suite already runs on stable on
three platforms, which is the promise, so the pin would add a way to be
wrong and no way to be right.

**And that argument does not reach the one crate with a fixed floor**,
which is why `just msrv` exists and has a job of its own. The objection
above is that a pinned job restates a **moving** promise more staleley
than the manifest does; `system-resolver`'s promise does not move, so the
job is its only statement rather than a second copy of one — and without
it `rust-version = "1.85.0"` would be a claim with nothing behind it,
which is the defect this file records against itself four times over. The
recipe reads the floor **out of the manifest** rather than carrying a
second copy, and refuses when the two disagree; both halves were checked
in the failing direction, by moving the manifest's number and by adding a
`let`-chain, which needs 1.88.
**Two TLS backends, both behind the same `TlsConnect` seam.**
`hclient-tls-rustls` is the default: memory-safe, and it behaves the same on
every platform. `hclient-tls-native-tls` uses the platform's own stack —
SChannel, Security.framework, OpenSSL — and exists for deployments whose
trust decisions live in the OS store: enterprise roots pushed by policy,
a FIPS-validated provider. That is a fact about an environment, not a
preference.

**Smartcards were on that list for four verticals and are not reachable
through this backend**, which was measured while building the mTLS seam
rather than assumed. `native_tls::Identity` has exactly two constructors
— `from_pkcs12(der, password)` and `from_pkcs8(pem, key)` — and both take
**bytes**, so a key the OS holds and will not export is as unreachable
here as it is through rustls. What the platform stack genuinely buys is
the trust *store* and the provider, which is the rest of the list; a
non-exportable client key needs a backend that can reach the keystore
itself (`rustls-cng` on Windows), and this workspace has none. The claim
was the *wrapper* shape one field over: the platform holds such keys, and
the crate binding the platform does not hand them over. It reports less back, and its own
module doc says exactly what: no protocol version and no cipher suite.
**The ALPN is no longer on that list**, and the story of why is the
section below.

### The wrapper was the limitation, and removing it took the workspace's second `unsafe`

`C16` made `hclient::Client` require `SendTransport`, and this backend
could not implement it: its handshake was
`async_native_tls::TlsConnector::connect`, a `pub async fn` whose future
has no name. So there was **no `Client` over the platform TLS stack at
all** — not a missing `Send` future, which is what the commit that landed
C16 said, but the cookie jar, redirects, the cache, decompression, digest
auth and SSE, all out of reach. It took measuring from outside the
workspace to see, because nothing in-tree depends on this crate.

**The fix was to stop being a wrapper.** Driving `native-tls`'s own
handshake was not enough on its own — `async_native_tls::TlsStream::new`
is `pub(crate)` and its adapter private, so the stream had to be owned
too. `crates/hclient-tls-native-tls/src/stream.rs` is that: a named
handshake future whose `Send` follows from `S`, and a `TlsStream` over
`native_tls`'s.

**It costs one `unsafe` and that is amendment C17.** `native-tls` is not
sans-io — it fronts SChannel, Security.framework and OpenSSL through a
synchronous `Read`/`Write` — so bridging it means handing the synchronous
side a way to reach the current task's waker. That is a raw `Context`
pointer, set immediately before a call and cleared by a `Guard` whose
`Drop` runs on unwind, asserted non-null rather than trusted. It is the
difference between the two TLS backends rather than a difference in care:
rustls is sans-io, so its handshake is a loop over buffers this workspace
owns.

**What it bought is two things, and the second was not the point.**
`Client` works over the platform stack again — which is the whole reason
this crate exists, since an organisation with MDM roots cannot use
rustls. And `reports_alpn` is `true`:
`native_tls::TlsStream::negotiated_alpn` is public, and only the wrapper's
absence of a re-export had hidden it. This crate's own doc called that
limitation concrete for two verticals while naming its cause correctly and
never acting on it.

**Measured: the graph fell from 66 crates to 32**, because
`async-native-tls` left with its dependencies.

**A second TLS seam, for QUIC, and it is not a widening of the first.**
`hclient-tls`'s `QuicTlsConnect` — behind its `quic` feature — exists because the intersection of
`TlsConnect`'s four methods with `quinn_proto::crypto::Session`'s eleven is
**empty** — QUIC wants key schedules per encryption level and CRYPTO-frame
payloads, `TlsConnect` can only hand back a wrapped byte stream — and the
failure mode is worse than a compile error: an adapter between them
type-checks *with an empty body*. `hclient-tls-rustls` implements it behind
a `quic` feature; `hclient-tls-native-tls` implements nothing and using it
for HTTP/3 is a compile error, which is honest rather than harsh, because
`native-tls` binds no QUIC API at any level.

**It was a separate crate for two verticals, on the argument that Cargo
unifies features — and that was reversed in `169dbdd` after the cost was
measured rather than assumed.** The argument is still literally true: a
neighbour switching the feature on does put `quinn-proto` in every graph
that has any TLS. What was wrong is that this cost had **already been
accepted one crate over** — enabling `hclient-native/http3` puts
`quinn-proto` and `ring` into `Native<Embassy, NoTls, IpLiteralOnly>`'s
graph, and the commit that did that called it *dead code in the graph, not
a broken one*. Refusing the identical cost here was the inconsistency
rather than the caution. Who newly pays is narrower still: of the five
crates depending on `hclient-tls`, two gain an edge, and both are only
ever in a graph beside a transport.

`TlsConnect` and `QuicTlsConnect` share `TlsIdentity`, so a connector has
one configuration identity rather than two.

### A client certificate is chosen by a name, and the name is the only portable thing

mTLS with **one** identity has always worked: `Rustls::from_config` takes a
`rustls::ClientConfig` built with `.with_client_auth_cert(chain, key)`, and
every connection presents it. What did not exist is choosing **per
request** — a tenant per certificate, a smartcard beside a software key —
and that is now `hclient_core::ClientIdentity` in the request's extensions,
`TlsRequest::identity` on the seam, and `Rustls::with_identity(name, cfg)`
on the backend.

**What travels is a label the caller invented, and that is the whole
design.** Not a certificate — a key in a smartcard cannot be handed over as
bytes. Not a store query — `CERT_FIND_SUBJECT_STR` means nothing to
PKCS#11, and a `SecIdentityRef` means nothing to Windows. A name is the
only value that means the same thing on Windows, macOS, PKCS#11 and
Android at once, because it means nothing on any of them until a backend
resolves it. `docs/mtls-design.md` §3.1.

**The seam is `TlsIdentity::config_id_for(name) -> Option<TlsConfigId>`,
defaulted to `None`** — `reports_alpn`'s and `applies_ech`'s shape, a
constant defaulted to the understating value, read by the layer above to
decide whether to *ask*. A backend that never heard of labels refuses every
name, and `hclient-native` turns that into an error naming the label
**before it opens a socket**. The alternative — connecting with the default
identity — is how one tenant's certificate reaches another tenant's server,
which is the *silently ignored setting* defect where the setting is a
credential.

**Isolation is by construction rather than by a check**, and it cost one
expression: `TlsConfigId` was already a component of `hclient-native`'s
pool key, so resolving the label into that key is the whole of it —
`Security::Tls(identity_id.unwrap_or_else(|| self.tls.config_id()))`. Two
labels to one origin therefore cannot share a connection. That is the
load-bearing property and it is mutation-verified: dropping the
`unwrap_or_else` back to the connector's own id fails exactly
`two_identities_to_one_origin_cannot_share_a_connection` and nothing else.
The control is the same label twice, which must share one.

**The label is `Cow<'static, str>` rather than an `Arc<str>`**, because it
is almost always a literal: `Cow::Borrowed` costs nothing where
`Arc::from(&str)` allocates on every request. A computed label pays a
`String` clone per hop, bounded by the redirect limit and a few bytes wide.

**The QUIC half was written by the compiler and one line was not.**
`QuicTlsRequest` carries the same field, and threading it through
`hclient-h3` was mechanical — except that `quic_config_for` accepted the
identity and ignored it, which the type checker cannot see. The design
document had warned about exactly this shape one section earlier and the
warning was realised inside its own implementation: **the compiler catches
the transport and never the backend.**

**The observation half is built, and it landed one commit later than the
selection half on purpose.** `ClientCertRequest` was written with the
seam, **removed before that commit** because it had no producer and no
reader — `UpgradeSupport`'s shape — and put back with both. A caller who
wants to *choose* interactively needs what the server asked for, and
rustls hands the `CertificateRequest`'s contents to a
`ResolvesClientCert` and to nothing else: there is no getter on
`ClientConnection` afterwards, so the only way to see them is to **be**
the resolver.

**So the record belongs to a connection and the resolver belongs to a
config, and those are different lifetimes.** A `ClientConfig` is shared —
cloning one is rustls' most expensive operation and this crate caches
per-ALPN copies precisely to avoid it — so the resolver cannot own the
slot it writes into. What it can do is write into whichever slot is
installed *for the duration of the call*, and `resolve` is reachable only
from inside `ClientConnection::process_new_packets`, which is a
**synchronous call this crate makes**. A guard scopes the slot around the
poll the way a lock scopes a critical section. That is
`hclient-tls-native-tls`'s waker trick with the sign flipped: there it
costs the workspace's second `unsafe`, and here it costs none, because
rustls is sans-io and the value being scoped is an `Arc` rather than a
borrowed `Context`.

**The wrap is unconditional and cannot change a handshake.** Every
constructor goes through it, `from_config` included — the caller who
builds their own config is the caller doing mTLS — and `Recording`
forwards `resolve`, `has_certs` and `only_raw_public_keys` verbatim. It
costs one config clone per *construction*, never per connection.

**`answered` is the field that earns the payload type.** Without it a
caller cannot tell *403 because I sent no certificate* from *403 because
I am not authorised*, so a picker fires on the wrong responses. It is
discriminated by exactly one of the three tests, which is what says it is
carrying a fact rather than decorating one.

**And the answer is three-state, because two would be a lie by
omission.** `Option<ClientCertRequest>` was the shape that shipped for
one commit, and it collapses *the server did not ask* into *this backend
cannot see whether it did*. Both sides are reachable in one program —
rustls observes by being the resolver, `native-tls` exposes no hook — and
**`hc --backend` chooses between them at run time**, so a caller writing
a picker would see nothing on one backend and conclude that no server
ever asks. `ClientCertAsk::{Unobserved, NotAsked, Asked(..)}` is
`Discovered::NoRecord`/`NotConsulted` a third time, and the default is
`Unobserved` — the understating value, asserted rather than described, so
a backend that forgets the field can never be read as having watched.

Only `hclient-tls-rustls` may say `NotAsked`, and it may because its
recording resolver sits in **every** config it hands out: an empty slot
after a completed handshake is an answer. `hc -v` prints all three —
nothing for `NotAsked`, the authorities and whether one was sent for
`Asked`, and *this TLS backend does not report whether one was requested*
for `Unobserved`, which is the same distinction the SSL line above it
already draws for the version and the suite. The QUIC path reports
`Unobserved`: quinn drives that handshake, so no slot of ours is
installed.

The enum is **not** `#[non_exhaustive]` where its payload is, and the
split is this file's own rule: the payload is handed back and only read,
where the enum is *branched on*, so exhaustiveness is the mechanism and a
fourth state must be a compile error at every reader.

It reaches a caller on `Connected`, by the path `tls_version`,
`tls_cipher` and `alpn` already take, and through the same **one** setter
for the same reason: a backend either read the handshake's outcome or it
did not.

**Two more things the seam owed and did not have.** `Rustls::config_for`
fell back to the **default config** for a label it did not recognise,
under a comment observing that the transport refuses first — a silent
substitution guarded by an argument about unreachability, which is the
exact failure the design exists to prevent, and the QUIC half had the
same fallback written a second way. Both refuse now, naming the label,
and the obligation is written on the seam: *a backend that answers `Some`
from `config_id_for` owes that identity at connect time, and owes a
refusal rather than a substitution if it cannot serve it.* The layer
above cannot catch a breach — it resolved the label, put the id in its
pool key, and has no way to learn the handshake used another. And
`ClientIdentity`'s field is private: it was an `Arc<str>` for a day, so
the representation is exactly the thing not to promise, and `Clone` is
what a pool key needs — which is why the h3 key holds the type rather
than the string inside it.

What is still not here is the picker itself, and `docs/mtls-design.md`
§3.5 is why it is the caller's: rustls has no handshake pause, so an
interactive choice is observe, abandon, choose, redial — one extra
handshake per origin per session, and a second failed mTLS attempt in
whatever the server counts.

**And this workspace was wrong twice about rustls and about smartcards.**
It was said here that a server sending a DN list lets rustls choose:
`SingleCertAndKey::resolve` takes `_root_hint_subjects` and `_sigschemes`
and **discards both by name**, so rustls ships no resolver that chooses at
all — nothing in this workspace narrows a certificate set, and the DN list
is a thing to hand a caller rather than a thing anything acts on. The
smartcard claim is corrected where it was made: `native_tls::Identity`
builds from PKCS#12 or PKCS#8 **bytes** only, so the platform backend
cannot reach an OS-held key either. Reaching one needs a backend bound to
the keystore — `rustls-cng` on Windows — and that is the next step rather
than a thing already there.

`NoTls` in `hclient-tls` is the third choice: no TLS at all, for a build
that has no room for a stack. `https://` then fails at connect with a typed
error, and `Capabilities::tls_config` reads `TlsSupport::None` rather than
claiming otherwise.

**`std` is required, and that is not our decision to reverse.** `http` 1.x
forbids `no_std` outright — `src/lib.rs` carries a commented-out
`#![cfg_attr(not(feature = "std"), no_std)]` next to
`compile_error!("`std` feature currently required, support for `no_std` may
be added later")` — and `http::{Request, Response, HeaderMap, Uri, Method}`
appear in the public API of ten crates here, including the sans-io
`hclient-proto`. `bytes` is a genuine `no_std` + `alloc` crate, and `url` is
gone from the graph entirely (see below), so the remaining obstacle is
`http` itself; a feature flag that claimed otherwise would not build.

For constrained targets that *do* have `std` — static musl binaries, small
containers, embedded Linux — see `NoTls` and `IpLiteralOnly`, and
`crates/hclient-native/examples/minimal.rs`.

**Bare-metal microcontrollers are not reachable today. A device with
`std` is — and the sentence that used to say so named the wrong runtime
for it.** It read that an esp-idf target is reachable *because*
`hclient-rt-embassy` implements `TcpConnect` over `embassy-net`, and the
pairing is wrong in both directions. Measured: `esp-idf-svc` 0.51.0
integrates `embassy-sync`, `embassy-time-driver` and `embassy-futures`
and **not `embassy-net`** — it gives you ESP-IDF's own lwIP through
`std::net`, so the runtime there is `hclient-rt-smol` or
`hclient-rt-tokio`, which have needed nothing special all along.
`embassy-net` is the `no_std` stack, and `no_std` is exactly what is out.

So a std device is reachable, and `Native<Embassy, ..>` is not how. What
`hclient-rt-embassy` is for is [the section on its real
value](#the-embassy-runtime-is-the-workspaces-only-send-counterexample);
what is still out is `no_std`, and the obstacle there is a dependency
rather than a design:

- **`http` 1.x, external.** The `compile_error!` above.
- **`url`, ours — and now removed.** `hclient-proto` used it at exactly one
  functional site — `Url::parse().join()` for RFC 3986 reference
  resolution — and that one call pulled `idna` -> `icu_normalizer` +
  `icu_properties`: measured at 1.9 MB, 1004 KB, 820 KB and 452 KB of
  vendored source, almost all Unicode tables for internationalised domain
  names. On a part with 256-512 KB of flash that is the entire budget, for
  a feature such a device rarely needs.

  `crates/hclient-proto/src/uri.rs` now implements RFC 3986 §5.2 directly,
  and `url` has moved to `[dev-dependencies]`, where it is the oracle for
  `tests/uri_resolution.rs` — a 96-pair differential corpus (all 42 RFC
  3986 §5.4 reference examples, plus the forms a client actually meets)
  that pins both implementations' answers and enumerates every place they
  deliberately differ. IDN survives as the `idn` feature of
  `hclient-proto`, forwarded by `hclient` and **on by default**, so a plain
  build behaves as before; `--no-default-features` removes the whole IDN
  implementation and turns a non-ASCII host into a typed
  `UriError::NonAsciiHost` naming the A-label to send instead. The
  `idn-feature-is-real` CI job checks all of that, in both directions, and
  runs the feature-off test suite that `--all-features` cannot reach.

  **The feature no longer names `idna`; it names `hclient-idn`**, which
  chooses the implementation by target in its own `build.rs`. `uri.rs`
  calls `hclient_idn::domain_to_ascii` and maps two error variants, and
  nothing in `hclient-proto` mentions `idna` any more. What that changes
  is where the Unicode tables are, not what a host converts to: on Linux
  and the other ELF unixes, and on wasm, the backend *is*
  `idna::domain_to_ascii_cow(…, AsciiDenyList::URL)` — the same call
  `uri.rs` used to make itself, with the same arguments. Measured before
  believing: the 96-pair corpus is unchanged and green in both feature
  settings, and `hclient_idn::domain_to_ascii` against `idna` directly
  over 9,739 inputs on this host gave 0 differences.

  So the crate count is now a fact about the target rather than one
  number. Measured `cargo tree -e normal`, unique crates, this tree:

  | build of `hclient-proto` | crates | what supplies UTS 46 |
  |---|---|---|
  | default (`idn`), x86-64 Linux | **41** | `idna` + the ICU data crates |
  | default (`idn`), `--target x86_64-pc-windows-msvc` | **17** | `icuuc.dll`, through `windows-sys` |
  | default (`idn`), `--target aarch64-apple-darwin` | **19** | Foundation, through `objc2-foundation` |
  | `--no-default-features` | **14** | nothing — `NonAsciiHost` |

  The Linux row was the old **36** plus `hclient-idn` itself and nothing
  else: `thiserror` was already there, and no new Unicode crate arrives.
  There is no `url` in any of them.

  Every row is three higher than that since `encode.rs` and `uri.rs`
  stopped carrying their own encoders — `base64`, then `form_urlencoded`
  and `percent-encoding` — and the Linux row is one higher again from
  upstream churn, the same drift the section above measures. That the
  crate's own changes moved **all four** rows while churn moved only the
  one with a large third-party subtree is the clearest statement of what
  separates them.

  **All four moved again, by exactly one, when `head.rs` arrived** —
  `winnow`, for RFC 9112 §4's response head, which is what let
  `CONNECT` stop needing an HTTP client. The rule above demonstrated
  itself: a change of this crate's own moves every row, and the +1 is
  the same +1 on Windows and macOS as on Linux because `winnow` has no
  Unicode tables and no platform half. It is **not** behind a feature,
  which was the first shape and was withdrawn: gating it would have
  bought one crate back at the floor and cost every consumer a feature
  to remember.

  **There is still no `url` in any of them, and that is the point of the
  last two.** `form_urlencoded` is its own crate over `percent-encoding`
  alone. This file and `encode.rs` both said for two verticals that
  taking it *"would bring `url` straight back"*, and the claim was never
  measured; it is two crates, no `idna`, no ICU, no build script, and
  both wasm targets. What the measurement does rule out is the near
  neighbour: `urlencoding` computes a different function at both sites.

**Name resolution has a third backend, and its problem was never the wire
format.** `hclient-dns-doh` (v0.3) puts DNS-over-HTTPS behind the same
`Resolve` seam, and the two questions worth knowing the answers to are
both about bootstrapping rather than about parsing. What makes the
request is a `Transport`, **never an `hclient::Client`** — a cookie jar,
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

SVCB parsing moved from `hclient-dns-system` up into `hclient-dns`
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
workspace does. That is not caution: `hclient-tls-rustls` *refuses* a
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
behind `hclient-native`'s `http2` feature, off by default; **HTTP/3 landed
in v0.3**, over QUIC — in its own crate then, and `hclient_native::H3` since `f4dfe48`. **WebSocket landed in
v0.3 (W4)**, and in v0.4 became a crate of its own,
`hclient-tungstenite` — and not as a method on `Transport`: it is its
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
calls, has to smuggle a `*mut Context`. `.notes/w4-upgrade-seam.md` has the
measurements and the decisions.

**It is `hclient-tungstenite`, its own crate, and until v0.4 it was a
`websocket` feature of `hclient-native` — the one pluggable thing here
that was not its own crate** (`.notes/w4-upgrade-seam.md` §8). Features are
additive, so that feature put `tungstenite` into every build in any graph
that switched it on: the argument that kept `hclient-h3` out of
`hclient-native` and `hclient-tls-quic` out of `hclient-tls`, applied to
the one place it was not. A dependency in the other direction cannot be
switched on from outside, and `graph-no-framing-in-the-transport` checks
it with `--all-features` on the transport rather than asserting it.

**The seam between them is "an upgraded byte stream, plus the `read_buf`
hyper had already read past" — the shape §2 rejects as the public seam.**
Both hold at once and they are about different levels: as the public seam
it excludes three of four backends, the browser among them; between a
transport and a framing crate it is only ever asked of the one backend
that can answer it. `hclient_native::Upgrading` is that seam, and it is
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
sends no requests. `hclient-fetch` is untouched by all of this and needs
no connector — a browser hands back *messages* — which is the asymmetry
that says the seam is in the right place.

**An open WebSocket is bounded by liveness, not by `Timeouts`, and the
knob is on the connector rather than on the seam.** `total` is
meaningless for a connection meant to outlive its exchange and
`between_bytes` would be actively wrong, since silence is a WebSocket's
normal state — so the bound is RFC 6455's ping/pong, off by default,
because a default that pings sends traffic nobody asked for. It is not on
the trait because a browser has neither `send(ping)` nor `onping`, the same
fact that keeps `Ping`/`Pong` out of `Message`; asking `hclient-fetch` for
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
source, deliberately agreeing with `hclient-fetch`'s treatment of a
`wasClean == false` close rather than inventing a second vocabulary, and
deliberately not `ErrorKind::Timeout`, since no `Timeouts` field is in
force. Nothing is spawned: the caller's `poll_next` is the only thing
driving the socket, **so a caller that stops polling gets no keep-alive**.
That is the mirror of HTTP/3, where a spawned driver turned out to be
necessary and not sufficient; here there is no driver at all.

See [`.notes/v01-acceptance.md`](.notes/v01-acceptance.md) for what v0.1
deliberately does not do,
[`.notes/v03-acceptance.md`](.notes/v03-acceptance.md) for what v0.3 does,
does not, and has not checked, and
[`.notes/v04-acceptance.md`](.notes/v04-acceptance.md) for v0.4 — which is
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
**erased** field is not: `Option<Box<dyn BoxedTransport + Send + Sync>>`,
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
since (see the section on those two below), all off by default on `json`'s
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
letting `H3` answer for itself.** `hclient_native::Staged` *owns* the
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
reverse of the order both were written in. `Selecting` asks `H3` to
*connect*; where that fails it records the origin in
`hclient_select::H3Failures` and routes the request — untouched, unsent,
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
TTL in hand and dropped it. `SvcbEndpoint::ttl` carries it now —
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
persisted, and `Selecting::network_changed()` is the only entry point —
public, for the caller who can see what the transport cannot. Until it is
called every entry behaves as `persist=1`, which is the unsafe direction and
is said where the setter is.

**The negative half is built now, and not by the race** (v0.4). It took
reading to establish that it was missing rather than misplaced:
`hclient-native`'s `NegativeCache` is a different fact — a TCP connect
through a discovered endpoint failed — and it never sees an h3 attempt,
because when `Selecting` routes to `H3` the native transport is not called
at all. `hclient_select::H3Failures` is the memory that was owed; the
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
it is fetched by it.** `hclient_native::Prefetch::prepare` does the
connector's own lookup — its resolver, its rule about where discovery
applies, its negative cache — and hands back a `Prepared`, which is the
request *with* the answer; `execute_prepared` then does not look again. A
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

### `embedded-nal-async` is the right seam for later and blocked twice now

Asked whether this workspace should implement `TcpConnect` over the
Embedded WG's network abstraction rather than over one stack. Measured,
and the answer is *not yet, and then yes* — with a caveat that does not
expire when the blocker does.

**The breadth argument is real, and it is the reason to want it.**
crates.io reports **27 reverse dependencies**, and about eight of them are
genuine stacks rather than consumers: `embassy-net` itself,
`embassy-nina`, `es-wifi-driver`, `esp8266-at-driver`, `rak811-at-driver`,
`nrf-modem`, `wincwifi`, `riot-wrappers` — AT-command WiFi modules, an
nRF9160 LTE modem, RIOT OS. One adapter reaches all of them where
`hclient-rt-embassy` reaches one. And `reqwless`, the incumbent embedded
HTTP client, is built on exactly this seam, which is evidence about the
niche rather than about us.

**Blocker one is the same `no_std` wall, so nothing changes today.** Every
one of those stacks is a `no_std` device, and `http` 1.5.0 still carries
its `compile_error!`. The only NAL implementations this crate could build
against are the std shims — `std-embedded-nal-async` and its neighbours —
where a caller has `std::net` and would use tokio.

**Blocker two is structural and outlives the first, which is the part
worth writing down.** `embedded_io_async::Write` is `write` and `flush`
and nothing else — read in 0.7.0 — and
`embedded_nal_async::TcpConnect::Connection<'a>` is bounded on
`embedded_io_async::Read + Write` and nothing more. So a NAL connection
**cannot half-close**, by the trait's own definition, and
`hyper::rt::Write::poll_shutdown` is how an HTTP client sends FIN while
still reading the response.

This crate has already met that and refused it. The W7 spike went through
embassy's own `TcpClient` — its NAL implementation — forwarded
`poll_shutdown` to `flush`, and recorded the result as *"a half-close
hyper believes it performed and did not"*. `hclient-rt-embassy` exists in
the shape it does precisely to avoid that: it owns the
`embassy_net::tcp::TcpSocket` rather than a `Connection`, so `close()` is
available and `poll_shutdown` sends the FIN and waits for it.

**Blocker three is the `Send` story, and it is the hardest of the three
because it is the one this workspace already solved on its own seams and
cannot solve on somebody else's.**

Measured: the auto trait `Send` appears **nowhere** in
`embedded-nal-async` 0.9.0 and **nowhere** in `embedded-io-async` 0.7.0.
The three hits a grep finds in `udp.rs` are the English verb in a doc
comment about sending datagrams. And every method in both crates is an
`async fn` in trait — `TcpConnect::connect`, `Dns::get_host_by_name`,
`Read::read`, `Write::write` — so every future they produce is an RPITIT
with no name.

**The two `TcpConnect` traits are nearly the same shape and differ in
exactly the place that decides this.** Both carry an associated
connection type with a lifetime; ours also names the *future*
(`type Connecting<'a>`) and theirs does not. That one difference is
amendment C15's whole subject: naming is not requiring, so each
implementor answers for itself and a consumer can still write the bound.
With an RPITIT a consumer cannot write it at all.

So an adapter over NAL could only box their future, and boxing decides
the answer for everybody:

- **boxed plain** — `!Send` permanently, for *every* NAL stack, including
  the ones whose connection genuinely is `Send`: a std shim, an
  AT-command driver behind a mutex. That is a `dyn` removing a property
  rather than hiding it, which this workspace has now fixed four times in
  its own code and would here be importing on purpose;
- **boxed `+ Send`** — needs to prove an RPITIT `Send` for a generic
  implementor, which is return type notation: **it works, and it is
  unstable**. Measured on 2026-08-28 against `embedded-nal-async` 0.9.0
  itself on 1.100.0-nightly: `S: TcpConnect<connect(..): Send>` compiles,
  `Box::pin(s.connect(addr))` goes into a `Send` box, `Read::read(..):
  Send` does the same, and a whole `OurTcpConnect for Adapter<S>` with a
  `Send` `Connecting<'a>` builds. So this blocker is the one RTN actually
  removes — see the note below on why that does not make it worth taking;
- **named** — needs `type Connecting<'a> = impl Future`, also unstable.

**And this is coherent on their side rather than a defect.** NAL is
designed for one core and one executor, where `Send` has no subject —
`reqwless`, the client built on it, never needs the property. The
incompatibility is at the seam between a `no_std`-shaped abstraction and
a client that also serves threaded hosts, and it is why
`hclient-rt-embassy` implements *our* `TcpConnect` against
`embassy_net::tcp::TcpSocket` directly: the socket is concrete, so its
`!Send`-ness is a measured fact about one type rather than a property
lost for all of them.

**Neither RTN nor a channel is needed for half the stacks, and that was
measured rather than assumed.** The rule this workspace states everywhere
— *at a concrete type `Send` is inferred, in a generic impl it must be
proven* — is constructive here: move the impl to the concrete type and
inference does the work. A macro is how.

```rust
hclient_rt_nal::adapt!(MyAdapter, my_stack::Stack);
// expands to an `impl TcpConnect for MyAdapter` whose
// `type Connecting<'a> = Pin<Box<dyn Future<..> + Send + 'a>>`
```

At the expansion site `Stack` is concrete, so `Box::pin(s.connect(addr))`
coerces into a **`Send`** box with nothing to prove. Measured against
`embedded-nal-async` 0.9.0 on **stable**: a stack whose connection is
`Send` compiles, and — the control that makes it honest — a stack holding
an `Rc` **fails**, at the boxing site, naming the future. So the macro
does not claim `Send` for everybody; each stack answers for itself, which
is the associated-type principle reached from the other side. A second
macro producing a plain box is the answer for the stacks that fail, the
same split `Transport` and `SendTransport` already are one layer down.

**Where the macro fails is where channels belong, and that is
`embassy-net`.** A `&RefCell` stack cannot be made `Send` by inference,
so the property has to be manufactured: an actor owning the stack, and
proxies holding channel endpoints. What crosses is bytes, so the proxy is
`Send` whatever the stack is. Two things make it cheaper than the earlier
reading of it — **one multiplexer task rather than one per connection**,
which removes the compile-time `pool_size` objection, and a buffer the
caller sizes at run time rather than a constant. `alloc` and atomics are
required by this workspace's construction anyway, so neither is a new
demand.

So the two are complements rather than alternatives: **the macro for
stacks that have the property, a channel actor for stacks that do not.**
And the channel half is what would put `hclient::Client` on embassy —
which is the thing wanted three questions before this one, `Send` having
only ever been the gate in front of it.

**Taking RTN from nightly for this one crate was asked and the answer is
no, on arithmetic rather than on policy.** It removes the third of three
blockers. `no_std` still stands — every one of those eight stacks is a
`no_std` device and `http` 1.5.0 still refuses — so the adapter would
compile only against the std shims, where a caller has `std::net` and
reaches for tokio. The half-close still stands. So the trade is: pin a
published crate to nightly, which makes every consumer nightly, to fix a
third of a problem for nobody who can use the result.

**And the obvious objection — *embedded is all on nightly anyway* — is a
fair recollection of a state that has ended.** It was true for years, and
it is measurably false now:

| measured, 2026-08-28 | |
|---|---|
| `#![feature(..)]` in `embassy-executor` 0.10, `embassy-net` 0.9.1, `embedded-nal-async` 0.9.0, `embedded-io-async` 0.7.0, `embedded-hal-async` 1.0.0 | **none, in any of them** |
| `embassy-executor`'s `nightly` feature | **optional**, not required — this workspace builds it with `platform-std, executor-thread` and nothing else |
| `reqwless` 0.14.0, the incumbent client on this very seam | `rust-version = "1.91"` — **stable** |
| this repository | `rust-toolchain.toml` says `channel = "stable"`, and `hclient-rt-embassy`'s 19 live TAP tests pass on it |

The last row is the sharpest: **this workspace is itself the evidence**,
since the embassy runtime and its live scenarios are green on stable in
CI on every push.

So the objection inverts the conclusion rather than softening it. If the
embedded audience were on nightly, a nightly-pinned adapter would cost
them nothing; because they are on stable, it would exclude precisely the
people it exists for — and `reqwless` shipping at a stable MSRV means a
competitor requiring nightly starts behind for a reason that has nothing
to do with its merits.

The policy cost is real too and points the same way. `rust-toolchain.toml`
says `channel = "stable"`, and this file refuses even an **MSRV job** on
the grounds that a pinned version is a promise that goes stale while
looking maintained — a nightly pin is that argument at its maximum, since
the pin breaks on a schedule somebody else sets.

And nothing is lost by waiting: if RTN stabilises the question becomes a
stable one again, and by then `http`'s `no_std` status may have moved too.
What needed capturing was the measurement, not the crate.

It also means blocker three does not lift with blocker one. A half-close
is a method upstream could add; this needs either return type notation to
stabilise and stop ICEing, or NAL to move to associated future types,
which breaks its trait for every implementor.

So the day `no_std` becomes reachable there are three options and none is
free: accept the false half-close **and** a permanently `!Send`
transport, which is what a NAL-based client must do; ask upstream for a
shutdown on `embedded-io-async` and for named futures on both, which is
the correct fix twice over and is somebody else's release schedule; or
keep a crate per stack that owns its socket, which is what exists and is
why the reach is one stack instead of eight.

### The restriction is at `Client`, and the converter was built and then dropped

Asked whether `Client` should simply be restricted to `Send` transports
with a converter for embassy. It already is — `Client::builder` demands
`SendTransport` — and a converter was written, `hclient-actor`: an actor
owning the `!Send` transport, with a `Send` handle in front of it.

**It is deleted, and by this file's own test for a thing's existence.**
Nothing depended on it, in this workspace or outside; it held no
dependency a feature would otherwise spread, its two being
`futures-channel` and `hclient-core`; and the one configuration it served
is itself `publish = false`, on the measurement that `Native<Embassy, ..>`
has no deployment today. A converter with no subject is the shape
`UpgradeSupport`'s spare variants were deleted for.

The layering below is what survives it, and it is worth keeping because
the sharper question the converter was built to answer does not depend on
the converter existing: **if such a boundary can manufacture the promise,
can the `!Send` accommodation come out of the seams?**

**No, and it deletes embassy rather than simplifying it.** Measured:
declaring `TcpConnect::Connecting` as a `Send` box makes
`hclient-rt-embassy` fail to compile at the boxing site, because the
future holds `Stack<'d> = &'d RefCell<Inner>`. So embassy would not be a
`TcpConnect` at all, `Native<Embassy, ..>` would not exist — and the
converter would have **nothing to wrap**, because it operates on a
`Transport` and the seams are below it. A boundary can manufacture a
promise; it cannot manufacture a transport the seams refused to let
exist. That is why deleting the converter costs this argument nothing:
what it establishes is a fact about the seams.

The layering that falls out is worth stating once:

| layer | demands `Send` | why |
|---|---|---|
| `TcpConnect`, `Timer`, `Resolve`, `TlsConnect` | **no** | each implementor names its own future's auto traits (C15) — this is what lets embassy exist |
| `Transport` | **no** | its future is an RPITIT, unnameable, so nothing could ask |
| `SendTransport` | it **is** the demand | an impl may carry bounds the trait does not (C16) |
| `hclient::Client` | **yes** | it boxes its transport `Send + Sync` |
| a converter above it | could manufacture it | and cannot exist without the row above the first |

Each layer restricts exactly where it can, and the accommodation at the
bottom is what any converter at the top would have to work on — which is
the reason the bottom row cannot be tightened, converter or no converter.

**And the converter is a crutch, which is the honest word for it.** It
buys `Client` at the price of streaming: the response is collected before
it crosses, so a body larger than `Limits::max_response` is an error
rather than a stream. On a device that is the trade to think about twice.
What it is not is a workaround for a design mistake — the seams are right,
and buffering at a thread boundary is what a thread boundary costs.

### The embassy runtime is the workspace's only `!Send` counterexample

Asked whether `hclient-rt-embassy` is needed at all, and the workspace's
own test for a crate — *does it hold a dependency a feature would
otherwise spread* — gives the wrong answer here, because the value is not
a dependency.

**As a deployment runtime it has no configuration today, measured.** It is
not `no_std` and cannot be: `http` 1.5.0 still carries
`compile_error!("`std` feature currently required")`. So its
configuration is *std plus `embassy-net`* — and `embassy-net` is the
`no_std` stack. The only device the docs named, esp-idf, integrates
`embassy-sync`, `embassy-time-driver` and `embassy-futures` and **not**
`embassy-net`, handing you lwIP through `std::net` instead; the runtime
there is smol or tokio. The one place `Native<Embassy, ..>` actually runs
is a Linux host over a TAP device, which is this repository's CI.

**As a design counterexample it is now the only one there is**, and that
is load-bearing. Measured across the seam implementors: `hclient-rt-tokio`
boxes three futures `Send`, `hclient-rt-smol` two, `hclient-dns-doh` two
since this week, `hclient-tls-rustls` and `hclient-tls-native-tls` box
none at all — and `hclient-rt-embassy` boxes exactly one, plain, because
`embassy_net::Stack<'d>` is `&'d RefCell<Inner>` and the crate carries no
`unsafe impl Send` anywhere.

Delete it and every test stays green while three decisions quietly lose
their subject: `TcpConnect::Connecting` could become a `Send` box,
`SendTransport` would look like ceremony because every transport would
satisfy it, and `Transport::execute`'s unbounded RPITIT would look like
caution with no case. **That is `UpgradeSupport`'s deletion inverted** —
those four variants went because the distinction had one reachable side,
and embassy is what makes the second side reachable here.

It is pinned rather than described: `tests/seam.rs` asserts that
`Native<Embassy, ..>` **is** a `Transport` and **is not** a
`SendTransport`, with a real negative rather than an `assert_not` that
accepts anything.

**Both things that followed are done.** The crate's own doc leads with
what it is — the `!Send` witness — rather than with embedded reach it does
not deliver. And it is **`publish = false`** as of the first release:
publishing would promise a deployment configuration measurement says does
not exist, and a published surface is one that must not move. Not
publishing is free and reversible; un-publishing is neither.

It follows `hclient-rt-pair-check`'s shape exactly, down to carrying no
licence symlinks and no README — which is also what keeps the hand-check
below (*two per publishable crate, plus one*) true rather than off by two.
Flip it back the day `no_std` becomes reachable: `http` growing it, or
this workspace dropping `http` from its public API. Nothing else changes —
the TAP suite still runs on every push, and the seam test still pins the
counterexample.

### Channels do not transfer to embassy, and the reason is what crosses them

Asked whether the repair that put `hclient-fetch` under wasm threads works
for `hclient-rt-embassy` too. Measured rather than reasoned, and the
answer separates into three options of which the obvious one is the worst.

**The `!Send` is genuine, not a `dyn` erasing a property that was there.**
Measured against `embassy-net` 0.9.1: **zero** `unsafe impl Send` or
`Sync` anywhere in the crate, and `Stack<'d>` is `&'d RefCell<Inner>`. The
plain box in `Embassy::Connecting` is plain *because* the property is
absent — which is the opposite of `connect.rs`'s `Answers` and DoH's
streams, where a `dyn` was throwing away a property the concrete type had.

**What would have to cross is a stream, not a value, and that is the whole
difference.** `hclient-fetch`'s actor hands over one
`http::Response<Body>` per request and is done. What an embassy caller
holds is `EmbassyIo`, which implements `hyper::rt::Read`/`Write` and is
polled for the length of the exchange. So it would not be an actor, it
would be an **IO proxy**: every `poll_read` a round trip, and — since a
`&mut [u8]` cannot be lent across a channel — **an extra owned buffer and
an extra copy per read**. `EmbassyIo` already carries a 2048-byte scratch
per connection, deliberately a field because a stack local compiled to a
`memset` per call; a proxy adds another buffer and two channels on top, in
the resource a microcontroller has least of. And
`#[embassy_executor::task]`'s `pool_size` is fixed at compile time, so an
actor per connection makes the maximum number of concurrent connections a
constant in the API.

**What it would buy is a property that target cannot use.**
`embassy_executor::Executor` runs `!Send` tasks on one core, and a
`&RefCell` cannot be shared across executors at all — so on a dual-core
part the net stack still lives on one core and the socket cannot leave it.
There is nowhere to send to.

**The second option is real and costs streaming.** An actor one layer up —
at `Transport::execute` rather than at the socket — carries a *value*
again: `http::Request<RequestBody>` in (already `Send`), the response
**collected to `Bytes`** out. One channel per request instead of per read,
and no IO proxy. What it gives up is streaming, which on a device is
exactly the thing you keep when a response is bigger than RAM. It also
needs the transport `'static`, which the `StaticCell` idiom already
provides.

**And the third is the one that answers the actual want.** Nobody wants
`Send` on a single-core microcontroller; they want `hclient::Client` —
redirects, the jar, the cache — and `Send` is only the gate
`SendTransport` puts in front of it. The direct route is a client surface
that does not ask for it: eight `Send` declarations in
`hclient-core`'s erased module are what stand between the two, and a
parallel facade is the cost. That is the "second surface" question,
unchanged, and it is the one to answer rather than this one.

### Every backend is a `SendTransport` now, and the last one took a channel

**Six backends, six impls**, so every one of them can back an
`hclient::Client`: `hclient-native` (including its HTTP/3 arm),
`hclient-fetch`, `hclient-wasi`, `hclient-urlsession`, `hclient-winhttp`
and `hclient-mock`.

**`hclient-native`'s is conditional and that is the design working, not a
gap.** It implements `SendTransport` for every `Native` whose runtime, TLS
backend and resolver name `Send` associated futures, and for no other — so
`Native` over `hclient-rt-embassy` is still a `Transport` and simply not a
`SendTransport`. Nothing is excluded from the seam; something is excluded
from a promise. The one resolver that used to fail that test was
`hclient-dns-doh`, fixed one section up.

**`hclient-fetch` was the last, and it needed a channel.** Its
`execute_send` boxed `execute`'s future, which holds a `js_sys::Promise`
across its one await — fine on a single-threaded target, where
wasm-bindgen marks JS handles `Send` truthfully, and impossible under
`-Ctarget-feature=+atomics`, where it stops. `execute` is now three
pieces: a synchronous `start` needing `&self`, an async `finish` needing
**nothing** of `self` — which is what lets a `spawn_local` task be
`'static` with no `Arc` and no `Clone` bound — and a `report` that emits
the hook where `&self` already is. `Transport::execute` is untouched, so
the spawn is paid for only by a caller who wants `Client`.

**What a spawn puts at risk is the drop-is-cancellation contract**, since
a spawned task does not stop because its spawner went away. `deliver`
races the work against `Sender::cancellation` and is a named function so
that `tests/deliver.rs` can be the pair that pins it — a `deliver`
ignoring cancellation passes one and fails the other, checked by applying
that mutation.

**The check that guarded the old state gained a direction rather than
being deleted.** `fetch-under-wasm-threads` now asserts the library
**builds** under threads *and* that `SingleThreaded<T>`'s `unsafe impl
Send` is still rejected there, `E0277` in `tests/promise.rs`. The second
is what the old recipe was really protecting and is unchanged; the first
would have been false the day before.

**What is left `!Send` is nothing** — measured from outside on the day, and
the one honest asterisk is that a JS `WebSocket` belongs to the realm that
made it, so `hclient-fetch`'s `WebSocketConnect` seam declares no `Send`
and asks for none.

### `Client` in wasm: everything but the constructor is the same source

Asked whether `Client` can be used in wasm without changing code, and
measured on one scratch crate rather than reasoned about. A function that
takes `&Client`, builds a request, sends it, calls `error_for_status`,
`collect` and `json` compiles **unchanged and with no `#[cfg]` at all** on
`x86_64-unknown-linux-gnu`, `wasm32-unknown-unknown` and `wasm32-wasip2`.
It does not even need the `default-transport` feature — a consumer that
takes a `Client` from its caller names no backend.

**Construction is the one place that differs, and it differs three ways:**

| target | how a `Client` is made |
|---|---|
| native | `Client::new()?` — fallible, the OS trust store can fail |
| `wasm32-unknown-unknown` | `Client::new()` — infallible, `Fetch::new()` cannot fail |
| `wasm32-wasip2` | **`Client::new` does not exist**: `DefaultTransport` is undefined there, so it is `Client::builder(WasiHttp::new()).build()?` |

The third is deliberate and recorded far from the other two — `hclient`
does not depend on `hclient-wasi`, so a WASI build names its transport.
Worth stating together, because *"there is no `?` on it — that is the only
difference"* is true of the first two and silent about the third.

So the portable shape is the ordinary one: construct at the top, where the
entry point is target-specific anyway, and pass `&Client` down.
`crates/hclient/examples/portable.rs` is exactly that and is built for all
three targets on every push, so this is a CI gate rather than a claim.

**One wrinkle the measurement exposed, and it is now handled.** Forgetting
the `?` produces `Result<Native<..>, ..>` where a transport was wanted, so
the new `on_unimplemented` note offered to implement `SendTransport` for a
`Result` — sound advice for the wrong problem. It now says first that a
`Result` here means a missing `?`, and why portable code meets exactly
that.

### The mock was built for this workspace's tests and not for anybody else's

`hclient-mock` is used by 35 files here and had **no test of its own and
no doc example**. Asked whether it is good for a library user writing unit
tests, the only honest way to answer was to write one from outside the
workspace — the same instrument that found `require_version` missing. It
took three failed compiles, and the fourth still could not make the
commonest assertion there is.

**Five walls.** `push_response` took `&'static str`, so a body built at
run time did not compile. `MockTransport` was not `Clone`, and
`Client::builder` takes its transport by value, so a test had to hand the
mock over and reach back through `transport_as`. **The request body was
not recorded at all** — only its `size_hint` — so *"my code posted the
right JSON"* could not be written. There was no way to ask whether a
scripted response went unused. And there was no example of any of it.

All five are closed. What is worth carrying forward is **why the obvious
fix to the third one is wrong.** Recording a `Rewindable` body by calling
its factory broke `hclient`'s
`a_rewindable_body_is_replayed_from_the_snapshot_taken_before_the_first_attempt`
inside a minute: that test counts factory calls to pin *one snapshot per
hop, not one per attempt* — a claim about the **client** — and the mock
had become a second caller. Purity is not the question; the contract makes
the *result* the same and says nothing about a caller counting calls. So
the factory is handed to the test as a factory and `snapshot()` is the
opt-in, where the extra call is the test's own choice. This crate's own
doc, applied to itself: *a faithful model of a backend, not something that
masks the defect under test.*

`RecordedBody` therefore has four cases rather than an `Option<Bytes>` —
"no body", "a body this mock will not read for you" and "a body nothing
can read twice" are different facts, and a streaming body is
`NotRecorded` rather than `Empty` because a silent empty would pass a test
that an honest refusal fails. It is `PartialEq` and deliberately not
`Eq`: a closure cannot keep reflexivity.

**Request matching is refused with a reason**, not omitted: the flows this
double exists for — a redirect chain, a `425` replay, a retry — are
**ordered**, and a matcher would let a test pass while the code made its
requests in the wrong order.

**The pattern across three findings this week is now clear enough to
state.** `require_version` was unreachable from the builder while the file
testing it built requests by hand; `hclient-mock` was comfortable for
tests written beside it and awkward for tests written against it; and the
CLI's backend refusal could not fail in the configuration CI runs. In each
case the workspace's own tests were the wrong instrument, because they
share the author's knowledge of where the doors are. **Writing a consumer
is a different measurement from writing a test.**

### `hc` gained `--sse` and `--ws`, and found two defects it had shipped all along

Both features are ordinary; the two things it turned up on the way are
not.

**`-L` was a flag that did nothing, and that is the *silently ignored
setting* defect inverted.** `Client` falls back to `Limit::default()` —
ten hops — when no policy is set, so `hc` followed redirects with or
without `-L`: measured against the built binary, same body and same exit
code either way. The usual defect is a setting the code ignores; this was
a setting that was silently already **on**, which no amount of reading
the flag's own code would show. It is `Forbid` without the flag now and
`Limit(--max-redirects)` with it — `Forbid` rather than `Limit::new(0)`,
because one is an answer and the other an error. **This changes `hc`'s
default**, to what curl, httpie and xh all do. The rule it earns: a CLI
must state *both* arms of a boolean flag, never only the `true` one,
because the library's default is not the tool's.

**`hc` corrupted every binary body on a pipe, for the CLI's whole life.**
Everything went through one `anstream::AutoStream`, and with colour off
that stream is an **ANSI parser**: it deletes bytes it cannot read as
text. Measured on the built binary — a PNG's `89 50 4e 47 0d 0a 1a 0a`
arrived as `50 4e 47 0d 0a 0a`; `ColorChoice::Always` is byte-exact and
`Never` is not.

That is the *wrapper was the limitation* shape a fourth time, after
`native-tls`'s ALPN, `hclient-fetch`'s `!Send` body and `TlsInfo` in
`hclient-native` — **and it is the worst of the four**, because the other
three were silent absences where this one silently broke a promise
`output.rs`'s own opening paragraph makes about bytes surviving. Payload
bytes bypass the filter now.

**A mutation that hangs is worse than one that fails**, and a suite that
drives a subprocess needs a watchdog for exactly the mutations whose
defect is non-termination. Making `--sse` reconnect unconditionally
produces *no answer* rather than a wrong one, so four of that mutation's
eight kills land on a 20-second watchdog rather than on an assertion.
`hclient-tungstenite`'s `BOUND` is the precedent; this is the rule behind
both rather than two local decisions.

**The `-j96` flake instrument earned its keep a fourth time.** A real
race in the new `--ws` code reproduced 7 times in 12 there and never at
`-j16` or `-j32`: `tungstenite`'s `read` can lift a data frame *and* the
close behind it off the socket in one go, so a `start_send` at stdin EOF
answers *"Sending after closing is not allowed"* — exit 4 on a session
that delivered every byte. The fix is two guards, and they were
discriminated properly: removing either alone gives 0 failures in 12,
removing **both** gives 7. So the suite pins their conjunction and
nothing pins either half, which is written where they are so that
neither is deleted as redundant.

**Two library facts the CLI work established**, both reasons rather than
defects. `SseStream` owns its `Response` and exposes neither `status()`
nor `headers()`, and `SseBuilder` carries a URL and headers and nothing
else — no body, no query, no redirect policy, no `require_version`. That
is why `--sse` refuses six flags by name instead of accepting them into
silence, and it is what would have to change for `--print h` over a
stream. And `hclient_proto::sse` strips one leading space after a
comment's colon, so a faithful re-serialisation is `": {text}"` — the
kind of thing only a round-trip test finds.

**The `--ws` obstacle was the one predicted and the fix is the better of
two.** `Tungstenite` borrows a `Native` and `backend::build` returned an
erased `Client`, so construction is split into *make the transport* and
*wrap it*. `transport_as::<Native<..>>()` was the alternative and is
worse: it returns an `Option` because nothing checked at `build()` which
backend is inside, so it answers a question at run time that splitting
removes. Both paths still go through `backend::choose`, so `--backend`'s
refusal is intact — checked by making `--ws` bypass it and watching the
exit code go from 3 to 4.

### Authentication became a seam, and the measurement says that is where the demand is

Digest was a hard-coded `401` branch in `Client::run`. It is now
`hclient::auth`: [`Auth`] is the configuration and [`AuthFlow`] is one
exchange's state, made fresh per hop. `Digest` is the one scheme this
crate ships, written on the seam like any other.

**The reason is a measurement rather than a taste for abstraction.** NTLM
and Negotiate need a platform's own security provider, and those crates
have real users — `libgssapi` at **565,785** downloads a month, `sspi` at
**561,893**, `cross-krb5` at **496,315**. What does not exist on
crates.io is any HTTP glue over them: there is no `reqwest-ntlm` and no
`hyper-ntlm`, because those clients have nowhere to put one. So this
crate does not grow a Kerberos dependency; it grows the two traits
somebody else needs to write one in their own crate.

**Two traits, for the reason the redirect and retry policies have one
each and a client-owned counter.** The scheme is shared by every clone of
the client; the *flow* is one hop's state, because a scheme with three
legs has to remember which leg it is on and a shared value cannot. It is
httpx's generator-based `Auth` — `response = yield request` — written as
a state machine, which is what Rust has instead.

**Three rules the client enforces and a flow cannot override**: a body
that cannot be replayed ends it (`retry_kind()`, asked before every extra
leg exactly as for `425` and a retry); credentials do not cross an origin
(digest's rule, now every scheme's); and `MAX_LEGS` bounds a flow that
never says `Done`, which would otherwise be an infinite exchange against
a server that keeps challenging.

**The first shape had two flows and the tests killed it in one run.** One
was made before the send and one after the response, so the second had
never seen the request — and digest hashes the method and the
request-target into `A2`, so it had nothing to hash. One flow per hop,
made before the first send, and it sits *outside* the retry loop: a retry
re-sends the same request, and a flow counting legs must not count an
attempt that failed for the network's reasons.

**`on_response` is given only the response, deliberately**, and the
consequence is that a flow stashes what it needs in `authorize` — which
the trait's contract makes safe, since `authorize` runs before every
send. `DigestFlow` does exactly that. The alternative, passing the method
and target to both methods, duplicates what the flow has already seen.

**And `cargo fmt` deletes a marker rather than moving it, which is new.**
This file records that `cargo fmt` moves a trailing comment off a line it
reflows, so a `send-bound-exception` marker on a `pub trait X: Send {`
line is lost. The obvious repair — a trait-level `where Self: Send,` —
is **worse**: `cargo fmt` removes that comment outright. Reproduced in
isolation before it was believed.

What that forced is better than what it broke: **neither trait declares
an auto trait at all.** `Send` is demanded where the facade *stores* the
value — `BoxedFlow` and `SharedAuth`, one line each — which is this
workspace's own rule for a seam, arrived at from a formatter rather than
from the argument.

### OpenTelemetry landed as a transport decorator, and the seam it wanted was the one it could not use

`hclient-otel`: `Instrumented::otel(transport)` or
`Instrumented::tracing(transport)`, one line at `Client::builder`, a span
per request with the OTel HTTP client attribute set, and `traceparent`
and `baggage` on the wire. `docs/otel-design.md` is the design and now
also the record of six places it was wrong.

**The obvious home was `Hooks` and it is not close.** `fn on(&self, event:
&Event<'_>)` takes an immutable event, `&self`, and returns nothing, so
**nothing reachable from a hook can put a header into an outgoing
request** — which is what that seam is *for*. Two smaller facts point the
same way: there is no request-start event, `Event` being the life of a
connection plus a head plus octets, and `fn hooks` is declared on four
backends of six. `Transport::execute` gives both halves away already: the
request arrives by value, so headers are editable, and `Self::Body` is an
associated type, so the body is wrappable — which is what makes the
duration the exchange's rather than the time to first byte.

**Its own crate by the local test, measured rather than asserted.**
`cargo tree -e normal`: `hclient-core` alone is 13 crates, `hclient-otel`
is **16** with `otel`, **18** with `tracing`, **19** with both, and **29**
on `wasm32-unknown-unknown`, where `opentelemetry`'s clock reaches for
`js_sys::Date::now`. A feature of `hclient` would put `opentelemetry` into
every graph in any tree that switched it on.

**And that measurement reversed the design's own arithmetic.** §8 said
`opentelemetry` depends on `tracing` already, so the second front adds no
graph. It depends on it only through the default `internal-logs` feature,
which this crate switches off — so the fronts are additive, and the one
that *propagates* is the **smaller** by two, because `futures-core`,
`futures-sink`, `pin-project-lite` and `thiserror` are already in the
client's graph and `tracing` brings `tracing-core` and `once_cell` that
are not. `otel` is therefore the default feature and `tracing` is not,
which is the opposite of what was planned.

**The two fronts are chosen at the constructor and never by a feature.**
Cargo unifies features, so a feature deciding what a *built* decorator
does would let a neighbour's build add a second span per request to this
one — `Collected::text`'s rule one crate over, that a call must not change
meaning with a feature. A feature decides which constructors exist.

**`tracing` emits and cannot inject, and that is structural.** A
`tracing` span's identity is a `tracing::span::Id` from whatever
subscriber is installed; `traceparent` is a W3C trace-id, and there is no
value to write. The tempting repair — with `opentelemetry` also compiled
in, inject `Context::current()` — is worse than the absence, and the
first draft of this paragraph was **wrong about why**, which is the whole
reason it is worth a paragraph. It said that context is empty under
`tracing-opentelemetry`. Measured against 0.33 on a scratch consumer:
that bridge has `with_context_activation`, on by default, and the current
context inside `execute` names the **enclosing** span — the caller's —
because activation happens on span *entry* and this crate never enters
the span it opens. So the repair writes the **wrong parent**: the
server's span comes out a sibling of the client span rather than its
child. Silently. Said at the constructor instead.

**The same run paid for itself twice.** It confirms the front's actual
value — under the bridge this span exports as an OTel span named `GET`,
kind `Client`, parented on the caller's span and sharing its trace, so
*"OTel for nothing"* is true of everything but the header. And it found a
defect no reading would have: `tracing-opentelemetry` maps a `u16` or a
`u64` field to a **string** and only an `i64` to an integer, so
`server.port`, `http.response.status_code` and `http.request.resend_count`
— all `int` in the registry — were arriving at collectors quoted. Every
number the `tracing` front records is an `i64` now, and the test reads
the `Visit` method each field arrived through rather than its rendering,
because a plain subscriber renders the two identically.

**Three attribute decisions the specification makes and the design got
wrong.** `http.request.method_original` was recorded as *absent, and that
is an answer*; normalising an unknown method to `_OTHER` is a **MUST**,
and it is also what bounds the span name — without it a caller who invents
a method per request puts it in the name, which is the cardinality
blow-up the `{method}` rule exists to prevent. `error.type` had one arm
and has two: a status that indicates an error is reported as the **status
number as a string**, without which a span for a `500` carries an `Error`
status and nothing an aggregation can group by. And `url.full`'s redaction
is userinfo *and* seven named query-string keys — a presigned URL carries
its signature in the query, which is the commoner case by far.

**`resend_count` is `hop + resend` and the field names invite the wrong
mapping**, which is the one thing the design got most emphatically right:
`Attempt` splits the total on a line OTel does not draw, and reading
`resend` alone reports nothing for the third hop of a redirect chain —
exactly the case the attribute exists for. Both halves travel beside it
as `hclient.hop` and `hclient.resend`, because *third send, first hop* and
*first send, third hop* are different failures.

**One mutation survived on purpose, and the measurement is why it is
kept.** Emptying `Recorder`'s `Drop` leaves all 39 tests green, because
both fronts close themselves — `opentelemetry_sdk::trace::Span` has an
`impl Drop` and a dropped `tracing::Span` fires the registry's
`on_close`. The impl stays because `opentelemetry::trace::Span` is a
**trait with no `Drop` requirement** and the API crate carries no `impl
Drop` on any span type: a provider whose spans do not self-end is
conforming, and this crate hands its spans to whatever the application
installed. Leaving the close to the SDK's convenience would make the
promise the SDK's. The killable half of the same rule is
`SpanBody::poll_frame` calling `end` at end-of-stream, and its test is
written with the body **still alive** — a body read to its end and then
dropped looks the same under either mutation.

**And a chain of redirects is flat rather than nested**, which OTel's
model chooses: one client span per request, and a redirect is a resend.
With no ambient span each hop is the root of its own trace; with one, all
three are children of the caller's span and none of each other, which is
what the test asserts rather than merely asserting one trace id.

### An axum app is testable in process, and the seam that allows it is the one reqwest has not got

`hclient_tower::app::AppTransport` makes a `tower::Service` a
[`Transport`], so a test drives the **real** `hclient::Client` — redirects,
the jar, the cache, retries, decompression, `.json()` — against a **real**
`axum::Router`, with no socket, no port and nothing spawned. httpx's
`ASGITransport`, in Rust.

**It exists here rather than anywhere else for a structural reason.**
`reqwest` has no `pub trait Connect` — measured in 0.13.4, zero hits — so
testing an axum app against it means binding a port. And what
`tower::ServiceExt::oneshot` already gives is a *service call*, not a
client: the `http::Request` is assembled by hand, no redirect is followed,
no cookie is stored, nothing is decompressed. The difference is the whole
of `Client`, and the test that pins it walks a `302`, stores a
`Set-Cookie` and presents it on the next hop — one `send()`, two hops, no
socket.

**The claim is executed rather than asserted.** `tests/axum_router.rs`
runs a real `axum::Router`, because a claim about a third party is exactly
as perishable as the check behind it — this file's own rule, and the
reason `axum` is a dev-dependency here. It costs 22 crates with
`default-features = false`, carrying neither tokio nor hyper, which is
what made proving it affordable.

**Two pieces were missing and both are boundary work.** `RequestBody` is
not an `http_body::Body` — it is an enum with a factory arm — so
`OutgoingBody` is the view that makes it one, built through
`RequestBody::reduce` rather than by matching: the enum is
`#[non_exhaustive]` now, so a match here needs a wildcard, and a wildcard
is where a new variant goes to be silently mishandled. `Reduced` is
exhaustive, owned by the crate that would add one, and already carries the
factory arm's depth bound.

And the response body needed mapping the other way. `BoxedTransport`'s
blanket impl requires `<T::Body>::Error: Into<Error>`, which a server-side
body does not satisfy — `Full`'s is `Infallible`, axum's is `axum::Error`.
Without `IncomingBody` an `axum::Router` could be a `Transport` and still
not back a `Client`, which is the whole point. **The transport is the
boundary, so the conversion belongs there** and not in a caller's test.

**The authority is named at construction and any other is refused.** An
in-process service has no origin and a client needs an absolute URI —
there is nowhere to resolve `Location: /other` against otherwise — so
httpx invents `http://testserver` and so does this. What is added is the
refusal: a test that names a real host would otherwise be *answered by
the local router*, and pass while reaching nothing. Checked by mutation:
removing the check kills exactly that test.

**The body mapping is the caller's one line, deliberately.**
`axum::Router` takes `axum::body::Body`, so the call site writes
`app.map_request(|r| r.map(axum::body::Body::new))`. A type parameter and
a stored closure on `AppTransport` would spare that line and would be this
crate reimplementing `ServiceExt::map_request`, which the caller already
has.

### Retry took the same shape, and the asymmetry between the two is the finding

`RetryPolicy` is a trait with one method, `Standard` is the configuring
struct that implements it, and the retry predicate is gone the way the
redirect one went. `SafeMethodsOnly`, `Never`, `RetryFromFn`, `RetryAll`
and `.and(..)` are the rest.

**But the two operations are not symmetrical, and forcing the symmetry
without saying so would have been wrong.** Following a redirect is what
happens *unless* a policy objects, so composing there is a vocabulary of
restrictions and the trait's default is `Follow`. Retrying happens *only
because* a policy permitted it, so the trait's default is `Stop` and
`and` narrows from **one configured permission** rather than from *yes*.

The consequence is a rule that needed writing down: **composing two
permitters gives their intersection, not their union.**
`Standard::default().and(OnStatus(..))` does not mean "unsent errors *or*
these statuses" — it means neither, because each refuses what the other
allows. To retry *more*, widen the one `Standard`; to retry *less*, `and`
a guard onto it. The same `and` reads as pure restriction one module over
because there is nothing to widen there.

`RetryAll`'s empty case is the same asymmetry once more: an empty
`redirect::All` permits everything and an empty `RetryAll` stops, because
the identity of a meet is the operation's default and the two defaults
are opposite.

**`SafeMethodsOnly` is what the whole method-safety argument was owed.**
This workspace states in four places that `RetryKind` answers *can this be
sent again* and nothing answers *may this be repeated* — and until there
was a guard seam, that principled refusal had nowhere for a caller to act
on it. It is now a named type: `GET`, `HEAD`, `PUT`, `DELETE`, `OPTIONS`,
`TRACE` and `QUERY`, the last included because it is safe and idempotent
by its own specification, which is the same reason this workspace does
not group it with `POST` for redirects.

**One capability was deliberately lost.** The old predicate was handed the
settled delay and could refuse a wait it found too long. The verdict now
*is* the delay, and `and` takes the **longer** of two — so a guard can
lengthen a wait and cannot shorten one. That is not an oversight: waiting
less is the less careful answer, and `Standard::max_retry_after` already
caps by **stopping** rather than by waiting less, which is the whole
reason `Retry-After` is refused rather than clamped.

**`jitter` moved onto the proposal**, so the policy stays a pure function
of its inputs — the client owns the entropy exactly as it owns the hop
count one module over, and for `Backoff::delay`'s own stated reason.

`crates/hclient/src/predicate.rs` is deleted. It had held two verdicts,
two proposals and two boxed closures; what survived is one error type,
which now sits beside the other redirect errors in `client.rs`. A module
named after a concept that no longer exists is worse than no module.

### The redirect policy is a trait now, and the predicate stopped being a second concept

`RedirectPolicy` was a `Copy` enum in `hclient-proto` and
`RedirectPredicate` was a boxed closure in `hclient` — two things for one
job, the second existing only because the first could not hold a closure.
Both are gone; there is one trait with **one method**:

```rust
fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict
```

`Forbid`, `Limit::new(n)`, `SameOriginOnly`, `HttpsOnly`, `FromFn`, `All`,
and `.and(..)` to compose. One setter where there were two, and one
`check_supported` branch where there were two.

**The line between policy and mechanism was drawn by measurement, not by
taste.** `decide` does seven things and only one was ever policy — the hop
limit. The rest is RFC: which statuses redirect, whether a `Location`
parses, RFC 3986 §5.2 resolution, what an origin is, RFC 9110 §15.4's
method table. The rule that separates them is *two correct clients could
disagree about it*, and it was checked against curl 8.18: there is a flag
for **every** step this rule calls policy — `--max-redirs`, `--post301/2/3`,
`--location-trusted`, `--proto-redir` — and **none** for any step it calls
mechanism. No client exposes a way to resolve a `Location` differently,
because doing it differently is an open redirect rather than a preference.

**So two more things became policy, and they are exactly curl's two.**
`Allow { preserve_method, keep_credentials }` — the 301/302/303 rewrite to
`GET`, and whether credentials survive a cross-origin hop. Granted **per
hop**, which is more than curl's three flags give: a policy can trust one
destination without trusting every future one, and can answer differently
per status without needing a parameter.

**`Allow` carries booleans and never a request, and that is the invariant
the type lives under.** A verdict carrying the rewritten request was
proposed and refused for two reasons. Composition: two policies answering
`Follow(req_a)` and `Follow(req_b)` have no defined meet, where two
`Allow`s meet field-wise. And escape: handing over the request hands over
the `uri`, which is the open-redirect surface the mechanism exists to
keep closed. **The rule is written beside the type — `Allow` may only ever
gain fields that switch a protection off.**

**One rule for all composition: the more conservative answer wins.**
`Follow < Stop < Refuse`, two `Follow`s meet their `Allow`s, and the chain
short-circuits at `Refuse` and **not** at `Stop` — because nothing can
raise a `Refuse`, where a later policy can raise a `Stop`. A pair of tests
pins exactly that: a `Stop` still asks the rest of the chain, a `Refuse`
does not.

**Order is unobservable, which is what separates this from middleware.**
Middleware composes as function composition and can rewrite; policies
compose as a meet on a lattice and can only narrow. That distinction is
why redirect-following is not expressible as a `tower` layer here and is
not exposed as one anywhere else either — OkHttp keeps
`RetryAndFollowUp` in its fixed chain, between the two user positions,
which is where `Client::run` already sits.

**The limit moved from a separate method into `follow`**, because the hop
count is already on the proposal. An `AtomicU8` inside the policy was
proposed and cannot work: the policy lives inside `Client`'s `Arc`, shared
by every clone and every request in flight, so the counter would be
per-client — `Limit::new(10)` would allow ten redirects for the life of
the program. `hops` is a local in `run`, per operation, which is where
per-request state belongs. **The policy holds the rule, the client holds
the state** — the same rule that already keeps a policy from recomputing
what an origin is.

**Four costs, each real.** Two policies are two types, so an `if`/`else`
choosing between them needs an `Arc` where an enum needed nothing —
`examples/portable.rs` shows it. The visited-URI list is now always
collected, where it used to be gated on a predicate being installed,
because one trait cannot be asked whether it reads history — a `Vec<Uri>`
bounded by a `u8` against as many round trips. The limit is consulted
*after* the `Location` is resolved rather than before, so a malformed
`Location` on the hop that would have exceeded it now reports
`InvalidLocation` rather than the limit; no hop goes anywhere different.
And "follow with no limit" became expressible by accident — the default
policy is still `Limit::new(10)`, so only a caller who *replaces* it is
exposed, and `hops: u8` is the structural backstop.

**A gap surfaced on contact, and it is ACT's finding one type over.**
`ProposedRedirect` had no public constructor, so a caller could not unit-
test their own policy — the wall `Response` was found to be. It has one
now.

**And the error got better rather than worse.** `TooMany(u8)` said one
thing and could not say the others; `RedirectRefused` carries the policy's
own reason plus the client's hop count, so *"refused a 302 to X after 10
hops: redirect limit reached"* covers a limit, an origin rule and a
caller's own closure with one shape. The number is the client's fact, not
the policy's, which is why a `&'static str` verdict loses nothing.

### A retry that is safe by construction, and the one gap the ecosystem measures

The competitive analysis in this workspace lists gaps against other
clients. Measured from the other direction — what people **bolt onto**
`reqwest`, in recent monthly downloads — the picture inverts:

| bolt-on | per month | here |
|---|---|---|
| `reqwest-middleware` | 15,551,838 | `hclient-tower` |
| **`reqwest-retry`** | **7,657,595** | **was missing** |
| `reqwest-tracing` | 4,516,360 | hooks, in the box |
| `reqwest-eventsource` | 2,510,406 | `hclient::sse` |
| `http-cache-reqwest` | 859,999 | `hclient::cache` |
| `reqwest-websocket` | 564,728 | `hclient-tungstenite` |
| `reqwest_cookie_store` | 346,932 | `hclient::cookie` |

Over 32 million downloads a month of things that are already inside this
crate, plus one that was not. That is ACT's *the gap is a pointer, not a
feature* at ecosystem scale.

**One candidate was killed by the same measurement**: `rvcr`, record and
replay, is at **437** downloads a month. Nobody in Rust wants a VCR, and
knowing that before proposing it is worth more than the feature would
have been.

**`ClientBuilder::retry` is the gap, and it comes out better in kind
rather than in degree.** A retry crate that wraps a client sits *above*
the transport, so it cannot tell a request that never left from one a
server received and acted on — both are an error — and must therefore
repeat both or neither. Here they are different values:

- `Error::is_unsent()` is a claim a **transport** makes at a site where it
  knows. `hclient-native` marks it where the Happy Eyeballs race ends with
  no connection and where no attempt was launched at all.
- `RequestBody::retry_kind()` answers *can this be sent twice* **before**
  the first attempt. A `Streaming` body is `Impossible` and no policy
  overrides it.

**The trap that makes the first of those necessary was found by reading
rather than by testing.** `ErrorKind::Connect` looks like it means
*nothing was sent* and does not: `hclient-native` classifies a response
head over `H1Opts::max_headers` as `Connect` too, on hyper's reasoning
that nothing usable came off the connection — and that happens **after**
the request went out. A retry deciding from the category would resend a
request the server had processed, which is exactly the guess this whole
design exists not to make. Two tests differing only in the mark, with
identical `ErrorKind`, are the pair that pins it; mutating the decision
back to the category kills one and leaves the other green.

**`Retry-After` that cannot be honoured stops the retry rather than being
rounded down.** A server asking for longer than `max_retry_after`, or
sending the `HTTP-date` form this module deliberately does not parse, ends
the retry — because waiting *less* than a server asked is the one
behaviour the header exists to prevent, and a client that caps the value
and retries anyway has turned a limit into a violation. The date form is
unparsed on purpose: reading it needs a calendar, `hclient-proto` is the
sans-io leaf whose dependency count is guarded, and the narrowing has a
direction.

**And the signature takes a clock because the alternative hangs.** The
first version was `retry(policy)`, using the client's own timer. Without
`default-transport`, `DefaultClock` is `NoClock`, whose `Sleep` is
`std::future::Pending` — so that version compiled everywhere and **hung
for ever** at the first backoff. Found by `just test-no-default`, where
four tests timed out at 300 s rather than failing; a hang is worse than a
panic and much worse than a refusal, so the precondition moved into the
signature, where `total_timeout` already had it. The tests supply a clock
whose sleeps are instant, which also means every assertion in them is
about the *decision* and never about a delay.

The policy itself is in `hclient-proto`: `RetryPolicy::decide` is a pure
function of a rule and one outcome, so every rule is tested with no
socket, no clock and no entropy. It is deliberately **not**
`#[non_exhaustive]`, on `TcpOpts`' argument — its whole use is
`RetryPolicy { statuses: .., ..Default::default() }`, and the attribute
forbids exactly that from outside the crate.

### The handshake was described three lines above where it was thrown away

Asked whether `hc` can show the TLS handshake the way `curl -v` does. It
could not, and the reason was not the CLI's.

`TlsInfo` has carried `protocol_version`, `cipher_suite`, `alpn` and
`peer_certificates` since TLS became a seam here, and
`hclient-tls-rustls` fills every one of them — the version normalised to
the dotted `TLSv1.3` spelling curl prints, the suite as its IANA registry
name. `hclient-native` receives it, binds it to a local, and **reads that
local three lines above the line where it emits `Connected`**, to pick
the protocol. Measured: `TlsInfo` and `tls_info` appear **zero** times in
`hclient-core` and in `hclient`. Nothing above the transport could see
any of it.

**That is the third sighting of one shape**, after `native-tls`'s ALPN
and `hclient-fetch`'s `!Send` body: *the limitation belongs to the
wrapper, and the layer beneath has the thing.* Here the wrapper was our
own transport rather than a third party's.

`Connected` gains `tls_version`, `tls_cipher` and `alpn`, and **the
freeze is what made that free**: the struct became `#[non_exhaustive]`
during the API-stability work, so adding three fields is not a breaking
change. They are borrowed `&'a str`/`&'a [u8]`, because `Connected<'a>`
already had the lifetime and the `TlsInfo` outlives the emission. They
are **strings rather than enums** — the vocabulary is the TLS registry's,
which gains entries, and a backend that meets a version this workspace
has never heard of should be able to say so instead of being pushed
through an `Other` arm.

One setter for all three, not three, because they come from one place: a
backend either read the handshake's outcome or it did not, and three
setters would let a caller set two and forget the third — which is
exactly what `Native::hooks` dropping the `1xx` installer while keeping
its capability already cost this workspace once.

**`None` means two things and the pair separates them.** Over `http://`
there was no handshake, and `timing.tls` is `None` too; over `https://`
it means the backend does not describe it, which is
`hclient-tls-native-tls`, whose own module doc says the platform stacks
expose no getter. So `(timing.tls, tls_version)` answers *no TLS* against
*TLS this backend will not describe*, and `hc -v` prints three different
things accordingly — including a line saying the connection was
encrypted and undescribed, because silence there would read as
plaintext.

`alpn` is deliberately **not** a second statement of `version`:
`version` is what the transport will speak, decided by the same function
that picks the handshake, where `alpn` is what the peer said. They agree
on every connection this workspace makes, and a hook that finds them
disagreeing has found something worth reporting.

Certificates are not exposed, and the reason is a dependency rather than
a decision: `TlsInfo::peer_certificates` is DER, and turning it into
curl's subject/issuer/dates needs an X.509 parser. The field is there for
whoever wants to add one.

### `hclient-fetch` ran zero tests on the main gate, and the fix was not the one that was built

Asked whether the browser body's channel machinery — the pump, the
cancellation, `poll_frame` — was the same pattern as the embassy
converter's. It is, at two granularities: `hclient-fetch` sends **frames**
over an `mpsc` and keeps streaming, where `hclient-actor` sent **one
collected value** over a `oneshot` and did not. So "an actor costs
streaming" was a fact about the granularity chosen, not about actors.

Measured, and the overlap was real: of `hclient-fetch/src/body.rs`'s 133
code lines, **64 mentioned nothing of JS at all** — `pump` had already
been written taking a `Stream` rather than anything of the browser's —
against 69 that are `web_sys` throughout. Nothing like the 20-of-402 the
date parsers turned out to be.

**So the shared half was extracted to `hclient-core`, and then put back,
and the round trip is the finding.** Two things happened in between.

`hclient-actor` was **deleted**, which took the second consumer with it —
and with one consumer the extraction is `futures-channel` costing +1 crate
to `hclient`, `hclient-core` and `hclient-rt-embassy` to serve a backend
that already had it. The workspace's own test for a boundary — *does it
hold a dependency a feature would otherwise spread* — has no subject when
there is nothing to spread to.

The argument that replaced it was that the **tests** would move onto the
main gate, and it was true and did not need the move.
**`hclient-fetch` runs zero tests in `cargo nextest run --workspace`** —
all 13 of its binaries are `#![cfg(target_arch = "wasm32")]`, so every
property it asserts is guarded by `wasm-pack test --headless` alone, the
job this file records silently failing to compile for six merges. But the
crate **builds on the host** — nextest was compiling those 13 binaries and
finding nothing to run — so a `#[cfg(test)] mod tests` inside `body.rs`
runs on every push with no browser, and needs no crate to move anywhere.

Five tests where there were none, and the cancellation pair among them:
mutating the pump to watch only the channel kills
`dropping_a_body_whose_read_will_never_answer_still_cancels` and leaves
`dropping_a_producing_body_stops_the_pump` green, which is what says
neither covers for the other.

**One of them caught a real detail on the first run**, and the assertion
was wrong rather than the code: the pump asks the dead source once more on
its way out, because `select` polls the stream before the cancellation, so
the turn that learns of the drop has already been told `Pending`. The test
now bounds the extra poll instead of forbidding it — an equality there
would have been pinning `futures_util::select`'s internals rather than
this pump's promise, which is that it stops.

**The rule this leaves behind**: *"these two crates share code" and "this
code is untested where it runs" are different problems, and only the first
is answered by moving it.* The extraction was the right instrument aimed
at the wrong defect, and aiming it correctly cost one `mod tests` and no
dependency at all.

### The first consumer reported, and the two costliest findings were things we already had

ACT ported four call sites onto `0.1.0-alpha.2` — a WIT fetch, an OAuth
flow, an OCI blob fetch and a `wasi:http` host — and no crate in that
workspace names `reqwest` directly any more. The report is the first
outside measurement this project has had, and what it measured is not the
API.

**Two of its findings are one finding, and it named the finding itself:**
*the gap is a pointer, not a feature.* It hand-rolled
`url::form_urlencoded::Serializer` twice, six lines each, for want of a
`.form()` — which **exists, behind no feature, in the very version it was
porting against**, verified by extracting the published `.crate`. And it
worked around `Response` having no public constructor before finding
`hclient-mock`, which is the answer.

That is this file's *a consumer is a different instrument from a test*
finding arriving from the other side, and it is sharper: the earlier
sightings were about surfaces that were genuinely awkward or genuinely
missing, where these two were **finished, documented and unfindable**. A
long method list on `RequestBuilder` and 25 crates in the family mean a
reader who does not already know a name does not meet it. The repair is a
*where things are* table on the front page and a signpost on `Response`
turning its missing constructor into the mock rather than into a wall.

**`impl AsRef<str>` is the whole of the `IntoUrl` question.** ACT asked
for something `IntoUrl`-shaped because call sites holding a `url::Url`
were writing `url.as_str()`. `url::Url` implements `AsRef<str>`, so
widening the seven verb methods reaches it **with no dependency on `url`**
— the crate `hclient-proto` removed at a measured 1.9 MB of ICU tables.
A trait of ours would have been worth exactly the conversions it named,
and the one worth naming is already reachable. `url` sits in
`[dev-dependencies]` as the witness, which is the same role it plays for
`uri.rs`'s differential corpus.

**It paid out in a way nobody planned: 90 `needless_borrow` warnings**,
each a `&format!(..)` at a call site in this workspace's own tests that no
longer needs the borrow. A widening whose benefit shows up as ceremony
clippy can now delete is a widening that was real.

**A missing method cannot say why it is missing.**
`Rustls::with_webpki_roots()` without its feature made rustc suggest
`Rustls::from_config` — a correct name and a far bigger detour, because
rustc offers the nearest name it can see and the nearest name is the
general-purpose escape hatch. The stand-in behind
`#[cfg(not(feature = "webpki-roots"))]` carries the message on an
unsatisfiable bound, with the same lifetime trap `Client::new()`'s hit the
night before: a `where Self: Trait` predicate mentioning no generic
parameter is checked where the method is **defined**, so the plain form
fails in this crate rather than at the caller.

**What the report confirms is worth as much as what it corrects**, and it
is listed because a design argument that is never tested from outside is
just an argument. `build()` refusing a configuration the backend cannot
honour read as correct on contact. A redirect predicate against an
internally-redirecting backend being an error at `build()` is the exact
question `.notes/` recorded as unanswerable from inside — ACT's answer is
that a predicate never consulted would have been its worst outcome,
because it would have believed a ceiling was enforced. `RedirectVerdict`'s
third arm is used *because* the other two exist to be contrasted with.
`ErrorKind` being an enum retired three of its tests that had to make real
network requests, because a `reqwest::Error` cannot be constructed. And
`Resolve` handing back A and AAAA as separate streams pushed a
policy-audit record out of their resolver, where neither stream can see
it, and into the request, where it is one event — their own code's comment
had already said that was where it belonged.

**The one thing a consumer cannot get on our side of the fence.**
`reqwest` is still in ACT's graph under `oci-client`, which brings
`hyper-rustls` with `aws-lc-rs` while `hclient-tls-rustls` uses `ring` —
so rustls correctly refuses to pick a provider and ACT installs one
explicitly. Nothing here is this workspace's defect, and it is recorded
because it is the difference between *we migrated* and *we have one HTTP
stack*: the lever is an OCI client that takes a transport, which is
somebody else's crate.

### A CLI, and the mutation that survived is what it is for

`crates/hclient-cli`, binary `hc` — httpie's request-item grammar,
curl's `--insecure` and `--resolve`, and `--backend` chosen at **runtime**.

**The differentiator is real but narrower than it first reads, and the
narrow version is the one to say.** curl supports several TLS backends
chosen at build time; only a `MultiSSL` build honours `CURL_SSL_BACKEND`,
the stock build on most distributions is not one, and curl's own man page
says an unknown name *"makes curl stay with the default"*. So: curl
**can**, in a build almost nobody has; when it cannot it **says
nothing**; and the choice belongs to whoever packaged the binary. `hc`
refuses a backend it has not got, by name, beside the list of what it
carries, with its own exit code so a script can tell that from an
unreachable server.

**It works only because `Client` names no type parameters.** Both arms
return the same `hclient::Client`, so `--backend` is an ordinary `match`.
A generic client would have made the builder function's return type name
a transport, and the arms build different ones — which is the erasure
paying for itself in the first consumer written after it.

**The finding is a mutation that survived.** Replacing the refusal with a
silent fallback — the exact defect the tool exists not to have — passed
all 30 tests, because the default build carries every backend and the
refusing arm is unreachable under the `--all-features` run CI does. The
repair is not another test: the decision is now a pure function of
`(requested, available)` **taking the available list as a parameter**, so
it is testable at any feature setting. This is the same week's third
sighting of one shape — a check that cannot fail in the configuration CI
runs — after the doctest fences and the crate that only built inside the
workspace.

**Two more, both found by building rather than by designing.** `--print H`
printed the caller's headers rather than the ones actually sent, so the
diagnostic lied about the `User-Agent` and `Content-Type` the tool itself
causes. And the item grammar reads `https://example.com` as a header named
`https`, because `:` is a separator — the likeliest mistake a caller can
make, producing a silently wrong request. A scheme followed by `//` is a
named refusal now, while `https:x` stays an ordinary header, so the
refusal is exactly as wide as the mistake.

**What it costs, which the research had listed as unmeasured.** The
default build — both TLS backends, tokio, the system resolver, four
decompressors, cookies, JSON, digest auth — is **144 crates and 5.2 MiB
stripped** on the ordinary release profile, against a two-backend probe
with no argument parsing at 3.30 MiB. Ubuntu's curl is 334 KB **plus 33
shared libraries**, so the honest comparison is one file against
thirty-four rather than 5.2 MiB against 334 KB. Nothing here is tuned:
`opt-level = "z"`, LTO and `panic = "abort"` are untried, and the number
is the default profile's.

**And it found a gap in the library it is written on**, recorded here
because the route to finding it generalises: `--http 2` had been wired to
a header nothing reads. `Capabilities` report the floor, so this file
names `RequireVersion` before the head as *the* honest route to knowing
which protocol will be used — and `RequestBuilder` had no setter for it,
because `RequireVersion` lives in `Extensions` and only `timeouts` and
`redirect` had one. `tests/require_version.rs` did not notice across two
verticals: every test in it builds its request with
`extensions_mut().insert(..)`, so testing the gate by going around the
builder is what let the builder have no route to the gate. **A consumer
written against the facade is a different instrument from a test written
beside it.**

### `curl -k` and `curl --resolve`, and the two land at different seams

Two of the flags a command-line client is expected to have, added as
library capabilities rather than as anything CLI-shaped — the crate is
the infrastructure and the flags are what a consumer needs to build.
**They land in different places, and that is the finding rather than an
implementation detail.**

**`--resolve` is a `Resolve` that wraps another `Resolve`.**
`hclient_dns::Overrides<D>` answers from a table where it has an entry
and hands the question to `D` where it does not, so it composes over the
system resolver, over DoH, over hickory, over anything — and a transport
that never heard of it needs no arm for it. Three decisions came out of
the seam rather than out of curl. The override is **host-wide**, where
curl's is `host:port:addr`: `Resolve` is asked for a name and a family and
carries no port at all, so a per-port table would be keyed on something
this seam cannot see. The **family filter applies to the override too**,
so Happy Eyeballs still races two families against an overridden host
rather than one arm getting everything. And an entry with an **empty
address list answers nothing rather than falling through**, because
otherwise "point this host at nowhere" and "do not override this host"
would be the same table, and the first is how a caller blocks a name.
SVCB passes through untouched: an override is an address, and minting a
record would put a port and an ALPN into the answer that nobody supplied.

**`--insecure` is a constructor on each TLS backend, behind a
`dangerous-insecure` feature.** A feature rather than a plain constructor
**for auditability**: a build either contains a path that skips
certificate verification or provably does not, and `cargo tree -f "{p}
{f}"` answers which. It is in no `default`, for this file's own reason —
Cargo unifies features, so a default would be a floor and one careless
crate in a tree would put the path into every other crate's binary.

**The two backends do not turn off the same amount, and that is the part
worth knowing before relying on either.** rustls keeps **signature**
verification: the custom verifier answers the chain, the expiry and the
name, and delegates `verify_tls12_signature`/`verify_tls13_signature` to
the provider's own, so the handshake still proves the peer holds the key
for what it sent. `native-tls` has no such seam — the platform stacks
verify as one operation — so it is the coarser of the two.

**Whether `native-tls` needs the hostname flag beside the certificate one
is a fact about the platform**, measured in 0.2.18: the OpenSSL backend
implements the certificate flag as `set_verify(NONE)`, which drops the
name check with everything else, while SChannel and Security.framework
forward the two independently. Both are set, or the method would mean
different things on Linux and on Windows — and **the test cannot show
it**: the mutation dropping the second setter survives on this host, and
the comment claiming otherwise was corrected rather than the test being
strengthened past what a Linux run can honestly prove.

**Neither insecure configuration can share a pooled connection with a
verifying one.** Each constructor draws a fresh `TlsConfigId`, which is
part of `hclient-native`'s pool key — asserted rather than assumed,
because the dangerous direction is a connection established without
verification being handed to a client that asked for it.

### The one claim this file marked as unverified was false, and it had shipped

`hclient-dns-system/src/sys/windows.rs` read an HTTPS record as
`DNS_SVCB_DATA` and dereferenced its `pszTargetName`. Its own header
recorded the claim underneath that as **taken on the project owner's word,
not verified here**, and named the exact consequence of it being wrong:
the record's payload would be raw response bytes, and reading it as that
struct would build a `PSTR` out of them.

**Measured on a real Windows 11, and that is what was happening.**
`DnsQuery_UTF8` for `cloudflare.com` type 65 reports `wDataLength = 61`
and the union holds `0001 00 0001 0006 02 68 33 02 68 32 …` — SVCB wire
format. `DNS_SVCB_DATA` is 32 bytes on x64 with `pszTargetName` at offset
8; offset 8 there holds `h3`. The 61 octets are **byte-for-byte** the
RDATA inside the `res_query` answer this repository had captured on Linux
in v0.3 and kept as a test fixture ever since.

**The rule is readable from the Win32 metadata and was confirmed in both
directions.** A type named by `DNS_RECORDA`'s data union arrives parsed
into that member; a type it does not name arrives as its own RDATA, with
`wDataLength` its length. `MX` came back as a structure and `CAA` as
RDATA; `DNS_TYPE_SVCB` is **64** while HTTPS is **65** and has no union
member — and `DNS_TYPE_HTTPS`, `DNS_TYPE_CERT` and `DNS_TYPE_LOC` all
exist as constants with no member, so it is the union that decides and not
the constant list.

**What made this reachable is the shape of the sentence, not the code.**
Every other unverified claim in this workspace is about a third party's
behaviour and is stated where the code depends on it; this one was too.
The difference is that nothing could fail: the module compiled on every
push, `cargo check --target x86_64-pc-windows-msvc` was clean, and the
crate's own tests exercised the Unix path. **A file whose header says it
has never been executed is not covered by any gate this project has** —
which is *a check that cannot fail* met from the one direction that had
never been named, where there is no check to break on purpose because
there is no machine to break it on.

The repair is not a test. It is `crates/system-resolver`: the platform
calls moved out of `hclient-dns-system` into a crate of their own, where
`Support::AnyExcept(&[..])` refuses the forty-three types Windows parses
**by name and before a query**, and where Windows 11's `DnsQueryRaw` —
resolved with `GetProcAddress`, because `windows-sys` emits it as a
`raw-dylib` import and naming one stops a process from starting on Windows
10 — hands over the wire message and makes that platform behave like the
other four. `hclient-dns-system` is an adapter above it and carries no
`unsafe` at all any more.

**And the second half of the fix is the part worth copying.** The two
Windows calls are compared against each other on a machine that has both:
`the_parsed_path_answers_the_same_rdata_as_the_raw_one` runs
`DnsQuery_UTF8` directly on Windows 11, where nothing else ever reaches
it, because the code for the platform this project cannot get hold of
would otherwise be the only code in the crate that never runs. That is the
nearest thing to a Windows 10 gate that exists without a Windows 10.

**The same shape was then found on a second platform, and there the code
had never worked at all.** Apple's arm called `res_9_query`, on this
workspace's word that it was `res_query` under another name, and on a
borrowed Mac every one of the crate's live tests failed in ten
milliseconds. The cause is that `res_query`'s state is per-thread and
Apple's copy is not safe to enter from several at once: 12 successes in 64
from eight threads. So the arm was not merely unverified in the Windows
sense — it could not have served a client, and the whole of the evidence
for it had been a table.

It is `DNSServiceQueryRecord` now, which hands over RDATA per record and
is routed by the daemon, so a Mac's supplemental resolvers — a VPN's
split-DNS, `.local` over mDNS — are answered rather than missed. Two of
its flags cost a wrong implementation before they were measured, and both
are the opposite of what their names suggest: `kDNSServiceFlagsTimeout`
**suppresses** the negative answer it is documented to bound, and
`kDNSServiceFlagsReturnIntermediates` is what makes one arrive — 1.2 ms
against nothing in four seconds. One capability is genuinely lost and is
said on the variant rather than papered over: the daemon reports a missing
name and a missing record type with one code, so `NameDoesNotExist` is
unreachable on Apple, and the live test **branches on the platform**
instead of accepting either answer, so a platform that could distinguish
and quietly stopped still fails a line.

**The acceptance is the whole tree rather than the crate**, because a
crate passing alone says nothing about the adapter above it: `cargo
nextest run --workspace --all-features` on macOS 27 is **2304 passed, 0
failed in 12.4 s**, and `hclient-dns-system`'s own live test walks the
whole path — the daemon's callback, this crate's `Record`, the SVCB
envelope — to an endpoint advertising `h3`.

**And the first attempt at that run failed for a reason that is a third
argument for nextest.** macOS's default `ulimit -n` is **256** against
Linux's 1024, and under `cargo test`, which runs a binary's tests as
threads of one process, the suite died with `TooManyOpenFiles` in
`hclient-native`'s fixtures. Under `cargo nextest run` the same tree
passes **at that same 256**, because each test is its own process and no
test's descriptors outlive it. Raising the limit is not the repair, which
was checked rather than assumed: at 8192 under `cargo test` the
descriptor failures give way to nine `hclient-otel` failures, since those
tests each install a global tracer provider and, sharing one process,
overwrite each other. Two symptoms, one runner. This is invisible on
Linux, which is what makes it worth writing down beside the two reasons
already here.

It is **not** an answer to the macOS CI hang recorded above, and the
distinction matters: this is arm64 hardware on macOS 27, where CI runs a
GitHub-hosted runner of another version on another architecture. What it
establishes is that the suite completes on a Mac at all, which nothing
had shown before.

### The decoder was chosen for its granularity, and the wrong one cost ninety lines

`hclient-dns-system` decodes HTTPS records with **`domain`** rather than
`dns-message-parser`, and the reason is not quality — the crate it
replaces is maintained, released a year ago and has no `unsafe` in its
`src`. It is that `dns-message-parser` exposes decoding at the **message**
level and nothing smaller (`decode(Bytes)`, one entry point), where
`system-resolver` hands over **RDATA**, because that is the only shape all
five of its platforms have.

So this crate used to build a **synthetic DNS response around every record
it wanted read**: a twelve-byte header, a question section, a
length-prefixed owner name per record, the TTL written back out — ninety
lines whose entire purpose was to be taken apart again by the next call.
`domain::rdata::svcb::Https::parse` takes a parser over one record's
octets, so the envelope has no subject.

**`hickory-proto` was the first candidate and the measurement is what
ruled it out.** It decodes RDATA directly too — `RData::read(decoder,
RecordType::HTTPS, Restrict::new(len))` — and it depends on `url`
**unconditionally**, for `caa.rs` and one error variant, a record type
nothing here asks about. `url` brings `idna` and the zerovec family,
`cargo tree -e normal`, unique crates:

| build | `dns-message-parser` | **`domain`** | `hickory-proto` |
|---|---|---|---|
| `hclient-dns-system`, Linux | 28 | **31** | 68 |
| `hclient-dns-system`, Windows | 31 | **34** | 71 |
| `hclient-dns-system`, macOS | 28 | **31** | 67 |
| `hclient` + `default-transport`, Linux | 92 | **94** | 103 |
| `hclient` + `default-transport`, **Windows** | 69 | **72** | **105** |

The Windows row is the one that decides it: `url`, `idna`, `zerovec`,
`yoke`, `tinystr` and the rest are **zero** there today, because
`hclient-idn` takes UTS 46 from `icuuc.dll` on purpose. `hickory-proto`
puts every one of them back, on the two platforms that crate exists to
keep them off. `domain` costs three — itself, `domain-macros` and
`octseq` — because `jiff` and `jiff-core` are already in the graph from
the cookie and cache date parsers, and it carries no `url` and no ICU at
all. It also takes `default-features = false`: `std` buys `hashbrown` and
`foldhash` for a map this path never builds, and `rand` a query id it
never mints.

**Three things the change bought, each pinned by a test.**

*An AliasMode record carrying SvcParams is ignored per RFC 9460 §2.4.1
rather than rejecting the whole RRSet* — and the test that pinned the old
behaviour named this outcome in advance: *"if this test ever fails,
upstream started honouring §2.4.1 and `endpoint_from_binding`'s AliasMode
branch becomes reachable with real parameters for the first time."* It
failed on the first run. Upstream did nothing; the branch had implemented
the MUST correctly since it was written and had never been reached with a
record that exercised it, because the decoder in front of it refused
those records first. **A rule can be right, tested and dead at the same
time**, and only a change underneath it says which.

*An RRSet larger than a DNS message can frame is answered rather than
refused.* The 65535-octet bound was the envelope's, never the resolver's,
and `SvcbLookupError::AnswerTooLarge` went with it — an error nothing can
raise is worse than no error, because a caller writes a match arm for a
case that cannot happen. `NameNotUsable` went the same way: it refused an
owner name with no *wire* form, and nothing writes a wire name any more.

*A compression pointer inside RDATA can now only reach this record's own
octets.* §2.2 forbids one there and a non-conformant sender may still
write one; the envelope had to **argue** that a pointer resolved against
a message this crate had assembled out of several records reached nothing
an attacker chose. Parsing where the bytes arrive makes that structural
instead of argued.

**And one hazard closed itself.** Two tests here existed only to pin
`dns-message-parser`'s `ServiceParameter` comparing and hashing **by key
number alone**, so that `assert_eq!(param, ServiceParameter::ALPN { .. })`
passed without checking a value. `domain` derives no `PartialEq` on
`AllValues` at all, so the trap cannot be sprung; what replaces the two
tests asserts that two records differing only *inside* a parameter come
out different, which is the property those tests were protecting.

**What it cost.** `SvcbLookupError::Malformed` carries
`domain::base::wire::ParseError` where it carried
`dns_message_parser::DecodeError` — a public change, free in a
pre-release and the reason to make it now. `bytes` left this crate's
direct dependencies with the envelope's one `Bytes::copy_from_slice`. And
RFC 9460 §2.2's *reject the entire RRSet* is now a `?` in a loop rather
than a property of a message-level decoder: the rule had to be **stated**
where it used to be inherited, which is the honest direction, since a
rule nobody wrote is a rule nobody can test.

**`hclient-dns` keeps its `codec` feature and `hclient-dns-system` no
longer asks for it.** That feature carries `binding_from_decoded`, the
decoder-shaped half of the conversion; the half that is shared —
`RawBinding`, `RawParam` and §2.4/§2.5/§8's client rules over them — is
behind no feature, because it names no decoder. `hclient-dns-doh` still
asks for `codec` and should: it decodes whole messages, which is what
that decoder is for. **Two decoders in one workspace is the cost of
having two granularities**, and it is the right cost — the alternative
measured above is one decoder and ICU on every platform.

### A crate was green in the workspace and did not build on its own

`cargo check -p hclient-native --all-features --all-targets` failed with
four errors on `main`, and had for as long as `tests/h3_two_runtimes.rs`
existed: it instantiates the HTTP/3 path under `Smol`, which needs
`hclient-rt-smol/udp`, and this crate's manifest did not ask for it —
`hclient-rt-tokio` one line above does. It compiled because another member
turns the feature on and **Cargo unifies features across a graph**.

**This is the third sighting of one shape**, after the two doctest
examples that compiled only because a neighbour enabled a feature and the
three backends that owed `SendTransport`. All three are one sentence: *a
green `--workspace` run is a claim about the workspace, not about any
crate in it.*

**`just features` could not have caught it, and that is structural.**
`cargo hack --each-feature --no-dev-deps` is blind to a dev-dependency by
construction and builds no test targets — so the defect was invisible to
the gate that looks closest to it. `just check-each-crate` is therefore a
new gate rather than a widened one: every member built alone, with its own
dev-dependencies and its own targets, which is how anybody who downloads
one builds it. Twenty-one crates; five are excluded because their code is
for a target this host is not, and `check-targets` covers those by
**naming the target** rather than by skipping the crate. Both halves were
checked in the failing direction, the second being a loop over nothing —
a green run over zero crates is this file's recurring defect. A sweep over
every other member found no second case.

### A response cache landed, and it is the counterpart `owns_cache` never had

`hclient::cache` — RFC 9111 freshness, validation, `Vary` and the
directives on both sides — **sans-io and clockless**, exactly as
`hclient::cookie` is, and reaching for neither `Client` nor
`hclient-core`. It was the `hclient-cache` crate when it landed; the
section on folding the two in says what moved and what that cost.
`ClientBuilder::cache(HttpCache::new())` switches it on behind
`hclient`'s `cache` feature, off by default. `Client` supplies the
`now` as `SystemTime::now()` for the reason the jar does — `Date`,
`Expires` and `Age` are calendar values and `Timer::Instant` is a
stopwatch with no epoch.

**It is a private cache**, a user agent's rather than a shared one, and
three rules turn on that: `private` is stored, `s-maxage` is not read at
all, and a response to an authenticated request is stored with the
credential in its `Selector` rather than refused — a narrowing of §3.5
that a private cache needs and the RFC does not require.

**`Capabilities::owns_cache` finally has a reader.** It had been a `bool`
set by one backend — `hclient-fetch`, because the browser caches inside
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
which `hclient`'s decompressor makes concrete. And `stale-while-
revalidate` is deliberately absent: it needs somewhere to run the
revalidation after the response has been handed over, and this client
does not spawn on a caller's behalf — the same sentence the h3 body pump
and the WebSocket keep-alive are written under.

**The wiring's own defect was found by a stack overflow three crates
away.** With the feature on, `hclient-native`'s
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

### The cache store waits now, and the client stopped holding a lock to ask it

`CacheStore`'s six methods are futures and every one takes `&self`. The
store this crate ships is in memory and answers [`std::future::Ready`], so
the shape costs it no allocation and no suspension; what it buys is the
stores that were unwritable before — on disk, in Redis, in
`moka::future` — for which a synchronous seam offered two options, block
the executor or do not exist.

**`&self` is not a second decision, it follows from the first.** A store
that awaits cannot be held behind a `&mut` across that await, and a store
that is remote is shared by everyone talking to it already. So
synchronisation is the store's own: `MemoryStore` holds a `Mutex` around
its map, which is a *narrower* lock than the one it replaced.

**What went was `Arc<Mutex<HttpCache>>` in `Client`.** The old paragraph
beside it argued the lock was safe because it was never held across an
`.await` — true, and it stopped being true the moment the store could
wait. Rather than an async mutex there is now no lock at that level at
all, so two requests no longer queue behind each other to read a header.
`Client::cache()` hands back a borrow rather than a `MutexGuard`, and the
warning it carried — that holding the guard across an `.await` could
stall a body belonging to another handle — is gone rather than reworded.

**Associated futures, not `async fn`**, for `TcpConnect::Connecting`'s
reason: `Client` boxes its cache `Send + Sync`, and an RPITIT cannot be
bounded. `AnyStore` is the erased half, and it is `BoxedTransport`'s split
one crate over — a private object-safe trait with a blanket impl, so **a
store author writes nothing** and `Send` is inferred where the type is
still concrete.

**Two costs, both measured.** `Client::execute`'s future went from 4,344
bytes to **5,776** when the two cache hooks became `async fn`, against a
6 KiB ceiling that is meant to have room; boxing those two calls brings it
to **4,784**, at two allocations per hop on a path about to touch a store.
And `AnyStore`'s `Debug` no longer prints a count, because `len` is a
future and a `Debug` cannot await one — a number a remote store might
disagree with is worse than none.

**The write is a state of the body rather than a call inside it.**
`Recorder::commit` was one line in `poll_frame`; it hands back a future
now, which the body drives to completion before it reports the end of the
stream. It is deliberately **not** spawned — a spawned write lets the body
end before the entry exists, so a caller who asks again immediately misses
what it was just told was cached, and this crate does not spawn on a
caller's behalf anywhere.

Setting that field and returning the poll's own answer was the first
shape, and it stored nothing at all: the body had ended, so nothing would
ever poll it again and the future was dropped where it stood. Three cache
tests said so on the first run.

**The suite needed a store that actually waits**, because every store in
this workspace answers `Ready` and a suite built on those would pass for a
seam that had never stopped being synchronous. `SuspendingStore` in
`tests/pluggable_stores.rs` suspends once per operation and counts it; the
test asserts a second request is served from it *and* that it suspended.
Removing the body's wait for the write fails four tests across two files.

### The jar and the cache became modules, and one feature shape is what made it free

`hclient-cookie` and `hclient-cache` are `hclient::cookie` and
`hclient::cache`. **No consumer's `use` line moved**: both were already
re-exported under exactly those names (`pub use hclient_cookie as
cookie`), so the change is `pub use` to `pub mod` and nothing else on the
public surface.

**What made it defensible is a measurement, not a preference.** This
workspace's test for a crate boundary is whether it holds a dependency a
feature would otherwise spread — `hclient-tls-quic` carries
`quinn-proto`, `hclient-tungstenite` carries `tungstenite`, and both are
kept for that. `cargo tree -i` named **`hclient` and nothing else** for
each of these two, and their dependencies (`jiff`, `winnow`,
`public-suffix`) are gated just as well by the `cookies` and `cache`
features from inside. The boundary was being kept for a third-party
consumer who did not exist.

**What it cost is one sentence in `docs/competitive-gaps.md` that is now
false and has been corrected**: the jar and the cache are still sans-io
and still clockless, and are no longer *separately usable*. That is the
whole of the loss, and it is worth stating plainly because the crates'
own module docs had argued the boundary made "cookies behave the same on
every backend" **structural** — it does not any more, it is a discipline,
and both module docs now say so in as many words.

**The part that nearly went wrong is a feature, and it is the reason to
read this section before touching the `cookies` feature.**
`hclient-cookie` carried `default = ["public-suffix"]`, and
`tests/without_the_list.rs` — the only thing asserting a no-list build is
*narrower* than a list build rather than quietly wider — ran only under
`-p hclient-cookie --no-default-features`. The obvious spelling of the
merge is `cookies = [.., "public-suffix"]`, and it makes that test
**unreachable**: features are additive, so *the module without the list*
stops being expressible. Measured before it was believed — the old
invocation ran 78 tests, the merged one compiled the file out entirely.

`justfile` had already recorded that deleting that line "would have been
the other direction", so the resolution is a shape rather than a
deletion: **`cookies` pulls the `public-suffix` crate, and a separate
flag of the same name — carried in `default` — gates the code path.** A
plain `--features cookies` behaves exactly as it did; `--no-default-
features --features cookies,test-util` is the no-list build, and it is in
`test-no-default`. The one thing that changed is that the no-list build
still links `public-suffix` as dead code; the test asserts behaviour, not
graph size, and `graph-no-cookie-jar` still pins the crate out of a
default build.

That guard is **weaker than it was** and the weakening is worth naming:
it looked for `hclient-cookie` and `public-suffix`, and there is no crate
name left to look for, so a jar compiled into a default build would no
longer show up there — only its list would.

**Two smaller things the move surfaced, both caught by `just docs`
rather than by the compiler.** Doc links in the moved files pointed at
their old crate root, which is a different crate root now; and a `///`
doc on the `pub mod` declaration makes the module's own `//!` links
resolve in the **parent's** scope, so thirteen of them stopped resolving
until that outer comment went. Neither is a compile error, and neither
would have been caught by the test suite.

Counts, measured: 25 publishable crates to 23, `hclient`'s own suite 308
tests to 465, and the workspace unchanged at 1755 — everything moved,
nothing was lost.


### Two date parsers were reported as identical, and the duplication was twenty lines

The premise was wrong and the measurement is worth more than the fix.
the cache's `date.rs` and the jar's share **no parsing at
all**: the first reads RFC 9110 §5.6.7's three fixed `HTTP-date` forms,
the second RFC 6265 §5.1.1's position-free algorithm, and their function
inventories intersect nowhere. What was genuinely duplicated is the
**civil-date arithmetic** — `days_from_civil` byte for byte, `is_leap`
and `days_in_month` differing only in integer type. Twenty lines of the
402.

**So the split that landed is: winnow parses the grammar, jiff answers
the calendar.** It maps onto the finding rather than onto the report —
the halves that differ stay per-crate and the half that was copied is
delegated, which removes the copy from both crates rather than moving it
to a third.

**Neither library is asked to parse, and that decided which objections to
either of them survived.** A date crate's `strftime` validates the
weekday against the date and refuses a mismatch, which §5.6.7 does not
require and this workspace deliberately does not do — a server with an
off-by-one weekday would lose its `Date` entirely. It also reads a
literal space as *any run of whitespace, including none*: measured on
chrono, `06Nov1994 08:49:37 GMT` yields an ordinary 1994 timestamp, as do
double spaces, tabs, a lowercase month, `+1994`, `-1994`, and one digit
wherever the grammar writes two. For a format §5.6.7 fixes down to the
character, that is a different grammar rather than leniency.

The asymmetry is why that is refused rather than weighed. `None` here
means **already stale** (§5.3), and this parser also reads `Date`, which
feeds the `Age` arithmetic — so a surplus refusal is safe and a surplus
*acceptance* mints a freshness lifetime out of a string no conforming
sender produced. The `httpdate` differential would have had to record
eight new divergences of the form *we accept what the oracle refuses*,
turning a test that pins one deliberate decision into a list of what a
dependency happens to do.

winnow keeps every one of those properties exactly:
`take_while(n..=n, ..)` is the `fixed_digits` this file used to hand-roll,
each separator is a literal that means itself, and the day name is
consumed by a parser that never shows a date library a weekday. Nothing
in either crate's behaviour changed — 62 cache tests and 95 cookie tests
pass unaltered, the differential among them.

**Three of the four objections to `jiff` were objections to its parser,
and the fourth was an artifact of the route taken to the answer.** With
winnow parsing, the weekday check and the POSIX `%y` window have no
subject. The one that looked structural was `jiff::Timestamp::MAX` —
`9999-12-30T22:00Z`, which cannot represent `Expires: Fri, 31 Dec 9999
23:59:59 GMT`, a real "never expires" idiom this parser has always read
as `253402300799`. That is true of `Timestamp` and **not** of the civil
types: `civil::Date::MAX` is `9999-12-31`, and
`DateTime::duration_since` against a civil epoch answers `253402300799`
exactly. Measured, after the first measurement said the opposite for the
wrong reason.

What survives is a trap rather than a defect: `civil::date(..)` and
`Date::at(..)` **panic** on a value the calendar does not have, one
function name from the `Date::new`/`Time::new` that return `Result`. Both
crates use the fallible pair, and the single panicking constructor is
inside a `const` — where an impossible date is a compile error rather
than something a header could trigger.

chrono was measured too and is equivalent for this half, agreeing with
the existing parser on every probed point. jiff is what landed, at the
same crate count — `jiff` + `jiff-core` against `chrono` + `num-traits`
— and with **no build script**, where `num-traits` carries one and an
`autocfg` build-dependency with it.

**The honest cost, because one of the three numbers went the wrong way.**

| file | code lines | |
|---|---|---|
| `cache/date.rs` | 138 → **118** | the calendar left; the grammar stayed |
| `cookie/date.rs` | 122 → **96** | same, plus §5.1.1's productions read better as combinators |
| `cookie/parse.rs` | 142 → **152** | **longer** |

Graphs, while the two were still crates of their own: `hclient-cache`
10 → 13 crates, `hclient-cookie` 11 → 14.
`default-features = false` on jiff is what makes it affordable in a
clockless leaf — its default `tz-system` reaches for the platform
timezone. Both build for `wasm32-unknown-unknown` and `wasm32-wasip2`,
checked directly rather than inferred, since `cargo nextest run
--workspace` builds for neither.

**`parse.rs` growing is the result worth keeping.** RFC 6265bis §5.2 is
not a grammar — it is a sequence of *cuts*, "the characters up to the
first `;`", then the same again — and a combinator that expresses a cut
costs more text than `position(|b| *b == b';')`. The productions in
§5.1.1 are a grammar and shrank; the algorithm around them is a search
and is still written as one. That is the same line the whole change is
drawn on, met from the far side: a parser combinator library pays where
there is a grammar, and charges where there is not.

### Three header grammars, and one of them needed the backtracking

The same line the date parsers were split on, applied to the parsers that
were left — and it came out three different ways, which is what makes it a
rule rather than a preference.

**`WWW-Authenticate` is the case that pays, and it pays in a defect
rather than in lines.** RFC 9110 §11.6.1 lets one value carry several
challenges separated by commas — *the same commas that separate a
challenge's own parameters*. Nothing local tells them apart, so the
hand-written splitter looked ahead: *a token then whitespace begins a new
challenge, a token then `=` is a parameter*. A combinator does not need
the lookahead at all — the parameter list stops where `auth-param` fails
to match, and the outer list takes the comma. `token68` is the arm that
keeps a `Negotiate YWJj==` beside a `Digest` one from derailing the
value: `auth-param` matches its `YWJj`, finds `=` with nothing behind it,
and the alternative swallows the whole thing. **294 code lines to 261,
and four hand-written helpers gone** — `split_challenges`,
`digest_params`, `unescape` and `split_outside_quotes`.

**`Cache-Control` is an ordinary win**: 196 to 185, RFC 9111 §5.2's
`token [ "=" ( token / quoted-string ) ]` written as itself.

**`charset` grew, 231 to 240**, which is the third sighting of the rule
and the second time it has been recorded against this workspace's own
hopes. Finding one parameter past a media type is a *cut*, and a cut
costs more as a combinator than as `position`. It is kept converted
anyway, because what it buys is not lines — see below.

**What it buys is that "split on a separator outside a quoted-string" is
now written zero times where it was written three.** The cache's
`directives.rs`, `hclient`'s `digest.rs` and its `response.rs` each
carried a copy, differing only in separator and in `&[u8]` versus `&str`.
They were **measured against each other first, on twelve inputs** —
unterminated quote, escaped separator, trailing backslash, empty input —
and agreed on all twelve, so this was tidy-up rather than a defect, and
worth saying because the same investigation could have found the
opposite.

Two of the three quoted-string parsers hand back a **borrow and do not
unescape**, and that is a decision rather than a shortcut: a
`Cache-Control` argument that is quoted at all is a field-name list and a
`charset` is an encoding label, neither of which can contain a quote, so
an unescape would allocate on every directive to change nothing. A
`quoted-pair` is still consumed, so a `\"` cannot end the value early.
`digest.rs`'s does unescape, because a `realm` is free text a deployment
chooses — which is the same split this workspace already had between
those two modules, now stated in the parsers instead of beside them.


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
`.notes/informational-1xx.md`, including what is still unmeasured —
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

That is the far point of `.notes/pooled-reuse-race.md`'s three, the one this
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

That makes four: `.notes/v03-acceptance.md` records three timing-based
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
(`TokioIo`'s `Socket`, `hclient-rt-smol`'s `SmolSocket`) because one
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
`every_capability_is_a_gate_or_a_report` lives in `hclient-core`, because
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
`hclient-urlsession` is the backend that genuinely reports `Transparent` —
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
`Event::Informational` landed with the `1xx` work and `hclient-fetch`'s
`tests/hooks.rs` never gained an arm, so every browser binary in that
crate failed to build through six merges that were each green on
`cargo nextest run --workspace --all-features` — which does not build for
`wasm32-unknown-unknown`, where `just test-browsers` is its own CI job.
`Event`'s exhaustiveness is what turned a new variant into a compile
error rather than silence: **the design worked and the running of it did
not.** *(That sentence read `Event` not being `#[non_exhaustive]` for as
long as it has existed, and the type has since taken the attribute —
with `every_event_is_accounted_for`, one in-crate exhaustive match, kept
as the thing that still makes a new variant one compile error in one
known file. The mechanism survived; the sentence naming it did not, which
is this file's own rule met once more.)* The cheap check that would have caught it needs no browser at
all — `cargo test -p hclient-fetch --target wasm32-unknown-unknown
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
change — the rule that kept `connect` off `hclient-h3` until v0.4 W1 and
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
  into one `AbortController`. `hclient-dns-doh` sets `None` for a third
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

**Two of reqwest's eight are still absent, and the third landed where it
belongs.** An adaptive window is hyper's, computed from measured RTT;
`h2` has none, so it would be ours to write and a wrong estimator is
worse than an honest constant. And `max_concurrent_streams` governs
streams the *server* opens, i.e. server push, which `h2` does not enable
and RFC 9113 §8.4 deprecates: a knob with no subject.

**Keepalive pings are `Native::h2_keep_alive`, and not an `H2Opts`
field** — this paragraph used to say they need somebody polling an idle
connection, which is true and is the whole shape of the answer: a
`SETTINGS` field is written once at handshake, where this is a *driver*
behaviour, so it belongs on the opt-in constructor that has a driver.
Set without `multiplexed()` it is inert, which is stated where the
setter is.

**The interval measures time, not silence, and that is the one place it
differs from the WebSocket keep-alive it is otherwise modelled on.**
`hclient-tungstenite`'s restarts on any inbound frame, which is what
makes it free on a busy connection; `h2::client::Connection` reports no
traffic at all, and a driver polling it cannot tell a poll that moved
bytes from one that did not. So a busy connection pays one `PING` per
interval — nine bytes, against a feature whose entire purpose is that
the path sees traffic. The second clock is the same in both, and `h2`
makes it easier: `poll_pong` resolves for *our* ping, so there is no
unsolicited pong to mistake for it — the mutation the WebSocket version
had to be taught.

Two tests, and the pair is the assertion. One models a middlebox — an IO
wrapper that cuts the connection after a bound with no inbound bytes,
which is what a NAT flow timer watches — and asserts the server's
**accept count** stays 1 across a pause three times that bound. The
other has the peer answer one request and then stop polling its own
connection while holding the socket open, which is what silence is, and
asserts the close names the probe. Checked by mutation rather than
assumed: suppressing the `send_ping` kills the first and leaves the
second passing, and making the deadline never fire kills the second and
leaves the first — so neither test covers for the other.

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
shape.** Two things kill it. `RedirectPolicy` lives in `hclient-proto`,
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
`hclient-proto`. It splits on `;` **outside quotes**, with backslash
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
while two of them are still listed in `.notes/v01-acceptance.md` as
deliberately not done — a list that was updated for its DoH half and not
for these.

- **`hclient-mock`** — `MockTransport`, behind `hclient`'s `test-util`
  feature. It is how every capability refusal in this workspace is tested,
  because "a jar against a jar-owning backend is refused at `build()`" is
  a fact about a type that never sends anything.
- **`hclient-tower`** — the `tower::Service` adapter, so this client fits
  a stack that already speaks that vocabulary.
- **`hclient-dns-hickory`** — the third `Resolve` backend, beside the
  system resolver and DoH.
- **`hclient-rt-embassy`** — a `TcpConnect`/`Timer` runtime over
  `embassy-net`, which is what makes the embedded target reachable at all;
  see the `std` paragraph above.

### A fifth backend: Apple's `URLSession`, and the three things it refuses to take

`hclient-urlsession` puts `URLSession` behind `Transport` — the fourth
**ambient** backend, owning no connection of its own, after `hclient-wasi`
and `hclient-fetch`. It exists for the list a userspace stack cannot reach
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
device rather than a preference, which is `hclient-tls-native-tls`'s
argument one seam over.

**What it refuses to take from the OS is the decision worth knowing.**
`URLSession` will keep cookies, a response cache and a redirect policy for
you, and this turns all three off. None of them is in the list above; all
three are portable behaviour this workspace already implements once; and
leaving them on would make this the second backend reporting
`owns_cookie_jar` and `owns_cache`, so a caller porting from
`hclient-native` would lose two features by changing one line.

**Redirects are the sharpest, and this backend is stronger than the
browser one.** `URLSession` lets a delegate refuse a redirect and a
browser does not — so this reports `RedirectSupport::Transparent` where
`hclient-fetch` must report `Internal`, and `Client`'s hop limit and its
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
for it was not the true one.** `PoolKey` is `hclient-h3`'s problem;
`hclient-webtransport` takes its `quinn::Connection` from outside, so
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
each other, in `hclient-proto`'s `encode` module.

**That module is where `base64` moved to**, and it is one function each:
`hclient-native`'s proxy had written its own for
`Proxy-Authorization: Basic`, and `Authorization: Basic` is the same
encoding for the same reason. Encode only, both: nothing here decodes
either, and a decoder is where the sharp edges live.

**All three are the crate now, and the sentence that stood here was the
defect.** It read that neither pulls a crate, because `url` was removed
from this graph at real cost and its `form_urlencoded` *"would bring it
straight back"*, while `base64` is *"a crate for twenty lines"*.
`form_urlencoded` is its own crate over `percent-encoding` alone — two
crates, no `url`, no `idna`, no ICU, no build script, both wasm targets —
and `base64` was already in the graph of any build that resolves DNS with
a codec. **The claim was never measured**, and it was restated once more
after `base64` landed, which is the shape this file records three times
over about checks: nothing forced a re-measurement, so the sentence
outlived the fact. All three crates were checked against the lines they
replace — every padding length for base64, the space/`*`/`~` rules for
the form serialiser, and the delimiters for the URI encoder — and every
one is byte-identical.

What the measurement rules out is the near neighbour rather than the
crates: `urlencoding` disagrees with the WHATWG serialiser on 3 of 11
probed inputs including the space, and one module over it escapes `/`,
`?`, `&` and `#`, turning `/a/b?x=1&y=2` into
`%2Fa%2Fb%3Fx%3D1%26y%3D2`.

**`base64` costs one crate rather than none, and the tense above is
load-bearing.** It shared `dns-message-parser`'s copy at 0.22; taking
0.23 makes it a **second** copy, which `cargo deny` reports as a
duplicate (`multiple-versions = "warn"`). That is the cheap kind by this
workspace's own rule — the two never exchange a `base64` type, since
nothing here does more than call `encode` — and it is worth knowing that
0.23 changes nothing this code touches: its additions are SIMD engines
behind a default-on `simd-unsafe` feature this build switches off,
decode-side error detail, and custom padding. Dropping back to 0.22
removes the duplicate and loses nothing.

**The SIMD engines are refused, and the measurement is the whole
argument.** Switching the feature on changes nothing by itself:
`simd-unsafe` gates the new `Simd`/`Avx2`/`Neon` modules, and `STANDARD`
is `GeneralPurpose`, which the feature does not touch — so SIMD would
have to be named at the call site. Both call sites encode a
`user:password` for `Authorization: Basic`, and at those sizes it is not
faster. Measured on x86-64, encode, nanoseconds per call:

| input | `STANDARD` | `Simd` | |
|---|---|---|---|
| `alice:hunter2`, 13 B | 23.9 | 20.4 | 1.17x |
| 40 B | 23.9 | 25.2 | **0.95x — slower** |
| 120 B | 51.6 | 53.1 | **0.97x — slower** |
| 64 KiB, for contrast | 17784 | 6208 | 2.86x |

The runtime detection and the fallback cost more than the scalar encode
at the sizes this workspace actually has, and the 2.86x needs an input
base64 never sees here. What it would cost is the sharper half:
`Simd` requires base64's **`std`** feature, where `hclient-proto` builds
`alloc`-only for both wasm targets, and none of the three engines exists
on `wasm32` at all — so the engine would be a per-target `#[cfg]` in a
sans-io leaf, which is the machinery this workspace removes rather than
hides. Nanoseconds against that, once per request, beside a network
round trip.

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
-size guards, `hclient/tests/future_size.rs` and `hclient-native`'s, and
**neither ceiling is a round number**: `Client::execute`'s future is
4,344 bytes and `Native::execute`'s is 15,480, but the figure that sets
both is what one extra `async fn` layer costs — measured at **1.81×**, so
a ceiling at 2× would be a guard that cannot fire for the defect it
names. They are 6 KiB and 24 KiB, and both are checked in the failing
direction by reintroducing the layer. `.notes/expect-continue.md` §7.

### The proxy protocols became sans-io, and `CONNECT` stopped needing an HTTP client

`hclient-proxy`: the seam, `Proxy<P>`, the bypass matcher and all three
protocols, as **state machines with no IO trait in them at all**. A
handshake is handed the bytes that arrived and answers with the bytes to
send, or *not yet*, or *the tunnel is open*; `hclient-native`'s driver is
thirty lines and is the only thing left in the family that knows what a
`poll_read` is.

**The premise of the move was `CONNECT`, and it was the one protocol that
looked immovable.** It drove `hyper`'s h1 dispatcher through
`crate::upgrade` — `http1::Connection`, `poll_without_shutdown`,
`into_parts` for the leftover read buffer — because writing one request
and reading one response needed an HTTP client. What made it movable is a
fact about the *message* rather than about hyper: **a `CONNECT` response
has no body under any framing rule** (RFC 9110 §9.3.6), so chunked
decoding, `Content-Length` and the interaction between the two — the hard
half of HTTP/1 — have no subject. What replaced forty lines of dispatcher
is `hclient_proto::head`, forty lines of parser.

**That parser is `winnow` and the head is a grammar**, which is this
workspace's own rule about where combinators pay, applied for the third
time and coming out the other way from the last two: `Cache-Control` was a
grammar and shrank, `charset` was a *cut* and grew. The input is
`winnow::Partial`, so **incomplete is the parser's answer rather than a
scan of ours** — the case a hand-written scan gets wrong is a terminator
split across two reads, and a scan restarted from the beginning on every
read is quadratic in the head's length. `a_head_arriving_one_byte_at_a_time_is_never_wrong_before_it_is_complete`
pins it at every prefix.

**Three refusals are the parser's own**, each a `MUST` somewhere: a bare
`LF` (RFC 9112 §2.2 *permits* accepting one, and two line grammars is a
thing two implementations disagree about), an obs-fold continuation (§5.2),
and whitespace before the colon (§5, the request-smuggling shape).

**What sans-io bought is not tidiness, it is reach.** Every rule in every
protocol is now killed by a test that opens no file descriptor, against
the byte sequences the RFCs print — and the SOCKS5 reply, whose length is
not known until `ATYP` and its length byte have arrived, is fed one byte
at a time with the buffer asserted untouched at each step. The contract
that makes that possible is one sentence and it is the only thing the two
SOCKS protocols share: **a handshake that answers `NeedMore` has consumed
nothing.** They share no bytes at all otherwise — `VN=4` against `0x05`,
`CD=90` against `REP=0`, NUL-terminated fields against length prefixes —
which is the same evidence for the seam that two protocols sharing nothing
was before the move. What they *used* to share, `frames.rs`'s byte-exact
IO, is gone: the driver does it once for all three.

**The seam is narrower than the trait it replaced, and the loss is
stated.** A protocol that has to **wrap** the IO cannot be written against
`Handshake` — TLS to the proxy itself is the real example — where the old
`ProxyProtocol::tunnel` took the stream and could have. That is not a
regression: it was unsupported before, and it is where
`system::ParseError::TlsToProxyUnsupported` already refuses. Lifting it is
a change to the driver's thirty lines, not to the seam.

**One knob turned out to have one setting and was deleted.**
`upgrade::exchange` took the accepted status as a parameter *because there
were two callers*, and its doc said so; with `CONNECT` gone there is one,
and a parameter with one value is the distinction-with-one-reachable-side
that `UpgradeSupport`'s spare variants were deleted for. The `101` check is
inlined and the paragraph explaining the parameter is now the paragraph
explaining its absence — which is the maintenance this file records
failing three times over.

**The regression net was already there, and it is the reason this was
attemptable at all.** `tests/proxy.rs` watches bytes from the proxy's own
side of the wire — the request line's shape, the origin travelling by name
— and its 20 tests passed against the rewritten implementation unchanged.
A refactor of a connect path with no such net would have been a different
proposition.

### The machine's own proxy settings, read by us, and the PAC script reported rather than run

`hclient-proxy`'s `system` feature, `Native::system_proxy()`, and
`hclient`'s `proxy`/`system-proxy` features so that a caller reaches all
of it without naming `hclient-native` — with `hclient::default_transport()`
as the piece that was missing, since a proxy is configured **on the
transport** and there was no way to get one from the facade.

**`Client::new()` reads them, and that is the good-citizen half.** It is
the `system-proxy` feature, **in `default`**, and no call: a
convenience constructor that ignored `HTTPS_PROXY` would be the one
program on the machine that does, and a port from curl or reqwest — both
of which honour it by default — would silently start going direct.
`default_transport()` deliberately does **not** read, because it is the
seam for configuring a transport and `unix_socket` refuses when a proxy
is configured: a chain that failed on machines with an `HTTP_PROXY` and
not on others is worse than an explicit line.

**The feature is positive and sits in `default`, which took the weak
form to be affordable.** `system-proxy = ["hclient-native?/system-proxy"]`
— with the question mark — reaches the transport only where the transport
is already in the graph, so a build without `default-transport` pays
nothing and a consumer turns the behaviour off with
`default-features = false`. The plain form would have pulled
`hclient-native` in, and tokio, rustls and the system resolver with it,
into every graph that took this crate's defaults: the floor
`default-transport` was reversed out of `default` for, arriving by the
back door. `just graph-default-has-no-transport` asserts both directions
and was checked by deleting the `?` and watching it fail.

**A feature that turned proxying off was the shape considered first and
it is the wrong one.** Cargo unifies features, so a negative switch is
unified too: one crate in a dependency tree could take proxying away from
every other, silently. A positive default that a consumer drops with
`default-features = false` puts the decision with the build that wants
it, which is the contract everybody already knows. The runtime lever —
`Client::builder(default_transport()?)`, which reads nothing — answers
the other question, and is pinned by `tests/proxy_default.rs` rather than
described.

That split needed a second translation rather than a second policy.
`http_proxies` **refuses** a configuration this client cannot express in
full, and `http_proxies_lossy` **installs what it can and reports the
rest** — because a refusal is only useful to somebody who can act on it,
and `Client::new()` did not ask. A constructor that refused would be a
client that will not start on a network with WPAD. Nothing is silent at
the API level: the lenient half returns the report, and only the
constructor discards it. A machine with a PAC script *and* a static
proxy gets the static one, which is WinINET's own fallback rather than an
invention of ours.

**`DefaultTransport` names `HttpConnect` even where no proxy is
configured**, and that is what keeps it one type: `Client::new` builds a
proxied transport on a proxied machine and an empty-listed one
everywhere else, so an alias naming `NoProxy` would have made
`transport_as::<DefaultTransport>()` — the documented way past the facade
— work on one machine and not on the next. Caught by the test that pins
two handles sharing one transport, which is the only place that downcast
is exercised.

**The readers are ours, and taking a crate for them was tried first and
measured.** `proxy_cfg` does all of it in one dependency; it also depends
on `url`, and through it on `idna` and the ICU tables — **28 crates**, on
exactly the two targets `hclient-idn` exists to keep those tables off —
and it pulls a second `windows-sys` major through `winreg`. Written here
over `windows-registry` and `system-configuration`, both of which expose
**safe** APIs, the cost is: **nothing on Linux**, where the environment
needs no crate at all; **+4** on Windows; **+6** on macOS; **+9** on
Android. And two things `proxy_cfg` does not read at all are now read —
the auto-config URL, and the platform's own SOCKS entry.

**Android is the dear one, and the reason is where its settings live.**
Every other platform keeps them in a file, a registry or a dictionary a
process can read; Android keeps them in **JVM system properties**, so
reaching them means calling into managed code — `jni` and `ndk-context`,
measured at 19 crates to 28, target-gated so no other build sees them.
Those properties rather than anything else because `java.net`'s own
`DefaultProxySelector` reads exactly them, which is what makes a proxy
this client takes the proxy every other client in the process takes.
An app that already carries `rustls-platform-verifier` has paid for the
same handle, since reaching the Android trust store needs it too.

That is also why the Android **backend** was weighed and not built: the
`rustls-platform-verifier` measurement leaves system proxy and the
cleartext policy as the whole of what a platform stack would add, and a
reader is what a setting needs — the same conclusion `hclient-urlsession`
reached from the other side when MDM roots turned out to be reachable
without it.

**Every rule is a pure function, and the OS-touching half holds no
decisions.** `WinHttpGetIEProxyConfigForCurrentUser`'s registry keys,
`SCDynamicStoreCopyProxies`'s dictionary and Android's four JNI calls are
a handful of lines each and cannot run on the platform this workspace is
developed on; what they hand to
`from_wininet` and to `from_parts` is data, and every rule — the
`scheme=host:port` list, `<local>`, which key means which scheme, the
`host:port` reading — is tested on any host. That is
`system-resolver`'s split between `sys` and its parsers, applied again —
named for `hclient-dns-system` until those platform modules moved there.

**Everything ambiguous is a named refusal rather than a quiet narrowing.**
A transport holds one proxy protocol, so a machine naming a SOCKS proxy
*and* an HTTP one is refused naming the SOCKS one; a bypass pattern the
matcher cannot state exactly is refused naming the pattern; a credential
that cannot become a header is refused naming its proxy. Each
alternative is the same defect in a different direction — dropping a
proxy sends traffic direct that the machine's owner routed, dropping a
bypass sends traffic through a proxy they excluded — and neither is
visible from the call site. This is the *silently ignored setting* defect
one layer below `Capabilities`, where the setting comes from the machine
rather than from the caller.

**A PAC script is the fourth refusal and the sharpest**, because ignoring
it means going **direct** on a machine whose owner routed its traffic
through a proxy — a policy violation, and on a network with no direct
egress a failure nobody can explain from the client's side. It is asked
first, before the static entries, because WinINET keeps those as the
script's *fallback*: honouring them would be taking the machine's second
answer while ignoring its first.

**Two rules came from looking at what a Mac actually ships, and both
would have been wrong by reasoning alone.** `Proxy::bypass_local()` is
the `<local>` / *Exclude simple hostnames* rule — a rule about the shape
of a name, so a flag rather than a pattern — and macOS ships it **on**.
And the bypass dialect grew a **subnet** form, `10.0.0.0/8` and the
abbreviated `169.254/16`, because `169.254/16` is in the default
exceptions list of every Mac: the design refused a subnet on the grounds
that the matcher deliberately has no address arithmetic, which would have
meant refusing the platform's own default configuration. A subnet never
matches a **name**, not even one that resolves into it — matching would
mean resolving a host to decide whether to proxy it, which is an extra
lookup and, on a proxied request, the DNS leak a proxy user is often
there to avoid.

**One `unsafe` came with the macOS reader, and it is amendment C13.**
`core-foundation` implements `ConcreteCFType` for `CFArray<*const
c_void>` alone, so the exceptions list can be downcast to an untyped
array and to no other, and its elements arrive as pointers with no safe
way to read one — checked in 0.9 and 0.10, and `objc2-core-foundation`
has the same wall one level up, at the dictionary. Skipping the list was
weighed and is worse, for the reason above: every Mac has one. What is
assumed is only that the pointer is a valid CF object; **which class it
is, is checked** — `downcast::<CFString>()` compares the type id. The
crate carries `#![deny(unsafe_code)]` rather than losing the attribute.

**Running the script was built, measured and withdrawn, and the
measurement is what is kept.** A PAC file is a JavaScript program, so
honouring one means carrying a JavaScript engine. It was written —
evaluator, the twelve host functions, 24 tests — and then removed, for
three reasons that are all about demand rather than about code:

- **reqwest does not run one**, checked by `grep` over 0.13.4's source:
  zero mentions. Neither does curl. The two most-used HTTP clients in the
  world ship without it.
- **The whole Rust ecosystem has one PAC crate**, `rama-pac`, at **57
  downloads** against its parent framework's 65,062.
- **It had no consumer here and no user anywhere.** `hclient-native`
  never asked it anything, and `hclient` is not published, so there was
  no request to answer. A feature with no reader is the shape this file
  records deleting `UpgradeSupport`'s spare variants for, one size up.

What it would have cost, measured on one program — the same PAC file and
the same four host functions on each engine, `opt-level = "z"`, fat LTO,
`panic = "abort"`, stripped:

| engine | crates | binary | ran the file |
|---|---|---|---|
| `boa_engine` 0.20 | 114 | 3,798 KiB | yes |
| `viperjs` 0.3 | **2** | **1,563 KiB** | yes |
| `nova_vm` 1.0 | 169 | — | not tried, heavier than Boa |

Two findings from that worth keeping. **Boa carries ICU without the
`intl` feature** — `icu_normalizer(_data)`, `icu_properties(_data)`,
`icu_collections`, `icu_locid(_transform)`, `icu_provider` — and it has
**no `default` feature at all**, so `default-features = false` buys
nothing; `icu_properties_data` alone is the 1.9 MB this file measures
elsewhere. And a 2-crate engine really does run a real PAC file
correctly, at 2.4× less binary — the argument against it is not size but
**WPAD**: a script can arrive from DHCP or DNS rather than from a
setting, which makes the engine an attack surface fed by the network, and
86% of test262 from one author is not the thing to point at it.

The wiring was never designed either, and its first question is the
sharpest: **fetching the script needs an HTTP client**, which is the
bootstrap problem `Doh::pinned`/`Doh::bootstrapped` solves for DNS and
nobody has solved here.

**What stayed is the half that was actually missing**:
`SystemProxies::pac()` reports the URL, behind no feature and at no
dependency cost, and `SystemProxyRefused::PacScript` turns a PAC machine
from *silently direct* into a named refusal pointing at
`hclient-urlsession`, which runs the script in the OS. The engine was
never what closed that defect.

**That last thing was recorded as knowable and not done, and the
obstacle recorded for it was the whole of the fix.**
`hclient-urlsession` reported `Capabilities::proxy == false` while the OS
applied a proxy underneath it — a capability that lies, the class this
file treats as worse than a silent downgrade because a caller can act on
a capability. What stood in the way was that `SystemProxies::detect`
reads the environment first, as curl and reqwest do, and `URLSession`
takes its proxies from the system configuration instead. So the addition
is `SystemProxies::detect_platform()`, which reads the platform store and
skips the environment, and the value is
`detect_platform().names_a_proxy()`.

**The order is not a preference, it is the only one that cannot
over-claim.** An environment-first read reports `true` on a machine whose
only proxy is a variable this transport ignores; the platform-only read is
exact if `URLSession` honours the system configuration alone and short of
the truth rather than ahead of it if it honours more. Which of those two
is the case is not settled by anything Apple publishes — DTS's own answer
is *"NSURLSession takes care of proxies for you"* and no more — so the
design is built to be right under either.

**`names_a_proxy` is not `is_empty` negated, and a PAC script is what
separates them.** `is_empty` answers *is there anything here this client
can install*, and a script is not, because nothing here runs one;
`names_a_proxy` answers *does the machine route through a proxy*, and a
script does. The collapse of an unknowable answer onto a `bool` is
towards `true` for the reason `SystemProxyRefused::PacScript` exists:
reporting a PAC machine as unproxied is the *silently direct* answer. A
`*` bypass is checked first and answers `false` even beside a script,
which is the under-claiming reading of a corner nothing documents. A
SOCKS entry answers `true` although `http_proxies` refuses to install one
— the report is read off the machine, not off what this client could do
with it.

**What it still cannot see is WPAD auto-discovery**, which macOS spells
`ProxyAutoDiscoveryEnable` and Windows keeps in a binary blob: this module
reads a script the machine *names*, and a discovered one has no URL to
report, so honouring it needs an answer `SystemProxies` has no shape for.
Stated rather than fixed, and it is the under-claiming direction.

The split is `system-resolver`'s: every rule is
`hclient-proxy`'s and is tested on this workspace's own Linux hosts, the
`URLSession` side is one expression, and the environment-exclusion is
pinned by a test that re-runs the test binary as a **child process** with
an `HTTPS_PROXY` in its environment — `std::env::set_var` being `unsafe`
in edition 2024 — and compares the child's `detect_platform` against the
parent's, so it asserts nothing about the host it runs on. The cost is
measured: `hclient-urlsession` goes from **19 crates to 30** on
`aarch64-apple-darwin`, four of them the platform bindings that do the
reading and none of them `url` or ICU, which `just graph-proxy-cost`
asserts in both directions. It is **31** since the delegate's queue became
a channel — `futures-channel` alone, because `futures-core` and
`futures-sink` were already there through `hclient-core`, which names both
on the WebSocket seam.

### Proxies: an HTTP one and SOCKS5, behind one seam

`Native::proxy(Proxy::new(protocol, host, port))`, behind `hclient-native`'s
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
today**, for the reason `.notes/v02-acceptance.md` already gives about the
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
`.notes/proxy-design.md`.

### The pooled-reuse race has three points, and the middle one was ours

A server can close a pooled HTTP/1 connection between the client's last
look and its write. Every HTTP/1 pool has this; `h1.rs` has called it
"residual" since v0.2 W2 and `.notes/nagle-and-nodelay.md` §6 names the two
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
`.notes/h3-research.md` §3.5 declines for 0-RTT; `RetryKind` answers only
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

`.notes/pooled-reuse-race.md`, including the mutation table and its
control — the connection's *error* arm, verified unreachable by replacing
it with a `panic!` and running the suite rather than by reading hyper.

### The floor moved under a green tree, and the fuzzer found something else

Rust 1.98 landed on 2026-08-18 and CI takes `channel = "stable"`, so the
first push after it met a compiler this workspace had never seen. Two things
came out of that, and only one of them is a lint.

**`clippy::result_large_err` is new here and fires twice**, on
`hclient-native`'s and `hclient-h3`'s private `stage`, whose `Err` is
`(Error, http::Request<RequestBody>)`. Measured rather than boxed: the pair
is **288 bytes, of which 264 are `http::Request<RequestBody>`** — a foreign
type — and 24 are `Error`. Boxing there would silence the lint and shrink
nothing a caller sees, because the public form is `connect`'s
`Result<Self::Staged, Refused>`, the same 288 bytes, which clippy does not
flag only because it is a trait implementation. So both sites carry an
`#[allow]` with that reasoning beside them, and shrinking `Refused` is left
as a seam decision for whoever needs it rather than a lint fix.

**The fuzzer found a real disagreement, and it is not about 1.98.**
`idn_policy_vs_idna` failed on
`"xn--qqqqqqqqqqHJJJJJJ'ｗJJJJJJJJJJJi-0dJd"`: the policy layer answers
`None` where `idna` answers
`Some("xn--qqqqqqqqqqhjjjjjj'wjjjjjjjjjjji-0djd")`. Reproduced locally, and
narrowed far enough to say what it is *not* — the apostrophe alone, the
fullwidth `ｗ` alone, `xn--` with either alone, and three short combinations
all agree. What is left is a long `xn--` label whose UTS 46 mapping leaves
it still undecodable as punycode, where `idna` passes it through and this
crate's own hand-written decoder rejects it.

**Why it matters more than a fuzz crash usually does**: on Linux
`domain_to_ascii` *is* `idna`, so the shipped path there agrees with the
oracle and the difference is invisible. The layer is what runs on Windows
and macOS over ICU and Foundation. So this is a **host that would be
contacted on one platform and refused on another**, which is the one thing
`hclient-idn` exists to prevent — its own claim is that the tables move and
the answer does not.

Not caused by anything in the rename or the dependency bumps: the fuzzer
simply had a different random walk than the 9,739-input corpus that found
zero differences.

**Fixed, and the diagnosis is one line: UTS 46 maps before it looks for an
ACE label, and this layer did it the other way round.** §4 maps first, so
`xn--` is only meaningful on a label mapping has already made ASCII. The
fullwidth `ｗ` maps to an ASCII `w`; decoding before that handed punycode —
which is defined over ASCII — a non-ASCII payload, where it could only
fail. The layer has no mapper of its own, so a label that is not yet ASCII
is now pushed through untouched and the backend does the whole of §4 on it.
What identified the *ordering* rather than the characters is that
substituting the mapped `w` by hand made both sides answer the same string,
before any change. `an_ace_label_is_decoded_after_mapping_and_not_before`
pins it, checked in the failing direction.

**The fuzzer then found a second disagreement that is not a defect, and
narrowing the target is the more interesting half.** On
`xn--xn--aaaaaaax*-nlw` the layer refuses and `idna` accepts — because
`idna` accepts an ACE label whose own `domain_to_unicode` output it then
**refuses to re-encode**. The layer verifies by round-tripping: it emits
the backend's answer only once that answer re-encodes to the label it was
given, so a backend that will not confirm its own output is one it is right
to decline.

So `assert_eq!` was the wrong contract — the layer was never transparent,
it is *confirming* — and the target now asserts the real one: the layer may
refuse where the backend does not, may **never** invent a different answer,
and where it refuses, `idna` must fail its own round trip. That is narrower
and still sharp: the ordering bug above is a case where `idna` round-trips
perfectly, so the new assertion fires on it exactly as `assert_eq!` did.
Checking that before narrowing is what makes this a contract rather than a
silenced alarm.

### One line put the Linux build behind the policy, and three defects fell out

`bundled_to_ascii` called `idna::domain_to_ascii_cow` **directly**, so on
Linux and wasm the shared policy layer never ran. The ICU path's own doc
had the argument against that and it had simply not been applied here —
*"the alternative is two statements of one contract and the newer one is
always the one that rots."* Routing it through cost one expression and
surfaced three defects at once, none of them new.

**The tell was a test that stayed green.** `ä..de` resolves under `idna`
and under Windows's ICU and is refused by Apple's Foundation — a host
reachable on two of this project's three platforms and not the third,
which is the one thing `hclient-idn` exists to prevent. The rule refusing
an empty label went into the shared policy, and `hclient-proto`'s corpus
**stayed green on Linux**: a rule that refuses `ä..de` cannot leave a row
pinning `xn--4ca..de` passing, so the layer was being bypassed. A fix that
changes nothing is a fix in the wrong place.

Refusing is the direction available — nothing here can make Foundation
accept — and the safe one, since an empty label is not a legal DNS label.
A single trailing empty label is the root and stays legal.

**A label can also become empty during mapping, so the rule holds of the
answer.** UTS 46 maps a soft hyphen to nothing, so `"\u{ad}.\u{ad}"`
arrives with two non-empty labels and leaves as `"."`. The fuzzer found it
in under a minute.

**The deny list ran before mapping, where UTS 46 validates after.**
`">\u{338}"` is `>` followed by a combining long solidus overlay, which
composes to `≯` — so the forbidden character is not in the name by the time
§4 validates, and `idna` answers `xn--hdh`. This is the ordering defect
above met a second time, one field over.

**Moving that check to the end was wrong and a test said so in a minute.**
`xn--%-0fa.de` decodes to `%ä`, because punycode preserves the basic code
points verbatim and a literal `%` rides through. The check that caught it
carried a comment reading *"this cannot fire — checked rather than
trusted"*. It could fire, and that comment is what nearly justified
deleting it: **checked rather than trusted is what saved it.** So the check
is narrowed rather than moved — judged on the decoded label, where punycode
could have carried one, and on the converted output, which is the string
that decides which host is contacted.

**Step 6: what this crate emits, this crate accepts.** Steps 1-5 confirm an
ACE label the caller *gave*; they said nothing about one the crate
*produced*, and a label carrying a character that only maps to ASCII is
pushed through untouched by design, so the platform can answer with an
`xn--` label nothing examined. `xn--xn--kd--kd-xn--kd--kdijaakkkx` resolved
on the way out and was refused on the way back — and **the second parse is
the one a redirect hop makes**, so a host reached once became unreachable
mid-chain. The confirmation is that second parse, run once, with `confirm`
a parameter rather than a recursive call because the inner pass must not
confirm its own answer.

All three were reachable before this week and none was reached, because
the fuzz target was asserting `idna`'s behaviour rather than this layer's
wherever the developer was sitting.

### Two versions of `quinn-udp` coexist, and that is the seam paying out

A dependency bump asked for `quinn-udp` 0.6.1. The first look said it was
unreachable — it arrives through `quinn`, and `quinn` 0.11.11 is the newest
release and still requires `^0.5`. That was wrong about *where* it arrives:
it is a **direct** dependency of `hclient-rt-tokio` and `hclient-rt-smol`,
optional, behind each crate's `udp` feature. Ours can move; quinn's cannot.

**What makes the split safe is a decision made long before, for a different
reason.** `hclient-rt` declares its own `EcnCodepoint` rather than
re-exporting `quinn_udp`'s, and the doc comment beside it says so. The
consequence only became visible here: the two sides never exchange a
`quinn-udp` type at all. A runtime converts *our* codepoint into its
`quinn_udp::EcnCodepoint`; `hclient-quinn` converts *our* codepoint into
`quinn::udp::EcnCodepoint`, which is quinn's own copy. Our type is the
interchange format, so the versions on either side of it are free to differ
— and no code changed to get 0.6.1, at any site.

The cost is one duplicate crate, in a build that both enables `udp` on a
runtime and uses quinn — which is the realistic HTTP/3 configuration.
`cargo deny` has `multiple-versions = "warn"`, so it says so without
failing. **Every crate count in this file is unchanged**, because the
duplicate exists only in a workspace-wide all-features graph and never in
any single crate's.

What a Linux run cannot settle is whether the two agree about ECN on a
platform where the answer differs. **There is no CI job for that**, and
this sentence originally said there was — see the ECN paragraph above,
where the job was withdrawn before it ever ran because the mutation is
unkillable everywhere. What exists is `test (macos-latest)`, which runs
the ordinary UDP suite on macOS, and that is what would catch a 0.6
regression there.

**Two more bumps came with it, and the refusal is the interesting one.**
`embassy-executor` 0.10 renamed `arch-std` to `platform-std` and moved the
fallible half of spawning: the `#[task]` macro's function now returns
`Result<SpawnToken<_>, _>` where `Spawner::spawn` returns `()`, so an
`expect` moves one call to the left. Verified by *running* the gated TAP
suite rather than compiling it. `smoltcp` 0.14 was **refused**:
`embassy-net` 0.9.1 pins 0.13, so taking it would put two copies of a
wire-format parser in one test binary, one of them handing types to a stack
built against the other. A duplicate is cheap for a crate whose types never
cross a seam and wrong for one whose whole job is the types.

### One crate was named after a family the dependency rule forbids

Asked before publishing whether any crate is redundant or misnamed, both
questions were measured rather than eyeballed, and they came out
differently.

**Nothing is redundant.** The test is this workspace's own: a crate exists
to hold a dependency a feature would otherwise spread to every graph. The
likeliest suspect passes it most sharply — `hclient-tls-quic` is **153
lines**, the smallest here, and it carries `quinn-proto`, which is exactly
the dependency the argument is about. `hclient-quinn` has one in-tree
consumer and an external reason (41 crates against `hclient-h3`'s 56 for a
caller who wants bare QUIC), enforced by a `just` recipe — **and it was
misnamed, which this pass checked it for redundancy and missed.** It was
`hclient-rt-quinn`, and the arrow points the other way from the rest of
that family: `hclient-rt-tokio` and its siblings implement *our* seam using
someone else's runtime, where this implements *quinn's* — `quinn::Runtime`,
`AsyncTimer`, `AsyncUdpSocket`, `UdpPoller` — using ours. Its own first line
had always said so. It is not a fourth runtime; it is what lets quinn run on
whichever runtime the caller already chose, and the family name made the
wrong reading the default. `hclient-tower`'s bare-foreign-name shape is the
one it takes now, and `.notes/quinn-adapter-extraction.md` went with it. Eleven crates have
no in-workspace consumer at all and are terminal by design: a user picks the
backend.

**`hclient-rt-pair-check` is the one that looks superfluous and is not.** A
5-line lib whose own doc says *deliberately empty*, 500 lines of tests, an
empty `[dependencies]`, `publish = false` — and it must depend on
`hclient-rt-tokio` **and** `hclient-rt-smol` at once, with `udp` on both,
which no shipped crate may do. Its name sits in the runtime-implementation
namespace while being a test harness, and it is left alone deliberately: the
name never reaches crates.io, and it is cited as evidence in seventeen doc
comments across the workspace, so it has become a landmark whose renaming
costs prose and buys nothing outside the repository.

**One name was wrong, and the interesting part is that the missing crate it
implied should not exist.** Three families follow `hclient-<seam>-<impl>`
and each has a seam crate: `hclient-rt` (which carries **hyper**),
`hclient-tls` (**hyper** again) and `hclient-dns` (`dns-message-parser`
behind `codec`). Each head exists to hold something `hclient-core` must
not. `hclient-ws-tungstenite` was the fourth `-<impl>` name and had no head
— and it should not have one: the `WebSocketConnect`/`WebSocket` pair is
**161 lines over `futures_core` and `futures_sink`**, nothing `hclient-core`
does not already have, so an `hclient-ws` would be a crate with nothing to
carry. The name promised a crate the dependency rule forbids.

It is **`hclient-tungstenite`** now, which keeps the framing library in the
name — that choice is argued in `.notes/w4-upgrade-seam.md`, `tungstenite`
over `tokio-tungstenite` because `WebSocketContext` takes the stream as a
parameter and needs no `*mut Context` — and promises no family. Renaming
before the first publish is free and after it is not, which is why the
question was worth asking at this exact moment. The reason lives in the
crate's own README, where a reader looking for `hclient-ws` will be.

One softer observation, recorded and not acted on. `hclient-idn` is not about HTTP at all — it is a UTS 46 crate that picks its
implementation by target, worth more to the ecosystem than the prefix lets
anyone find. It is the owner's call and it is not wrong.

All 29 names were checked against crates.io and all are free. **The first
run of that check answered `403` for every name including its control**,
`reqwest` — crates.io refusing a request with no `User-Agent` — which is
this file's rule about a check whose answers cannot differ, met one more
time. With a `User-Agent` the controls separate: `reqwest` 200, a nonsense
name 404.

### The front page listed 73 items and twelve of them were for the caller

`hclient`'s rendered index was one flat alphabetical list, so **`AnyList` —
a type nobody writes — sat above `Client`**. Counted rather than eyeballed:
73 entries, of which about twelve are what a caller reaches for. The rest
divided into four groups that each wanted a door.

- **14 error payloads**, reached only through `Error::source` — now
  `error::`, with `Error` and `ErrorKind` kept at the root because they are
  on every signature in the crate and a door in front of them is one every
  caller walks through immediately.
- **11 hooks types** re-exported from the core for a seam most callers never
  touch — now `hooks::`.
- **6 capability reports** — now `caps::`.
- **5 response-body wrappers** that are public for one reason only: the
  alias `ClientBody<B, Tm> = Limited<Decompressed<Deadline<Cached<B>, Tm>>>`
  names them, and an alias cannot name a private type. Now `body::`, with
  the alias, so the four wrappers sit beside the thing that needs them.

Plus `sse::`, `redirect::` and `erased::` — the last for `AnyList` and
`AnyStore`, which a caller meets only as `CookieJar<AnyList>`.

**Nothing about a type changed. What changed is the path a reader types**,
and rustdoc renders modules before items, so the page is now 16 names and
12 doors. Free before the first publish and a major version after it, which
is the same window the crate renames used.

**Two orphaned `#[cfg]` attributes came out of the edit, and the second one
is the instructive one.** Deleting a re-export left its attribute behind,
where it silently attached to the *next* item: `#[cfg(feature = "charset")]`
landed on `pub use response::{Collected, Response};`, so without that
feature the two most-used types in the crate did not exist. `cargo check`
with default features never saw it — `charset` is off by default, but
`--all-features` is what the suite runs. **`test-no-default` is what
caught it**, which is the recipe this file records as having once printed
`error:` and exited zero. It earns its place here.

### `hclient::Client` names no type parameters, and the browser decided what that costs

`Client` is one concrete type. `Clone` is an `Arc` bump, and a library takes
`&Client` where it used to write five `where` lines — measured on this
workspace's own consumer, `examples/portable.rs`, whose `fetch` went from
`fetch<T, S>` with four transport bounds to `fetch<S>` with none. A generic
function has to restate its callee's where-clause, and that is the tax
erasure removes.

**Two recorded blockers were cleared by not asking the question.**
`docs/competitive-gaps.md` §G13 said `Transport`'s RPITIT needs return type
notation (`E0658` on 1.98, true) and that `Timer::Instant: Copy` is
permanent (also true). Both are irrelevant: the boxed future declares no
`Send`, so there is nothing to prove and `BoxedTransport` gets a **blanket
impl** over every `Transport` — a backend author writes nothing — and
`ErasedInstant` answers *how long ago was this*, so the instant never
crosses the boundary and `Copy` is asked of nothing.

**The first attempt at this was abandoned, and the reason it was abandoned
is the reason this one works.** That version put `Send` on the boxed future
so a request could be spawned, and following the bound down needs it on
seven seam methods — at which point `hclient-rt-embassy`'s `connect` future,
which holds `RefCell<embassy_net::Inner>` because embassy's executor is
single-threaded, is excluded. Dropping the bound removes the whole chain.

**What it costs is three things, and only one of them was in the plan.**

**The embedded target has no `Client`.** The plan asserted *"`Embassy` is
refused there, and already was"* — wrong, and the distinction is the lesson:
being `!Send` and being *refused* are different, and the generic `Client`
built over Embassy perfectly well, it merely could not cross a thread. An
erased `Client` boxes its transport `Send + Sync`, and `RefCell<embassy_net::
Inner>` is not `Sync`. The nine live TAP scenarios are written against
`Native`/`Transport` directly now, ~20 lines of helper, and the CI job is
unchanged and green; what it no longer covers is the target-independent
`Client` layer, exercised by the rest of the suite on every other backend.

**Nothing a request produces is `Send`, and the browser is why.** One
`ClientBody` serves every backend, and `hclient-fetch`'s body holds a `dyn
Stream` with no auto trait — so `Send` on the erased body does not weaken
the browser backend, it **excludes** it: `Client::builder(Fetch::new())`
stops compiling. Measured, and only after the `Send` version had been
written and the entire native suite made green under it, because **`cargo
nextest run --workspace` does not build for `wasm32-unknown-unknown`** —
this file records the same blind spot hiding a broken browser suite for six
merges once before. The cheap check is `cargo test -p hclient-fetch --target
wasm32-unknown-unknown --no-run`. So `tokio::spawn` of a response body is
gone, which worked on `hclient-native`; a caller who needs it reaches past
the facade with `Client::transport_as::<Native<..>>()`. The request future
was cheaper to lose, because `Native`'s was *already* `!Send` — and that
half has since been fixed at its cause, so the sentence now cuts the
other way: see the section below. **`Client` itself stays `Send +
Sync`**, which is the half that has to.

**True, and not for the reason it reads as — measured later, and the
difference is what makes it fixable.** *The browser* is not a category
that is `!Send`. `wasm32-unknown-unknown` without atomics is one thread,
and wasm-bindgen marks its handles accordingly: asked directly on that
target, `JsValue`, `js_sys::Promise` and
`web_sys::ReadableStreamDefaultReader` are all **`Send`**, and so is
`Fetch::execute`'s own future. `hclient-wasi` is `Send` **throughout** —
transport, future and body — so it was never part of this question at
all. What is `!Send` is exactly one third-party type: `js_sys::JsFuture`
holds `Rc<RefCell<Inner<T>>>` for its two promise callbacks (0.3.104,
still the latest), and it arrives in the body through
`wasm_streams::readable::IntoStream`. `NativeBody` is `Send` as well, so
the erased `ClientBody` is **one crate's read loop** away from being
`Send` on every backend here.

**And the adapter that closes it already exists in this crate**, which
the paragraph above got wrong within a day of being written.
`promise::SendJsFuture` is exactly the `Arc<Mutex<..>>`-where-`JsFuture`-
has-`Rc<RefCell<..>>` shape, with `SingleThreaded<T>` carrying the one
`unsafe impl Send` this workspace allows (amendment C7, and the only
`unsafe` in the crate). `Timer::sleep` and the WebSocket are built on it.
The body is not: `from_response` reaches for
`wasm_streams::ReadableStream::into_stream()`, and `IntoStream` holds a
`js_sys::JsFuture`. So the remaining work is **using the adapter in one
more place**, not writing one — a hand-rolled loop over
`ReadableStreamDefaultReader::read()`, whose cost is the reader
lifecycle and cancel-on-drop that `wasm_streams` is doing today.

**The actor is what landed, and it is the more expensive of the two on
purpose.** `hclient_fetch::Body` is `Send` now: `body::pump` owns the
`IntoStream` on the thread that made it and hands `Bytes` — already
`Send` — over a `futures_channel::mpsc` of capacity zero, so no JS handle
crosses to the caller's side and nothing about the property depends on
how many threads there are. The adapter would have been fewer lines and
would have died under `+atomics`, which is the configuration this was
chosen against.

Three things it cost, each answered rather than waived. **A spawn**,
which this workspace refuses everywhere else — the refusal is about work
continuing behind a caller who walked away, and the pump is bounded one
chunk ahead by the channel and ends when the `Body` drops. **One crate**,
`futures-channel` (33 to 34; `wasm-bindgen-futures` was already there
through `wasm-streams`), bought rather than written because a hand-rolled
single-slot channel is waker code and its defects are silent hangs.
**And cancellation, which had to be built rather than inherited**: a drop
used to reach `IntoStream` synchronously and fire `wasm-streams`'
`cancel_on_drop`, where now it reaches the pump. Closing the channel is
noticed at the send and is enough for a body that is producing; a pump
parked on a `read()` a quiet peer will never answer never gets there. So
the `Body` also holds a `oneshot::Sender` it never sends on, selected
against every read. The pair of tests is the assertion — a pump watching
only the channel passes
`dropping_a_pending_body_cancels_the_underlying_reader` and fails
`dropping_a_body_whose_read_will_never_answer_still_cancels`, verified by
applying exactly that mutation.

**And the declaration it was for has followed.**
`erased::{BoxBody, BoxSleep, BoxInstant}` carry `Send` now — amendment
C14 — so a `Response<ClientBody>` from the erased `Client` crosses a
thread again, which it stopped doing when `Client` gave up its type
parameter. `crates/hclient/tests/spawnable_body.rs` collects one on
another thread.

**The request future is deliberately still `!Send`.** `BoxExchange` is
unbounded, so `Transport::execute` is untouched and
`hclient-rt-embassy`'s `RefCell`-holding `connect` future is not
excluded — the bound that abandoned the first erasure attempt is exactly
the one not taken here. What crosses a thread is what a request
*produced*, not the act of making it; a caller who needs the second
still reaches past the facade.

**Checked where it would have hurt most.** `hclient` with
`default-transport` builds for `wasm32-unknown-unknown` under
`-Ctarget-feature=+atomics` — the browser keeps `Client` under wasm
threads. That took one more repair of the same kind: `BrowserClock::Sleep`
was `Discard<SendJsFuture>`, whose `Send` is the `unsafe impl` the
atomics `cfg` strips, so it is `timer::Elapsed` now — a
`oneshot::Receiver<()>` the spawned waiter fires, holding no JS and
claiming nothing about threads. The timer still starts when `sleep` is
called, because the promise is built before the spawn.

`fetch-must-fail-under-atomics` still rejects, which is the check working
rather than a leftover: `SendJsFuture` is what the WebSocket and the
sleep's own waiter still run on, and its `Send` must still disappear
under threads.

**A sweep for what is still `!Send` found one more of the same, and it
was a feature away rather than a target away.** `http2::On1xx` was
`&'a dyn Fn(StatusCode, &HeaderMap)`, held across an await, and its doc
said the callback "neither outlives this call nor crosses a thread" —
true, and it was the erasure rather than the callback that settled the
second half. So every build with `http2` on had a `!Send` future,
including one whose hook is an ordinary `Send` type. It is a type
parameter now, and the property is inferred: an `Rc`-holding hook still
yields a `!Send` future and nothing else does.

That one had no gate, because the workspace run is `--all-features`,
where `http3` switches `tests/send_future.rs` off. `just test-no-default`
runs this crate's suite under `--features http2`, which is where it is
checked now — 282 tests.

**What is left, re-measured from outside the workspace — and this table
has now gone stale twice, the second time within hours of being
corrected.** The first correction fixed three of four rows that C15 and
C16 had quietly made true. The DoH row survived it, said `!Send`, and was
fixed by `8eeb9e6` **the same day** — after which the row went on saying
`!Send` for another week, and was quoted back into a design discussion as
though it were a fact. A claim is as perishable as the thing it
describes, and this file's own example of the rule is this paragraph.

| what | measured | |
|---|---|---|
| `Client`, its request future, `Response`, `ClientBody`, `Collected`, `Error` | **all `Send`** | asserted from a scratch crate depending on `hclient` by path |
| `Native::execute`, plain | **`Send`** | |
| `Native::execute` **with the `http3` arm installed** | **`Send`** | the bounds live on the opt-in `Native::http3`, amendment C15 — the row that used to say the blanket impl could not prove them |
| `hclient-tower`'s `Service::call` future | **`Send`** | its `type Future` has declared `Send` since C16; the row saying it needs return type notation outlived the fix |
| a transport whose resolver is `hclient-dns-doh::Doh` | **`Send`** | since `8eeb9e6`: `execute_send` names its future, so the streams are declared `Send` — and `tests/send.rs` asserts it |
| `hclient-fetch` | `!Send` under `+atomics` by nature | a JS `WebSocket` belongs to the realm that made it |

**So nothing is left, and the paragraph that stood here proposed a
repair that had already been made.** It read: DoH resolves through a
generic `C: Transport` whose `execute` is an RPITIT, so its streams
cannot be declared `Send`; `SendTransport` would fix it at the cost of
narrowing the bound; *measured, not done*. Every word was true when
written and none of it after `8eeb9e6`, which made exactly that change —
`C: SendTransport`, `execute_send`'s named `BoxSendExchange`, and two
`Send` stream aliases.

**The cost of leaving it was not the sentence, it was the decision it
shaped.** Asked which seams this workspace still owes, this row was read
off the table and reported as an open item — twice, in the same
conversation, before anyone opened `tests/send.rs` and found its first
line saying *this **was** the last `!Send` in the workspace*. A table is
read where a test is not, which is an argument for the table naming the
test rather than restating it.

Re-measured from a scratch crate outside the workspace: `Client` and its
request future, `Response`, `ClientBody`, `Collected`, `Error`,
`Native::execute` with and without the h3 arm, `hclient-tower`'s
`Service::call`, and `Doh`'s `lookup_ipv4`, `lookup_svcb` and the
resolver itself — **all `Send`**. The only remaining row is
`hclient-fetch` under `+atomics`, which is not a defect but a fact about
a JS realm.

Everything else that grep finds is a trait object whose trait already
declares `Send` (quinn's `AsyncTimer`, `AsyncUdpSocket`, rustls'
`ClientSessionStore`) or a `dyn Any`/`dyn Error` that never crosses an
await.

**The fourth row is closed, and it cost no `unsafe` and no actor.**
`FetchWebSocket` is `Send`: the state cell is `Arc<Mutex<..>>` like
`promise::State` beside it, and the three `Closure`s ride
`promise::SingleThreaded`, which already carries this crate's one
`unsafe impl Send`. Its own module doc had read *"`Rc<RefCell<..>>`, not
`Arc<Mutex<..>>` — so no `unsafe`"* for a vertical, and the second half
did not follow from the first: `Arc<Mutex<..>>` needs no `unsafe` either,
and the wrapper the closures needed was already written.

**Giving the closures a `Send` inner `dyn` was tried first and is
impossible**, which is worth knowing before someone tries it again:
`WasmClosure` is implemented for `dyn FnMut(..) -> R + 'a` and no other
shape (wasm-bindgen 0.2.126, `convert/closures.rs`), so
`Closure<dyn FnMut() + Send>` is a type that exists, satisfies `Send` by
auto-derivation, and cannot be constructed. That is also the reason
`promise.rs`'s `unsafe` cannot be deleted.

**And the actor was refused here, having been chosen one module over.**
The difference is what each buys. `body::pump` feeds `ClientBody`, an
erased type shared by every backend, which must be `Send` for everyone or
for no one — so paying a spawn there bought the property for the whole
facade. `WebSocketConnect`/`WebSocket` declare no `Send` at all and
nothing erases them, so an actor here would buy a property nothing reads
— and it would cost a real one, because `Sink::start_send`'s refusal and
`poll_close` are **synchronous** today and a channel would make both
asynchronous. So this is `Send` exactly as far as `JsValue` is, and it
disappears under `+atomics`, which is honest: a JS `WebSocket` belongs to
the realm that made it.

**Three of those four are one blocker, and it was taken all the way to a
working build before being reverted.** They are not four walls: they are
the same wall, that a `dyn` declaring `Send` obliges whoever boxes into it
to *prove* it, and proving it for a generic parameter means naming an
RPITIT future. Return type notation is the language feature for naming
one, so the whole thing was built on nightly under `--cfg rtn_probe` to
see what it actually costs.

**It works.** `T: Transport<execute(..): Send> + Sync` on
`BoxedTransport`'s blanket impl, `Send` on `BoxExchange`, the same
treatment for `http3::arm`'s three boxes with
`StagedConnect<connect(..): Send, exchange(..): Send>` and its bounds on
the opt-in `Native::http3`, and `Send` on `hclient-tower`'s `type Future`
— the whole workspace compiles, and
`assert_send(client.get(u).send())` passes. **`Client::execute`'s future
is `Send` under RTN**, which is the property this file has recorded as
lost since erasure. Neither `hclient-dns-doh` nor `hclient-rt-embassy` is
touched: nothing moves to a seam, so nothing has to be satisfied by a
backend that cannot.

**And then the bill arrives somewhere else, which is the finding.** Two
consumers in this workspace stop compiling, and both are the same shape:

- `crates/hclient/tests/two_runtimes.rs` is generic — `fetch_once<R>` over
  `R: TcpConnect + Timer + Blocking`. Under RTN, `Client::builder(t)`
  demands `execute(..): Send` of a *type parameter*, so the caller must
  restate the whole chain: `TcpConnect<connect(..): Send>`,
  `Blocking<run(..): Send>`, `Resolve<lookup_ipv4(..): Send, ..>`. That is
  **the seven-seam cascade this file already records — moved out of the
  seam and into every generic consumer**, which is the exact tax erasure
  was introduced to remove.
- `crates/hclient-tower/tests/round_trip.rs` returns `impl Transport`, and
  an opaque type does **not** leak an RPITIT bound, so it has to be
  restated there too — and **it cannot be**, because an opaque type has no
  name to hang a bound on. Re-measured on 2026-08-28 with a minimal
  two-crate reproduction: a `fn make() -> impl Seam` gives its caller a
  clean `E0277` and no way to say what would fix it; only the *producer*
  writing `-> impl Seam<go(..): Send>` does, and that is a foreign crate's
  signature. **RTN does not travel through `impl Trait`**, which is the
  durable statement.

  The sentence here used to say that restating it *ICEs*, with the
  `DefId(.. Transport::execute::{anon_assoc#0}) does not have a "type_of"`
  from 1.100.0-nightly (f7d782a3b, 2026-08-19). That was seen, in this
  workspace's real code, and it does **not** reproduce minimally on the
  same nightly — so it is one manifestation rather than the rule, and the
  rule above is what a reader should act on. A crash observed once is
  weaker evidence than a limitation reproduced on demand.

So the answer to *can we fix all of it* is: **yes for a concrete
transport, and the generic case pays what the seam would have paid.**
Everything above was reverted; what is kept is the measurement, because
the next person to ask will otherwise re-derive it. `hclient-tower`'s own
module doc says the fix is one bound when #109417 lands — true of that
crate, and this is the rest of the bill.

### A sweep for stale limitations, and what separates the two kinds

Two documented "cannot"s turned out to be false in one week —
`hclient-tls-native-tls`'s ALPN and `hclient-fetch`'s `!Send` body — and
both had **named their own cause correctly** while nobody acted on it. So
the rest were swept for the same shape. Six more were false and are
fixed; four were checked and stand.

**What was false**, all of it about `Send` and all of it made obsolete by
the associated-future work rather than by upstream drift:
`hclient-core`'s `erased` module doc (*nothing boxed here declares
`Send`*, above four aliases that now do, and *a backend author writes
nothing*, which is one method now); `BoxBody`'s own doc (*Not `Send`*,
directly above the line declaring it); `hclient`'s crate doc (*what a
request produces is not `Send`*); `hclient-mock`'s (*`Client::execute`'s
future is `!Send` whatever is underneath it*); `hclient-tower`'s (*today
that cannot be fixed here*, plus *when #109417 lands, the fix is one
bound*); and `hclient-native`'s long block on why the QUIC arm could not
be `Send`.

**One of them was hiding an unrun test.** `hclient-native`'s
`tests/send_future.rs` was gated `not(feature = "http3")`, which was
honest when the arm was `!Send` — and left the property out of the
workspace's own `--all-features` run once it stopped being. The gate is
gone and the file's positive doctest fence is back, having been removed
for the same reason.

**What was checked and stands**, recorded so the next sweep does not
re-derive it: `native-tls` really reports no protocol version and no
cipher suite — its `Protocol` type is a setter pair, not a getter;
`h2::client::Connection` really reports no traffic, so an h2 keep-alive
cannot measure silence the way the WebSocket one does; every third-party
version a doc comment cites is the version in the graph; and `h3` 0.0.8's
client really has no `enable_webtransport` — though that line now says
which setting it means, because the crate *does* announce
`enable_extended_connect` and `enable_datagram` three lines away and the
compressed phrasing read as announcing nothing.

**The rule the sweep produced.** A claim about a third party goes stale
in two ways, and only one of them is upstream's doing. Version drift is
the obvious one and was absent here. The other is a claim about a
**wrapper** — *this crate does not expose X* — where the layer beneath
does, and which stays true of the wrapper for ever while the conclusion
drawn from it quietly stops being. Both of this week's findings were that
shape, and `negotiated_alpn` is the sharpest: the doc named the wrapper as
the cause, correctly, in the same paragraph that called the limitation
concrete.

### `Client`'s request future is `Send`, and RTN was never needed

**`tokio::spawn(client.get(u).send())` compiles.** The route is not the
one above and does not wait on anything: the four seams whose futures
`Native::execute` awaits — `Blocking`, `TcpConnect`, `TlsConnect`,
`Resolve` — carry **associated future types** instead of RPITITs, so a
consumer can *name* them, and `SendTransport` (amendment C16) is a
separate trait whose impl carries the bounds `Transport` does not.

**Naming is not requiring, and that is the whole of why this works where
`Resolve → BoxStream` did not.** A fixed `Send` box in a seam excludes
whoever cannot satisfy it; an associated type lets each implementor
answer for itself. Measured in-tree, three ways:

- `Tokio` and `Smol` box `Connecting` `Send`; `hclient-rt-embassy` boxes
  it plain, because `embassy_net::Stack` is `&'d RefCell<Inner>`. Both
  are `TcpConnect`s.
- `hclient-dns-doh` boxes its streams plain, because it resolves through
  a generic `C: Transport` whose `execute` is an RPITIT. It is a
  `Resolve` like any other; what it loses is one layer up.
- `hclient-tls-native-tls` boxes its handshake plain, because
  `async_native_tls::TlsConnector::connect` is a `pub async fn` and its
  future has no name.

**One seam needed a named type rather than a box, and the reason
generalises.** `TlsConnect::connect` is generic over `S`, so its future is
`Send` exactly when `S` is — a box would have to pick one answer for
every `S`, and both answers are wrong (`+ Send` excludes embassy's IO,
which would take embassy out of `Native` entirely; without it every
handshake is `!Send` for everybody). `hclient-tls-rustls` writes
`Handshaking<S>`, two states and a poll loop, and the answer is derived.
The sync preparation moved into `connect`, which is what made that a
dozen lines instead of a state machine.

**What a generic consumer pays is real and, unlike under RTN, payable.**
`two_runtimes.rs` and `reaper.rs` restate `for<'a> R::Connecting<'a>:
Send` and its neighbours — ordinary bounds, three lines each. The same
restatement expressed as return type notation is unstable and, across a
crate boundary, ICEs.

**What it costs at runtime** is one allocation per connect, resolve,
handshake and blocking call. `hclient-tls-rustls`'s handshake allocates
none: it is a named type, not a box.

**And what it costs a backend** is one method, whose body at a concrete
type is `Box::pin(self.execute(req))` — `Send` inferred, not proved.
Proof is only ever owed by generic code, which is the asymmetry the whole
design rests on.

**Three backends owed that method and did not have it for a day**, and
the workspace was green over all three: `hclient-fetch`, `hclient-wasi`
and `hclient-urlsession` build for targets `cargo nextest run --workspace`
does not — `wasm32-unknown-unknown`, `wasm32-wasip2` and Apple — so
`Client::builder(Fetch::new())` and its two siblings stopped compiling and
nothing said so. This file already records that blind spot twice; this is
the third, and the cheap checks that catch it are the ones already
written: `wasm-pack test --headless --firefox`, `cargo check -p
hclient-wasi --target wasm32-wasip2 --all-targets`, and `cargo check -p
hclient-urlsession --target aarch64-apple-darwin --all-targets`, which is
clean on a Linux host.

**The two are not interchangeable, and which one is right depends on a
configuration nobody here builds yet.** The adapter's `Send` is a claim
about there being one thread, so it is stripped under `+atomics` by the
same `cfg` that strips wasm-bindgen's own — under wasm threads it cannot
help, and nothing holding a `JsValue` can be `Send` there by any means.
The actor can: it keeps every JS handle on the thread that owns it and
hands `Bytes` — already `Send` — across a channel, so the type crossing
the boundary holds no JS at all. So the adapter is the cheap answer for
the single-threaded target this workspace ships for today, and the actor
is the only answer that survives wasm threads. Worth knowing before the
cheap one is taken as the answer to both.


**A `!Send` hook cannot be watched through `Client`.** `hclient-fetch`'s P13
test — *a single-threaded runtime can watch* — ran through `Client` with a
hook holding an `Rc`. It asserts the same property at the `Transport` layer
now, which is where the property actually lives; what is lost is watching a
`!Send`-hooked browser transport *through the facade*, so cookies, redirects
and the cache are unavailable to a caller who wants that.

**A `#[cfg]` making `BoxBody` `Send` off-wasm is refused**, though it would
give native callers their spawnable bodies back and would not be a
capability that lies — `Send` is a claim about threads and the wasm targets
have none. It is refused because a cfg-alias hides the symptom rather than
removing the cause, and because a `ClientBody` whose auto traits depend on
the target is a thing a portable library cannot reason about.

**The `send-bound-exception` markers live on two short aliases now**, and
that is the same rule this file has recorded three times from the other
side: `cargo fmt` moves a trailing comment off a line it reflows, and a
marker lost that way was lost twice more during this change.
`erased::SharedTransport` and `erased::SharedTimer` are
`dyn .. + Send + Sync` on one line each, every use site writes
`Box<SharedTransport>`, and `cargo fmt` and `just invariants` now pass
*together*.

**The rule is narrower than it was stated, which is worth knowing before
paying for it again.** "Deletes one from a `where` clause" is false as a
general claim — measured with `rustfmt --edition 2024` on both shapes: a
short predicate keeps its trailing marker untouched, and a predicate long
enough to wrap keeps it too, because the marker travels with the
continuation line that carries the `Send`. Since `no-send-or-sync` scans
for a `Send`/`Sync` line and a marker on that same line, both survive the
check. `Native::http3`'s four bounds are written that way and `cargo fmt`
and `just invariants` pass together over them.

So the shape that actually loses a marker is the one the first sentence
names — a comment on a line fmt has to reflow *around*, not one on a
`where` predicate. Believing the wider version cost three helper traits
and two public re-exports in `hclient-native` before it was measured. Four `amendment-C1` markers **left** `client.rs` with
the type parameter, because `Transport::to_error` is now called where `Self`
is concrete.

`.notes/erased-client.md` has the measurements, including the
`package-build` trap met on the way: it verifies against the shared
`target/debug/deps`, so a stale `rmeta` for an unchanged version makes it
fail — or, worse, pass — misleadingly.

### A `dyn` that declares no auto traits does not hide `Send` — it removes it

`Native::execute`'s future is `Send` now, and the change is one field of
one private struct: `connect.rs`'s `Answers` held the resolver's stream as
`Pin<Box<dyn Stream<..> + 'a>>` and holds it as `Pin<Box<S>>`. Same
allocation, same absence of `unsafe` — the box is there for pin
projection, which is what its comment always said — and the concrete type
is simply no longer thrown away. **No bound, no `send-bound-exception`
marker, no dependency**: the property is *inferred* per instantiation,
so a `!Send` resolver still works and still yields a `!Send` future.

`tokio::spawn` of a request works, which is what the consumer asking for
this needed. `tests/send_future.rs` pins it twice — once on the type, once
by actually spawning one against a server — and was checked in the failing
direction, where the old `dyn` names `Answers` as the type that contains
it.

**This crate's own doc comment had named the box as the single cause for
two verticals and drawn the opposite conclusion**, because it weighed
exactly one repair: declaring `+ Send` on the `dyn`. That does oblige the
seam — `Resolve::lookup_ipv4` returns `impl Stream`, unnameable, so
unbounded, still `E0658` on 1.98 — and the argument was right about its
own question. *Removing* the `dyn` was never asked.

**The alternative was built and measured before this one was believed.**
Converting `Resolve` to `BoxStream` and `Blocking::run` to `BoxFuture`
(both from `futures-core`, already in every graph here; one feature,
`alloc`, no crate added) works, makes `Resolve` object-safe, and takes 71
call sites across 17 files. It costs **`hclient-dns-doh` entirely**: DoH
resolves through a generic `C: Transport`, whose RPITIT future is equally
unnameable, and no consumer can supply the impl, because both the trait
and the type are foreign to them. One crate, no fix inside the design.

**The rule that came out of it is the part worth keeping**, and it
explains the erased-`Client` section above from the other side:

- at a **concrete** type `Send` is *inferred* — nothing has to be named;
- in a **generic** impl `Send` must be *proven* — every RPITIT future in
  the chain has to be named, and `impl Future` has no name.

So the six RPITIT seams here — `Transport`, `Resolve`, `TcpConnect`,
`TlsConnect`, `Blocking`, `WebSocketConnect` — block a *declaration* and
not an *instantiation*. Which is why the cheap repairs are the places a
`dyn` discards a property the concrete type already had, and the
expensive ones are the places something must promise it in advance.

**The `http3` arm is still `!Send`, and it is the same decision rather
than a second one.** `H3` itself is clean — `H3::resolve` boxes the
concrete stream, and `UdpBind::bind` and
`QuicTlsConnect::quic_client_config` are **synchronous**, so nothing
there loses the property. What loses it is `http3::arm`'s deliberate
erasure, `Box<dyn BoxedStaged<'_>>` and `Staging<'a>`, which exists to
keep `H3`'s bounds off `Native`'s `Transport` impl. Declaring `Send`
there obliges that **blanket** impl to prove `StagedConnect::connect`'s
RPITIT future `Send` for a generic `T`, and behind it `Resolve` again.
So the QUIC arm and the DoH resolver are one question with one answer.

**And the doctest gate did not catch the claim going stale**, which is
this file's recurring rule met from a new direction. The `compile_fail`
fence asserting the old `!Send` had to start failing, and it did — in the
**default** build. `just test-doc` runs `--all-features`, where `http3`
keeps the future `!Send`, so the fence still passed there. A doctest
cannot be gated on `not(feature = "http3")`, so the positive half lives
in `tests/send_future.rs` and the fence that stays is the one true in
every configuration. Worth knowing before adding another: **a doc fence
can only assert what holds under `--all-features`.**


### A default is not a default when Cargo unifies features — it is a floor

`cargo add hclient` gives a client that does not compile: `Client::new()`
needs `default-transport`, and `default` is `["idn"]`. That cost is real and
lands in the five minutes where someone decides whether to keep reading, so
the feature **was** moved into `default` — and moved back out one commit
later, which is the part worth keeping.

**Cargo unifies features across a graph, so a default here is a floor.**
Measured on a scratch workspace rather than argued: `lean` depending on
`hclient` with `default-features = false`, `fat` depending on it with
defaults, and cargo builds **one** `hclient` — `default,default-transport,idn`
— which `lean` links. `lean` alone resolves zero tokio, rustls or hyper; in
the shared graph it gets all three. **The party who wanted the small graph
is not the party who decides.**

That is the same argument that keeps `hclient-tls-quic` out of
`hclient-tls` and the WebSocket framing in a crate of its own. **It is not
what keeps `hclient-h3` out of `hclient-native`** — that reason was
measured and is wrong, see the HTTP/3 section. Applying it to the two it
fits and not to a feature list would have been the inconsistency.

**The audience it protects is narrower than "every constrained build",
which is worth knowing before the next time this is raised.**
`hclient-native/examples/minimal.rs` reaches `Transport` directly and never
names `hclient`, so a 512 KB target is unaffected either way. The one who
pays is the caller who wants `Client` over a transport of their own —
`Native<Smol, NativeTls, Hickory>` — and would carry tokio, rustls and the
system resolver because something else in the graph was careless.

So the cost is paid in text: both READMEs and the crate docs now lead with
`cargo add hclient --features default-transport`. A flag someone reads
before compiling is cheaper than a graph they cannot get out of afterwards.

**One real defect came out of the round trip and is kept.** `DefaultClock`
had three `#[cfg]` arms — native-with-feature, browser-with-feature, and
`not(feature)` — and `wasm32-wasip2` **with** the feature matched none of
them, so the crate did not compile there at all. Its doc comment called that
"the same deliberate compile error as `DefaultTransport`", and the two are
not the same: `DefaultTransport` is named only by someone asking for it,
where `DefaultClock` is the default type parameter of `ClientBuilder`,
`RequestBuilder` and both forks of `Client`. And by the same unification
above, the trigger was never that user's own choice. `Client`'s forked
declaration had the identical gap for the identical reason — it keyed on
*the feature* where the question is *does `DefaultTransport` exist*. Both
now use one pair of conditions, negations of each other, so the arms are
exhaustive and non-overlapping by construction.

### The flag is paid in text, and the text was the only thing paying

The paragraph above ends *"the cost is paid in text"*, and being a user
showed the text is not where the cost lands. Measured from a fresh crate
outside this workspace, on the two lines the crate's own front page opens
with: `cargo add hclient` resolves, and `Client::new()` is `error[E0599]:
no associated function or constant named "new" found for struct "Client"`
— **no part of which mentions a feature**. A reader who skipped one line
of prose gets a message that reads as *this crate does not have that
function*.

**The free function beside it needs nothing at all, and that asymmetry is
what decided the shape of the fix.** `hclient::default_transport()` under
the same build is `error[E0425]` carrying rustc's own *"found an item that
was configured out"* note, which points at the `#[cfg]` line and underlines
`feature = "default-transport"`. That note is emitted for **path**
resolution and not for associated-item lookup — so a free function
announces its own gate and an inherent `fn` in a `#[cfg]`-ed-out `impl`
block does not. One stub, not a pair.

**So on the branch where there is no default transport, `Client::new`
exists** — with a where-clause nothing can satisfy and the message on the
trait it names, through `#[diagnostic::on_unimplemented]`. The obvious
spelling of that does not compile: a where-clause predicate carrying no
generic parameter is checked at the **definition** site, so
`where Self: DefaultTransportFeature` refuses to build this crate at all,
`error[E0277]` on the `where` line. A lifetime parameter the caller never
writes is what defers the predicate to the call site.

**The headline forks, because there are two reasons to be standing there
and only one of them is the feature.** `wasm32-wasip2` reaches the same
stub with `default-transport` **already on** — `hclient` does not depend on
`hclient-wasi`, so there is no branch to resolve — and telling that caller
to add a feature they have is the one wrong answer a single message would
have given. Within the stub's own gate the pair is exhaustive and mutually
exclusive by construction, which is the shape `DefaultClock`'s arms were
repaired into one section up.

`just first-five-minutes` is the check, in the `no-default` job, and it is
the reader's own instrument rather than a test written beside the code: a
crate outside this workspace, with a path dependency, built four ways. Each
arm was broken on purpose and watched — deleting the stub returns the
`E0599`, giving WASI the feature message trips the third arm, renaming
`default_transport` trips the second — and the control is the same source
compiling with the feature on, without which the recipe would be green for
a crate that refuses everything.

**What is not fixed is the first line of the error, and it cannot be.**
`Client::new()` still fails to compile; what changed is that the failure
names the flag and the command. Nothing here widens the default feature
set, for the reason the section above measures.

### The workspace was `http-ng`, and the prefix is what decided its replacement

Three objections killed the old name, and they are independent of each
other. `-ng` means *next generation of X*, so `http-ng` in a dependency list
said it superseded the `http` crate — which has **932 million downloads**
and which this workspace *depends on*. Wrong in both directions. The
`http-*` namespace is plumbing besides: `http` 932M, `http-body` 800M,
`http-types` 53M, `http-client` 7.5M are all types-and-traits crates, while
every client that found an audience is a distinctive word — `reqwest` 654M,
`ureq` 182M, `attohttpc` 32M, `isahc` 17M. And `-ng` has no Rust precedent:
`zlib-ng`, `mio-ng` and `tokio-ng` do not exist, and the single hit,
`libz-ng-sys`, is only that because the upstream C library is literally
named `zlib-ng`. It is a C convention, and it dates.

**What decided the replacement is that this publishes a family of 29, and a
family prefix should be legible rather than clever.** Fifty-four candidate
names were checked for availability; most good single words are gone, and
the survivors — `wend`, `voyage`, `portage`, `transom`, `wayfare` — all win
*distinctiveness* and lose the thing that matters here.
`hclient-dns-doh`, `hclient-rt-embassy`, `hclient-tls-rustls` tell a reader
who lands on any one of them which world they are in. `wend-dns-doh` does
not until they look `wend` up. `h` is not arbitrary either: `h2` and `h3`
are the ALPN identifiers and the names of the canonical Rust crates, so `h`
means HTTP in this domain already.

**The cost is real and is not hidden**: `hclient` is descriptive where the
successful clients are oblique. A descriptive name has nothing to say in a
sentence and it invites near-neighbours — `hclient2`, `hclient-rs` — where a
coined word does not. For a kit of 29 the legibility was judged to win.

**`www-*` was raised afterwards and declined, on the same rule that chose
`hclient`.** It fails the prefix test hardest: `www-tls-rustls` says "web
TLS", and `www-idn`, `www-rt-tokio`, `www-rt-pair-check` carry no
information at all. It also names the whole domain rather than this thing —
the mirror of `http-ng`'s failure, *the Web itself* instead of *the
successor to `http`* — and `www` is a hostname convention, so `www-` reads
as a subdomain. The names are free; the objection is merit, not
availability. Recorded here because the question will otherwise be asked a
third time.

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
have looked right in every git view and shipped nothing. Each of the 24
publishable crates carries its own copy, as a symlink — cargo follows one
and packs the content, verified by extracting the `.crate` rather than by
reading the file list: 18 files where there were 16, and the first line of
`LICENSE-MIT` inside the tarball is the copyright.

**One trap comes with the symlinks, and it was walked into twice.** `sed -i`
does not follow a symlink — it writes a new file and renames over it, so a
workspace-wide `git ls-files | xargs sed -i` silently turns every licence
link, and `CLAUDE.md`'s link to this file, into copies. Both renames this
week did it. The tell is `git status`'s `T` (type change), not `M`, and the
check that settles it is the **index**: `git ls-files -s | awk '$1=="120000"'`
should count **two per publishable crate, plus one** for `CLAUDE.md` — 55
today at 27 crates, and stated as a relationship rather than a number
because it was written down as 59 at 29 crates and was wrong within the
week, and as 53 at 26 until `system-resolver` arrived. `just packaging` is the gate that does not go stale, because it
asserts against the packaged file list. Restoring is one loop; noticing is
the hard part, because a copy behaves identically until it drifts.

A README is the same shape one step down, and it was the same absence: no
crate had one, `readme` was set nowhere, so 25 crates.io pages would have
carried a single line of `description`. Each crate has one now, and each
says the thing this workspace's own arguments turn on — **why it is its own
crate** — because that is the question a reader landing on
`hclient-tls-quic` actually has.

`just packaging` is the check, in the `lint` job, and it asserts against
the **packaged file list** rather than the working tree, because the tree
can hold a file the tarball drops — which is the whole defect. It fails
closed on the loop not running, and it was checked in the failing direction
by removing one README and watching it name the crate.

**`just package-build` is the other half, and it is the one that builds.**
`cargo package --workspace` does what a publish does and stops before the
upload: each `.crate` is built from the files that would ship, and then
**verified** by compiling it out of that tarball. That is the only check
here that builds a crate the way a reader would get it rather than the way
this workspace sits on disk — the same distinction the doctest job exists
for, where two examples compiled only because another member turned a
feature on.

**It failed on its first run, and the cause would have blocked the entire
publication.** `hclient-fetch` and `hclient-native` each dev-depend on
`hclient`, which depends on both — `DefaultTransport` is `Fetch` on wasm and
`Native` elsewhere. Cargo allows that cycle inside a workspace and refuses
it at package time, because a dev-dependency carrying a version has to
resolve from the registry and `hclient` cannot be there until those two
are. Nothing else could see it: not `cargo check`, not `cargo nextest`, and
not `just packaging`, which packages with `--no-verify`. Both are path-only
now, so cargo strips them from the published manifest, and the reason is
written at both sites — every other workspace dependency in those files is
`{ workspace = true }`, so the odd one out invites a tidy-up that would put
the defect straight back.

It is deliberately **not** `cargo publish --dry-run`: that also asks the
registry about ownership and version collisions, which is a different
question and one CI has no credentials for. Nothing in the workflow
publishes and there is no registry token in it.

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
looked like the fifteenth — and `hclient-dns-system` and `hclient-dns-doh`
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

### `pin_project_lite` reaches one of five boxes, and the other four are the finding

`Pin<Box<T>>` around a **concrete** type appears at five sites here, and
each carries the same note: `tokio::time::Sleep` is `!Unpin`, this
workspace forbids `unsafe`, so there is no projection to be had and the
box is what stands in for one. The premise is false —
`pin_project_lite` is a projection with no `unsafe` at the call site, it
was already in this crate's graph, and `http3/runtime.rs`'s `SeamTimer`
had been using it for a vertical.

**`pool::Reaper` is the one that converted, and it went from two boxes to
none.** The inner box held the sleep; an outer `Box<ReaperState<..>>`
existed to make `Reaper` `Unpin` for every `R`. Neither was needed,
because `Unpin` was never the requirement: `Spawn` declares no bounds and
neither shipped runtime's impl adds one, so a spawned future may be
`!Unpin`. That is a change to a public type, and it is safe here because
the only thing anyone does with a `Reaper` is hand it to `spawn`, which
takes it by value.

**`IdleTimeout` is the one that must not, and it was measured rather than
argued.** It is the site with the most to gain — it drops and rebuilds
its sleep on **every gap between frames**, so the box is an allocation
per gap rather than per connection. Converted, it compiles and all 2259
tests pass. What it costs is invisible from in here: `IdleTimeout` is
`NativeBody`, a **public** response body, and
`http_body_util::BodyExt::frame()` is `where Self: Unpin`. A green suite
means this workspace never calls it; a consumer holding a body from
`Native::execute` directly does. `pin_project_lite` generates its own
conditional `Unpin` and offers no way to keep the hand-written one, so
the choice is binary and the box stays.

`connect::Answers` and `http2::keepalive::Phase` are the same wall from
the `&mut` side: both are polled through `&mut self` by a chain of
ordinary functions, so converting either means threading `Pin<&mut ..>`
through that chain to save two allocations per connection and one per
ping interval. `hclient-core`'s `MapErr` is a third shape — `box_body`
allocates twice per response body, and one of them would go — at the
price of putting `pin-project-lite` into `hclient-core`, measured at
13 crates to 14 for every graph that has no transport.

**Two limits of the macro are worth knowing before the next attempt,
because both look like a mistake in your own code.** In 0.2.17 a bound
list is one predicate at a time — `I: Read + Write` is `no rules expected
"+"` — and a `///` on any **field** is `no rules expected "="`. The
second is why `SeamTimer` had no field docs and read as a style choice.
Nothing is lost by writing them `//` where the fields are private, since
rustdoc does not render those anyway.

### No crate for a job the standard library now does, and the gate is not `cargo deny`

`cfg-if` and `assert_matches` are gone — `core::cfg_select!` (stable
1.95) and `std::assert_matches!` (stable 1.96) replace them, at 98 call
sites for the second. Neither crate is banned for anything it did: both
are small, well made, and were the right answer when they were taken.
What changed is underneath them, so each now buys a name that `core`
already exports.

**The gate reads the manifests rather than the resolved graph, and
`cargo deny` was tried before it was rejected.** It is already wired into
`just supply-chain` and its `[bans]` table has a `use-instead` field that
says exactly what this rule wants to say. Both entries were written and
run, and the result separates the two crates:

- `assert_matches` is **not in the resolved graph at all**, so the entry
  is silent and means precisely *nobody here may declare it*;
- `cfg-if` fails at once — `error[banned]: crate 'cfg-if = 1.0.4' is
  explicitly banned` — because it arrives through **nineteen**
  third-party parents: `ring`, `sha2`, `js-sys`, `openssl`,
  `encoding_rs`, `getrandom`, `chacha20`, `parking_lot_core` and the
  rest, not one of them ours.

`wrappers` would take the second case at the price of a list of somebody
else's parents that goes stale on their next release. But the reason to
reject the tool is not that one crate is awkward: **`[bans]` reads what a
build resolves, and this rule is about what this workspace declares.**
The quiet entry is quiet by luck — the day any dependency takes up
`assert_matches`, the identical entry starts failing for something nobody
here did, which is `cfg-if` today arriving early. A check that cries wolf
gets silenced, which this file treats as the mirror of a check that
cannot fail.

So `scripts/no-crate-for-what-std-does.sh` reads every dependency table
in every workspace manifest — `dev-` and `build-` included, their
`[target.<cfg>]` forms, and the root's `[workspace.dependencies]`, where a
re-entry would most likely be staged. `tomllib` rather than `grep`,
because `system-resolver`'s own manifest argues about `cfg-if` in prose
for a paragraph and a grep cannot tell an argument from a dependency. A
renamed dependency is caught by its `package` key. Checked in three
failing directions: a plain dependency, a renamed dev-dependency, and a
run over zero manifests, which fails closed.

The message names the replacement and the release that made it possible,
which is the one idea worth taking from `cargo deny`: a reader who meets
the refusal is told what to write instead of being told no.

### `cfg_select!` pays for a module and charges for a function body

`system-resolver`'s `sys/mod.rs` took it first, `hclient-idn` second —
four sites there where a condition had been written twice, once negated:
the *at least one backend* guard, `backend()`'s tail, and the two
`testing` functions whose bodies were two `#[cfg]` blocks in a row,
mutually exclusive only because `build.rs` says so. Both `testing` arms
are written with **two positive arms and no `_`**, so a third platform
backend is *none of the predicates in this `cfg_select` evaluated to
true* at the line that would have been wrong, rather than a silent
routing into Foundation's body.

**Then the workspace was swept for the same shape, and the sweep is worth
more than the conversions.** Seventeen sites write a condition twice with
one negated. Thirteen are a single feature — `#[cfg(feature = "cookies")]`
beside `#[cfg(not(feature = "cookies"))]` — where the pair is already
minimal and symmetric and a macro would be five lines for two attributes.
Two more are `#[cfg(unix)]`/`#[cfg(not(unix))]`. `hclient-otel`'s enum
variants and `decompress`'s match arms cannot be selected by this macro
at all, and their features are **additive**, so an ordered select would be
the wrong shape twice over. `client.rs`'s default-transport gate repeats
its condition for a reason written beside it — *the branch a reader is
standing in should say what it excludes.*

**Two conversions were built, measured and reverted**, and that is the
rule. `ecn_is_really_on` in both UDP runtimes is a six-platform list
written twice, the second time negated — the textbook case — and
`hclient-proxy`'s `platform()` is the same with three. Both were
converted and both were undone, because **rustfmt does not descend into a
macro arm**: checked in the failing direction by mangling the indentation
inside a converted arm and watching `cargo fmt --check` stay green over
it. So the conversion silently removes 17 lines from `just fmt-check`'s
reach in the first case and about 150 in the second — and the second is
platform code nobody on this host compiles, which is exactly where
formatting drift would go unnoticed longest.

So the line is what an arm *contains*: a module declaration
(`sys/mod.rs`), a short expression or an empty block (`hclient-idn`, 25
lines in total and eleven of them a `compile_error!` string) is on the
paying side; a function body is on the charging side. A check that
quietly stops covering code is this file's recurring defect with the
subject changed.

### Every error type lives in a file called `error.rs`

A reader asking *what can this crate refuse* had to read the crate. Error
types sat wherever the code that raises them sits — `lib.rs`, `body.rs`,
`keepalive.rs` — so the answer was a grep rather than a file. They are all
in an `error.rs` now, in every crate that has one, and the convention is
held by `just invariants` rather than by memory.

**The rule is about the file *name*, not about one file per crate.** A
self-contained subsystem keeps its own: `hclient-native/src/http2/error.rs`
and `.../http3/error.rs` are legal, and so is `hclient/src/cookie/error.rs`.
What the check forbids is an error type beside the code that raises it,
which is the state that made the question a grep. The boundary is written
in each `error.rs`'s own module doc, so the next type has somewhere to look
rather than a precedent to copy.

**A type goes by what it is, not by which half of a `Result` it appears
in.** `hclient-mock`'s `RecordedBody` is not an error and stays where the
recorder is; `hclient-native`'s `ConnectTimedOut` is one and moves — and
there are two of that name, in the crate root and under `http3/`, bounding
a TCP connect and a QUIC handshake respectively. That is the boundary
working rather than a collision.

**No consumer's `use` line moved.** Every type is re-exported under the
path it already had, and a type that was private becomes `pub(crate)` —
the smallest visibility change that compiles, never `pub`, because the
convention is about where a maintainer looks and not about widening a
surface.

**Ordering inside the file is by how far the request got** — what a
builder refuses, then what a connect refuses, then what a body loses —
because that is the order a reader debugging a failure arrives in, and
alphabetical order is the order of a type nobody is looking for.

**The check is `scripts/errors-live-in-error-rs.sh`, and it is two greps
rather than one.** The survey that sized this work looked for
`#[derive(thiserror::Error)]` alone and undercounted by three crates:
`hclient-cli`, `hclient-tls-rustls` and `hclient-tower` write
`impl Display` + `impl Error` by hand, and those were exactly the types
nobody had looked at recently — the check finds a class it would otherwise
have been blind to by construction. A `#[cfg(test)]` module is exempt, by
brace counting rather than by *after the first `#[cfg(test)]`*, so a test
module in the middle of a file does not blind the check to everything
below it; the exemption is about who reads the type, since a fixture that
exists to make one test fail has no caller asking what the crate refuses.
It fails closed on finding fewer than 15 `error.rs` files, because a
`find` that matched nothing and a tidy tree print the same thing
otherwise — this file's own recurring defect.

**Two things went wrong doing it and both are worth knowing.** A carve
script that moves a `pub struct` by walking back over `///` and `#[` lines
stops at the *first* line ending in `)]`, so a multi-line `#[error(..)]`
below a `#[derive(..)]` is left behind attached to nothing — three types
here, caught by the compiler. And moving a hand-written error moves *three*
items, not one: the struct, its `Display` and its `Error` impl. A script
matching only the struct leaves two orphans that still compile, so nothing
fails — it was the new check that found them, on the same run that was
meant to confirm the work was finished.

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
(`fetch_once<R>` in `crates/hclient/tests/two_runtimes.rs`, bounded by
`hclient_rt::{TcpConnect, Timer, Blocking} + Clone`, with no `#[cfg]` anywhere
in the test code — the file's only conditional is the `#![cfg(not(target_family
= "wasm"))]` gate excluding it from wasm targets, where its native
dev-dependencies do not build) actually drives an HTTP/1.1 request over real TCP to a real
server on loopback — once under `hclient_rt_tokio::Tokio` inside a
`tokio::runtime::Runtime`, once under `hclient_rt_smol::Smol` on a bare
`futures_executor::block_on`. The property is confirmed by more than a green
run: adding `R::Instant: PartialEq<std::time::Instant>` to `fetch_once`'s
bound (the same mutation trick `hclient-rt-pair-check`'s `pair_property.rs`
already applied to runtime capabilities individually) breaks instantiation on
`Tokio` (`Instant = tokio::time::Instant`, a wrapper, `E0277: can't compare
tokio::time::Instant with std::time::Instant`) and does not break `Smol`
(`Instant = std::time::Instant` directly) — the test is sensitive to a
regression of the seam, not just to whether the file compiles at all.

**The HTTP/1 exchange runs without spawn and without a reactor where there
isn't one.** `hclient-native/tests/h1.rs`'s
`works_on_a_bare_futures_executor_with_no_spawn` checks this on IO with no
reactor at all (Task 12); `two_runtimes.rs` above checks the same property of
the transport (`Native`), now under real runtime backends, not just under the
test busy-spin.

**`DefaultTransport`/`Client::new()`** (the `Client<T = DefaultTransport>`
this line named for two verticals is gone — `Client` names no parameters) — the
`default-transport` feature, **not** in `hclient`'s `default`, as for every
crate in the vertical — it was moved in for one commit and back out, and
the section on features as a floor is why. On any non-wasm target it
resolves to
`Native<Tokio, Rustls, SystemDns<Tokio>>` with the system trust store
(`rustls-platform-verifier`, not `webpki-roots` — a client that "just works",
not one with explicitly chosen roots). On `wasm32-unknown-unknown` it resolves
to `hclient_fetch::Fetch`, and `Client::new()` there returns `Self` rather than
a `Result`, because fetch's constructor cannot fail. Without the feature, or on
`wasm32-wasip2` (`target_os = "wasi"`), the type doesn't exist at all — a
compile error, not a silently weaker transport, and since the section above
one that names which of the two reasons it is; on wasip2/wasip1 there's deliberately no branch that reuses the
already-built `hclient_wasi::WasiHttp` through this mechanism — `hclient`
doesn't depend on `hclient-wasi` (an invariant recorded in
`hclient-wasi/Cargo.toml`), and adding that dependency here would mean a path
that no CI job in this repository builds (the `wasip2` job runs `hclient-wasi`
directly). The direct path on WASI remains `Client::builder(hclient_wasi::
WasiHttp::new())`, same as before this task. Resolution details are in the
`DefaultTransport` doc comment in `crates/hclient/src/lib.rs`.

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
milliseconds: the Alt-Svc fixture answered once and closed
**without `Connection: close`**, which RFC 9112 §9.6 makes a MUST. The
client pooled a connection the peer had already closed and the next request
raced the FIN — `hclient-native`'s pooled-reuse window, recorded in `h1.rs`
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
`.notes/grpc-yardstick.md` is the row-by-row report.

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
no code of their own** — which is what `.notes/grpc-yardstick.md` predicted
when it classified them as downstream of the first. It spawns the h2
connection's driver, so the connection outlives the stream (the queued
`RST_STREAM(CANCEL)` reaches the wire) and outlives the request (a `PING`
is answered while idle), and concurrent requests share it: eight
concurrent calls, **one** accept, eight streams open at once by the
server's own count.

**The bound sits on that constructor and nowhere else, which is the whole
of the design.** `hclient_rt::Spawn` declares zero bounds, so
`<R as Spawn<F>>::spawn` coerces to `fn(&R, F)` and lives in a field that
demands nothing of `R` — no signature a `Spawn`-less runtime meets
changes, and `two_runtimes.rs` still runs `Native` on a bare
`futures_executor::block_on`. A runtime with no `Spawn` gets `E0277` where
it wrote `multiplexed()`, and so does a hook holding an `Rc`, because the
driver carries `H` so that a shared connection's `Closed` has an emitter
at all — the collision `hclient-h3` met from the other side and could not
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
`.notes/h2-multiplexing.md` §11.

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
`crates/hclient-native/tests/http2.rs` pins both halves: an `h2::server` on a
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
the correction in `.notes/v02-design.md` §W3). And **h2 is offered only over a
TLS backend that can report the negotiated ALPN** — `TlsConnect::reports_alpn`,
defaulting to `false`, overridden to `true` by `hclient-tls-rustls`: a backend
that sends the ALPN list and cannot read the answer back (which is exactly
`hclient-tls-native-tls`) would otherwise leave the client speaking HTTP/1
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
streaming request bodies (**since done — v0.2 W6 on `hclient-native`, v0.3
on `hclient-h3`, where they arrive with real full duplex**); `first_byte`/
`between_bytes` timeouts (declared unsupported via `Capabilities`, rather than
silently unimplemented — **since done in v0.2 W4**, declared and enforced in
one commit, and measured against servers that answer never, fall silent after
the head, and stall mid-body: `crates/hclient-native/tests/timeouts.rs`); a
single `getaddrinfo` call for both address families
instead of separate v4/v6 slots; h1 upgrade.

### Vertical 1 (WASI): what's proven

**Proven.** The `Transport` shape actually works against an ambient backend
with no socket of its own on the guest side — not in theory, but under a real
`wasmtime` host (`crates/hclient-wasi/tests/live_roundtrip.rs`). A setting the
transport doesn't support becomes a typed `UnsupportedCapability` error
already at `ClientBuilder::build()`, rather than being silently ignored; the
same holds one level down — the `wasi:http` host rejecting a request-option
value (timeout, method, scheme) also becomes an error rather than being
dropped, and this isn't only verified by hand during implementation — it's
held in place by static analysis in CI (the `no-discarded-wasi-setter-result`
ast-grep rule, with the corpus it was accepted against next to it in
`scripts/ast-grep/rule-tests`) on every push.

**`full_duplex` is declared `false` — and that's about the `hclient-wasi`
implementation, not about the shape of the seam.** The `wasi:http` 0.3
protocol itself supports duplex request bodies: body data can flow while the
host hasn't yet returned a response. The shipped `WasiHttp::execute` doesn't
give you that — `convert::race_send_with_body` waits for both `send` and the
full body write (except on an early `send` failure). Measured on a live
`wasmtime` host (host-specific behavior, not pinned down by `wasi:http`): the
response already existed on the server at t≈0.10s, but the caller saw it only
at t≈2.00s, once the body finished writing; for a body with no end, it would
never see it.

The limitation is lifted **inside `hclient-wasi`, without touching
`Transport`.** `Transport::execute` returns `http::Response<Self::Body>`, and
`Self::Body` is `hclient_wasi::Body`, a type from that same crate: the
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
`hclient-wasi`.

The two invariants CI enforces and every exception to them:
[`docs/exceptions.md`](docs/exceptions.md).
