# Task runner for hclient. `just` with no arguments lists the recipes.
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
# `HCLIENT_REQUIRE_BROWSERS`, `HCLIENT_REQUIRE_WASMTIME`,
# `HCLIENT_REQUIRE_TUNTAP`, `HCLIENT_REQUIRE_NIGHTLY` — and the marker turns
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

# one crate, optionally one test: `just t hclient-proto uri`
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
    # **Streamed through `tee`, not captured into a variable**, and that is
    # a fix rather than a style. This recipe used to do
    # `out="$(cargo nextest run ...)"` and print `$out` afterwards, so a run
    # that never finished printed *nothing at all* — which is how the macOS
    # and Windows jobs sat at this step for six hours a day for twelve days
    # with no evidence of which test was stuck. The fail-closed Summary
    # check below still needs the whole output, so it reads the file `tee`
    # wrote instead of a variable.
    log="$(mktemp)"
    trap 'rm -f "$log"' EXIT
    # `set +e` around the pipeline: `pipefail` plus `-e` would exit on a red
    # run before the status could be read, and `${PIPESTATUS[0]}` is the
    # only place nextest's own exit code survives a pipe.
    set +e
    cargo nextest run --workspace --all-features --color never --no-fail-fast \
      -E 'not test(parsing_scales_linearly_not_quadratically)' 2>&1 | tee "$log"
    rc=${PIPESTATUS[0]}
    set -e
    [ "$rc" -eq 0 ] || exit "$rc"
    if ! grep -qE '[0-9]+ tests? run:' "$log"; then
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
    out="$(cargo nextest run -p hclient-proto --all-features --color never \
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

# `hclient-rt-embassy`'s live scenarios drive a real embassy-net stack over a
# TAP device and are the only thing proving the W1 cancellation contract
# holds there. Without HCLIENT_REQUIRE_TUNTAP they SKIP — quietly, which is
# what this is here to prevent. Measured on the ubuntu-24.04 image: `unshare
# -Ur --net` fails, and the marker turned that into a red job instead of nine
# silent skips. The cause is AppArmor's restriction on unprivileged user
# namespaces, lifted the documented way, and only when the marker is set —
# a laptop does not get its sysctls rewritten by a test runner.

# the embassy tuntap scenarios (they skip without HCLIENT_REQUIRE_TUNTAP)
test-embassy-live:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "${HCLIENT_REQUIRE_TUNTAP:-}" ]; then
      # `|| true`: if a future image drops the knob, the run below is what
      # reports it, with the test's own message, rather than this line
      # failing with a sysctl error.
      sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0 || true
      unshare -Ur --net true || {
        echo "::error::unprivileged user namespaces are still unavailable after lifting the AppArmor restriction — the embassy live scenarios cannot run on this image. Do not paper over this by dropping HCLIENT_REQUIRE_TUNTAP: that returns nine silent skips."
        exit 1
      }
    fi
    cargo nextest run -p hclient-rt-embassy --all-features --color never

# Linkers disagree about a reference that only dead code makes: `ld
# --gc-sections` and macOS `ld64` drop the section before anything resolves
# it, MSVC's `link.exe` resolves first. That asymmetry cost a windows-only
# `LNK2019: unresolved external symbol __embassy_time_queue_item_from_waker`
# that no amount of green Linux and macOS could have found.
#
# BOTH feature sets, and not for symmetry: the first version ran
# `--all-features` alone and passed while the default build had no
# `critical-section` implementation either.

# hclient-rt-embassy's tests must link under a strict linker
embassy-strict-link:
    #!/usr/bin/env bash
    set -euo pipefail
    export RUSTFLAGS="-C link-arg=-Wl,--no-gc-sections"
    for features in "" "--all-features"; do
      rc=0
      out="$(cargo test -p hclient-rt-embassy $features --no-run --color never 2>&1)" || rc=$?
      printf '%s\n' "$out"
      if [ "$rc" -ne 0 ]; then
        echo "::error::hclient-rt-embassy's tests do not link with --no-gc-sections for [${features:-default features}]. An undefined symbol above is a Windows LNK2019 waiting to happen: see the embassy_executor note in crates/hclient-rt-embassy/src/lib.rs and the critical-section note in its Cargo.toml. Do not silence this by dropping the flag or a feature set."
        exit 1
      fi
      # Fail closed, per binary: a renamed package or test file would
      # otherwise leave this green forever while linking less than it claims.
      # **Three, and the list went stale the day `tests/seam.rs` was
      # added.** That is the failure mode this loop exists for, met by the
      # loop itself: a file appears, nothing names it, and the check keeps
      # reporting on the two it knew about. `seam.rs` is also the binary
      # that was failing to link, which is not a coincidence — it is the
      # one test file here that names no embassy type of its own.
      for exe in 'Executable unittests src/lib.rs' 'Executable tests/tuntap.rs' 'Executable tests/seam.rs'; do
        if ! printf '%s\n' "$out" | grep -qF "$exe"; then
          echo "::error::cargo reported no '$exe' for [${features:-default features}] — this check linked less than it claims to"
          exit 1
        fi
      done
    done

# `crates/hclient-dns-doh/tests/live.rs` queries Cloudflare and Google. It is
# the only test in this repository that talks to a server nobody here runs,
# and every other test in that crate answers itself: the fixture and the
# parser share an author.
#
# **It is deliberately NOT in `just ci`, and not in any CI job.** A test whose
# subject is a third party's server goes red for reasons nobody here can fix,
# and a red build nobody can fix is how people learn to ignore red builds. The
# argument, the three things it found, and what would change the answer are
# written out beside the tests themselves; the short form
# is that what it measures is a fact about an operator, not a property of this
# code, and the two do not belong on the same signal.
#
# So it is a recipe a human runs, and its findings are dated in the docs.
# Without HCLIENT_LIVE_DOH (which this recipe sets) every test in the file
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
    export HCLIENT_LIVE_DOH=1
    rc=0
    out="$(cargo nextest run -p hclient-dns-doh --test live \
      --color never --no-capture --no-fail-fast 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -eq 0 ] || exit "$rc"
    ran="$(printf '%s\n' "$out" | grep -c '^LIVE-DOH-RAN ' || true)"
    echo "live DoH exchanges that actually happened: $ran (minimum 16)"
    if [ "$ran" -lt 16 ]; then
      if [ -n "${HCLIENT_REQUIRE_NETWORK:-}" ]; then
        echo "::error::only $ran of the expected 16 live DoH exchanges happened, and HCLIENT_REQUIRE_NETWORK is set — this run promised the network and skipped instead. Do not answer this by lowering the number: a live suite that skips is a green run that checked nothing, which is the defect this count exists to catch."
        exit 1
      fi
      echo "NOTICE: only $ran of 16 live DoH exchanges happened — this environment could not reach every endpoint, so the suite skipped rather than checked. Set HCLIENT_REQUIRE_NETWORK to make that a failure."
    fi

# ── the other two targets ───────────────────────────────────────────────

