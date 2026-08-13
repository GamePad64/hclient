# Task runner for http-ng. `just` with no arguments lists the recipes.
#
# These mirror what CI runs, deliberately: a recipe that drifts from the job
# it stands for is worse than no recipe, because it is the one people trust
# before pushing.
#
# **That sentence used to be a promise and is now a check.** Every `run:` in
# `.github/workflows/ci.yml` is a `just` call, and `scripts/ci-mirrors-just.py`
# fails if one is not — or if it names a recipe that does not exist here. The
# jobs are still jobs, because a matrix, a toolchain, a cache and a runner OS
# are things a workflow does and a task runner does not; what they no longer
# hold is a decision.
#
# Three layers:
#
#   just test    the suite, everywhere, fast
#   just check   the above plus fmt and clippy — what to run before a commit
#   just ci      everything the pipeline runs that is not bound to one OS
#
# `just ci` is slow (the fuzz targets alone are three minutes) and it SKIPS
# rather than fails where the environment cannot answer: no browser, no
# wasmtime, no /dev/net/tun, no nightly. CI sets a marker for each of those —
# `HTTP_NG_REQUIRE_BROWSERS`, `HTTP_NG_REQUIRE_WASMTIME`,
# `HTTP_NG_REQUIRE_TUNTAP`, `HTTP_NG_REQUIRE_NIGHTLY` — and the marker turns
# the skip into a failure, in the job that promised to install the thing.
# A skip nobody notices is the defect docs/ci.md was written about.

default:
    @just --list --unsorted

# ── the everyday loop ───────────────────────────────────────────────────

# nextest, not `cargo test`. See AGENTS.md, "Running the tests": `cargo test`
# abandons the remaining test binaries after the first failure, so a red run
# hides every failure but the earliest one.

# the whole workspace, all features
test *ARGS:
    cargo nextest run --workspace --all-features --no-fail-fast {{ARGS}}

# one crate, optionally one test: `just t http-ng-proto uri`
t PKG *FILTER:
    cargo nextest run -p {{PKG}} --all-features --no-fail-fast {{FILTER}}

# rustfmt, in place
fmt:
    cargo fmt --all

# rustfmt, check only — the `lint` job's first command
fmt-check:
    cargo fmt --all --check

# clippy over every target, warnings are errors
lint:
    cargo clippy --workspace --all-features --all-targets -- -D warnings

# fmt + clippy + the suite, cheapest first
check: && test
    cargo fmt --all --check
    cargo clippy --workspace --all-features --all-targets -- -D warnings

# ── the workspace suite as CI runs it ───────────────────────────────────

# `parsing_scales_linearly_not_quadratically` is excluded here and runs alone
# in `test-sse-complexity`: sharing a runner is what made it flake. The
# Summary check is not decoration — a filterset that selects nothing exits 0
# on some settings, and a green run over zero tests is the failure mode this
# repository keeps finding.

# the workspace suite, minus the one test that needs a runner to itself
test-workspace:
    #!/usr/bin/env bash
    set -euo pipefail
    rc=0
    out="$(cargo nextest run --workspace --all-features --color never --no-fail-fast \
      -E 'not test(parsing_scales_linearly_not_quadratically)' 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -eq 0 ] || exit "$rc"
    if ! printf '%s\n' "$out" | grep -qE '[0-9]+ tests? run:'; then
      echo "::error::nextest printed no Summary — the workspace test run did not happen"
      exit 1
    fi

# Sustained overcommit was measured to break this test: known-linear code
# reached 18.9x against an 8x threshold. Isolation is the fix; widening the
# threshold is not, because a threshold robust to that noise would pass a
# real quadratic regression. EXACTLY one test, because "at least one ran"
# would not say that the isolation happened.

# the SSE complexity test, alone
test-sse-complexity:
    #!/usr/bin/env bash
    set -euo pipefail
    rc=0
    out="$(cargo nextest run -p http-ng-proto --all-features --color never \
      -E 'test(=sse::lines::tests::parsing_scales_linearly_not_quadratically)' 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -eq 0 ] || exit "$rc"
    if ! printf '%s\n' "$out" | grep -qE '1 test run: 1 passed'; then
      echo "::error::did not run exactly one test — parsing_scales_linearly_not_quadratically was renamed or moved. Fix it here AND in test-workspace's filterset."
      exit 1
    fi

# `local_address_selects_the_connecting_source_ip` needs a source IP distinct
# from the default route; only Linux and Windows make all of 127.0.0.0/8
# loopback. The test degrades to a weaker assertion without it, so the bind
# probe is what keeps the strong one: a silently failed alias would otherwise
# leave the suite green on the weaker branch with nothing saying so. Bound to
# one OS, so it is not part of `just ci`.

# give macOS the second loopback address the socket-option tests need
macos-loopback:
    #!/usr/bin/env bash
    set -euo pipefail
    sudo ifconfig lo0 alias 127.0.0.2 up
    python3 - <<'PY'
    import socket, sys
    s = socket.socket()
    try:
        s.bind(("127.0.0.2", 0))
    except OSError as e:
        sys.exit(f"::error::127.0.0.2 is still not bindable after `ifconfig lo0 alias`: {e}")
    PY

# `http-ng-rt-embassy`'s live scenarios drive a real embassy-net stack over a
# TAP device and are the only thing proving the W1 cancellation contract
# holds there. Without HTTP_NG_REQUIRE_TUNTAP they SKIP — quietly, which is
# what this is here to prevent. Measured on the ubuntu-24.04 image: `unshare
# -Ur --net` fails, and the marker turned that into a red job instead of nine
# silent skips. The cause is AppArmor's restriction on unprivileged user
# namespaces, lifted the documented way, and only when the marker is set —
# a laptop does not get its sysctls rewritten by a test runner.

