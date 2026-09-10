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
set -euo pipefail

python3 - "$@" <<'PY'
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent if "__file__" in dir() else pathlib.Path(".")
ROOT = pathlib.Path(".").resolve()

ALLOW = re.compile(r"#\s*\[\s*allow\s*\(\s*clippy::")

def justified(lines, i):
    """Does the allow on line `i` (0-based) carry a reason?"""
    line = lines[i]

    # rustc's own `reason = ".."`, on this line or the lines the attribute
    # wraps onto — a multi-line `#[allow(.., reason = "..")]`.
    window = "\n".join(lines[i : i + 6])
    head = window.split("]", 1)[0] if "]" in window else window
    if "reason" in head and "=" in head:
        return True

    # A trailing comment on the attribute line itself.
    after = line.split("]", 1)[1] if "]" in line else ""
    if after.strip().startswith("//") and len(after.strip()) > 4:
        return True

    # A comment block above, walking back over any other attributes and
    # doc lines that sit between the comment and this allow.
    j = i - 1
    while j >= 0:
        s = lines[j].strip()
        if s.startswith("//"):
            return True
        if s.startswith("#[") or s.startswith("#!["):
            j -= 1
            continue
        return False
    return False

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
        seen += 1
        if not justified(lines, i):
            rel = path.relative_to(ROOT)
            bare.append(f"{rel}:{i + 1}: {line.strip()}")

# Fails closed: a run that examined nothing is not a clean run. The figure
# is a floor with room rather than today's count, because a number in a
# check goes stale the way a number in prose does.
if seen < 40:
    print(
        f"::error::every-allow-names-its-reason examined only {seen} allows "
        "— the scan did not run over this workspace",
        file=sys.stderr,
    )
    sys.exit(1)

if bare:
    print(
        f"::error::{len(bare)} `#[allow(clippy::..)]` with no reason beside it. "
        "Say what bounds the cast, why the lint is wrong about this code, or "
        "what the hand-written impl prints instead — a comment above the "
        "attribute, a trailing one after it, or rustc's own `reason = \"..\"`.",
        file=sys.stderr,
    )
    for b in bare:
        print(f"  {b}", file=sys.stderr)
    sys.exit(1)

print(f"every-allow-names-its-reason: {seen} clippy allows, all with a stated reason")
PY