# the wasi example, built as a component
# Every example in `hclient` that can run without a socket, RUN rather
# than built — because a build is green for an example that asserts
# nothing, which is the defect `portable-example-three-targets` already
# guards against from the other side by pinning its claims in a test.
#
# Each example asserts its own claims and panics on failure, so a non-zero
# exit is the signal. What this recipe adds is that **no example can be
# forgotten**: it derives the file list from the directory and refuses one
# that is in neither table, so adding an example without deciding whether
# it runs is an error rather than a silence.
examples:
    #!/usr/bin/env bash
    set -euo pipefail
    declare -A RUN=(
      [configured]=test-util,cookies,cache
      [custom_cache_store]=test-util,cache
      [auth_scheme]=test-util
      [streaming]=test-util
      [file_upload]=test-util
      [testing_with_mock]=test-util,json
    )
    # Built and not run: these two need a network or a target this runner
    # is not. `portable` is built for three targets by
    # `build-three-targets`, and its behaviour is pinned by
    # `tests/portable_example.rs`.
    BUILD_ONLY="portable no_tls_no_resolver"
    ran=0
    for name in "${!RUN[@]}"; do
      echo "==> run $name (--features ${RUN[$name]})"
      cargo run -q -p hclient --example "$name" --features "${RUN[$name]}"
      ran=$((ran + 1))
    done
    [ "$ran" -gt 0 ] || { echo "::error::no example ran — the loop found nothing, which reads the same as a clean run"; exit 1; }
    missing=""
    for f in crates/hclient/examples/*.rs; do
      name="$(basename "$f" .rs)"
      [ -n "${RUN[$name]+x}" ] && continue
      case " $BUILD_ONLY " in *" $name "*) continue ;; esac
      missing="$missing $name"
    done
    [ -z "$missing" ] || { echo "::error::example(s) in neither table:$missing — add it to RUN with its features, or to BUILD_ONLY with the reason it cannot run"; exit 1; }
    echo "examples: $ran run, $(echo $BUILD_ONLY | wc -w) built elsewhere"

build-wasi-example:
    cargo build -p hclient-wasi --example fetch --target wasm32-wasip2

# `live_roundtrip.rs` is the only automated protection of the property this
# backend exists for, and it used to skip silently when `wasmtime` was
# absent. HCLIENT_REQUIRE_WASMTIME turns that skip into a panic; the grep is
# a second belt in case the variable never reaches the test. It needs
# `--no-capture`: nextest replays a test's output only on failure, so a
# PASSING skipped test prints nothing and the grep would be vacuous.

# the wasi suite under wasmtime (runner comes from .cargo/config.toml)
test-wasi:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo nextest run -p hclient-wasi --target wasm32-wasip2 || exit $?
    rc=0
    out="$(cargo nextest run -p hclient-wasi --test live_roundtrip \
      --color never --no-capture 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -eq 0 ] || exit "$rc"
    if printf '%s\n' "$out" | grep -q 'NOTICE: `wasmtime` not found'; then
      if [ -n "${HCLIENT_REQUIRE_WASMTIME:-}" ]; then
        echo "::error::the live wasip2 run skipped the tests instead of executing them — HCLIENT_REQUIRE_WASMTIME did not reach the test, or wasmtime is not on PATH"
        exit 1
      fi
      echo "NOTICE: no wasmtime on PATH — the live wasip2 roundtrip was skipped, not run."
    fi
    cargo nextest run -p hclient-wasi --test shape

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
# to whoever built the transport).
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
      if [ -n "${HCLIENT_REQUIRE_BROWSERS:-}" ]; then
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
    # 123 -> 128 when `Event::Progress` landed and this crate gained a
    # browser test for it. Measured by running the suite, not taken from
    # the change's own report, which said 129 — a floor set one above the
    # truth is a gate that fails for everybody until somebody notices.
    #
    # 128 -> 130 when the events gained a `RequestId`: the pair asserting
    # that an event names the request that was sent, and that it names
    # nothing where there was none. Measured the same way.
    run_browser_suite crates/hclient-fetch 130
    # `--test wasm_default` and not the whole crate, for a reason worth
    # stating: `wasm-pack test` also compiles the crate's **doctests** for
    # `wasm32-unknown-unknown`, and two of them cannot build there —
    # `Client::new()?` is fallible on native and infallible in a browser,
    # so the `?` is a compile error, and another names `tokio`. Their home
    # is `just test-doc`, which runs them on the host where they are
    # written to run. Naming the one file that carries
    # `#[wasm_bindgen_test]` scopes this job to what it is for.
    run_browser_suite crates/hclient 6 --test wasm_default --features default-transport,test-util

# both browser suites, on both engines — CI runs one engine per matrix leg
test-browsers: (test-browser "chrome") (test-browser "firefox")

# Every target this workspace ships for, checked in one cheap command.
#
# **Why this exists, and why it is `check` rather than `test`.** Three
# backends stopped satisfying `SendTransport` on the day `Client` began
# requiring it — `hclient-fetch`, `hclient-wasi` and `hclient-urlsession` —
# and `cargo nextest run --workspace --all-features` stayed green over all
# three, because it builds for the host and they do not live there. CI
# would have caught it (`browser`, `build-wasi-example`, `test
# (macos-latest)`); nobody ran those before pushing, because each is slow,
# needs a browser or a second machine, and none of them is one command.
#
# This is that one command, and it costs seconds on a warm tree: no test
# runs, no wasm-pack, no browser, no Apple hardware. `--all-targets` is
# load-bearing — the wasi break was in an *example*, which a plain
# `cargo check` does not build.
#
# It is a cross-**check**, not a cross-build: `aarch64-apple-darwin` and
# `x86_64-pc-windows-msvc` type-check from a Linux host without a linker,
# which is what makes covering them affordable at all.
check-targets:
    #!/usr/bin/env bash
    set -uo pipefail
    # A missing target must fail rather than skip: a silent skip is the
    # defect this recipe exists to remove, one level up.
    # The list is one place, and the count below is derived from it: a
    # figure written twice is a figure that drifts, which this workspace
    # has fixed three times elsewhere. Adding a target here is the whole
    # of adding one.
    TARGETS="wasm32-unknown-unknown wasm32-wasip2 aarch64-apple-darwin x86_64-pc-windows-msvc aarch64-linux-android x86_64-unknown-freebsd x86_64-unknown-linux-musl"
    for t in $TARGETS; do
      rustup target list --installed | grep -qx "$t" || {
        echo "::error::target $t is not installed — \`rustup target add $t\`. Skipping it would return exactly the blind spot this recipe covers."
        exit 1
      }
    done
    # Deliberately NOT `set -e`, and every line is checked instead: three
    # backends broke at once last time, and a run that stops at the first
    # would have reported one of them and hidden two. That is the same
    # argument `docs/ci.md` makes for nextest over `cargo test`, one level
    # up.
    failed=()
    ran=0
    check() {
      echo "==> $*"
      ran=$((ran+1))
      cargo check "$@" --color never || failed+=("$*")
    }
    # The browser: the backend with its tests, and the facade over it —
    # the pair that broke.
    check -p hclient-fetch --target wasm32-unknown-unknown --all-targets
    # Android's proxy reader, which is the one place in `hclient-proxy`
    # that calls into a JVM. Nothing here runs on it — the pure half is
    # in `read.rs` and is tested on this host — so a check for the target
    # is the whole of what keeps the JNI half honest.
    check -p hclient-proxy --target aarch64-linux-android --all-features --all-targets
    # The Android resolver, for the same reason: `android_res_nquery` is
    # declared here and called nowhere this host can run.
    check -p hclient-dns-system --target aarch64-linux-android --all-features --all-targets
    # FreeBSD, whose arm was added on the owner's decision with its own
    # gap named: the symbol is established out of `lib/libc/resolv/Symbol.map`
    # and the per-thread claim is read from `resolver(3)` rather than run,
    # because nothing here is a FreeBSD machine. So this is the only gate
    # that arm has until somebody runs the live suite on one — the same
    # position `hclient-winhttp` is in, further down. `--all-targets`
    # because the live test is where the unrun half would be established,
    # and a live test that does not compile establishes nothing.
    check -p system-resolver --target x86_64-unknown-freebsd --all-features --all-targets
    check -p hclient-dns-system --target x86_64-unknown-freebsd --all-features --all-targets
    # musl, which is a **different backend from glibc** in this crate and
    # was built by nothing until a ceiling shipped inside it: its
    # `res_query` refuses a type number above 255, so `Support::Any` was a
    # capability that lied, and neither the workspace run nor any gate here
    # compiled the arm that says so. `--all-targets` because the live tests
    # are where the ceiling is asserted.
    check -p system-resolver --target x86_64-unknown-linux-musl --all-features --all-targets
    check -p hclient-dns-system --target x86_64-unknown-linux-musl --all-features --all-targets
    # `--lib` and not `--all-targets` here, and the reason is a fact about
    # this crate's dev-dependencies rather than about wasm: `wait-timeout`
    # and `getrandom`'s host backend are host-only and do not build for
    # `wasm32-unknown-unknown` at all. The browser suite reaches this
    # crate's wasm tests through `wasm-pack`, which builds the ones that
    # can; what this line defends is the facade itself compiling there.
    check -p hclient --target wasm32-unknown-unknown --features default-transport --lib
    # WASI, including the example — where one of the three breaks was.
    check -p hclient-wasi --target wasm32-wasip2 --all-targets
    # The instrumenter, on both wasm targets and `--all-features`, which
    # is `docs/otel-design.md` §10's *"one property is not a test but a
    # build"*: a client whose whole claim is one API everywhere must not
    # grow an instrumenter that only exists on native. `--lib`, because
    # its dev-dependencies are host-only — `tokio`'s `rt` and
    # `opentelemetry_sdk`'s in-memory exporter — and what is being
    # defended is the crate a consumer would compile, not its own
    # harness. Nothing in it is `#[cfg]`-ed by target: the reason it can
    # break here and not on Linux is a dependency, `opentelemetry`, whose
    # `wasm32-unknown-unknown` clock reaches for `js_sys::Date::now` and
    # brings ten crates with it.
    check -p hclient-otel --target wasm32-unknown-unknown --all-features --lib
    check -p hclient-otel --target wasm32-wasip2 --all-features --lib
    # Apple, from here. `hclient-urlsession` has no other build on this
    # machine and its live tests need a Mac; this is what keeps its shape
    # honest in between.
    check -p hclient-urlsession --target aarch64-apple-darwin --all-features --all-targets
    # Windows: `hclient-proxy`'s WinINET reader is four lines that cannot
    # run here and must still compile.
    check -p hclient-proxy --target x86_64-pc-windows-msvc --all-features --all-targets
    # The WinHTTP backend, whose every line is Windows-only: no line of it
    # has ever been run, so compiling it here is the only gate it has.
    check -p hclient-winhttp --target x86_64-pc-windows-msvc --all-targets
    # The two runtimes, on Windows, because both carry a `cfg(unix)` split
    # and neither was covered here. `Tokio` and `Smol` each gate an
    # associated type on `cfg(unix)` and each has a `connect_unix` that
    # names a Unix-only type, so a missing `#[cfg(not(unix))]` arm is an
    # `E0046` plus two resolution failures — which is exactly what both
    # were, found by the Windows CI job rather than by anything a
    # developer runs. This recipe is the developer's instrument, so the
    # crates with a platform split belong in it.
    check -p hclient-rt-tokio --target x86_64-pc-windows-msvc --all-features --all-targets
    check -p hclient-rt-smol --target x86_64-pc-windows-msvc --all-features --all-targets
    # The transport itself, on Windows, and `--lib` rather than
    # `--all-targets` for a reason that is not about Windows: its
    # dev-dependencies pull `ring`, whose build script needs an assembler
    # for the target, so a test build cannot be type-checked from here at
    # all. The library half carries the `cfg(unix)` — `unix_socket` — and
    # that is the half this line defends.
    check -p hclient-native --target x86_64-pc-windows-msvc --no-default-features --lib
    # `hclient-idn`'s **test** target on both platforms, which is where
    # the platform column of its differential corpus lives. Its `use` of
    # `hclient_idn::testing` is `#[cfg]`-ed to the two platform backends,
    # so losing that line broke Windows and macOS and left Linux green —
    # every site that needs it is inside a function the same `cfg`
    # removes. `--all-targets` is the load-bearing half here: the library
    # compiled fine throughout.
    check -p hclient-idn --target x86_64-pc-windows-msvc --all-features --all-targets
    check -p hclient-idn --target aarch64-apple-darwin --all-features --all-targets
    # **Android is here and nowhere else.** No runner in this project is an
    # Android device, so `src/android.rs` has never been executed; what a
    # cross-check proves is that the JNI signatures and the class names
    # type-check, which is the difference between a backend that is unrun
    # and one that does not build. Both settings of the feature, because
    # the target has two shapes: `android.icu.text.IDNA` without it and the
    # bundled tables with it.
    check -p hclient-idn --target aarch64-linux-android --all-targets
    check -p hclient-idn --target aarch64-linux-android --all-features --all-targets
    # macOS's transport half, mirroring the Windows line above: the
    # library carries `cfg(unix)` and a test build cannot be checked from
    # here, because `ring` needs an assembler for the target.
    check -p hclient-native --target aarch64-apple-darwin --no-default-features --lib
    # The embassy runtime's **test** targets on both, which is where this
    # gap was: its `hclient-*` dev-dependencies are declared under
    # `[target.'cfg(target_os = "linux")'.dev-dependencies]`, and a test
    # file that forgot the matching `#![cfg]` compiles nowhere else. The
    # library was never the problem, so `--all-targets` is the whole
    # point. Both `test (macos-latest)` and `test (windows-latest)` were
    # red on it, and nothing a developer runs said so.
    check -p hclient-rt-embassy --target x86_64-pc-windows-msvc --all-features --all-targets
    check -p hclient-rt-embassy --target aarch64-apple-darwin --all-features --all-targets

    # **Counted rather than written down.** This line said `6` while the
    # recipe made thirteen calls, which is this file's own recurring
    # defect — a number in prose that nothing forces to move. `ran` is
    # incremented by `check` itself, so it cannot drift; the floor below
    # is what keeps a recipe whose body got deleted from reporting
    # success over nothing.
    if [ "$ran" -lt 6 ]; then
      echo "::error::only $ran cross-target checks ran — a green run over almost nothing is the defect this recipe exists for"
      exit 1
    fi
    if [ ${#failed[@]} -ne 0 ]; then
      echo "::error::cross-target check failed for ${#failed[@]} of $ran invocations:"
      for f in "${failed[@]}"; do echo "  cargo check $f"; done
      exit 1
    fi
    echo "cross-target check: $ran invocations, $(echo $TARGETS | wc -w) targets, all clean"

# the Transport acceptance: one source, no #[cfg], three targets
build-three-targets:
    cargo build -p hclient --example portable
    cargo build -p hclient --example portable --target wasm32-wasip2
    cargo build -p hclient --example portable --target wasm32-unknown-unknown

# `hclient-fetch` carries an `unsafe impl Send` that is sound only because
# wasm32-unknown-unknown is single-threaded. With `+atomics` the build must
# FAIL, and with a specific E0277 about Send — not merely fail for any
# reason, which a typo in the flag would also achieve.

# the unsafe Send must be rejected when wasm atomics are on
fetch-under-wasm-threads:
    #!/usr/bin/env bash
    set -euo pipefail
    # `rust-toolchain.toml` pins stable and outranks the installed channel;
    # `-Z build-std` needs nightly.
    export RUSTUP_TOOLCHAIN=nightly
    if ! rustc --version 2>/dev/null | grep -q nightly; then
      if [ -n "${HCLIENT_REQUIRE_NIGHTLY:-}" ]; then
        echo "::error::this check needs nightly for -Z build-std — RUSTUP_TOOLCHAIN did not take effect"
        exit 1
      fi
      echo "NOTICE: no nightly toolchain — skipping the wasm-threads check."
      exit 0
    fi
    [ -d crates/hclient-fetch ] || { echo "::error::crates/hclient-fetch is missing — this check must run, not skip"; exit 1; }
    flags="-Ctarget-feature=+atomics,+bulk-memory"
    common="--target wasm32-unknown-unknown -Zbuild-std=std,panic_abort"

    # ── direction 1: the library MUST build ──────────────────────────────
    #
    # It did not, for two verticals, and the recipe that lived here
    # asserted the failure. What changed is the cause rather than the
    # check's honesty: `SendTransport::execute_send` used to box
    # `execute`'s future, which holds a `js_sys::Promise` across an await,
    # and under threads wasm-bindgen correctly stops calling that `Send`.
    # A channel moved the JS to the thread that owns it, so a browser build
    # with wasm threads can back an `hclient::Client` again.
    if ! RUSTFLAGS="$flags" cargo check -p hclient-fetch --lib $common; then
      echo "::error::hclient-fetch must build under wasm threads — the channel in execute_send exists so that it can"
      exit 1
    fi

    # ── direction 2: the unsafe `Send` MUST still disappear ──────────────
    #
    # And this is the half the old recipe was really protecting.
    # `promise::SingleThreaded<T>` carries this crate's one `unsafe impl
    # Send` (amendment C7), sound only because there is one thread. Under
    # `+atomics` that claim must stop compiling — `tests/promise.rs`'s
    # `future_is_send_on_the_default_target` is the assertion, and it is
    # required to FAIL here.
    #
    # **`--test promise`, not `--tests`, and that is a repair rather than a
    # narrowing.** With every test target in one invocation, which target
    # cargo reaches before the first error decides what is in the log —
    # `hooks` and `websocket` are `!Send` under threads too, and when they
    # fail first cargo may never compile `promise` at all. The check then
    # reports that the failure was in the wrong file, having never given
    # the right one a chance. It passed here and failed on CI from the same
    # commit, which is what a race looks like from the outside; naming the
    # one target removes it rather than making it less likely.
    log="$(mktemp)"
    if RUSTFLAGS="$flags" cargo check -p hclient-fetch --test promise $common > "$log" 2>&1; then
      echo "::error::expected tests/promise.rs to be rejected under +atomics — the guarantee SingleThreaded<T> exists to provide is not being enforced"
      exit 1
    fi
    # It must fail for the RIGHT reason, in the RIGHT file: a typo in the
    # flag also fails, and so would any unrelated break.
    if ! grep -q 'error\[E0277\]' "$log" \
       || ! grep -q 'cannot be sent between threads safely' "$log" \
       || ! grep -q 'tests/promise.rs' "$log"; then
      echo "::error::the tests failed under +atomics, but NOT with the expected Send rejection in tests/promise.rs — full output follows"
      cat "$log"
      exit 1
    fi
    echo "OK: the library builds under wasm threads, and SingleThreaded's Send is correctly rejected there"

# ── the external oracle ─────────────────────────────────────────────────

# Every fixture in `crates/hclient-tungstenite/tests/websocket.rs` was
# written beside the implementation it observes, which is the arrangement
# in which a fixture agrees with a bug. The Autobahn TestSuite is the
# answer to that: 517 client cases nobody here wrote, served by `wstest
# --mode fuzzingserver` out of `crossbario/autobahn-testsuite`.
#
# **Two recipes, and the split is the point.** The PARSER decides whether
# 517 external cases passed, so it must be checked on every machine and
# not only on the ones with Docker — `autobahn-parser-selftest` needs
# nothing and always runs. The RUN needs a container, so it skips without
# one, and `HCLIENT_REQUIRE_DOCKER` turns that skip into a failure in the
# job that promised to provide it — the same shape as
# HCLIENT_REQUIRE_WASMTIME and HCLIENT_REQUIRE_TUNTAP.
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
    agent=hclient-tungstenite
    reports="$PWD/target/autobahn"
    container=hclient-autobahn
    if ! docker info >/dev/null 2>&1; then
      if [ -n "${HCLIENT_REQUIRE_DOCKER:-}" ]; then
        echo "::error::docker is unusable, and this job promised to provide it — the Autobahn run must happen, not skip. Do not paper over this by dropping HCLIENT_REQUIRE_DOCKER: that returns a green job that tested nothing."
        exit 1
      fi
      echo "NOTICE: no usable docker — the Autobahn TestSuite run was skipped, not run."
      exit 0
    fi
    # The driver is built before the container starts, so a compile error
    # is a compile error rather than a connection timeout.
    cargo build -p hclient-tungstenite --example autobahn || exit $?
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
# The same is true of `hclient`'s `public-suffix`, and of
# `hclient-native`'s h1-only build, which went uncompiled from the moment h2
# landed behind a feature.
#
# **This recipe is the one place where CI now does MORE than it did**, and
# deliberately: `hclient-cookie --no-default-features` was in this recipe and
# NOT in the job it claimed to mirror, so `tests/without_the_list.rs` — the
# only thing checking that a no-list build is NARROWER than a list build,
# rather than quietly wider — ran on laptops and nowhere else. 78 tests.
# Resolving the drift by deleting the line would have been the other
# direction.
#
# That line survived the jar becoming a module of `hclient`, and it took
# a feature shape to do it: `cookies` pulls the `public-suffix` *crate*
# and a separate flag of that name, carried in `default`, gates the code
# path — so `--no-default-features --features cookies` is still a build
# with the module and without the list. Spelling it the obvious way
# (`cookies = [..., "public-suffix"]`) makes that combination
# unreachable, because features are additive, and the test would have
# gone silent rather than red. `--no-fail-fast` is the same story one size smaller: the recipe
# had it, the job did not, and it only ever reports more.

# the feature-off builds, which --all-features can never exercise
# **The first five minutes, from outside.** `cargo add hclient` resolves and
# then the front page's own first line does not compile, because
# `Client::new()` is behind `default-transport` and that feature is
# deliberately not a default. What a reader used to get for that was
# `error[E0599]: no associated function or constant named `new``, naming no
# feature at all — rustc's *"found an item that was configured out"* note is
# emitted for path resolution, so `hclient::default_transport()` announces
# its own gate and an inherent `fn` in a `#[cfg]`-ed-out `impl` block does
# not.
#
# Both halves are checked, and from a crate outside this workspace rather
# than from a test beside the code, because a test written here shares the
# author's knowledge of where the doors are.

# `Client::new()` without the feature must name the feature, not the name
first-five-minutes:
    #!/usr/bin/env bash
    set -uo pipefail
    root="$(pwd)"
    [ -d "$root/crates/hclient" ] || { echo "::error::crates/hclient is missing — this check must run, not skip"; exit 1; }
    # The third arm below needs it, and a skipped arm is the defect this
    # file records against `test-wasi` and `check-targets` alike.
    rustup target list --installed | grep -qx wasm32-wasip2 || {
      echo "::error::wasm32-wasip2 is not installed — \`rustup target add wasm32-wasip2\`. The WASI half of this check would silently not run."
      exit 1
    }
    dir="$(mktemp -d)"
    trap 'rm -rf "$dir"' EXIT
    mkdir -p "$dir/src"
    # A path dependency rather than the registry: the question is what this
    # working tree does, and a published version would answer for a
    # different commit. `[workspace]` keeps it out of this one's.
    cat > "$dir/Cargo.toml" <<EOF
    [package]
    name = "first-five-minutes"
    version = "0.0.0"
    edition = "2024"

    [workspace]

    [dependencies]
    hclient = { path = "$root/crates/hclient" }
    EOF
    sed -i 's/^    //' "$dir/Cargo.toml"
    # The crate's front page, copied rather than paraphrased.
    cat > "$dir/src/lib.rs" <<'EOF'
    pub async fn f() -> Result<(), hclient::Error> {
        let client = hclient::Client::new()?;
        let text = client.get("https://example.com").send().await?.collect().await?.text()?;
        let _ = text;
        Ok(())
    }
    EOF
    sed -i 's/^    //' "$dir/src/lib.rs"

    # ── direction 1: it must fail, and for the RIGHT reason ──────────────
    log="$dir/off.log"
    if (cd "$dir" && cargo build --color never) > "$log" 2>&1; then
      echo "::error::the front page compiled without \`default-transport\` — either the feature moved into \`default\` (see AGENTS.md on why a default here is a floor) or this fixture stopped exercising \`Client::new\`"
      exit 1
    fi
    if ! grep -q 'error\[E0277\]' "$log" \
       || ! grep -q 'needs the `default-transport` feature' "$log" \
       || ! grep -q 'cargo add hclient --features default-transport' "$log"; then
      echo "::error::it failed, but not with the refusal that names the feature — full output follows"
      cat "$log"
      exit 1
    fi

    # ── direction 2: rustc's own note, which is why there is one stub ────
    #
    # `default_transport()` is a free function, so path resolution reports
    # its `#[cfg]` unprompted. If that ever stops being true, the asymmetry
    # this recipe's header states is wrong and the missing half is owed.
    cat > "$dir/src/lib.rs" <<'EOF'
    pub fn g() -> Result<(), hclient::Error> {
        let _ = hclient::default_transport()?;
        Ok(())
    }
    EOF
    sed -i 's/^    //' "$dir/src/lib.rs"
    log="$dir/free.log"
    if (cd "$dir" && cargo build --color never) > "$log" 2>&1; then
      echo "::error::\`default_transport()\` compiled without the feature"
      exit 1
    fi
    if ! grep -q 'found an item that was configured out' "$log" \
       || ! grep -q 'default-transport' "$log"; then
      echo "::error::rustc no longer names the gate for a configured-out free function — \`default_transport\` now owes the same stub \`Client::new\` carries; output follows"
      cat "$log"
      exit 1
    fi

    # ── direction 3: on WASI the same refusal must NOT name the feature ──
    #
    # `wasm32-wasip2` reaches this stub with `default-transport` already on,
    # because `hclient` does not depend on `hclient-wasi` and there is no
    # branch to resolve. Telling that caller to add a feature they have is
    # the one wrong answer a single message would have given, so the
    # headline forks and both halves are checked.
    cat > "$dir/src/lib.rs" <<'EOF'
    pub async fn f() -> Result<(), hclient::Error> {
        let client = hclient::Client::new()?;
        let text = client.get("https://example.com").send().await?.collect().await?.text()?;
        let _ = text;
        Ok(())
    }
    EOF
    sed -i 's/^    //' "$dir/src/lib.rs"
    sed -i 's|^hclient = .*|hclient = { path = "'"$root"'/crates/hclient", features = ["default-transport"] }|' "$dir/Cargo.toml"
    log="$dir/wasi.log"
    if (cd "$dir" && cargo build --target wasm32-wasip2 --color never) > "$log" 2>&1; then
      echo "::error::\`Client::new()\` compiled for wasm32-wasip2 — there is no default transport there, so either one was added or this fixture stopped exercising it"
      exit 1
    fi
    if ! grep -q 'no default transport to build on `wasm32-wasip2`' "$log" \
       || grep -q 'cargo add hclient --features default-transport' "$log"; then
      echo "::error::the WASI refusal is missing or is telling the caller to add a feature that is already on — full output follows"
      cat "$log"
      exit 1
    fi

    # ── the control: with the feature, the same source builds ────────────
    #
    # Without this the recipe is green for a refusal that has nothing to do
    # with the feature — a broken crate refuses everything.
    cat > "$dir/src/lib.rs" <<'EOF'
    pub async fn f() -> Result<(), hclient::Error> {
        let client = hclient::Client::new()?;
        let text = client.get("https://example.com").send().await?.collect().await?.text()?;
        let _ = text;
        Ok(())
    }
    EOF
    sed -i 's/^    //' "$dir/src/lib.rs"
    sed -i 's|^hclient = .*|hclient = { path = "'"$root"'/crates/hclient", features = ["default-transport"] }|' "$dir/Cargo.toml"
    if ! (cd "$dir" && cargo build --color never); then
      echo "::error::the front page does NOT compile with \`default-transport\` — the two lines a reader copies are wrong"
      exit 1
    fi
    echo "OK: without the feature the refusal names it; with the feature the front page compiles"

test-no-default:
    #!/usr/bin/env bash
    # `-e`, and it is load-bearing: the four clippy lines at the end are
    # unguarded, so without it the recipe's exit code is the LAST one's and a
    # failure in any earlier step is reported as success. That is how three
    # dead-code errors under `--no-default-features` sat on `main` while this
    # recipe — and the CI job that calls it — stayed green.
    set -euo pipefail
    for args in "-p hclient-proto --no-default-features" \
                "-p hclient-native --no-default-features" \
                "-p hclient-native --features http2" \
                "-p hclient-rt-embassy" \
                "-p hclient --no-default-features --features cookies,test-util" \
                "-p hclient --no-default-features --features test-util"; do
      rc=0
      out="$(cargo nextest run $args --color never --no-fail-fast 2>&1)" || rc=$?
      printf '%s\n' "$out"
      [ "$rc" -eq 0 ] || exit "$rc"
      if ! printf '%s\n' "$out" | grep -qE '[0-9]+ tests? run:'; then
        echo "::error::nextest reported no summary for [$args] — the run did not happen"
        exit 1
      fi
    done
    cargo clippy -p hclient-proto --all-targets --no-default-features -- -D warnings
    cargo clippy -p hclient-native --all-targets --no-default-features -- -D warnings
    cargo clippy -p hclient-rt-embassy --all-targets -- -D warnings
    cargo clippy -p hclient --all-targets --no-default-features --features test-util -- -D warnings

# The whole claim of `hclient-idn` is that the platform's ICU answers what
# the bundled `idna` crate answers. The platform column of
# `tests/differential.rs` does not compile where there is no platform
# backend, which is why HCLIENT_IDN_REQUIRE_PLATFORM is set on Windows
# alone — the one runner in this project that has one. Apple had a backend
# until the corpus was run against it: `NSURL` is a URL parser, so it does
# not case-fold ASCII and does not validate an ACE label. The default
# (`platform`, resolved by target) AND `--all-features`, which is what the
# workspace suite runs: features have to stay additive, so `idna` on a
# target that already takes it is a no-op, not an error.

# the IDN differential corpus, on both feature settings
test-idn:
    #!/usr/bin/env bash
    set -euo pipefail
    for args in "" "--all-features"; do
      rc=0
      out="$(cargo nextest run -p hclient-idn $args --color never --no-fail-fast 2>&1)" || rc=$?
      printf '%s\n' "$out"
      [ "$rc" -eq 0 ] || exit "$rc"
      if ! printf '%s\n' "$out" | grep -qE '[0-9]+ tests? run:'; then
        echo "::error::nextest reported no summary for [$args] — the run did not happen"
        exit 1
      fi
    done

# The corpus in a browser — the check the Apple backend never had.
#
# `wasm32-unknown-unknown` has a backend of its own now: the browser's
# `new URL()`, which the WHATWG URL Standard *defines* as UTS 46, unlike
# Foundation's undocumented conversion. That difference is a claim and
# this is what tests it, on the same 40 rows that caught Foundation — so
# the backend is judged row by row from the day it lands rather than by
# the two-name acceptance probe, which Foundation passed.
#
# Fails closed on a run that reported nothing: `wasm-pack` exits zero for
# a binary with no tests in it, and this file's whole subject is a check
# that cannot fail.
test-idn-browser browser="firefox":
    #!/usr/bin/env bash
    set -euo pipefail
    cd crates/hclient-idn
    rc=0
    out="$(wasm-pack test --headless --{{browser}} 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -eq 0 ] || exit "$rc"
    if ! printf '%s\n' "$out" | grep -qE '[0-9]+ passed'; then
      echo "::error::wasm-pack reported no test result for hclient-idn — the browser run did not happen"
      exit 1
    fi

# clippy on hclient-idn's two feature settings
lint-idn:
    cargo clippy -p hclient-idn --all-targets -- -D warnings
    cargo clippy -p hclient-idn --all-targets --all-features -- -D warnings

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

# every crate builds from its own published tarball, without publishing
package-build:
    #!/usr/bin/env bash
    set -euo pipefail
    # `cargo package --workspace` does the two things a publish would do and
    # stops before the upload: it builds each `.crate` from the files that
    # would actually be shipped, and then **verifies** each one by compiling
    # it out of that tarball. That second half is the point — it is the only
    # check here that builds a crate the way a reader would get it rather
    # than the way this workspace sits on disk, which is the defect
    # `test-doc` was written for one level up.
    #
    # It found a real one on its first run: `hclient-fetch` and
    # `hclient-native` each dev-depend on `hclient`, which depends on them —
    # a cycle cargo allows inside a workspace and refuses at package time,
    # because a dev-dependency carrying a version has to resolve from the
    # registry and `hclient` cannot be there until they are. Both are path-
    # only now, so cargo strips them from the published manifest.
    #
    # Deliberately not `cargo publish --dry-run`: that does this and also
    # talks to the registry about ownership and version collisions, which is
    # a different question and one CI has no credentials to ask.
    rc=0
    out="$(cargo package --workspace --allow-dirty --color never 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -eq 0 ] || { echo "::error::a crate does not build the way it would be published"; exit "$rc"; }
    # Fail closed on a run that packaged little or nothing: a `cargo package`
    # that quietly did five crates would otherwise report success for
    # twenty-five nobody built. Verified, not merely packaged — the two
    # counts must both be there and must agree.
    pkg="$(printf '%s\n' "$out" | grep -c 'Packaged' || true)"
    ver="$(printf '%s\n' "$out" | grep -c 'Verifying' || true)"
    # Derived rather than written down: a literal here goes stale the next
    # time a crate is added or folded in, and a stale floor is a check that
    # passes for a run that did less than it should.
    want="$(cargo metadata --format-version 1 --no-deps \
        | python3 -c 'import json,sys; print(sum(1 for p in json.load(sys.stdin)["packages"] if p.get("publish") != []))')"
    if [ "$pkg" -lt "$want" ] || [ "$ver" -ne "$pkg" ]; then
      echo "::error::packaged $pkg and verified $ver — expected at least $want of each, and equal"
      exit 1
    fi
    echo "$ver crates build from their own published tarball"

# every publishable crate ships its licence texts and a README
packaging:
    #!/usr/bin/env bash
    set -euo pipefail
    # `license = "MIT OR Apache-2.0"` is a claim; the texts are what make it
    # a grant, and this workspace declared the one without carrying the
    # other for its whole life. The detail that decides where they live is
    # that **a file at the repository root never reaches the tarball** —
    # `cargo package` takes only what is inside the crate's own directory —
    # so each crate carries its own copy, and nothing but packaging one
    # would have shown that. A README is the same shape one step down: with
    # none, a crates.io page is a single line of `description`.
    #
    # Asserted against the packaged file list rather than against the
    # working tree, because the tree can hold a file the tarball drops.
    n=0
    for d in crates/*/; do
      grep -q '^publish = false' "$d/Cargo.toml" && continue
      c="$(basename "$d")"
      list="$(cargo package -p "$c" --no-verify --allow-dirty --list 2>/dev/null)"
      for f in LICENSE-APACHE LICENSE-MIT README.md; do
        printf '%s\n' "$list" | grep -qx "$f" || {
          echo "::error::$c would publish without $f"; exit 1; }
      done
      n=$((n+1))
    done
    # Fail closed on the loop never running: an empty `crates/*/` glob, or a
    # `publish = false` added everywhere, would otherwise report success
    # over nothing checked at all.
    #
    # **Derived, not written down**, the same way `package-build` above
    # derives its own floor and for the reason that recipe gives: a literal
    # goes stale the next time a crate is added or folded in, and a stale
    # floor is a check that passes for a run that did less than it should.
    # It was a literal until the jar and the cache became modules of
    # `hclient` — 25 to 23 — which is exactly the edit the derivation
    # removes.
    want="$(cargo metadata --format-version 1 --no-deps \
        | python3 -c 'import json,sys; print(sum(1 for p in json.load(sys.stdin)["packages"] if p.get("publish") != []))')"
    [ "$n" -eq "$want" ] || { echo "::error::checked $n crates, workspace has $want publishable — the glob missed some"; exit 1; }
    echo "$n publishable crates carry both licence texts and a README"

