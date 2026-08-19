# What CI checks, and why

`.github/workflows/ci.yml` is short on purpose; the reasoning lives here.
This file explains what each job guarantees and the trap it defends
against. The archaeology — which review round found which defect — is in
`git log` for the workflow file, where it belongs.

## Where the commands are

**In the `justfile`, not in the workflow.** Every `run:` in `ci.yml` is a
call to a `just` recipe, and `scripts/ci-mirrors-just.py` fails the build if
one is not — or if it names a recipe the justfile does not define. What a job
still carries is what a job is for: a matrix, a runner OS, a toolchain, a
cache, an `env:` and an `if:`. What it no longer carries is a decision. Every
flag, filterset, threshold and grep-over-output lives in a recipe, where it
can be run before pushing rather than discovered afterwards.

The justfile's header has claimed since it was written that its recipes
mirror CI. The guard is what turned that from a hope into a check; before it,
nothing at all compared the two. It fails closed three further ways: an
`EXCEPTIONS` entry naming a step that no longer exists, a workflow it cannot
parse, and finding no `run:` steps at all. `EXCEPTIONS` is currently empty
and is meant to stay that way.

Three layers, for three questions:

| | |
|---|---|
| `just test` | the suite, everywhere, fast |
| `just check` | the above plus fmt and clippy — before a commit |
| `just ci` | everything the pipeline runs that is not bound to one OS |

**`just ci` skips where the environment cannot answer, and CI turns each skip
into a failure.** No headless browser, no `wasmtime`, no `/dev/net/tun`, no
nightly toolchain: on a laptop each of those prints a `NOTICE` and moves on,
because a laptop that cannot run the browser suite is a limitation and not a
regression. In the job that *promised to install the thing*, a marker —
`HTTP_NG_REQUIRE_BROWSERS`, `HTTP_NG_REQUIRE_WASMTIME`,
`HTTP_NG_REQUIRE_TUNTAP`, `HTTP_NG_REQUIRE_NIGHTLY` — makes the same skip a
red step with a message naming what is missing. The strictness sits exactly
where the promise was made; everywhere else the run degrades honestly. Two of
the four predate this arrangement and are the pattern the other two copy.

**There is a fifth marker with no job behind it, deliberately.**
`HTTP_NG_REQUIRE_NETWORK` gates `just test-doh-live`, the only suite in this
repository that talks to servers nobody here runs (Cloudflare's and Google's
public DoH endpoints). It is in no workflow, and the reasoning is written out
in `docs/v03-acceptance.md`'s "Should this be in CI?": what that suite
measures is a fact about a third party, so it goes red when *they* change,
and a red build nobody here can fix is how people learn to ignore red builds.
The marker exists anyway, and is worth having without a job — it is what
lets a human run the suite in the mode that refuses to skip, and it is what
a future scheduled workflow would set on the day this project has somewhere
for a nightly's result to go. Until then the record of what it said, and
when, lives in the acceptance doc rather than in a badge.

## The two rules the whole file is built on

**Every check fails closed.** If the crate or file a check names is absent,
the job goes red rather than skipping. A silent green check is more
dangerous than a red one: after a typo in a crate name it stays green
forever. Guards that print `::notice::` and exit zero were removed for
exactly this reason.

**A check must be able to fail.** Most defects found in this repository were
checks that could not: a `grep` whose pattern never matched anything, a test
run whose filter selected nothing, a count parsed out of colourised output.
When adding a check, break it on purpose once and confirm it goes red.

## Two traps that have each cost a red run, and will again

**ANSI escapes.** Anything that greps a tool's own output passes
`--color never`. A colourised `Summary` line begins with an escape byte, so
`^`-anchored patterns silently never match — a clean 126-test pass was
reported as "the run did not happen".

**Output capture under `set -e`.** `out="$(cmd)"` aborts the step before the
line that prints `$out`. Every capture uses `|| rc=$?`, prints, then
propagates — otherwise a failing test run produces no indication of which
test failed.

**`rust-toolchain.toml` outranks the installed toolchain.** The file in the
repository root pins stable for *every* rustup-proxied `cargo` call in this
directory, whatever the setup action installed. A job that installs
`nightly` and then runs a bare `cargo` silently gets stable. The two recipes
that need nightly — `fuzz-smoke` and `fetch-must-fail-under-atomics` — set
`RUSTUP_TOOLCHAIN` themselves, which outranks the file, and then *assert*
what `rustc --version` reports, because an override that silently fails to
apply is the same defect one level down. Measured: `cargo --version` → 1.97.1
plain, 1.97.0 with `RUSTUP_TOOLCHAIN=1.97.0`. The export lives in the recipe
rather than in the job's `env:` so that running it by hand does the same
thing; what the job supplies is `HTTP_NG_REQUIRE_NIGHTLY`, which decides
whether a missing nightly is a skip or a failure.

