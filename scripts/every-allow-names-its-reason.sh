#!/usr/bin/env bash
# Every `#[allow(clippy::..)]` in this workspace says why.
#
# `clippy::pedantic` arrived with 1309 findings, of which 285 wanted a
# decision rather than a rewrite, and about a third of those decisions were
# *the lint is wrong about this code* — a wire-format cast bounded by a
# check a dozen lines up, a `Debug` that curates rather than dumps, a
# `#[cfg(not(feature))]` twin whose signature must match the half it stands
# in for. Each of those is a claim, and the rule this workspace settled on
# is that **an allow naming its bound is a claim somebody can check, where a
# bare one is a claim nobody can**.
#
# That rule was held by nothing but habit until this script. It is the shape
# this repository records against itself over and over: a rule everybody
# agrees with, stated in prose, enforced by no one — `test-doc` before a job
# called it, `test-no-default` while it exited zero over `error:`, the
# rendered docs before `just docs` existed. A reason is exactly as
# perishable as the check behind it.
#
# **Two spellings both count**, because both are in the tree and both are
# legible:
#
#     // `password_auth` refuses over 255 bytes at the setter.
#     #[allow(clippy::cast_possible_truncation)]
#
#     #[allow(clippy::missing_fields_in_debug)] // hand-written: `H` is not `Debug`
#
# and the second is the one to be careful with, for a reason this workspace
# has paid for twice: **`cargo fmt` moves a trailing comment off a line it
# reflows**, so a justification written there is one edit away from being
# lost. Both are accepted rather than only the first, because a rule that
# refuses a legible form invites the bare allow it exists to prevent.
#
# `#[allow(clippy::..)]` carrying rustc's own `reason = ".."` counts on its
# own — that is the same claim in the form the compiler understands.
#
# What is NOT checked is whether the reason is *true*. Nothing can check
# that. What this stops is the allow with nothing beside it at all, which is
# the one a reader cannot even argue with.
#
# Checked in the failing direction before it was believed: a bare allow
# added to `hclient-core` exits 1 naming its file and line, and a run that
# matches no files at all exits 1 rather than reporting an empty tree as
# clean — this file's own recurring defect.
#
# **`clippy::allow_attributes_without_reason` is on as well, and this
# script is not redundant beside it.** The lint demands rustc's own
# `reason = ".."` and nothing else, which is the form a machine can check
# and `cargo fmt` cannot carry away from its attribute; every allow in this
# workspace carries one now. What the lint cannot do is fail closed on
# having examined nothing, and it is silent for a crate CI forgot to
# compile — this workspace has met that shape four times, most recently
# where seven crates opted out of the workspace lint table and reported
# zero findings on three targets nobody built. So the two overlap on
# purpose: the lint is the per-site check, the script is the census.
#
# It counts every `allow`, not only clippy's — `dead_code`, `unused_mut`,
# `unreachable_code` — and the two shapes the attribute takes when a
# reason makes it long: an `#[allow(` whose arguments wrap onto their own
# lines, and an `allow` nested inside a `cfg_attr`. Both were invisible to
# the earlier pattern, which is how the figure went from 123 to 237
# without a single allow being added.
set -euo pipefail

python3 - "$@" <<'PY'
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent if "__file__" in dir() else pathlib.Path(".")
ROOT = pathlib.Path(".").resolve()

ALLOW = re.compile(r"#!?\s*\[\s*(?:cfg_attr\s*\([^)]*\)\s*,\s*)?allow\s*\(")

def justified(lines, i):
    """Does the allow starting on line `i` (0-based) carry rustc's own
    `reason = ".."`?

    One spelling only, now that every allow in the tree carries it. The
    two comment forms this script used to accept were what made the rule
    legible before the compiler could check it; `clippy::
    allow_attributes_without_reason` checks it per site now, so accepting a
    comment here would only re-open the gap — and the gap was real: a
    `///` doc comment on the *item* sits directly above its attributes and
    says nothing about the allow, so the old walk-backwards accepted a
    bare allow under any documented type. Checked by adding one.
    """
    # The attribute may wrap over several lines; read to its closing `]`.
    window, depth = [], 0
    for l in lines[i : i + 24]:
        window.append(l)
        depth += l.count("[") - l.count("]")
        if depth <= 0 and len(window) > 0:
            break
    text = "\n".join(window)
    return bool(re.search(r"\breason\s*=", text))

bare = []
seen = 0
for path in sorted(ROOT.glob("crates/**/*.rs")):
    if "/target/" in str(path):
        continue
    try:
        lines = path.read_text().split("\n")
    except (OSError, UnicodeDecodeError):
        continue
    for i, line in enumerate(lines):
        if not ALLOW.search(line):
            continue
        # A comment that *discusses* an allow is not one. Three files in
        # this workspace explain the unsafe-code policy by quoting the
        # attribute, and a gate that complains about prose is a gate that
        # gets silenced.
        if line.lstrip().startswith("//"):
            continue
        seen += 1
        if not justified(lines, i):
            rel = path.relative_to(ROOT)
            bare.append(f"{rel}:{i + 1}: {line.strip()}")

# Fails closed: a run that examined nothing is not a clean run. The figure
# is a floor with room rather than today's count, because a number in a
# check goes stale the way a number in prose does.
if seen < 150:
    print(
        f"::error::every-allow-names-its-reason examined only {seen} allows "
        "— the scan did not run over this workspace",
        file=sys.stderr,
    )
    sys.exit(1)

if bare:
    print(
        f"::error::{len(bare)} `#[allow(..)]` without `reason = \"..\"`. "
        "Say what bounds the cast, why the lint is wrong about this code, or "
        "what the hand-written impl prints instead — in rustc's own "
        "`reason = \"..\"`, which is the one spelling a machine can check "
        "and `cargo fmt` cannot carry away from its attribute.",
        file=sys.stderr,
    )
    for b in bare:
        print(f"  {b}", file=sys.stderr)
    sys.exit(1)

print(f"every-allow-names-its-reason: {seen} allows, all carrying `reason`")
PY