# the embassy tuntap scenarios (they skip without HTTP_NG_REQUIRE_TUNTAP)
test-embassy-live:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "${HTTP_NG_REQUIRE_TUNTAP:-}" ]; then
      # `|| true`: if a future image drops the knob, the run below is what
      # reports it, with the test's own message, rather than this line
      # failing with a sysctl error.
      sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0 || true
      unshare -Ur --net true || {
        echo "::error::unprivileged user namespaces are still unavailable after lifting the AppArmor restriction — the embassy live scenarios cannot run on this image. Do not paper over this by dropping HTTP_NG_REQUIRE_TUNTAP: that returns nine silent skips."
        exit 1
      }
    fi
    cargo nextest run -p http-ng-rt-embassy --all-features --color never

# Linkers disagree about a reference that only dead code makes: `ld
# --gc-sections` and macOS `ld64` drop the section before anything resolves
# it, MSVC's `link.exe` resolves first. That asymmetry cost a windows-only
# `LNK2019: unresolved external symbol __embassy_time_queue_item_from_waker`
# that no amount of green Linux and macOS could have found.
#
# BOTH feature sets, and not for symmetry: the first version ran
# `--all-features` alone and passed while the default build had no
# `critical-section` implementation either.

# http-ng-rt-embassy's tests must link under a strict linker
embassy-strict-link:
    #!/usr/bin/env bash
    set -euo pipefail
    export RUSTFLAGS="-C link-arg=-Wl,--no-gc-sections"
    for features in "" "--all-features"; do
      rc=0
      out="$(cargo test -p http-ng-rt-embassy $features --no-run --color never 2>&1)" || rc=$?
      printf '%s\n' "$out"
      if [ "$rc" -ne 0 ]; then
        echo "::error::http-ng-rt-embassy's tests do not link with --no-gc-sections for [${features:-default features}]. An undefined symbol above is a Windows LNK2019 waiting to happen: see the embassy_executor note in crates/http-ng-rt-embassy/src/lib.rs and the critical-section note in its Cargo.toml. Do not silence this by dropping the flag or a feature set."
        exit 1
      fi
      # Fail closed, per binary: a renamed package or test file would
      # otherwise leave this green forever while linking less than it claims.
      for exe in 'Executable unittests src/lib.rs' 'Executable tests/tuntap.rs'; do
        if ! printf '%s\n' "$out" | grep -qF "$exe"; then
          echo "::error::cargo reported no '$exe' for [${features:-default features}] — this check linked less than it claims to"
          exit 1
        fi
      done
    done

# `crates/http-ng-dns-doh/tests/live.rs` queries Cloudflare and Google. It is
# the only test in this repository that talks to a server nobody here runs,
# and every other test in that crate answers itself: the fixture and the
# parser share an author.
#
# **It is deliberately NOT in `just ci`, and not in any CI job.** A test whose
# subject is a third party's server goes red for reasons nobody here can fix,
# and a red build nobody can fix is how people learn to ignore red builds. The
# argument, the three things it found, and what would change the answer are
# written out in docs/v03-acceptance.md under "Live, v0.3 W3"; the short form
# is that what it measures is a fact about an operator, not a property of this
# code, and the two do not belong on the same signal.
#
# So it is a recipe a human runs, and its findings are dated in the docs.
# Without HTTP_NG_LIVE_DOH (which this recipe sets) every test in the file
# prints a NOTICE and returns, so `just test` stays hermetic.
#
# The receipt count is the belt, and it is the one that matters: a gate that
# returns "skip" for the wrong reason turns the file into nine green tests
# that made no request. 16 = eight tests against two operators, plus one each
# for the IPv6-literal and Quad9 tests, which have one endpoint each. Raise it
# when a test is added, the way `test-browser`'s minimum is raised.
#
# `--no-capture` is required, not stylistic: nextest replays a test's output
# only on failure, so a PASSING skipped test prints nothing and both the grep
# and the count below would be vacuous.

# the DoH suite against real public resolvers (needs the internet; not in CI)
test-doh-live:
    #!/usr/bin/env bash
    set -euo pipefail
    export HTTP_NG_LIVE_DOH=1
    rc=0
    out="$(cargo nextest run -p http-ng-dns-doh --test live \
      --color never --no-capture --no-fail-fast 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -eq 0 ] || exit "$rc"
    ran="$(printf '%s\n' "$out" | grep -c '^LIVE-DOH-RAN ' || true)"
    echo "live DoH exchanges that actually happened: $ran (minimum 16)"
    if [ "$ran" -lt 16 ]; then
      if [ -n "${HTTP_NG_REQUIRE_NETWORK:-}" ]; then
        echo "::error::only $ran of the expected 16 live DoH exchanges happened, and HTTP_NG_REQUIRE_NETWORK is set — this run promised the network and skipped instead. Do not answer this by lowering the number: a live suite that skips is a green run that checked nothing, which is the defect this count exists to catch."
        exit 1
      fi
      echo "NOTICE: only $ran of 16 live DoH exchanges happened — this environment could not reach every endpoint, so the suite skipped rather than checked. Set HTTP_NG_REQUIRE_NETWORK to make that a failure."
    fi

# ── the other two targets ───────────────────────────────────────────────

# the wasi example, built as a component
build-wasi-example:
    cargo build -p http-ng-wasi --example fetch --target wasm32-wasip2

# `live_roundtrip.rs` is the only automated protection of the property this
# backend exists for, and it used to skip silently when `wasmtime` was
# absent. HTTP_NG_REQUIRE_WASMTIME turns that skip into a panic; the grep is
# a second belt in case the variable never reaches the test. It needs
# `--no-capture`: nextest replays a test's output only on failure, so a
# PASSING skipped test prints nothing and the grep would be vacuous.