---

## Supply chain and graph invariants (`dependency-graph`)

`cargo deny`, not `cargo tree | grep`, wherever it can express the claim.
The project's preference is proven community tooling over scripts written
here, and this job is where that changed hands.

`cargo deny --all-features check` runs against `deny.toml`: RUSTSEC
advisories, the licence allow-list (measured with `cargo deny list`, not
guessed) and the source policy. Neither question had *any* check here
before — this is new coverage, not a rewrite.

Two graph invariants moved to `cargo deny` configs in `.github/deny/`:

- **`ambient.toml`** — the browser and `wasi:http` builds must contain no
  `tokio`, `hyper` or `h2`. Run per backend with `--manifest-path` and `-t`;
  `exclude-dev` because the claim is about what a consumer links, and the
  test servers and the `url` oracle are not that.
- **`smol-path.toml`** — `[[bans.features]] crate = "tokio", exact = true,
  allow = ["default", "sync"]`. Absence of tokio is not achievable and never
  was (hyper requires it unconditionally); what must hold is that the tokio
  present is the inert `sync` leaf and not a reactor. This replaced forty
  lines of bash that parsed `cargo tree -f '{p} [{f}]'` and had to fail
  closed on three separate ways that format could shift.

Both were mutation-tested in the direction that matters: deleting the
`[[bans.features]]` block makes the tokio path pass, and emptying
`ambient.toml`'s deny list makes `http-ng-native` pass. Restoring either
turns it red again.

**One check stays hand-rolled, on purpose.** `http-ng-proto pulls in no
async runtime` bans a *family* by prefix — `^(tokio|futures-|async-|smol|
compio)`. `cargo deny` bans crates by name, so enumerating today's names
would pass for tomorrow's `futures-whatever`, which is precisely the
regression the check exists for. `tree-guard.sh` also remains for the `idn`
job's "must be PRESENT" assertion: `cargo deny` has no such thing, and
`cargo tree -i` exits 0 whether the crate is there or not (measured), so
the output has to be read either way.

`graph-quinn-adapter-is-shared` is the other user of `present`, and it uses
it for something an `absent` check structurally cannot express. The claim is
that `http-ng-quinn` holds the `quinn::Runtime` adapter *and nothing else
holds a copy of it*, so one assertion bans HTTP/3 from the adapter's own
graph and another **requires** `http-ng-h3` to depend on the adapter. The
second is what fails when someone re-adds a private `mod runtime` and drops
the dependency — a change no ban would notice, because nothing forbidden
appears anywhere. `docs/quinn-adapter-extraction.md` §7.

## Build and test

### `test` (ubuntu, macOS, windows)

The workspace suite: `cargo nextest run --workspace --all-features`.

`nextest`, not `cargo test`, and `--no-fail-fast` on top of it. Both defaults
hide failures: `cargo test` abandons the remaining test binaries after the
first one fails, and nextest cancels the remaining tests. One macOS failure
once hid an identical failure in a neighbouring crate, and one Windows
failure hid the 122 tests that had not run yet.

`fail-fast: false` on the matrix, for the same reason one level up: a
platform-specific failure must not cancel the other platforms. Windows twice
reported red having run nothing at all, cancelled mid-download.

nextest cannot run doctests. The workspace has none — measured, not assumed
— so nothing is lost today. If that changes, this job needs a
`cargo test --doc` step.

**macOS gets a second loopback address** before the suite runs.
`local_address_selects_the_connecting_source_ip` needs a source IP distinct
from the default route to `127.0.0.1`, and only Linux and Windows make the
whole of `127.0.0.0/8` loopback. The test degrades to a weaker assertion
where the address is unassignable, so the `ifconfig lo0 alias` is followed
by a bind probe: without it a silently failed alias would leave the tests
green on the weaker branch with nothing saying so.

`parsing_scales_linearly_not_quadratically` is excluded here and runs in
`sse-complexity-guard` instead.

### `sse-complexity-guard`

Runs the one SSE complexity test alone on a runner. Sustained overcommit —
several heavy test processes at once — was measured to break it: known-linear
code reached 18.9× against an 8× threshold, and best-of-N amplified the bias
rather than damping it. Isolation is the fix; widening the threshold is not,
because a threshold robust to that noise would pass a real quadratic
regression.

The job checks that **exactly one** test ran. nextest exits 4 on an empty
filterset, so a renamed test would already fail — but that is a default
(`--no-tests` can change it), and "exactly one ran" is the stronger claim
this job actually needs.

