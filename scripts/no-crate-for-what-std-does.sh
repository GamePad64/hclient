#!/usr/bin/env bash
# No third-party crate for a job the standard library now does itself.
#
# Two crates are on the list and both were the right answer when they were
# taken. `cfg-if` gave `sys/mod.rs` an ordered `if`/`else if`/`else` over
# targets, so each arm stated only its own platforms instead of the target
# list being written six times, five of them negated. `assert_matches`
# gave 98 call sites a pattern assertion that prints the value it did not
# match. Neither is bloated, unmaintained or badly made, and neither is
# banned for anything it did.
#
# What changed is underneath them: `core::cfg_select!` is stable in 1.95
# and `std::assert_matches!` in 1.96, so each dependency now buys a name
# that `core` already exports. That is the whole rule — **not** a list of
# crates somebody dislikes, and the table below carries the replacement
# and the release beside each entry so that a reader who meets the
# refusal is told what to write instead of being told no.
#
# # Why this is not `cargo deny`, which was tried before it was rejected
#
# `cargo deny` is already wired into `just supply-chain`, and its `[bans]`
# table has a `use-instead` field that says exactly what this rule wants
# to say. Both entries were written into `deny.toml` and run. The result
# separates the two crates, and the separation is the argument:
#
#   - `assert_matches` is **not in the resolved graph at all** — `cargo
#     tree --invert` answers *did not match any packages* — so the ban is
#     silent and means precisely "nobody here may declare it";
#   - `cfg-if` fails immediately, `error[banned]: crate 'cfg-if = 1.0.4'
#     is explicitly banned`, because it arrives through **nineteen**
#     third-party parents: `ring`, `sha2`, `js-sys`, `openssl`,
#     `encoding_rs`, `getrandom`, `chacha20`, `parking_lot_core` and the
#     rest. Not one of them is ours to change.
#
# The `wrappers` escape hatch would take the second case, at the price of
# a list of somebody else's parents that goes stale on their next release.
# But the reason to reject the tool is not that one crate is awkward: it
# is that `[bans]` reads what a build **resolves**, and this rule is about
# what this workspace **declares**. The quiet entry is quiet by luck. The
# day any dependency takes up `assert_matches`, the identical entry starts
# failing for something nobody here did — which is `cfg-if` today, arriving
# early. A check that cries wolf is silenced, and this repository treats
# that as the mirror of a check that cannot fail.
#
# So the instrument is the manifests. `tomllib` rather than `grep`,
# because `system-resolver`'s own manifest discusses `cfg-if` in prose for
# a paragraph and a grep cannot tell an argument from a dependency.
#
# Every dependency table is read — `dependencies`, `dev-dependencies`,
# `build-dependencies`, their `[target.<cfg>]` forms, and the root's
# `[workspace.dependencies]`, which is where a re-entry would most likely
# be staged. A renamed dependency is caught by its `package` key rather
# than by the name it was given.
set -uo pipefail

python3 - "$(git rev-parse --show-toplevel)" <<'PY'
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])

# name -> (what to write instead, the release that made it possible)
FORBIDDEN = {
    "cfg-if": ("core::cfg_select!", "1.95"),
    "assert_matches": ("std::assert_matches!", "1.96"),
}

DEP_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")


def declared(table):
    """Every package name a dependency table names, renames included."""
    for given, spec in (table or {}).items():
        if isinstance(spec, dict) and "package" in spec:
            yield spec["package"], given
        else:
            yield given, given


def tables(manifest):
    for name in DEP_TABLES:
        yield manifest.get(name)
    for cfg in (manifest.get("target") or {}).values():
        for name in DEP_TABLES:
            yield cfg.get(name)
    yield (manifest.get("workspace") or {}).get("dependencies")


manifests = sorted(
    [root / "Cargo.toml"] + list((root / "crates").glob("*/Cargo.toml"))
)

bad = 0
checked = 0
for path in manifests:
    if not path.is_file():
        continue
    checked += 1
    manifest = tomllib.loads(path.read_text())
    for table in tables(manifest):
        for package, given in declared(table):
            if package not in FORBIDDEN:
                continue
            bad = 1
            replacement, since = FORBIDDEN[package]
            named = package if package == given else f"{given} (package = {package})"
            print(
                "::error::%s depends on %s — write %s instead, stable since %s"
                % (path.relative_to(root), named, replacement, since)
            )

# Fail closed on the search itself. A glob that matched nothing and a tidy
# workspace print the same thing otherwise, which is this repository's own
# recurring defect.
if checked < 20:
    print(
        "::error::only %d manifests read — the check looked in the wrong place "
        "rather than finding a tidy workspace" % checked
    )
    sys.exit(1)

if not bad:
    print(
        "no-crate-for-what-std-does: %d manifests, none declaring %s"
        % (checked, ", ".join(sorted(FORBIDDEN)))
    )
sys.exit(bad)
PY