# the wasi suite under wasmtime (runner comes from .cargo/config.toml)
test-wasi:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo nextest run -p http-ng-wasi --target wasm32-wasip2 || exit $?
    rc=0
    out="$(cargo nextest run -p http-ng-wasi --test live_roundtrip \
      --color never --no-capture 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -eq 0 ] || exit "$rc"
    if printf '%s\n' "$out" | grep -q 'NOTICE: `wasmtime` not found'; then
      if [ -n "${HTTP_NG_REQUIRE_WASMTIME:-}" ]; then
        echo "::error::the live wasip2 run skipped the tests instead of executing them — HTTP_NG_REQUIRE_WASMTIME did not reach the test, or wasmtime is not on PATH"
        exit 1
      fi
      echo "NOTICE: no wasmtime on PATH — the live wasip2 roundtrip was skipped, not run."
    fi
    cargo nextest run -p http-ng-wasi --test shape

# The `--features` below are wasm-pack's own arguments, not a `cargo test --`
# passthrough — `-- --features ...` is rejected outright.
#
# The minimum count is the check: a suite that silently stops running tests
# is the failure mode, not one that fails. 65 -> 68 in v0.2 W1 (three tests
# for `Transport::execute`'s drop-cancellation contract), 68 -> 78 in W6 (ten
# for the streaming request body and its probe), 78 -> 99 in v0.3 W4 step 3
# (twenty-one for the browser's WebSocket behind `WebSocketConnect`),
# 100 -> 118 in v0.4 W2 (eighteen for the observability hook: ten for the one
# event this backend can emit, four counting the browser's own clock, four
# measuring why `Connected` cannot be built out of `PerformanceResourceTiming`),
# 118 -> 119 when `Head::version` became an `Option` (one test pairing the
# event's `None` with the `version_reported: false` that says the same thing
# to whoever built the transport — see docs/v04-w2-hooks-ambient.md §9).
# Measured by running both engines, not inferred from the attribute count:
# several `#[wasm_bindgen_test]`s are cfg-gated, so the two numbers do not
# agree. Firefox 153 reports 119; Chrome was NOT run for this change —
# `wasm-pack` cannot acquire a chromedriver on the machine it was made on
# (`http status: 404`), reproducibly and independently of any branch.

# one browser suite: `just test-browser firefox`
test-browser BROWSER="chrome":
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v wasm-pack >/dev/null 2>&1; then
      if [ -n "${HTTP_NG_REQUIRE_BROWSERS:-}" ]; then
        echo "::error::wasm-pack is not on PATH, and this job promised to install it — the browser suites must run, not skip"
        exit 1
      fi
      echo "NOTICE: no wasm-pack — skipping the {{BROWSER}} suites."
      exit 0
    fi
    run_browser_suite() {
      local crate="$1" min="$2" log passed
      shift 2
      [ -d "$crate" ] || { echo "::error::$crate is missing — this must run browser tests, not skip them"; exit 1; }
      log="$(mktemp)"
      if ! wasm-pack test --headless --{{BROWSER}} "$crate" "$@" 2>&1 | tee "$log"; then
        echo "::error::browser tests failed for $crate on {{BROWSER}}"
        exit 1
      fi
      passed="$(grep -oE '[0-9]+ passed' "$log" | awk '{s+=$1} END {print s+0}')"
      if [ "$passed" -lt "$min" ]; then
        echo "::error::$crate on {{BROWSER}} ran $passed browser tests, expected at least $min — a renamed test file, a dropped #[wasm_bindgen_test], or drift between the --features here and the crate's own cfg gate all produce a green run that tests nothing"
        exit 1
      fi
      echo "$crate on {{BROWSER}}: $passed browser tests passed (minimum $min)"
    }
    run_browser_suite crates/http-ng-fetch 119
    run_browser_suite crates/http-ng 6 --features default-transport,test-util

# both browser suites, on both engines — CI runs one engine per matrix leg
test-browsers: (test-browser "chrome") (test-browser "firefox")

# the Transport acceptance: one source, no #[cfg], three targets
build-three-targets:
    cargo build -p http-ng --example portable
    cargo build -p http-ng --example portable --target wasm32-wasip2
    cargo build -p http-ng --example portable --target wasm32-unknown-unknown

# `http-ng-fetch` carries an `unsafe impl Send` that is sound only because
# wasm32-unknown-unknown is single-threaded. With `+atomics` the build must
# FAIL, and with a specific E0277 about Send — not merely fail for any
# reason, which a typo in the flag would also achieve.

# the unsafe Send must be rejected when wasm atomics are on
fetch-must-fail-under-atomics:
    #!/usr/bin/env bash
    set -euo pipefail
    # `rust-toolchain.toml` pins stable and outranks the installed channel;
    # `-Z build-std` needs nightly.
    export RUSTUP_TOOLCHAIN=nightly
    if ! rustc --version 2>/dev/null | grep -q nightly; then
      if [ -n "${HTTP_NG_REQUIRE_NIGHTLY:-}" ]; then
        echo "::error::this check needs nightly for -Z build-std — RUSTUP_TOOLCHAIN did not take effect"
        exit 1
      fi
      echo "NOTICE: no nightly toolchain — skipping the +atomics rejection check."
      exit 0
    fi
    [ -d crates/http-ng-fetch ] || { echo "::error::crates/http-ng-fetch is missing — this check must run, not skip"; exit 1; }
    log="$(mktemp)"
    if RUSTFLAGS="-Ctarget-feature=+atomics,+bulk-memory" \
        cargo check -p http-ng-fetch --tests \
        --target wasm32-unknown-unknown -Zbuild-std=std,panic_abort \
        > "$log" 2>&1; then
      echo "::error::expected a compile error under +atomics — the Send guarantee SingleThreaded<T> exists to provide is not being enforced"
      exit 1
    fi
    # It must fail for the RIGHT reason: a typo in the flag also fails.
    if ! grep -q 'error\[E0277\]' "$log" || ! grep -q 'cannot be sent between threads safely' "$log"; then
      echo "::error::the build under +atomics failed, but NOT with the expected Send rejection (E0277 / cannot be sent between threads safely) — full output follows"
      cat "$log"
      exit 1
    fi
    echo "OK: correctly rejected under +atomics with E0277 on the Send bound"

