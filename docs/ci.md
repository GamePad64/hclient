# What CI checks, and why

`.github/workflows/ci.yml` is short on purpose; the reasoning lives here.
This file explains what each job guarantees and the trap it defends
against. The archaeology — which review round found which defect — is in
`git log` for the workflow file, where it belongs.

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
`nightly` and then runs a bare `cargo` silently gets stable. The two jobs
that need nightly —
`fuzz-smoke` and `fetch-must-fail-under-wasm-threads` — set
`RUSTUP_TOOLCHAIN`, which outranks the file, and then *assert* what
`rustc --version` reports, because an override that silently fails to apply
is the same defect one level down. Measured: `cargo --version` → 1.97.1
plain, 1.97.0 with `RUSTUP_TOOLCHAIN=1.97.0`.

---

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

### `invariants` — text scans, no toolchain

- **No `Send`/`Sync` bounds in the core surface.** Erasing a type behind
  `dyn Trait` drops auto-traits (spec amendment C1), so declaring the bound
  in the seam forces it on backends that cannot satisfy it. Exceptions carry
  a `send-bound-exception: amendment-C…` marker. The runtime crates and
  `http-ng-dns-system` are excluded by name — an exclusion list that grew to
  cover the whole workspace would make this check scan nothing, so the job
  fails if it does.
- **`unsafe_code` stays forbidden by declaration.** The workspace sets
  `unsafe_code = "forbid"` and every crate root repeats it, so *rustc*
  enforces the absence of `unsafe` — a `forbid` cannot be overridden by a
  local `allow`. What CI has to check is the one thing the compiler cannot:
  that no crate quietly downgrades `forbid` to `deny` or `allow`.
  `http-ng-fetch` and `http-ng-dns-system` are the two allowed to, for
  amendments C7 and C8, and their `allow(unsafe_code)` sites must name the
  amendment.
- **`http-ng-proto` contains no `async fn`.** It is the sans-io crate.
- **No discarded `wasi:http` setter results**
  (`.github/scripts/no_discarded_wasi_setters.py`). `wasi:http` setters
  return `result<_, _>` precisely so the host can refuse; the predecessor
  library discarded seven such results with `let _ =`. The scanner
  understands `let _ =`, `drop(...)`, `_ = ...`, `if let Err(_)`, and the
  `.ok()` / `.unwrap_or…` family.
- **`examples/portable.rs` has no `#[cfg]`** — see above.

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

### `workflow-yaml-is-valid`

Parses every workflow file, then checks the structural rules Actions
enforces but YAML does not: every step has exactly one of `uses:` and `run:`.

Valid YAML is not a valid workflow. An edit once deleted a `- shell: bash`
line, merging a `uses:` step into the `run:` step below it — a mapping with
both keys is well-formed YAML, and Actions rejected the entire file. No job
started, no job log existed, and the only diagnostic was "this run likely
failed because of a workflow file issue".

## Deliberately not separate jobs

**`two-runtimes`** ran `cargo nextest run -p http-ng --test two_runtimes` on
three operating systems. The workspace `test` matrix already runs that file
on the same three, so the job was paying for three extra runners to repeat
it. Its dependency-graph half moved to `dependency-graph`.

**A job per text scan.** Five greps that need no toolchain were five runner
setups. They are steps of one job now; a failed step is named in the UI just
as a failed job was.

**`msrv`.** The MSRV policy is "the latest stable release", so a job pinning
a version was a second, older statement of the same promise — and the one
people would believe. The moment stable moved past the pin it would have
gone on passing, verifying a toolchain nobody supports. The full suite
already runs on stable across three platforms, and that is the promise in
full.
