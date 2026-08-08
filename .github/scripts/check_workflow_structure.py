import sys
import yaml
from pathlib import Path

ok = True
for path in sorted(Path(".github/workflows").glob("*.yml")):
    try:
        doc = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as e:
        print(f"::error file={path}::{path} is not valid YAML: {e}")
        ok = False
        continue
    print(f"{path}: valid YAML")

    # Valid YAML is not a valid workflow. Actions rejects the whole
    # FILE for a structural error — no job starts, no job log
    # exists, and the only thing said out loud is "this run likely
    # failed because of a workflow file issue". That is the worst
    # possible diagnostic, so the cheap structural rules are
    # checked here where a message can name the step.
    #
    # Measured, which is why this exists: an edit deleted a
    # `- shell: bash` line, merging a `uses:` step into the `run:`
    # step below it. PyYAML was perfectly happy — a mapping with
    # both keys is well-formed — and the entire workflow was
    # rejected on push.
    for job_name, job in (doc.get("jobs") or {}).items():
        for i, step in enumerate(job.get("steps") or []):
            if not isinstance(step, dict):
                print(f"::error file={path}::{job_name} step {i} is not a mapping")
                ok = False
                continue
            uses, run = "uses" in step, "run" in step
            where = f"{job_name} step {i} ({step.get('name') or step.get('uses') or 'unnamed'})"
            if uses and run:
                print(f"::error file={path}::{where} has BOTH `uses:` and `run:` — usually a deleted step separator merging two steps into one")
                ok = False
            elif not uses and not run:
                print(f"::error file={path}::{where} has neither `uses:` nor `run:`; keys present: {sorted(step)}")
                ok = False
sys.exit(0 if ok else 1)