# ── the external oracle ─────────────────────────────────────────────────

# Every fixture in `crates/http-ng-ws-tungstenite/tests/websocket.rs` was
# written beside the implementation it observes, which is the arrangement
# in which a fixture agrees with a bug. The Autobahn TestSuite is the
# answer to that: 517 client cases nobody here wrote, served by `wstest
# --mode fuzzingserver` out of `crossbario/autobahn-testsuite`.
#
# **Two recipes, and the split is the point.** The PARSER decides whether
# 517 external cases passed, so it must be checked on every machine and
# not only on the ones with Docker — `autobahn-parser-selftest` needs
# nothing and always runs. The RUN needs a container, so it skips without
# one, and `HTTP_NG_REQUIRE_DOCKER` turns that skip into a failure in the
# job that promised to provide it — the same shape as
# HTTP_NG_REQUIRE_WASMTIME and HTTP_NG_REQUIRE_TUNTAP.
#
# Neither is in `just test`: that is the everyday recipe and it must not
# need Docker. Both are in `just ci`. The whole run takes about 20s on
# loopback in a debug build, which is why it is on every push rather than
# on a schedule.

# fifteen mutants of a passing Autobahn report, none of which may pass
autobahn-parser-selftest:
    python3 scripts/autobahn-report-selftest.py

# the WebSocket client against the Autobahn TestSuite (needs docker)
test-autobahn: autobahn-parser-selftest
    #!/usr/bin/env bash
    set -euo pipefail
    agent=http-ng-ws-tungstenite
    reports="$PWD/target/autobahn"
    container=http-ng-autobahn
    if ! docker info >/dev/null 2>&1; then
      if [ -n "${HTTP_NG_REQUIRE_DOCKER:-}" ]; then
        echo "::error::docker is unusable, and this job promised to provide it — the Autobahn run must happen, not skip. Do not paper over this by dropping HTTP_NG_REQUIRE_DOCKER: that returns a green job that tested nothing."
        exit 1
      fi
      echo "NOTICE: no usable docker — the Autobahn TestSuite run was skipped, not run."
      exit 0
    fi
    # The driver is built before the container starts, so a compile error
    # is a compile error rather than a connection timeout.
    cargo build -p http-ng-ws-tungstenite --example autobahn || exit $?
    rm -rf "$reports" && mkdir -p "$reports" || exit $?
    docker rm -f "$container" >/dev/null 2>&1
    trap 'docker rm -f "$container" >/dev/null 2>&1' EXIT
    docker run -d --rm --name "$container" \
      -v "$PWD/scripts/autobahn:/config:ro" -v "$reports:/reports" \
      -p 127.0.0.1:9001:9001 crossbario/autobahn-testsuite >/dev/null || {
      echo "::error::could not start crossbario/autobahn-testsuite"
      exit 1
    }
    # Readiness, and it must be an ANSWER rather than an accept: under
    # rootless docker the port-publishing proxy takes the connection the
    # moment `docker run` returns, seconds before `wstest` is listening
    # behind it, so a bare TCP connect said "ready" and the driver then
    # failed its very first handshake. Bounded, and a failure rather than
    # a shorter run.
    python3 - <<'PY' || { echo "::error::the fuzzingserver never answered on 127.0.0.1:9001 — container log follows"; docker logs "$container" 2>&1 | tail -40; exit 1; }
    import socket, sys, time
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", 9001), 1) as s:
                s.sendall(b"GET / HTTP/1.1\r\nHost: 127.0.0.1:9001\r\n\r\n")
                if s.recv(64):
                    sys.exit(0)
        except OSError:
            pass
        time.sleep(0.1)
    sys.exit(1)
    PY
    # A bound on the whole run, and it is not decoration. Measured, by
    # mutating `Shim::write` to claim `buf.len()` on a partial write —
    # exactly the defect that function's doc comment is about: the suite
    # has no timeout for a client that stops mid-message, so the driver
    # sat for ever on case 1 and the job would have been killed by the
    # runner with no verdict at all. 900s against a 16s run.
    secs=900
    if command -v timeout >/dev/null 2>&1; then
      bound=(timeout "$secs")
    elif command -v gtimeout >/dev/null 2>&1; then
      bound=(gtimeout "$secs")
    else
      echo "::error::neither timeout nor gtimeout is on PATH, so the Autobahn run would be unbounded — and a defect that hangs the driver then produces no verdict rather than a red one"
      exit 1
    fi
    "${bound[@]}" ./target/debug/examples/autobahn ws://127.0.0.1:9001 "$agent"
    rc=$?
    if [ "$rc" -ne 0 ]; then
      [ "$rc" -eq 124 ] && echo "::error::the Autobahn driver hung and was killed at ${secs}s — a case it cannot finish is a defect, not a slow machine; the last 'case N:' line above names it"
      echo "::error::the Autobahn driver could not complete the run (exit $rc) — the report, if any, covers only part of the suite"
      exit 1
    fi
    python3 scripts/autobahn-report.py "$reports" "$agent"

# ── the paths --all-features cannot reach ───────────────────────────────