### `embassy-tests-link-under-a-strict-linker`

`cargo test -p http-ng-rt-embassy --no-run` and the same with
`--all-features`, both under `RUSTFLAGS=-C link-arg=-Wl,--no-gc-sections`.

Linkers disagree about a reference that only dead code makes. `ld
--gc-sections` and macOS `ld64`'s dead-strip drop the section before
anything has to resolve it; MSVC's `link.exe` resolves first and runs
`/OPT:REF` afterwards. So an undefined symbol nothing calls is green twice
and red once — which is exactly how `http-ng-rt-embassy`'s `lib test` came
to fail on windows-latest alone with `LNK2019: unresolved external symbol
__embassy_time_queue_item_from_waker`, while the Linux and macOS legs of
`test` ran the whole suite.

This job puts the strict rule on the cheap runner. It was broken on purpose
first: with the `use embassy_executor as _` removed from
`crates/http-ng-rt-embassy/src/lib.rs` it names that same symbol, out of
the same `TimerQueueItem::from_embassy_waker` that Windows named.

**Both feature sets, because the first version of this job ran
`--all-features` alone and was green over a second defect of the same
kind.** The default build had no `critical-section` implementation, so
`_critical_section_1_0_acquire` was undefined there. Neither feature set
supplied one — `cargo tree -e features -i critical-section` shows `default`
alone either way — and the only difference was which archive members each
link happened to need. A guard that covers one configuration of a crate
whose other configuration CI also builds is worse than it looks: it reads
as "this is settled". `idn-feature-is-real` runs
`cargo nextest run -p http-ng-rt-embassy` with no features at all, because
`an_address_family_left_out_of_the_build_is_a_typed_error` lives under
`#[cfg(not(feature = "proto-ipv6"))]` and cannot compile with them on.

All test targets rather than `--lib`: with the implementation supplied
instead of discarded, `tests/tuntap.rs` links here too. Each run checks
that both binaries were actually reported, so a renamed package or test
file fails closed rather than quietly shrinking what is covered.

Scoped to the one crate whose dependencies bind across crate boundaries by
symbol name — `embassy-time-driver`, `embassy-executor-timer-queue` and
`critical-section` are three separate `#[no_mangle]` contracts — because
that arrangement is what produces this class of defect.

### `lint`

`cargo fmt --all --check` and `cargo clippy --workspace --all-targets
--all-features -- -D warnings` — one job, because they were two runners for
two commands.

### `fuzz-smoke`

Short `cargo fuzz` runs over the SSE parser.

`--target` is explicit and has to be: `cargo fuzz` defaults to the triple it
was itself compiled for, and the binary is fetched prebuilt rather than
compiled here. The musl build made every fuzz target fail with `sanitizer is
incompatible with statically linked libc`. The host triple is read out of
`rustc -vV`.

### `autobahn`

The WebSocket client against the Autobahn TestSuite —
`crossbario/autobahn-testsuite` in `fuzzingserver` mode, 517 client cases,
none of them written in this repository. Every other WebSocket fixture here
was written beside the implementation it observes, which is the arrangement
in which a fixture agrees with a bug; this is the job that does not have
that property. The table and the verdict are in `docs/v03-acceptance.md`.

`HTTP_NG_REQUIRE_DOCKER` is set here and only here: this is the job that
promises a daemon, so a missing one is a broken runner rather than a laptop.
Without the marker the recipe prints a NOTICE and skips.

Three things in the recipe are there because a run without them was wrong,
not because they looked prudent.

**Readiness is an answer, not an accept.** Under rootless Docker the
port-publishing proxy takes the connection the moment `docker run` returns,
seconds before `wstest` is listening behind it. A bare TCP probe said ready
and the driver's first handshake failed; the probe sends an HTTP request and
requires bytes back.

**The driver runs under `timeout`.** The suite has no bound for a client
that stops mid-message. Measured, by mutating `Shim::write` to lose a
partial write: 222 cases scored, then both sides waited for ever, and the
job would have been killed by the runner with no report at all rather than
going red with one. There is no unbounded fallback — a machine with neither
`timeout` nor `gtimeout` is an error.

**The report is parsed by something that can say no.** A missing report,
unparseable JSON, an unknown agent, fewer than 517 cases, an undeclared
failure and a declaration that has gone stale are six separate failures in
`scripts/autobahn-report.py`. `autobahn-parser-selftest` is a *prerequisite*
of the run rather than a sibling, so the parser's fifteen mutants are
checked even on the machines that skip the run itself.

## Targets other than the host