# Which crates have changes since the version they last published.
#
# **Diagnostics, not a step.** The release policy is to publish every
# crate on every release (`docs/publishing.md` §5), which cannot leave one
# behind, so nothing has to answer this. It is kept for seeing what has
# accumulated before choosing a version level, and for the day the policy
# goes back to selecting with `-p` — which cargo-release does not compute
# for you: measured, with a tag one commit back and one crate touched, a
# plain `cargo release patch` still planned every upload.
#
# **Deliberately not in `ci`.** It asks crates.io over the network, which
# is the kind of flakiness a gate must not have, and the answer is only
# wanted before a release.

# Everything a release needs green, in the order a failure is cheapest to
# find. `packaging` and `package-build` are the two that only a publish
# would otherwise exercise: the first checks what a `.crate` would carry,
# the second builds each one out of its own tarball — the only check here
# that compiles a crate the way a reader would receive it rather than the
# way this workspace sits on disk.
#
# `release-pending` is last and is **diagnostics**: the policy publishes
# every crate (`docs/publishing.md` §5), so nothing here has to answer
# which changed. It is printed so whoever is releasing can see what has
# accumulated before choosing a level.
#
# Not in `ci`: `package-build` is minutes of work for a question only a
# release asks, and `release-pending` reaches the network.