# `--all-features` turns `idn` ON, so every `#[cfg(not(feature = "idn"))]`
# test — the typed NonAsciiHost error, the divergence list — runs only here.
# The same is true of `http-ng-cookie`'s `public-suffix`, and of
# `http-ng-native`'s h1-only build, which went uncompiled from the moment h2
# landed behind a feature.
#
# **This recipe is the one place where CI now does MORE than it did**, and
# deliberately: `http-ng-cookie --no-default-features` was in this recipe and
# NOT in the job it claimed to mirror, so `tests/without_the_list.rs` — the
# only thing checking that a no-list build is NARROWER than a list build,
# rather than quietly wider — ran on laptops and nowhere else. 78 tests.
# Resolving the drift by deleting the line would have been the other
# direction. `--no-fail-fast` is the same story one size smaller: the recipe
# had it, the job did not, and it only ever reports more.

# the feature-off builds, which --all-features can never exercise
test-no-default:
    #!/usr/bin/env bash
    # `-e`, and it is load-bearing: the four clippy lines at the end are
    # unguarded, so without it the recipe's exit code is the LAST one's and a
    # failure in any earlier step is reported as success. That is how three
    # dead-code errors under `--no-default-features` sat on `main` while this
    # recipe — and the CI job that calls it — stayed green.
    set -euo pipefail
    for args in "-p http-ng-proto --no-default-features" \
                "-p http-ng-native --no-default-features" \
                "-p http-ng-rt-embassy" \
                "-p http-ng-cookie --no-default-features" \
                "-p http-ng --no-default-features --features test-util"; do
      rc=0
      out="$(cargo nextest run $args --color never --no-fail-fast 2>&1)" || rc=$?
      printf '%s\n' "$out"
      [ "$rc" -eq 0 ] || exit "$rc"
      if ! printf '%s\n' "$out" | grep -qE '[0-9]+ tests? run:'; then
        echo "::error::nextest reported no summary for [$args] — the run did not happen"
        exit 1
      fi
    done
    cargo clippy -p http-ng-proto --all-targets --no-default-features -- -D warnings
    cargo clippy -p http-ng-native --all-targets --no-default-features -- -D warnings
    cargo clippy -p http-ng-rt-embassy --all-targets -- -D warnings
    cargo clippy -p http-ng --all-targets --no-default-features --features test-util -- -D warnings

# The whole claim of `http-ng-idn` is that the platform's ICU answers what
# the bundled `idna` crate answers. The platform column of
# `tests/differential.rs` does not compile where there is no platform
# backend, which is why HTTP_NG_IDN_REQUIRE_PLATFORM is set off Linux only.
# The default (`platform`, resolved by target) AND `--all-features`, which is
# what the workspace suite runs: features have to stay additive, so
# `system-icu` off Windows and `bundled` on it are no-ops, not errors.

# the IDN differential corpus, on both feature settings
test-idn:
    #!/usr/bin/env bash
    set -euo pipefail
    for args in "" "--all-features"; do
      rc=0
      out="$(cargo nextest run -p http-ng-idn $args --color never --no-fail-fast 2>&1)" || rc=$?
      printf '%s\n' "$out"
      [ "$rc" -eq 0 ] || exit "$rc"
      if ! printf '%s\n' "$out" | grep -qE '[0-9]+ tests? run:'; then
        echo "::error::nextest reported no summary for [$args] — the run did not happen"
        exit 1
      fi
    done

# clippy on http-ng-idn's two feature settings
lint-idn:
    cargo clippy -p http-ng-idn --all-targets -- -D warnings
    cargo clippy -p http-ng-idn --all-targets --all-features -- -D warnings

# the doctests, which nextest cannot run
test-doc:
    #!/usr/bin/env bash
    set -euo pipefail
    rc=0
    out="$(cargo test --workspace --all-features --doc --color never 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -eq 0 ] || exit "$rc"
    # Fail closed, twice, because this recipe's whole failure mode is
    # reporting `ok` over nothing. A run with no `test result:` line at all
    # did not happen; an `ignored` doctest is a fenced code block rustdoc
    # never compiles, which is the permanently-unwatched example this
    # project has already been bitten by twice.
    if ! printf '%s\n' "$out" | grep -qE 'test result:'; then
      echo "::error::no doctest summary at all — the run did not happen"
      exit 1
    fi
    # awk rather than `paste | bc`: `bc` is not guaranteed present, and a
    # missing one would leave this empty, which `${ign:-0}` would read as
    # zero — a guard that passes when its own arithmetic is unavailable is
    # the defect this recipe exists to stop.
    ign="$(printf '%s\n' "$out" | awk -F'[;.]' '/test result:/ {for (i=1;i<=NF;i++) if ($i ~ /ignored/) n+=$i} END {print n+0}')"
    if [ "$ign" -ne 0 ]; then
      printf '%s\n' "$out" | grep -E '\.\.\. ignored$' || true
      echo "::error::$ign doctest(s) are \`ignore\`d — rustdoc compiles none of them, so they rot unwatched. Use \`no_run\` with hidden setup lines, or \`text\` if the block is quoted code rather than an example."
      exit 1
    fi

# nightly is not optional here (sanitizer flags), and `RUSTUP_TOOLCHAIN` is
# how it survives rust-toolchain.toml. The explicit `--target` is not
# cosmetic either: cargo-fuzz defaults to the triple it was itself built for,
# which is musl when it arrives as a prebuilt binary — every target then
# fails with "sanitizer is incompatible with statically linked libc".

# fuzz one target for N seconds: `just fuzz sse_accounting 30`
fuzz TARGET="sse" SECONDS="60":
    cd crates/http-ng-proto/fuzz && \
      RUSTUP_TOOLCHAIN=nightly cargo fuzz run \
        --target "$(rustc -vV | sed -n 's/^host: //p')" \
        {{TARGET}} -- -max_total_time={{SECONDS}}

