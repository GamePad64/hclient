#!/usr/bin/env python3
"""Every `run:` in ci.yml is a call to a recipe that exists in the justfile.

The justfile's header says its recipes mirror what CI runs. Until this script
existed nothing checked that, and it is exactly the kind of claim that goes
quietly false: a step edited in the workflow, a recipe left behind, and the
command people run before pushing stops being the command that gates the push.

The rule is narrow on purpose. A job may still carry a matrix, a toolchain, a
cache, an `env:`, an `if:` and a `working-directory:` — those are things a
workflow does and a task runner does not. What it may not carry is a decision:
a flag, a filterset, a grep over output, a threshold. Those live in the
justfile, where they can be run.

Two ways to fail, both fail closed:

  * a `run:` that is not a `just` call, and is not in EXCEPTIONS below;
  * a `just` call naming a recipe the justfile does not have.

And two ways the check itself refuses to be vacuous: finding no `run:` steps
at all is an error (a parse that silently matched nothing would pass forever),
and an EXCEPTIONS entry naming a step that no longer exists is an error too —
the same rule `unsafe-code-policy.sh` applies to its ALLOWED map, for the same
reason.

The shape is `crates/http-ng-wasi/tests/live_roundtrip.rs`'s
`the_job_that_installs_wasmtime_exports_the_marker_this_guard_keys_on`: a
check that reads the workflow file and asserts a symmetry, rather than a
comment asserting it in prose.
"""

import re
import subprocess
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
CI = ROOT / ".github" / "workflows" / "ci.yml"

# (job, step name or "step #N" when the step has no `name:`) -> why this run
# block is allowed not to be a `just` call. Keep it short, and keep the reason
# specific enough that it can be argued with. Empty is the intended state.
EXCEPTIONS: dict[tuple[str, str], str] = {}

# `just <recipe>` with optional arguments. `${{ … }}` is a workflow expression
# (a matrix leg, usually) and is substituted before the shell sees it, so it
# is allowed to stand where an argument does.
EXPR = re.compile(r"\$\{\{[^}]*\}\}")
CALL = re.compile(r"^just\s+([A-Za-z0-9][A-Za-z0-9_-]*)((?:\s+\S+)*)\s*$")


def recipes() -> set[str]:
    out = subprocess.run(
        ["just", "--summary", "--justfile", str(ROOT / "justfile")],
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    if out.returncode != 0:
        sys.exit(
            f"::error::`just --summary` failed, so this check cannot run and "
            f"must not pass:\n{out.stderr.strip()}"
        )
    names = set(out.stdout.split())
    if not names:
        sys.exit("::error::`just --summary` listed no recipes — the justfile is empty or unreadable")
    return names


def steps():
    """Yield (job, step label, run script) for every step that has a `run:`."""
    try:
        doc = yaml.safe_load(CI.read_text(encoding="utf-8"))
    except yaml.YAMLError as e:
        # A traceback here reads as broken infrastructure; it is a broken
        # workflow file, which is the more likely thing and the one actionlint
        # will also have something to say about.
        sys.exit(f"::error::{CI} is not valid YAML, so this check cannot run:\n{e}")
    jobs = doc.get("jobs") if isinstance(doc, dict) else None
    if not jobs:
        sys.exit(f"::error::{CI} has no `jobs:` — the workflow was renamed or this parse is broken")
    for job, body in jobs.items():
        for i, step in enumerate(body.get("steps", []), start=1):
            if not isinstance(step, dict) or "run" not in step:
                continue
            yield job, step.get("name") or f"step #{i}", step["run"]


def main() -> int:
    known = recipes()
    seen: set[tuple[str, str]] = set()
    problems: list[str] = []
    calls = 0

    for job, label, script in steps():
        seen.add((job, label))
        if (job, label) in EXCEPTIONS:
            continue
        lines = [
            ln.strip()
            for ln in script.splitlines()
            if ln.strip() and not ln.strip().startswith("#")
        ]
        if not lines:
            problems.append(f"{job} / {label}: an empty `run:` block")
            continue
        for ln in lines:
            m = CALL.match(EXPR.sub("EXPR", ln))
            if not m:
                problems.append(
                    f"{job} / {label}: `{ln}` is not a `just` call. Move what it "
                    f"decides into a recipe; the job keeps the matrix, the "
                    f"toolchain and the environment. If it genuinely cannot be a "
                    f"recipe, add it to EXCEPTIONS in {Path(__file__).name} with a reason."
                )
                continue
            calls += 1
            if m.group(1) not in known:
                problems.append(
                    f"{job} / {label}: calls `just {m.group(1)}`, which the "
                    f"justfile does not define"
                )

    for key, why in EXCEPTIONS.items():
        if key not in seen:
            problems.append(
                f"EXCEPTIONS names {key[0]} / {key[1]} ({why}), but ci.yml has no "
                f"such step — a renamed step must fail this check, not silently "
                f"keep its exemption"
            )

    if not seen:
        sys.exit(
            "::error::no `run:` steps found in ci.yml at all — this check parsed "
            "nothing and would pass forever"
        )

    if problems:
        for p in problems:
            print(f"::error::{p}")
        return 1

    print(
        f"ci.yml mirrors the justfile: {calls} `just` calls across "
        f"{len(seen)} run steps, {len(EXCEPTIONS)} exceptions"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