### `wasip2`

Runs `http-ng-wasi`'s tests under a real `wasmtime` host, via the target
runner in `.cargo/config.toml`.

`tests/live_roundtrip.rs` is the only automated protection of the property
this backend exists for, and it used to skip silently when `wasmtime` was
absent. `HTTP_NG_REQUIRE_WASMTIME` turns that skip into a panic, and the job
greps for the skip notice as a second belt in case the variable never reaches
the test.

That grep needs `--no-capture`: nextest, like libtest, replays a test's
output only on failure, so a *passing* skipped test prints nothing.
Re-measured when this job moved off `cargo test` — 1 match with the flag, 0
without.

The contract has a third side: a test inside `live_roundtrip.rs` reads this
workflow and asserts the job still installs `wasmtime`. It checks a `bins:`
line rather than the word `wasmtime`, since the comments here discuss
wasmtime at length and a bare substring match would pass with the installer
deleted.

### `browser` (chrome, firefox)

`wasm-pack test --headless` for `http-ng-fetch` and for `http-ng`'s own
browser tests, each with a minimum passing count — a suite that silently
stops running tests is the failure mode, not a suite that fails.

`fail-fast: false`: engine-specific behaviour is the entire point of running
both, so a Chrome failure must not cancel Firefox.

### `portable-example-three-targets`

Builds `examples/portable.rs` for native, `wasm32-wasip2` and
`wasm32-unknown-unknown`, and checks the file contains no `#[cfg]`. Three
green builds alone would also be green for an example that quietly branched
per target, which is the one thing this acceptance exists to disprove.

### `fetch-must-fail-under-wasm-threads`

`http-ng-fetch` carries an `unsafe impl Send` that is sound only because
`wasm32-unknown-unknown` is single-threaded. With `-C target-feature=+atomics`
the build must FAIL, with a specific `E0277` about `Send` — not merely fail
for any reason, which a typo in the flag would also achieve.

## Invariants that no build can express

### `invariants` — text scans, no build

Four of the five things this job checks are `ast-grep` rules in
`scripts/ast-grep/rules`, run by `just ast-grep`. They replaced 235 lines of
python and two greps; what makes that a fair trade rather than a tidier-looking
one is `scripts/ast-grep/rule-tests`, which holds the corpus each rule was
accepted against — every input was run through the scanner AND the rule, and
the two had to agree before the scanner was deleted.

`ast-grep` does not fail closed on its own: a rule whose `files:` glob matches
nothing scans nothing and reports success. `scripts/ast-grep-scan.sh` expands
every rule's globs first and errors on an empty one, then runs the rules' own
tests, then the scan.

- **No discarded `wasi:http` setter results**
  (`no-discarded-wasi-setter-result`). `wasi:http` setters return
  `result<_, _>` precisely so the host can refuse; the predecessor library
  discarded seven such results with `let _ =`. The rule knows `let _ =`,
  `let _ident =`, `let _: Type =`, a bare `_ = ..`, `drop(..)`,
  `if let Err(_e) = .. {}` with a body holding nothing but comments, and the
  `.ok()` / `.is_ok()` / `.is_err()` / `.unwrap_or…` / `.map_or(..)` family at
  *any* position in a chain of combinators. A `?` reached before the discard
  is propagation and excuses it; a `?` reached after one does not, because
  `.ok()?` has already lost the error.

  **One companion rule is deliberately stricter than the scanner it
  replaces**: `no-wasi-setter-inside-a-macro`. tree-sitter flattens macro
  arguments into a token tree, so `assert!(opts.set_x(y).is_ok())` contains no
  call for a structural rule to find, while the text-based scanner did flag
  it. Refusing setters inside macros outright is the only way to be no weaker
  without reimplementing an expression parser over token soup. Measured when
  it was written: zero such sites in the crate.
- **`examples/portable.rs` has no `#[cfg]`** (`no-cfg-in-the-portable-example`)
  — see above. Comments and string literals are nodes rather than text to be
  stripped, which matters here: the file's own module doc and its closing
  `println!` both mention `#[cfg]` on purpose, and a scan that read them would
  go red on the honest file.
- **`http-ng-proto` contains no `async fn`** (`no-async-fn-in-the-sans-io-crate`).
  It is the sans-io crate. Strictly stronger than the grep it replaced, which
  missed `const async unsafe extern "C" fn` — the two words are not adjacent
  there — and needed a second grep to subtract lines *beginning* with a
  comment, leaving a mention mid-line in a doc comment standing.