# the short fuzz runs CI does on every push
fuzz-smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    export RUSTUP_TOOLCHAIN=nightly
    if ! rustc --version 2>/dev/null | grep -q nightly || ! cargo fuzz --version >/dev/null 2>&1; then
      if [ -n "${HTTP_NG_REQUIRE_NIGHTLY:-}" ]; then
        echo "::error::cargo fuzz is about to run on a non-nightly toolchain, or cargo-fuzz is missing — RUSTUP_TOOLCHAIN did not take effect, or \`bins:\` did not install it"
        exit 1
      fi
      echo "NOTICE: no nightly toolchain or no cargo-fuzz — skipping fuzz-smoke."
      exit 0
    fi
    host="$(rustc -vV | sed -n 's/^host: //p')"
    [ -n "$host" ] || { echo "::error::could not read the host triple out of rustc -vV"; exit 1; }
    ( cd crates/http-ng-proto/fuzz && \
      cargo fuzz run --target "$host" sse -- -max_total_time=60 && \
      cargo fuzz run --target "$host" sse_accounting -- -max_total_time=30 -max_len=256 ) || exit 1
    # `http-ng-idn`'s own safe layer — the deny list, the error mask, the
    # ASCII/A-label handling — which sits in front of every backend and so is
    # worth fuzzing on a Linux runner even though Linux has no platform
    # backend. The first is deliberately NOT differential against `idna`:
    # with the bundled backend `domain_to_ascii` IS `idna::domain_to_ascii_cow`,
    # so that comparison could not fail. The second IS, because
    # `testing::policy_over` takes the backend as an argument, so handing it
    # `idna` cancels `idna` out of both sides and leaves only this crate's
    # code — the hand-written RFC 3492 decoder in src/policy.rs, sixty lines
    # of arithmetic in the path that decides which host is contacted.
    ( cd crates/http-ng-idn/fuzz && \
      cargo fuzz run --target "$host" idn_policy -- -max_total_time=45 -max_len=512 && \
      cargo fuzz run --target "$host" idn_policy_vs_idna -- -max_total_time=45 -max_len=512 ) || exit 1

# ── invariants no build can express ─────────────────────────────────────

# the text scans, together
invariants: ast-grep no-send-or-sync unsafe-policy ci-mirrors-just

# the ast-grep rules, their own corpus tests, and a fail-closed glob check
ast-grep:
    ./scripts/ast-grep-scan.sh

# The script's header says why this one is NOT an ast-grep rule: the
# exception marker is a trailing comment on the same line, and ast-grep's
# relational operators know the tree and not the line.

# no Send/Sync bounds declared in the core surface
no-send-or-sync:
    ./scripts/no-send-or-sync-in-the-core-surface.sh

# unsafe_code stays forbidden by declaration
unsafe-policy:
    ./scripts/unsafe-code-policy.sh

# The guard reads the workflow with a real YAML parser rather than by
# indentation, because it has to find every `run:` and missing one is a
# silent hole. `pyyaml` is preinstalled on the GitHub runner images and on
# most distributions; the fallback is the same three lines ci.yml carried
# before this moved, kept because "the check could not run" must not read as
# "the check passed".

# every `run:` in ci.yml is a call to a recipe in this file
ci-mirrors-just:
    #!/usr/bin/env bash
    set -euo pipefail
    python3 -c "import yaml" 2>/dev/null \
      || python3 -m pip install --quiet --break-system-packages pyyaml \
      || python3 -m pip install --quiet pyyaml \
      || { echo "::error::pyyaml is missing and could not be installed, so scripts/ci-mirrors-just.py cannot run — a check that cannot run must not pass"; exit 1; }
    python3 scripts/ci-mirrors-just.py

# ── dependency facts this project makes claims about ────────────────────

# advisories, licences and sources for the shipped graph
supply-chain:
    cargo deny --all-features check

# the browser and wasi graphs must contain no tokio, hyper or h2 at all
tree-ambient:
    cargo deny --manifest-path crates/http-ng-fetch/Cargo.toml \
        --config .github/deny/ambient.toml -t wasm32-unknown-unknown check bans
    cargo deny --manifest-path crates/http-ng-wasi/Cargo.toml \
        --config .github/deny/ambient.toml -t wasm32-wasip2 check bans

# The two seams v0.3 changed for HTTP/3 must stay implementable without a
# QUIC stack: `http-ng-rt` re-declares `RecvMeta`/`EcnCodepoint` instead of
# re-exporting `quinn-udp`'s, and `http-ng-tls-quic` is a separate crate
# rather than a feature of `http-ng-tls`. Both are trades paid in conversion
# code and crate count, and both are worthless the moment this stops holding.

# the runtime and TLS seams contain no QUIC
graph-no-quic:
    #!/usr/bin/env bash
    set -euo pipefail
    for c in http-ng-rt http-ng-tls; do
      cargo deny --manifest-path "crates/$c/Cargo.toml" \
        --config .github/deny/no-quic-in-the-seams.toml check bans
    done

# And the other direction, because a ban that would pass against an empty
# graph proves nothing: the tokio runtime's `udp` feature really does pull
# `quinn-udp`, so the ban above is checked against a graph where the crate is
# one step away rather than absent from the workspace.