# everything a release needs, plus what has accumulated since the last one
release-check: ci packaging package-build release-pending

# crates with unreleased changes (network; run before releasing)
release-pending:
    ./scripts/release-pending.sh

# install a toolchain for every floor a crate declares, for `msrv`
#
# **The list is the manifests', like the recipe below.** A crate leaves
# the family's "MSRV is the latest stable" by writing its own literal
# `rust-version`, and that line is the only statement of its floor — so
# the toolchain to install is derived from it rather than named in the
# workflow, where it would be a second copy that goes stale the day a
# floor moves.
#
# `msrv` refuses a floor whose toolchain is missing rather than skipping
# it, which is what makes this worth a recipe: forget to run it and the
# check fails loudly instead of quietly testing nothing.
msrv-toolchains:
    #!/usr/bin/env bash
    set -euo pipefail
    floors="$(grep -h '^rust-version = "' crates/*/Cargo.toml | sed 's/.*"\(.*\)".*/\1/' | sort -u)"
    [ -n "$floors" ] || { echo "::error::no crate declares its own rust-version, so this installed nothing — which reads the same as a tidy tree"; exit 1; }
    for f in $floors; do
      echo "==> rustup toolchain install $f"
      rustup toolchain install "$f" --profile minimal --no-self-update
    done

