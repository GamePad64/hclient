# Task runner for http-ng. `just` with no arguments lists the recipes.
#
# These mirror what CI runs, deliberately: a recipe that drifts from the job
# it stands for is worse than no recipe, because it is the one people trust
# before pushing. Where a recipe cannot be the same command — browser tests
# need a headless browser, the wasi suite needs wasmtime — it says so.

default:
    @just --list

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

# clippy over every target, warnings are errors
lint:
    cargo clippy --workspace --all-features --all-targets -- -D warnings

# fmt + clippy + the suite, cheapest first
check: && test
    cargo fmt --all --check
    cargo clippy --workspace --all-features --all-targets -- -D warnings

# ── the other two targets ───────────────────────────────────────────────

# the wasi suite under wasmtime (runner comes from .cargo/config.toml)
test-wasi:
    cargo nextest run -p http-ng-wasi --target wasm32-wasip2

# The `--features` below are wasm-pack's own arguments, not a `cargo test --`
# passthrough — `-- --features ...` is rejected outright.

# the browser suites: `just test-browser firefox`
test-browser BROWSER="chrome":
    wasm-pack test --headless --{{BROWSER}} crates/http-ng-fetch
    wasm-pack test --headless --{{BROWSER}} crates/http-ng --features default-transport,test-util

# the Transport acceptance: one source, no #[cfg], three targets
build-three-targets:
    cargo build -p http-ng --example portable
    cargo build -p http-ng --example portable --target wasm32-wasip2
    cargo build -p http-ng --example portable --target wasm32-unknown-unknown

# ── the paths --all-features cannot reach ───────────────────────────────

# `--all-features` turns `idn` ON, so every `#[cfg(not(feature = "idn"))]`
# test — the typed NonAsciiHost error, the divergence list — runs only here.
# The same is now true of `http-ng-cookie`'s `public-suffix`: with the
# feature on, the no-list branch of `BuiltinList` never executes, and
# `tests/without_the_list.rs` is the only thing that checks a no-list build
# is NARROWER than a list build rather than quietly wider.

# the feature-off build, which --all-features can never exercise
test-no-default:
    cargo nextest run -p http-ng-proto --no-default-features --no-fail-fast
    cargo nextest run -p http-ng --no-default-features --features test-util --no-fail-fast
    cargo nextest run -p http-ng-cookie --no-default-features --no-fail-fast

# Doctests: nextest cannot run them, so `just test` does not either. The
# workspace had none until `http-ng-cookie`; this is the recipe that runs
# the one it has, and CI has no equivalent step yet.

# the doctests, which nextest cannot run
test-doc:
    cargo test --workspace --all-features --doc

# nightly is not optional here (sanitizer flags), and `RUSTUP_TOOLCHAIN`
# is how it survives rust-toolchain.toml. The explicit `--target` is not
# cosmetic either: cargo-fuzz defaults to the triple it was itself built
# for, which is musl when it arrives as a prebuilt binary.

# fuzz one target for N seconds: `just fuzz sse_accounting 30`
fuzz TARGET="sse" SECONDS="60":
    cd crates/http-ng-proto/fuzz && \
      RUSTUP_TOOLCHAIN=nightly cargo fuzz run \
        --target "$(rustc -vV | sed -n 's/^host: //p')" \
        {{TARGET}} -- -max_total_time={{SECONDS}}

# ── dependency facts this project makes claims about ────────────────────

# The same script the `dependency-graph` job runs, not a second copy of it:
# this recipe existed to mirror CI, and a hand-rolled twin is exactly the
# drift the header warns about.

# the browser and wasi graphs must contain no tokio, hyper or h2 at all
tree-ambient:
    cargo deny --manifest-path crates/http-ng-fetch/Cargo.toml \
        --config .github/deny/ambient.toml -t wasm32-unknown-unknown check bans
    cargo deny --manifest-path crates/http-ng-wasi/Cargo.toml \
        --config .github/deny/ambient.toml -t wasm32-wasip2 check bans

# advisories, licences and sources for the shipped graph
supply-chain:
    cargo deny --all-features check