- **`unsafe_code` stays forbidden by declaration**
  (`scripts/unsafe-code-policy.sh`). The workspace sets
  `unsafe_code = "forbid"` and every crate root repeats it, so *rustc*
  enforces the absence of `unsafe` — a `forbid` cannot be overridden by a
  local `allow`. What CI has to check is the one thing the compiler cannot:
  that no crate quietly downgrades `forbid` to `deny` or `allow`.
  `http-ng-fetch`, `http-ng-dns-system` and `http-ng-idn` are the three
  allowed to, for amendments C7, C8 and C9, and their `allow(unsafe_code)`
  sites must name the amendment *and* sit in a file that amendment names.
- **No `Send`/`Sync` bounds in the core surface**
  (`scripts/no-send-or-sync-in-the-core-surface.sh`). Erasing a type behind
  `dyn Trait` drops auto-traits (spec amendment C1), so declaring the bound
  in the seam forces it on backends that cannot satisfy it. Exceptions carry
  a `send-bound-exception: amendment-C…` marker. The runtime crates and
  `http-ng-dns-system` are excluded by name — an exclusion list that grew to
  cover the whole workspace would make this check scan nothing, so the job
  fails if it does.

  **This one stays a grep, and that is a finding rather than an omission.**
  ast-grep expresses the *detection* better: `Send` in a `trait_bounds` or a
  `bounded_type` is a node, so prose and string literals stop being false
  positives a second grep has to subtract. What it cannot express is the
  *exception*, and the exception is the whole design — the marker is a
  trailing comment on the **same line**, and ast-grep's relational operators
  work over the tree with no notion of a line. `precedes` was measured against
  this tree and is wrong in both directions: it misses markers on
  `field_declaration` and `enum_variant`, where the separating `,` stands
  between the node and its comment, and with `stopBy: end` it excuses far too
  much, because any ancestor followed by a marker excuses everything inside it
  (measured: one bound came out excused by the marker belonging to the bound
  one line below it). ast-grep's own `// ast-grep-ignore` suppressions are
  line-oriented and would work, but must sit on the line *above* the match and
  would replace a marker convention the spec amendments name by hand — 38
  sites across seven crates, changed in service of the tool. A scanner that is
  honest beats a rule that is quiet.
- **Every `run:` in `ci.yml` is a `just` call**
  (`scripts/ci-mirrors-just.py`) — see "Where the commands are" above.

### `dependency-graph` — `cargo tree`, no build

- **`http-ng-proto` pulls in no async runtime.**
- **The ambient builds contain no `tokio`, `hyper` or `h2`**: `http-ng-fetch`
  on `wasm32-unknown-unknown` and `http-ng-wasi` on `wasm32-wasip2`. Both
  named with `--target` explicitly — without it the check described the host
  graph while claiming to police a wasm one. It fails closed if `cargo tree`
  itself errors or returns nothing, because an empty tree is a broken
  invocation, not a passing check.
- **The smol path pulls in no `async-compat`**, and the `tokio` that *is*
  present is the inert `sync` leaf every `hyper` build carries — not a
  reactor. The assertion is on the feature set, not on absence, because
  absence is not achievable: `hyper` depends on `tokio` unconditionally.
  Checked for three host triples from one runner via `cargo tree --target`.

### `actionlint`, a step of `invariants`

Valid YAML is not a valid workflow. An edit once deleted a `- shell: bash`
line, merging a `uses:` step into the `run:` step below it — a mapping with
both keys is well-formed YAML, and Actions rejected the entire file. No job
started, no job log existed, and the only diagnostic was "this run likely
failed because of a workflow file issue".

A hand-written validator was written for exactly that class and has been
replaced by `actionlint` (the official image, and a `prek` hook), which
catches it with a line number and a great deal more besides — including
shellcheck over the `run:` blocks, which nothing did before. Verified by
mutation: reinstating the merged step is reported at the exact line.

## Deliberately not separate jobs

**`two-runtimes`** ran `cargo nextest run -p http-ng --test two_runtimes` on
three operating systems. The workspace `test` matrix already runs that file
on the same three, so the job was paying for three extra runners to repeat
it. Its dependency-graph half moved to `dependency-graph`.

**A job per text scan.** Five greps that need no build were five runner
setups. They are steps of one job now; a failed step is named in the UI just
as a failed job was. The job does still install a toolchain, for one reason
only: `bins:` is how `just` and `ast-grep` arrive. Nothing in it compiles
anything.

**`msrv`.** The MSRV policy is "the latest stable release", so a job pinning
a version was a second, older statement of the same promise — and the one
people would believe. The moment stable moved past the pin it would have
gone on passing, verifying a toolchain nobody supports. The full suite
already runs on stable across three platforms, and that is the promise in
full.