# the one crate with a fixed compiler floor, built on it
msrv:
    #!/usr/bin/env bash
    set -uo pipefail
    # **`system-resolver` alone, because it is the only crate here with a
    # floor to check.** Every other member says `rust-version.workspace =
    # true`, and this workspace's policy is that the MSRV *is the latest
    # stable* — a moving promise, already stated by the toolchain CI runs
    # on, and a job pinning a version would be a second statement of it
    # that goes stale while looking maintained. That argument is in
    # `AGENTS.md` and it stands.
    #
    # It does not reach this crate. Its floor is a **fixed** version, so a
    # job checking that version is the promise's only statement rather
    # than a staler copy of one. Without it `rust-version = "1.85.0"` is a
    # claim with nothing behind it, which is the defect this repository
    # records against itself four times over.
    #
    # The floor is `std::assert_matches!`'s, with `core::cfg_select!` one
    # release below it; every dependency's own `rust-version` is far below
    # both — `thiserror` 1.71, `windows-sys` 1.71, `windows-strings` 1.82.
    # **The crate list is derived, not written.** A crate has its own floor
    # exactly when its manifest declares a literal `rust-version` instead
    # of inheriting the family's, which is the same two-line gesture that
    # takes it out of the shared version — so a third such crate is
    # covered here the day it writes those lines, with no edit to this
    # recipe and no chance of one being forgotten.
    #
    # The floor itself is read from each manifest rather than restated
    # here. There used to be a second copy and a check that the two
    # agreed; one statement needs no such check, which is the better half
    # of the same idea.
    crates=""
    while read -r manifest; do
      floor="$(grep -m1 '^rust-version = "' "$manifest" | sed 's/.*"\(.*\)".*/\1/')"
      [ -n "$floor" ] || continue
      name="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$manifest" | head -1)"
      crates="$crates$name:$floor "
    done < <(ls crates/*/Cargo.toml)
    [ -n "$crates" ] || { echo "::error::no crate declares its own rust-version — this recipe found nothing to check, which reads the same as a tidy tree"; exit 1; }
    failed=0
    checked=0
    for entry in $crates; do
      name="${entry%%:*}"; FLOOR="${entry##*:}"
      if ! rustup toolchain list | grep -q "^$FLOOR"; then
        echo "::error::toolchain $FLOOR is not installed, and $name declares it — \`rustup toolchain install $FLOOR\`. Skipping it would return exactly the blind spot this recipe covers."
        exit 1
      fi
      # `--ignore-rust-version` is required and is not a loophole: the
      # *workspace* manifest still declares the family's floor, and cargo
      # refuses the whole build on the higher number before it ever looks
      # at this crate's own. What is being checked is the crate, which
      # carries the lower one.
      echo "==> cargo +$FLOOR check -p $name --all-targets"
      cargo "+$FLOOR" check -p "$name" --all-targets --ignore-rust-version --color never || failed=1
      echo "==> cargo +$FLOOR test -p $name"
      out="$(cargo "+$FLOOR" test -p "$name" --ignore-rust-version --color never 2>&1)" || failed=1
      printf '%s\n' "$out"
      # Fails closed on a run that tested nothing: a suite compiled and not
      # run, and a tidy tree, print the same summary otherwise.
      ran="$(printf '%s\n' "$out" | grep -c '^test result: ok\.')"
      if [ "$ran" -lt 2 ]; then
        echo "::error::only $ran test binaries reported a result for $name on $FLOOR — a green run over nothing is the defect this recipe exists for"
        exit 1
      fi
      if [ "$failed" -ne 0 ]; then
        echo "::error::$name does not build or pass its tests on its declared floor, $FLOOR"
        exit 1
      fi
      checked=$((checked + 1))
      echo "msrv: $name builds and passes $ran test binaries on $FLOOR"
    done
    echo "msrv: $checked crate(s) with a floor of their own, each checked on it"

# the compatibility promise, for the one crate that is in a position to make one
semver rev="":
    #!/usr/bin/env bash
    set -euo pipefail
    # **Only `system-resolver`, and that is the finding rather than a first
    # step.** Every other crate here rides `[workspace.package].version`,
    # which is a pre-release — and inside a pre-release every step is a
    # major one, so cargo-semver-checks skips every lint it has and reports
    # success. Measured on this crate, 0.50.0: `0.1.0-alpha.2 ->
    # 0.1.0-alpha.2` runs **0 checks of 254**, `0.1.0-alpha.2 -> 0.1.0`
    # also runs **0**, and `0.1.0 -> 0.1.1` runs **196**. A job over the
    # whole family would therefore be green for a tree in which every
    # promise had been broken.
    #
    # `rev` is for a check against a point in history — `just semver
    # v0.1.0`. With no argument the baseline is the newest release on
    # crates.io, which is the question that matters: what a caller who
    # already typed `cargo add system-resolver` is holding.
    if ! command -v cargo-semver-checks >/dev/null; then
      echo "::error::cargo-semver-checks is not installed, so this check could not run — and a check that could not run must not pass"
      exit 1
    fi
    # **The crates are derived, and so is whether each is publishable
    # yet.** A crate is gated here exactly when it carries its own
    # `[package.metadata.release] shared-version` — the same two-line
    # gesture that gives it its own version and its own floor — and only
    # once crates.io has it, because before the first publish there is no
    # baseline and cargo-semver-checks can only report *not found in
    # registry*.
    #
    # Deriving both is what keeps this from going stale in either
    # direction: a second independently-versioned crate is gated the day
    # it is published, with no edit here, and an unpublished one does not
    # turn the gate red for a reason that is not a defect.
    published=""
    pending=""
    vacuum=""
    stepped=""
    # Whether two versions share a semver-compatible range, which is what
    # decides whether a lint can run at all: `^0.1.0` covers 0.1.x and not
    # 0.2.0, so below 1.0 the *minor* is the major component. Written out
    # rather than delegated because the whole recipe exists to avoid
    # trusting a tool to tell it what it is checking.
    same_compat_range() {
      a_major="${1%%.*}"; a_rest="${1#*.}"; a_minor="${a_rest%%.*}"
      b_major="${2%%.*}"; b_rest="${2#*.}"; b_minor="${b_rest%%.*}"
      [ "$a_major" = "$b_major" ] || return 1
      [ "$a_major" != "0" ] || [ "$a_minor" = "$b_minor" ] || return 1
      return 0
    }
    while read -r manifest; do
      # `name *=` rather than `name =`: two manifests align the value
      # with their neighbours, and a pattern that missed them killed this
      # loop under `set -e` rather than skipping them — found by the loop
      # stopping after the first candidate.
      name="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$manifest" | head -1)"
      [ -n "$name" ] || { echo "::error::$manifest declares no package name"; exit 1; }
      grep -q "^shared-version = \"$name\"" "$manifest" || continue
      latest="$(curl -sS -H "User-Agent: hclient semver gate (gamepad64@gmail.com)" \
                  "https://crates.io/api/v1/crates/$name" \
                | sed -n 's/.*"max_version":"\([^"]*\)".*/\1/p')"
      if [ -z "$latest" ]; then
        pending="$pending $name"
      elif printf '%s' "$latest" | grep -q -- '-'; then
        # **The published baseline is a pre-release, so nothing can be
        # checked against it and that is not a defect.** Every step out of
        # a pre-release is a major one, where breaking is permitted, so
        # cargo-semver-checks skips all of its lints — measured against
        # this very workspace. The exemption ends by itself the day a
        # stable version is published, because this reads the registry
        # rather than a list.
        vacuum="$vacuum $name($latest)"
      elif ! same_compat_range "$latest" "$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$manifest" | head -1)"; then
        # **A deliberate major step, where no lint can run either**, and
        # the same reasoning as the pre-release above rather than a second
        # exemption: cargo-semver-checks permits breaking across a major
        # boundary, so it executes none of its lints and the zero-check
        # guard below would fire on a state that is correct.
        #
        # Read off two numbers rather than a list — the registry's and the
        # manifest's — so it ends by itself on the day the new version is
        # published and the baseline moves. Dodging the gate this way
        # costs a major version, which is the loudest thing a release can
        # do, and the crate is named in the output either way.
        stepped="$stepped $name($latest->$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$manifest" | head -1))"
      else
        published="$published $name"
      fi
    done < <(ls crates/*/Cargo.toml)
    # `if` rather than `[ .. ] && echo`: under `set -e` that idiom exits
    # the recipe when the variable is empty, which is the common case.
    if [ -n "$pending" ]; then
      echo "semver: not on crates.io yet, so no baseline at all:$pending"
    fi
    if [ -n "$vacuum" ]; then
      echo "semver: baseline is a pre-release, where every step is major and no lint runs:$vacuum"
    fi
    if [ -n "$stepped" ]; then
      echo "semver: a deliberate major step, where breaking is permitted and no lint runs:$stepped"
    fi
    if [ -z "$published" ]; then
      echo "::error::no independently-versioned crate has a stable release, so this gate checked nothing — which reads the same as a clean run"
      exit 1
    fi
    args=(check-release --color never)
    for name in $published; do args+=(-p "$name"); done
    if [ -n "{{rev}}" ]; then args+=(--baseline-rev "{{rev}}"); fi
    rc=0
    out="$(cargo semver-checks "${args[@]}" 2>&1)" || rc=$?
    printf '%s\n' "$out"
    if [ "$rc" -ne 0 ]; then
      # Its own report is above and names the lint and the item; adding a
      # summary of ours would be a second statement of one fact.
      exit "$rc"
    fi
    # **The half a zero exit does not cover.** A run that executed nothing
    # prints `0 checks: 0 pass, 254 skip`, says `no semver update
    # required`, and exits zero — which is this project's recurring defect
    # met once more, a check that cannot fail. So the count is read, and a
    # zero is a failure that names its own cause.
    # **Read per crate rather than in total**, because an aggregate hides
    # exactly the case this half exists for: one crate's 196 checks and
    # another's zero add up to a green run over a crate nothing examined.
    counts="$(printf '%s\n' "$out" | sed -n 's/.*[^0-9]\([0-9][0-9]*\) checks:.*/\1/p')"
    if [ -z "$counts" ]; then
      echo "::error::no \`N checks:\` line in the output — cargo-semver-checks changed its report and this gate is reading nothing"
      exit 1
    fi
    n=0
    total=0
    for c in $counts; do
      n=$((n + 1))
      total=$((total + c))
      if [ "$c" -eq 0 ]; then
        echo "::error::one crate had every lint skipped, so it proved nothing: a stable baseline and a major step, where breaking is permitted"
        exit 1
      fi
    done
    want=0
    for _ in $published; do want=$((want + 1)); done
    if [ "$n" -ne "$want" ]; then
      echo "::error::$want crate(s) were asked about and $n report line(s) came back — the gate and the tool disagree about what was checked"
      exit 1
    fi
    echo "semver: $total checks across $n crate(s) with a stable baseline"

# rustdoc warnings, which nothing checked until publishing made them visible
docs:
    #!/usr/bin/env bash
    set -euo pipefail
    # This project's actual product is its prose, and it had no check at
    # all: 96 warnings had accumulated across 17 crates. Four kinds, in
    # rising order of harm — a `redundant explicit link target`; a link
    # from public prose to a private item, which docs.rs renders as
    # literal `[`brackets`]`; an unresolved link; and two unclosed HTML
    # tags, which silently *delete* the rest of the sentence from the
    # page. The dominant cause was one shape: a link target wrapped
    # across two `///` lines. rustdoc does not rejoin the path, so twelve
    # targets were `crate::` followed by a newline.
    #
    # `--no-deps` because a dependency's warnings are not ours to fix, and
    # `--all-features` because a feature-gated item's docs are the half of
    # this crate a reader most needs — every backend, coding and knob.
    # A crate that declares a docs.rs `default-target` is documented for
    # **that** target below and excluded here, because the two runs
    # compile different files: `hclient-winhttp` is `#![cfg(windows)]`, so
    # a host pass sees an empty crate in which every link to one of its
    # own types is unresolved — a failure about this recipe rather than
    # about the prose. Excluded, not skipped: what a reader sees on
    # docs.rs is what gets checked, one line down.
    excluded=""
    while read -r crate _; do excluded="$excluded --exclude $crate"; done < <(./scripts/docsrs-targets.sh)
    rc=0
    out="$(RUSTDOCFLAGS="-D warnings" cargo doc --workspace $excluded --all-features \
             --no-deps --color never 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -eq 0 ] || { echo "::error::rustdoc reported warnings"; exit "$rc"; }
    # Fails closed on the run not happening, the way `test-doc` does:
    # rustdoc printing nothing is indistinguishable from rustdoc finding
    # nothing, and reporting `ok` over a build that never ran is this
    # recipe's whole failure mode.
    if ! printf '%s\n' "$out" | grep -qE 'Generated .*index\.html|Finished'; then
      echo "::error::no sign rustdoc ran at all — the check did not happen"
      exit 1
    fi
    # **A crate that documents for another target is documented for it**,
    # and this is not thoroughness — it is the only run that sees the
    # crate at all. `hclient-winhttp` is `#![cfg(windows)]`, so the
    # workspace pass above compiles an empty file for it and every link to
    # one of its own types is unresolved. It carries
    # `default-target = "x86_64-pc-windows-msvc"`, which is what docs.rs
    # builds and therefore what a reader sees, so that is what is checked.
    #
    # This was found the way it usually is: the run that was green was not
    # the run CI makes. `cargo doc --target x86_64-pc-windows-msvc` was
    # clean while `just docs` had three unresolved links, because they are
    # two different compilations of two different files.
    targets=$(./scripts/docsrs-targets.sh)
    [ -n "$targets" ] || { echo "::error::no crate declares a docs.rs default-target — the query found nothing rather than a tidy tree"; exit 1; }
    # A target that is not installed must say so, rather than reaching
    # rustc and coming back as `can't find crate for core` — which is what
    # it did on CI, from a recipe that was green here because this machine
    # has every target. `check-targets` states the same rule one recipe
    # over, and this is the second place that needed it.
    while read -r crate target; do
      rustup target list --installed | grep -qx "$target" || {
        echo "::error::$crate documents for $target, which is not installed — \`rustup target add $target\`. Skipping it would publish a page nothing checked."
        exit 1
      }
    done <<< "$targets"
    printf '%s\n' "$targets" | while read -r crate target; do
      echo "==> $crate for $target"
      RUSTDOCFLAGS="-D warnings" cargo doc -p "$crate" --target "$target" \
        --all-features --no-deps || exit 1
    done
# nightly is not optional here (sanitizer flags), and `RUSTUP_TOOLCHAIN` is
# how it survives rust-toolchain.toml. The explicit `--target` is not
# cosmetic either: cargo-fuzz defaults to the triple it was itself built for,
# which is musl when it arrives as a prebuilt binary — every target then
# fails with "sanitizer is incompatible with statically linked libc".

# fuzz one target for N seconds: `just fuzz sse_accounting 30`
fuzz TARGET="sse" SECONDS="60":
    cd crates/hclient-proto/fuzz && \
      RUSTUP_TOOLCHAIN=nightly cargo fuzz run \
        --target "$(rustc -vV | sed -n 's/^host: //p')" \
        {{TARGET}} -- -max_total_time={{SECONDS}}

# the short fuzz runs CI does on every push
fuzz-smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    export RUSTUP_TOOLCHAIN=nightly
    if ! rustc --version 2>/dev/null | grep -q nightly || ! cargo fuzz --version >/dev/null 2>&1; then
      if [ -n "${HCLIENT_REQUIRE_NIGHTLY:-}" ]; then
        echo "::error::cargo fuzz is about to run on a non-nightly toolchain, or cargo-fuzz is missing — RUSTUP_TOOLCHAIN did not take effect, or \`bins:\` did not install it"
        exit 1
      fi
      echo "NOTICE: no nightly toolchain or no cargo-fuzz — skipping fuzz-smoke."
      exit 0
    fi
    host="$(rustc -vV | sed -n 's/^host: //p')"
    [ -n "$host" ] || { echo "::error::could not read the host triple out of rustc -vV"; exit 1; }
    ( cd crates/hclient-proto/fuzz && \
      cargo fuzz run --target "$host" sse -- -max_total_time=60 && \
      cargo fuzz run --target "$host" sse_accounting -- -max_total_time=30 -max_len=256 ) || exit 1
    # **`hclient-idn` has no fuzz targets any more**, and the reason is
    # its definition rather than a maintenance decision: it is a
    # smaller-binary `idna` with no layer of its own, so on the Linux
    # runner any fuzzer uses, the bundled backend **is** `idna`. A
    # differential target would compare `idna` with itself and an
    # idempotence target would measure `idna`'s — checks that cannot fail.
    # What they were aimed at is whether Foundation and ICU answer what
    # `idna` answers, and only Windows and macOS can be asked that;
    # `crates/hclient-idn/tests/differential.rs` asks them on every push.

# ── invariants no build can express ─────────────────────────────────────

# the text scans, together
invariants: ast-grep no-send-or-sync unsafe-policy errors-in-error-rs no-crate-for-what-std-does versions-agree ci-mirrors-just

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

# every error type lives in a file named `error.rs`
errors-in-error-rs:
    ./scripts/errors-live-in-error-rs.sh

# no crate for a job `core` or `std` now does itself — `cfg-if` and
# `assert_matches`, each with the macro that replaced it
no-crate-for-what-std-does:
    ./scripts/no-crate-for-what-std-does.sh

# every in-workspace requirement names the workspace version
versions-agree:
    ./scripts/versions-agree.sh

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
    cargo deny --manifest-path crates/hclient-fetch/Cargo.toml \
        --config .github/deny/ambient.toml -t wasm32-unknown-unknown check bans
    cargo deny --manifest-path crates/hclient-wasi/Cargo.toml \
        --config .github/deny/ambient.toml -t wasm32-wasip2 check bans

# The two seams v0.3 changed for HTTP/3 must stay implementable without a
# QUIC stack: `hclient-rt` re-declares `RecvMeta`/`EcnCodepoint` instead of
# re-exporting `quinn-udp`'s, and `hclient-tls-quic` is a separate crate
# rather than a feature of `hclient-tls`. Both are trades paid in conversion
# code and crate count, and both are worthless the moment this stops holding.

# the runtime and TLS seams contain no QUIC
graph-no-quic:
    #!/usr/bin/env bash
    set -euo pipefail
    # `hclient-rt` has no QUIC at all, optional or otherwise, so `cargo deny`
    # is the right tool: it analyses every edge a graph *could* have, which
    # is the strong claim.
    cargo deny --manifest-path crates/hclient-rt/Cargo.toml \
      --config .github/deny/no-quic-in-the-seams.toml check bans
    # `hclient-tls` is the weaker and more useful claim — **not by default**
    # — and `cargo deny` structurally cannot express it: it walks optional
    # edges regardless of features (checked with `--no-default-features`,
    # which does not change its answer). `cargo tree` respects features, so
    # the claim is made with it, in both directions.
    ./scripts/tree-guard.sh absent '^(quinn-proto|quinn|quinn-udp|h3) ' \
      "hclient-tls pulls a QUIC crate with no feature asked for. The quic seam is behind a feature precisely so a NoTls build carries none of it" \
      -- -p hclient-tls
    ./scripts/tree-guard.sh present '^quinn-proto ' \
      "hclient-tls/quic does not pull quinn-proto, so the check above is vacuous — it would pass a crate whose feature had stopped doing anything" \
      -- -p hclient-tls --features quic

# And the other direction, because a ban that would pass against an empty
# graph proves nothing: the tokio runtime's `udp` feature really does pull
# `quinn-udp`, so the ban above is checked against a graph where the crate is
# one step away rather than absent from the workspace.

# ...and the udp feature really does pull quinn-udp, off by default
graph-udp-pulls-quic:
    #!/usr/bin/env bash
    set -euo pipefail
    out="$(cargo tree -p hclient-rt-tokio --features udp -e normal --prefix none)"
    printf '%s\n' "$out" | grep -q '^quinn-udp ' || {
      echo "::error::hclient-rt-tokio/udp no longer pulls quinn-udp — either the feature is dead or the ban in graph-no-quic is now vacuous"
      exit 1
    }
    off="$(cargo tree -p hclient-rt-tokio -e normal --prefix none)"
    if printf '%s\n' "$off" | grep -q '^quinn-udp '; then
      echo "::error::quinn-udp is in hclient-rt-tokio's default graph; the udp feature is not off by default"
      exit 1
    fi

# the smol path carries no reactor, only hyper's inert tokio leaf
graph-smol-path:
    #!/usr/bin/env bash
    set -euo pipefail
    for t in x86_64-unknown-linux-gnu x86_64-apple-darwin x86_64-pc-windows-msvc; do
      cargo deny --manifest-path crates/hclient-rt-smol/Cargo.toml \
        --config .github/deny/smol-path.toml -t "$t" check bans
    done

# The WebSocket framing lives in its own crate, machine-checked in both
# directions. It was a `websocket` feature of `hclient-native` once, and the
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
        "hclient-native has the WebSocket framing in its graph again. It lives in hclient-tungstenite, which depends on this crate rather than the other way round, precisely so that no feature switched on by a neighbour can put it here" \
        -- -p hclient-native --all-features
    ./scripts/tree-guard.sh present '^tungstenite ' \
        "hclient-tungstenite does not depend on tungstenite, so the ban above is vacuous — it would pass a workspace with no framing at all" \
        -- -p hclient-tungstenite

# **quinn stays inside one module**, which is what `hclient-quinn` used to
# be a crate for. The adapter — `quinn::Runtime` over `hclient_rt::{Timer,
# Spawn, UdpBind}` — folded into `hclient-native` when its only consumer
# turned out to be `hclient-h3`, which folded in too. What a crate boundary
# was enforcing, a module boundary and this check enforce instead: nothing
# outside `src/quinn.rs` names `quinn`, so the third-party API has exactly
# one place it can leak from.
#
# Checked in the failing direction by naming `quinn::Endpoint` in
# `src/lib.rs` and watching it fire.
quinn-stays-in-its-module:
    #!/usr/bin/env bash
    set -euo pipefail
    cd crates/hclient-native/src
    # `mod http3` legitimately names `quinn` — it drives the connection —
    # and so does its `runtime` submodule, which implements
    # `quinn::Runtime`. The boundary is that directory against the rest of
    # the crate: what must stay out is everything a build without the
    # `http3` feature compiles.
    #
    # One `grep -vE` where there were two alternatives, because the module
    # move put both inside `http3/`. `runtime.rs` is also no longer called
    # `quinn.rs`, which it could not be: a submodule of that name shadows
    # the crate for every `quinn::` path in `http3/mod.rs`.
    #
    # `(?<!crate::)` so this crate's own `crate::http3::` module paths are
    # not mistaken for the third-party crate the boundary is about.
    stray="$(grep -rlnP '(?<!crate::)\bquinn(_proto|_udp)?::' . --include='*.rs' \
        | grep -vE '^\./http3/' || true)"
    if [ -n "$stray" ]; then
      echo "::error::quinn is named outside \`mod http3\`, which is the boundary the crate merge kept when the crate went away:"
      echo "$stray"
      exit 1
    fi
    n="$(grep -rcP '(?<!crate::)\bquinn(_proto|_udp)?::' http3/runtime.rs | head -1)"
    if [ "${n:-0}" -eq 0 ]; then
      echo "::error::src/http3/runtime.rs names quinn nowhere, so the check above is vacuous — it would pass a crate whose adapter had been emptied out"
      exit 1
    fi
    echo "quinn named in mod quinn and mod h3 only; $n sites in quinn.rs"

# Still `tree-guard`, and deliberately: `cargo deny` bans crates by name, and
# this one bans a FAMILY by prefix. Enumerating today's names would pass for
# tomorrow's `futures-whatever`, which is the regression the check exists
# for. docs/ci.md says so.

# hclient-proto pulls in no async runtime, on any target
graph-proto-sans-io:
    #!/usr/bin/env bash
    set -euo pipefail
    for t in "" "--target wasm32-unknown-unknown"; do
      ./scripts/tree-guard.sh absent '^(tokio|futures-|async-|smol|compio)' \
        "hclient-proto picked up an async dependency ${t:-on the host} — this crate must stay sans-io on every target, not just the host" \
        -- -p hclient-proto $t
    done

# The `cookies` feature is off by default because the compiled-in public
# suffix list is +77 KiB — a claim nothing checked until here. Exact
# analogue of the idna/ICU guard below.
#
# `public-suffix` alone now, where this named `hclient-cookie` too: the jar
# is a module of `hclient` rather than a crate, so there is no crate name
# left to look for and the dependency is the whole of what the feature
# costs. That is a weaker check than it was and the weakening is real —
# a jar compiled into a default build would no longer show up here, only
# its list would.

# the default hclient build carries no public suffix list
graph-no-cookie-jar:
    ./scripts/tree-guard.sh absent '^public-suffix ' \
        "hclient's default build pulled in the public suffix list — the cookies feature is off by default precisely so this does not happen" \
        -- -p hclient

# `url` is gone from the graph either way: hclient-proto writes out RFC 3986
# §5.2 in src/uri.rs precisely so that it does not depend on `url`, which
# belongs in [dev-dependencies] where it is the oracle for
# tests/uri_resolution.rs.

# url is absent from hclient-proto, with the idn feature and without
graph-no-url:
    #!/usr/bin/env bash
    set -euo pipefail
    for flags in "" "--no-default-features"; do
      ./scripts/tree-guard.sh absent '^url ' \
        "hclient-proto ${flags:-with default features} depends on url again — RFC 3986 §5.2 is written out in src/uri.rs precisely so that it does not; url belongs in [dev-dependencies], where it is the oracle for tests/uri_resolution.rs" \
        -- -p hclient-proto $flags
    done

# What `hclient-proxy` costs, asserted instead of written down. Three
# claims, and the middle one is the one that changed the design: reading
# the machine's settings costs **nothing on Linux** (the environment needs
# no crate), four small Microsoft crates on Windows and six on macOS —
# where the crate that was taken first, `proxy_cfg`, cost 28 including
# `url`, `idna` and the ICU tables on exactly the two targets
# `hclient-idn` exists to keep those off.
#
# Measured while writing: 19 crates with no features, 19 with `system` on
# Linux, 23 on Windows, 25 on macOS. The counts are deliberately NOT
# pinned here; what is pinned is the absence, for the reason the
# crate-count table in CLAUDE.md gives: a check that fails for an upstream
# release which broke nothing is a check people silence.

# the proxy protocols cost no third-party crate; only the OS and PAC do
graph-proxy-cost:
    #!/usr/bin/env bash
    set -euo pipefail
    for flags in "" "--features system"; do
      ./scripts/tree-guard.sh absent '^(hyper|tokio) ' \
        "hclient-proxy \${flags:-with no features} pulls in an HTTP client or a runtime — the whole point of the sans-io handshakes is that driving them is the transport's job" \
        -- -p hclient-proxy $flags
    done
    # The ICU tables, on every target rather than only this one: the
    # reader is written here precisely so that reading a registry key does
    # not cost a Unicode table.
    for target in "" "--target x86_64-pc-windows-msvc" "--target aarch64-apple-darwin"; do
      ./scripts/tree-guard.sh absent '^(url|idna|idna_adapter|icu_)' \
        "hclient-proxy with 'system' \${target:-on this host} pulls in url or the ICU tables — reading the machine's proxy settings must not cost a Unicode table, which is what taking a crate for it did cost" \
        -- -p hclient-proxy --features system $target
    done
    # A JavaScript engine, which is what running a PAC script would take
    # and what this crate deliberately does not carry — see the withdrawn
    # feature in CLAUDE.md. Named rather than left implicit because the
    # measurement that withdrew it is the argument, and an engine
    # arriving quietly is how the argument gets lost.
    ./scripts/tree-guard.sh absent '^(boa|viperjs|nova_vm|quickjs|rquickjs)' \
      "hclient-proxy has picked up a JavaScript engine — running a PAC script was measured at 114 crates and +3.4 MB and withdrawn for want of a consumer, so an engine arriving here needs that argument answered rather than repeated" \
      -- -p hclient-proxy --all-features
    ./scripts/tree-guard.sh absent '^(url|idna|idna_adapter|icu_)' \
      "the 'proxy' feature of hclient-native now costs url or the ICU tables — it never has, and a transport that speaks to a proxy must not have to carry a Unicode table to do it" \
      -- -p hclient-native --features proxy
    # And the other direction, which no `absent` check can see: each
    # reader must actually arrive when it is asked for, or every check
    # above would pass over a feature that does nothing.
    ./scripts/tree-guard.sh present '^windows-registry ' \
      "hclient-proxy with 'system' has no registry reader on Windows" \
      -- -p hclient-proxy --features system --target x86_64-pc-windows-msvc
    ./scripts/tree-guard.sh present '^system-configuration ' \
      "hclient-proxy with 'system' has no dynamic-store reader on macOS" \
      -- -p hclient-proxy --features system --target aarch64-apple-darwin
    # And the backend that reads them for its own capability report.
    # `URLSession` applies the machine's proxy configuration itself, so
    # `Capabilities::proxy` there is a read of the dynamic store rather
    # than a constant — and a build with no reader in it could only
    # report `false`, which is the capability that lies this closed.
    ./scripts/tree-guard.sh present '^system-configuration ' \
      "hclient-urlsession has no dynamic-store reader on macOS — its Capabilities::proxy would be a hardcoded false while the OS proxies underneath it" \
      -- -p hclient-urlsession --target aarch64-apple-darwin
    ./scripts/tree-guard.sh absent '^(url|idna|idna_adapter|icu_)' \
      "hclient-urlsession pulls in url or the ICU tables — it takes hclient-proxy for one bool, and the whole reason that reader is written here rather than taken from a crate is that reading a proxy setting must not cost a Unicode table" \
      -- -p hclient-urlsession --target aarch64-apple-darwin

# `system-proxy` is in `hclient`'s `default`, and the only thing that makes
# that affordable is the question mark in `hclient-native?/system-proxy`:
# the weak form enables the feature where the native transport is already
# in the graph and pulls nothing where it is not. Drop the `?` and every
# build that takes this crate's defaults acquires tokio, rustls and the
# system resolver — the floor `default-transport` was reversed out of
# `default` for, arriving by the back door.
#
# Measured while writing: 51 crates for a default `hclient`, none of them
# from the native stack.

# the default feature set pulls no transport
graph-default-has-no-transport:
    #!/usr/bin/env bash
    set -euo pipefail
    ./scripts/tree-guard.sh absent '^(hclient-native|tokio|hyper|rustls|quinn) ' \
      "a default build of hclient pulls in the native stack — almost certainly the '?' lost from 'hclient-native?/system-proxy' in [features], which turns a weak dependency feature into one that drags the whole transport in" \
      -- -p hclient
    # The other direction, which no `absent` check can see: with a
    # transport in the graph the feature must actually reach it, or the
    # default would be a default that does nothing.
    ./scripts/tree-guard.sh present '^hclient-proxy ' \
      "hclient with 'default-transport' has no proxy crate, so 'system-proxy' reached nothing" \
      -- -p hclient --features default-transport

# icu_properties_data alone is 1.9 MB of vendored source; that is the entire
# measurable benefit of the feature. The usual cause of a failure is some
# crate depending on hclient-proto WITH default features, unioning idn back
# on — see [workspace.dependencies].

# `form_urlencoded` and `percent-encoding` were named here too, as proxies
# for url — taking either used to mean taking url with them. They are direct
# dependencies now (encode.rs and uri.rs), and they are not proxies: both are
# leaves with no Unicode tables, which is why the claim they were standing in
# for had to be measured rather than inherited. What this recipe guards is
# url itself; the 1.9 MB of ICU is `graph-idn-feature`'s, below, and it names
# `idna|idna_adapter|icu_` directly rather than through anybody.

# without idn there is no IDN implementation, with it there is
graph-idn-feature:
    #!/usr/bin/env bash
    set -euo pipefail
    ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
      "--no-default-features still pulls in idna/ICU" \
      -- -p hclient-proto --no-default-features
    ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
      "hclient --no-default-features still pulls in idna/ICU — the facade's idn feature is not forwarding, or something else in its graph turns it on" \
      -- -p hclient --no-default-features
    # Without this the two checks above would also pass if idn had simply
    # been deleted. Since `idn` stopped naming `idna` and started naming
    # `hclient-idn` (which picks its backend by target), the implementation
    # being in the graph is the target-independent claim.
    ./scripts/tree-guard.sh present '^hclient-idn ' \
      "the default build of hclient-proto has no hclient-idn — the idn feature is in [features] default, so either it was removed or the dependency is no longer reached" \
      -- -p hclient-proto
    # What it COSTS on this runner, and deliberately not target-independent:
    # measured, `--target x86_64-pc-windows-msvc` brings `windows-sys` and no
    # `idna`; `aarch64-apple-darwin` brings `idna` like Linux does.
    # This is the line that must MOVE rather than be widened if this ever
    # runs anywhere but Linux.
    ./scripts/tree-guard.sh present '^idna ' \
      "the default build of hclient-proto on Linux has no idna — hclient-idn takes the bundled tables on every target that is not Windows or Android, so these are the tables the feature is supposed to bring here" \
      -- -p hclient-proto
    # And that the feature is the ONLY thing bringing it, stated
    # target-independently: the idna|icu_ absence above passes trivially on
    # Windows and Android, where those never appear.
    ./scripts/tree-guard.sh absent '^hclient-idn ' \
      "--no-default-features still pulls in hclient-idn — the idn feature is the only thing that should" \
      -- -p hclient-proto --no-default-features
    ./scripts/tree-guard.sh absent '^hclient-idn ' \
      "hclient --no-default-features still pulls in hclient-idn — the facade's idn feature is not forwarding" \
      -- -p hclient --no-default-features

# The saving the crate exists for, on the two targets that get it. Its size
# in bytes is unverified on both — no Windows or Apple linker produced this
# crate — but its absence from the graph is not, and that is what this
# checks. The Linux half is the opposite invariant, and it is not decoration:
# it is the half that breaks silently if the target predicate in build.rs and
# the ones in Cargo.toml ever drift apart. `libloading` stays named even
# though nothing references it now — the ELF dlopen backend was removed on
# purpose, and its name is the tripwire.

# hclient-idn's Unicode tables come from the platform, or from idna — by target
graph-idn-backend:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "{{os()}}" = "linux" ]; then
      ./scripts/tree-guard.sh present '^idna ' \
        "hclient-idn on Linux has no idna, so this target has no IDN implementation at all" \
        -- -p hclient-idn
      ./scripts/tree-guard.sh absent '^(libloading|windows-sys|objc2) ' \
        "hclient-idn on Linux pulls in a loader or a platform binding — the ELF dlopen backend was removed on purpose, see the crate docs" \
        -- -p hclient-idn --all-features
      # Android is checked from here because no runner in this project is
      # one, and the graph is a fact about the manifest rather than about
      # the device: without the feature the tables stay off and the JVM
      # binding is what arrives, with it the tables come back. That pair
      # is the whole meaning of the feature, on the target where it costs
      # the most.
      ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
        "the default hclient-idn build for Android pulls in idna/ICU — android.icu.text.IDNA is the backend there, and the tables are what this crate keeps off a phone" \
        -- -p hclient-idn --target aarch64-linux-android
      ./scripts/tree-guard.sh present '^jni ' \
        "hclient-idn for Android links no jni, so android.icu.text.IDNA is unreachable and the backend is not compiled in at all" \
        -- -p hclient-idn --target aarch64-linux-android
      ./scripts/tree-guard.sh present '^idna ' \
        "--features idna does not bring the bundled tables to Android, so the forcing switch does not force" \
        -- -p hclient-idn --target aarch64-linux-android --features idna
      # **Apple is checked from here for the reason Android is, and what
      # it asserts is the opposite of what it used to.** Foundation was a
      # backend until it was measured against the corpus: `NSURL` converts
      # an IDN host as a side effect of parsing a URL, so it does not
      # case-fold ASCII and does not validate an ACE label. Apple takes
      # the bundled tables now, like Linux and wasm, and the pair below is
      # that decision rather than a description of it — a reappearing
      # `objc2-foundation` means the backend came back without the corpus
      # being consulted.
      ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
        "the default hclient-idn build for Apple pulls in idna/ICU — Foundation plus src/ace.rs is the backend there, and the tables are what this crate keeps off it" \
        -- -p hclient-idn --target aarch64-apple-darwin
      ./scripts/tree-guard.sh present '^objc2-foundation ' \
        "hclient-idn for Apple links no objc2-foundation, so Foundation is unreachable and the backend is not compiled in at all" \
        -- -p hclient-idn --target aarch64-apple-darwin
      ./scripts/tree-guard.sh present '^idna ' \
        "--features idna does not bring the bundled tables to Apple, so the forcing switch does not force" \
        -- -p hclient-idn --target aarch64-apple-darwin --features idna
      # iOS is the same predicate — `target_vendor = "apple"` — and is
      # named anyway, because a predicate is not a check. It is the target
      # where the tables cost most, so an Apple build that quietly stopped
      # resolving to the backend would be the version of this mistake
      # nobody would look for.
      ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
        "the default hclient-idn build for iOS pulls in idna/ICU — it shares the Apple predicate, and iOS is where a wrongly-resolved target costs most" \
        -- -p hclient-idn --target aarch64-apple-ios
      # **The browser, where the tables cost the most.** A wasm module has
      # almost nothing else in it, so the ICU data is most of the
      # download: measured through the full `wasm-pack` pipeline, 20.7 KiB
      # against 143.0 KiB. `wasm32-wasip2` is deliberately not here — it
      # has no URL parser and keeps the tables, which says the
      # predicate is about the browser rather than about wasm.
      ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
        "the default hclient-idn build for the browser pulls in idna/ICU — the URL parser is the backend there, and the tables are most of a wasm module" \
        -- -p hclient-idn --target wasm32-unknown-unknown
      ./scripts/tree-guard.sh present '^wasm-bindgen ' \
        "hclient-idn for the browser links no wasm-bindgen, so the URL parser is unreachable and the backend is not compiled in at all" \
        -- -p hclient-idn --target wasm32-unknown-unknown
      # **And `web-sys` is refused rather than merely unused.** It has
      # `Url` ready made and would pull `js-sys`, whose optional
      # `futures-core-03-stream` feature anything streaming from JS
      # switches on — which took `hclient-proto`'s sans-io property away
      # on this target the first time this backend was written.
      # `graph-proto-sans-io` catches that from the other end; this
      # catches it here, where the dependency would be added.
      ./scripts/tree-guard.sh absent '^(web-sys|js-sys) ' \
        "hclient-idn for the browser pulls web-sys or js-sys — those cost hclient-proto its sans-io property through js-sys' futures feature, and a constructor and a getter are ten lines" \
        -- -p hclient-idn --target wasm32-unknown-unknown --all-features
      ./scripts/tree-guard.sh present '^idna ' \
        "hclient-idn for wasip2 has no idna — WASI has no URL to ask, so it keeps the bundled tables" \
        -- -p hclient-idn --target wasm32-wasip2
    else
      ./scripts/tree-guard.sh absent '^(idna|idna_adapter|icu_)' \
        "the default hclient-idn build on this target pulls in idna/ICU — the whole point of a platform backend is that the OS supplies the tables" \
        -- -p hclient-idn
      # **Named per OS rather than as a union**, because a union passes
      # for a runner that linked the other platform's binding. This branch
      # runs on whichever non-Linux runner the matrix is on, and each has
      # exactly one right answer.
      if [ "{{os()}}" = "macos" ]; then
        ./scripts/tree-guard.sh present '^objc2-foundation ' \
          "hclient-idn on macOS links no objc2-foundation, so no platform backend is compiled in at all" \
          -- -p hclient-idn
      else
        ./scripts/tree-guard.sh present '^windows-sys ' \
          "hclient-idn on this target links no windows-sys, so no platform backend is compiled in at all" \
          -- -p hclient-idn
      fi
      # **The opposite of what this line used to say**, and the change is
      # the feature's meaning rather than a relaxation. `--all-features`
      # is `--features idna`, which exists precisely to force the bundled
      # tables everywhere; asserting their absence under it would be
      # asserting that the one switch this crate has does nothing.
      ./scripts/tree-guard.sh present '^idna ' \
        "--features idna does not bring the bundled tables to a target with a platform backend, so the forcing switch does not force" \
        -- -p hclient-idn --all-features
    fi