# ...and the udp feature really does pull quinn-udp, off by default
graph-udp-pulls-quic:
    #!/usr/bin/env bash
    set -euo pipefail
    out="$(cargo tree -p http-ng-rt-tokio --features udp -e normal --prefix none)"
    printf '%s\n' "$out" | grep -q '^quinn-udp ' || {
      echo "::error::http-ng-rt-tokio/udp no longer pulls quinn-udp — either the feature is dead or the ban in graph-no-quic is now vacuous"
      exit 1
    }
    off="$(cargo tree -p http-ng-rt-tokio -e normal --prefix none)"
    if printf '%s\n' "$off" | grep -q '^quinn-udp '; then
      echo "::error::quinn-udp is in http-ng-rt-tokio's default graph; the udp feature is not off by default"
      exit 1
    fi

# the smol path carries no reactor, only hyper's inert tokio leaf
graph-smol-path:
    #!/usr/bin/env bash
    set -euo pipefail
    for t in x86_64-unknown-linux-gnu x86_64-apple-darwin x86_64-pc-windows-msvc; do
      cargo deny --manifest-path crates/http-ng-rt-smol/Cargo.toml \
        --config .github/deny/smol-path.toml -t "$t" check bans
    done

# `docs/w4-upgrade-seam.md` §8, machine-checked in both directions. The
# framing used to be a `websocket` feature of `http-ng-native`, and the
# argument for moving it out was that Cargo's features are additive: one
# crate anywhere in a graph switching it on put `tungstenite` into every
# other crate's build of the transport. `--all-features` is therefore the
# whole of the check — it asks for every feature this crate has at once,
# which is the strongest thing any neighbour could do to it.
#
# The `present` half is not decoration: an `absent` check whose pattern
# matches nothing anywhere would pass a workspace that had lost the framing
# altogether, which is the failure mode this repository keeps finding.

# the WebSocket framing stays out of the transport, whatever features are on
graph-no-framing-in-the-transport:
    #!/usr/bin/env bash
    set -euo pipefail
    ./scripts/tree-guard.sh absent '^(tungstenite|sha1|data-encoding) ' \
        "http-ng-native has the WebSocket framing in its graph again. It lives in http-ng-ws-tungstenite, which depends on this crate rather than the other way round, precisely so that no feature switched on by a neighbour can put it here — docs/w4-upgrade-seam.md §8" \
        -- -p http-ng-native --all-features
    ./scripts/tree-guard.sh present '^tungstenite ' \
        "http-ng-ws-tungstenite does not depend on tungstenite, so the ban above is vacuous — it would pass a workspace with no framing at all" \
        -- -p http-ng-ws-tungstenite

# The same argument one seam down, and in the direction that catches a
# COPY rather than a feature. `SeamRuntime` was `mod runtime` inside
# `http-ng-h3` — private, so unreachable rather than additive, which is the
# other way a shared thing stops being shared. It is `http-ng-rt-quinn` now,
# and the two things that would undo that are: the adapter growing an
# HTTP/3 dependency (so it is no longer usable by anything else that wants
# QUIC over the seam), or `http-ng-h3` growing a private copy again and
# dropping the dependency. One `absent` and one `present` catch one each.

# the quinn/seam adapter carries no HTTP/3, and h3 has not taken a copy back
graph-quinn-adapter-is-shared:
    #!/usr/bin/env bash
    set -euo pipefail
    ./scripts/tree-guard.sh absent '^(h3|h3-quinn|http-ng-h3) ' \
        "http-ng-rt-quinn has HTTP/3 in its graph. It is the quinn::Runtime over http_ng_rt::{Timer, Spawn, UdpBind} and nothing above it — a crate that wants bare QUIC over the seam must be able to take it without h3 (docs/v04-w2-webtransport.md §4)" \
        -- -p http-ng-rt-quinn --all-features
    ./scripts/tree-guard.sh present '^quinn ' \
        "http-ng-rt-quinn does not depend on quinn, so the ban above is vacuous — it would pass a workspace where the adapter had been emptied out" \
        -- -p http-ng-rt-quinn
    ./scripts/tree-guard.sh present '^http-ng-rt-quinn ' \
        "http-ng-h3 no longer depends on http-ng-rt-quinn — either the adapter moved back in as a private copy, or the extraction was reverted. Copying is what this workspace does not do" \
        -- -p http-ng-h3

# Still `tree-guard`, and deliberately: `cargo deny` bans crates by name, and
# this one bans a FAMILY by prefix. Enumerating today's names would pass for
# tomorrow's `futures-whatever`, which is the regression the check exists
# for. docs/ci.md says so.

# http-ng-proto pulls in no async runtime, on any target
graph-proto-sans-io:
    #!/usr/bin/env bash
    set -euo pipefail
    for t in "" "--target wasm32-unknown-unknown"; do
      ./scripts/tree-guard.sh absent '^(tokio|futures-|async-|smol|compio)' \
        "http-ng-proto picked up an async dependency ${t:-on the host} — this crate must stay sans-io on every target, not just the host" \
        -- -p http-ng-proto $t
    done

# The `cookies` feature is off by default because `http-ng-cookie`'s
# compiled-in public suffix list is +77 KiB — a claim nothing checked until
# here. Exact analogue of the idna/ICU guard below.

# the default http-ng build carries no cookie jar
graph-no-cookie-jar:
    ./scripts/tree-guard.sh absent '^(http-ng-cookie|public-suffix) ' \
        "http-ng's default build pulled in the cookie jar and its public suffix list — the feature is off by default precisely so this does not happen" \
        -- -p http-ng

# `url` is gone from the graph either way: http-ng-proto writes out RFC 3986
# §5.2 in src/uri.rs precisely so that it does not depend on `url`, which
# belongs in [dev-dependencies] where it is the oracle for
# tests/uri_resolution.rs.