# `cargo hack --each-feature` enumerates combinations instead of listing
# them. Two crates are excluded, and not for convenience: their features are
# not independent. `hclient-idn` needs at least one backend and says so with
# a `compile_error!`; `hclient-rt-embassy` forwards to `smoltcp`, which
# demands at least one protocol and one medium the same way. Isolating one
# feature of either is a build the crate deliberately refuses, so those two
# keep the targeted checks they already have.

# every feature combination compiles (58 of them)
# every crate built the way a reader would build it: on its own
check-each-crate:
    #!/usr/bin/env bash
    set -uo pipefail
    # `just features` cannot see what this sees, and the reason is
    # structural rather than a matter of coverage: it passes
    # `--no-dev-deps`, so a dev-dependency missing a feature is invisible
    # to it, and it never builds test targets. This builds each member
    # ALONE with its own dev-dependencies and all of its targets, which is
    # what `cargo check -p <crate>` does for whoever downloads the crate.
    #
    # The defect it exists for: `hclient-native`'s h3 two-runtime test
    # needed `hclient-rt-smol/udp` and its own manifest did not ask for
    # it. The file compiled under `--workspace` because another member
    # turned the feature on and Cargo unifies features across a graph, so
    # the workspace run was green over a crate that did not build on its
    # own. Same shape as the two doctest examples in `CLAUDE.md`, and as
    # the three backends that owed `SendTransport` — the third time this
    # workspace has been green over something a reader could not build.
    #
    # Excluded, each because its code is for a target this host is not:
    # the two wasm backends, Apple, Windows, and embassy. `just
    # check-targets` is what covers those, and it covers them by naming
    # the target rather than by skipping the crate.
    skip="hclient-wasi hclient-fetch hclient-urlsession hclient-winhttp hclient-rt-embassy"
    # No `set -e`: one crate failing must not hide the next, which is the
    # same argument `check-targets` makes one recipe over.
    failed=()
    n=0
    for c in $(cargo metadata --no-deps --format-version 1 \
                 | python3 -c "import json,sys;print(' '.join(p['name'] for p in json.load(sys.stdin)['packages']))"); do
      case " $skip " in *" $c "*) continue;; esac
      n=$((n+1))
      cargo check -p "$c" --all-features --all-targets --color never || failed+=("$c")
    done
    # Fails closed on the loop not running at all: a `cargo metadata` that
    # returns nothing would otherwise report success over zero crates,
    # which is this repository's recurring defect rather than a new one.
    if [ "$n" -lt 20 ]; then
      echo "::error::checked only $n crates — the member list did not resolve; a green run over nothing is the defect this recipe exists for"
      exit 1
    fi
    if [ ${#failed[@]} -ne 0 ]; then
      echo "::error::${#failed[@]} of $n crates do not build on their own:"
      for f in "${failed[@]}"; do echo "  cargo check -p $f --all-features --all-targets"; done
      exit 1
    fi
    echo "each-crate check: $n crates build standalone"

features:
    #!/usr/bin/env bash
    set -euo pipefail
    # `hclient-idn` is back in, and the exclusion's reason is gone rather
    # than waived: it needed at least one backend and said so with a
    # `compile_error!`, so isolating one of its four features was a build
    # it deliberately refused. It has one feature now — `idna`, a forcing
    # switch — and every setting of it compiles on every target.
    cargo hack --workspace \
        --exclude hclient-rt-embassy \
        --each-feature --no-dev-deps check
    # And the POWERSET over the four response codings, which
    # `--each-feature` does not reach: it builds each feature on its own,
    # where `decompress.rs`'s `#[cfg]` shape is about the COMBINATIONS —
    # the wildcard arm that catches "whichever codings this build has no
    # decoder for" is `not(all(..))` of four, and the exhaustive arm is
    # `not(any(..))` of four, so exactly one of the sixteen sets is the
    # boundary for each. Two of sixteen were checked before this line, and
    # `cargo hack --each-feature` would have gone on saying `ok`.
    cargo hack -p hclient --feature-powerset \
        --include-features gzip,brotli,deflate,zstd \
        --no-dev-deps check

# every dependency-graph claim, together
graph: supply-chain tree-ambient graph-no-quic graph-udp-pulls-quic graph-no-framing-in-the-transport quinn-stays-in-its-module graph-smol-path features graph-no-cookie-jar graph-proto-sans-io graph-no-url graph-proxy-cost graph-default-has-no-transport graph-idn-feature graph-idn-backend

# ── the whole pipeline ──────────────────────────────────────────────────

# everything CI runs except what is bound to one OS (`macos-loopback`)
ci: fmt-check lint invariants graph test-workspace test-doc test-sse-complexity test-no-default test-idn lint-idn test-embassy-live embassy-strict-link build-three-targets build-wasi-example test-wasi test-browsers fetch-under-wasm-threads test-autobahn fuzz-smoke