# url is absent from http-ng-proto, with the idn feature and without
graph-no-url:
    #!/usr/bin/env bash
    set -euo pipefail
    for flags in "" "--no-default-features"; do
      ./scripts/tree-guard.sh absent '^(url|form_urlencoded|percent-encoding) ' \
        "http-ng-proto ${flags:-with default features} depends on url again — RFC 3986 §5.2 is written out in src/uri.rs precisely so that it does not; url belongs in [dev-dependencies], where it is the oracle for tests/uri_resolution.rs" \
        -- -p http-ng-proto $flags
    done

# icu_properties_data alone is 1.9 MB of vendored source; that is the entire
# measurable benefit of the feature. The usual cause of a failure is some
# crate depending on http-ng-proto WITH default features, unioning idn back
# on — see [workspace.dependencies].

# without idn there is no IDN implementation, with it there is
graph-idn-feature:
    #!/usr/bin/env bash
    set -euo pipefail
    ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
      "--no-default-features still pulls in idna/ICU" \
      -- -p http-ng-proto --no-default-features
    ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
      "http-ng --no-default-features still pulls in idna/ICU — the facade's idn feature is not forwarding, or something else in its graph turns it on" \
      -- -p http-ng --no-default-features
    # Without this the two checks above would also pass if idn had simply
    # been deleted. Since `idn` stopped naming `idna` and started naming
    # `http-ng-idn` (which picks its backend by target), the implementation
    # being in the graph is the target-independent claim.
    ./scripts/tree-guard.sh present '^http-ng-idn ' \
      "the default build of http-ng-proto has no http-ng-idn — the idn feature is in [features] default, so either it was removed or the dependency is no longer reached" \
      -- -p http-ng-proto
    # What it COSTS on this runner, and deliberately not target-independent:
    # measured, `--target x86_64-pc-windows-msvc` brings `windows-sys` and no
    # `idna`, `aarch64-apple-darwin` brings `objc2-foundation` and no `idna`.
    # This is the line that must MOVE rather than be widened if this ever
    # runs anywhere but Linux.
    ./scripts/tree-guard.sh present '^idna ' \
      "the default build of http-ng-proto on Linux has no idna — http-ng-idn takes the bundled tables on every target that is not Windows or Apple, so these are the tables the feature is supposed to bring here" \
      -- -p http-ng-proto
    # And that the feature is the ONLY thing bringing it, stated
    # target-independently: the idna|icu_ absence above passes trivially on
    # Windows and Apple, where those never appear.
    ./scripts/tree-guard.sh absent '^http-ng-idn ' \
      "--no-default-features still pulls in http-ng-idn — the idn feature is the only thing that should" \
      -- -p http-ng-proto --no-default-features
    ./scripts/tree-guard.sh absent '^http-ng-idn ' \
      "http-ng --no-default-features still pulls in http-ng-idn — the facade's idn feature is not forwarding" \
      -- -p http-ng --no-default-features

# The saving the crate exists for, on the two targets that get it. Its size
# in bytes is unverified on both — no Windows or Apple linker produced this
# crate — but its absence from the graph is not, and that is what this
# checks. The Linux half is the opposite invariant, and it is not decoration:
# it is the half that breaks silently if the target predicate in build.rs and
# the ones in Cargo.toml ever drift apart. `libloading` stays named even
# though nothing references it now — the ELF dlopen backend was removed on
# purpose, and its name is the tripwire.

# http-ng-idn's Unicode tables come from the platform, or from idna — by target
graph-idn-backend:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "{{os()}}" = "linux" ]; then
      ./scripts/tree-guard.sh present '^idna ' \
        "http-ng-idn on Linux has no idna, so this target has no IDN implementation at all" \
        -- -p http-ng-idn
      ./scripts/tree-guard.sh absent '^(libloading|windows-sys|objc2-foundation) ' \
        "http-ng-idn on Linux pulls in a loader or a platform binding — the ELF dlopen backend was removed on purpose, see the crate docs" \
        -- -p http-ng-idn --all-features
    else
      ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
        "the default http-ng-idn build on this target pulls in idna/ICU — the whole point of a platform backend is that the OS supplies the tables" \
        -- -p http-ng-idn
      ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
        "--all-features drags idna back into http-ng-idn on a target with a platform backend" \
        -- -p http-ng-idn --all-features
      ./scripts/tree-guard.sh present '^(windows-sys|objc2-foundation) ' \
        "http-ng-idn on this target links neither windows-sys nor objc2-foundation, so no platform backend is compiled in at all" \
        -- -p http-ng-idn
    fi

# `cargo hack --each-feature` enumerates combinations instead of listing
# them. Two crates are excluded, and not for convenience: their features are
# not independent. `http-ng-idn` needs at least one backend and says so with
# a `compile_error!`; `http-ng-rt-embassy` forwards to `smoltcp`, which
# demands at least one protocol and one medium the same way. Isolating one
# feature of either is a build the crate deliberately refuses, so those two
# keep the targeted checks they already have.

# every feature combination compiles (58 of them)
features:
    cargo hack --workspace \
        --exclude http-ng-idn --exclude http-ng-rt-embassy \
        --each-feature --no-dev-deps check

# every dependency-graph claim, together
graph: supply-chain tree-ambient graph-no-quic graph-udp-pulls-quic graph-no-framing-in-the-transport graph-quinn-adapter-is-shared graph-smol-path features graph-no-cookie-jar graph-proto-sans-io graph-no-url graph-idn-feature graph-idn-backend

# ── the whole pipeline ──────────────────────────────────────────────────

# everything CI runs except what is bound to one OS (`macos-loopback`)
ci: fmt-check lint invariants graph test-workspace test-doc test-sse-complexity test-no-default test-idn lint-idn test-embassy-live embassy-strict-link build-three-targets build-wasi-example test-wasi test-browsers fetch-must-fail-under-atomics test-autobahn fuzz-smoke
