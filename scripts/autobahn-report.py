#!/usr/bin/env python3
"""Read the Autobahn TestSuite's own verdict and turn it into an exit code.

`wstest --mode fuzzingserver` writes `index.json` when the driver asks for
`/updateReports`. This script is the only thing that reads it, and the
whole of its job is to **fail closed**: a report it cannot find, cannot
parse, or that holds fewer cases than the suite offers is a failure, not a
pass. That is not hypothetical — this repository has twice shipped a green
check that ran nothing, which is why `test-workspace` greps for nextest's
Summary line and why `test-browser` asserts a minimum test count.

# The vocabulary, and why a pass is narrower than it looks

`behavior` is the case's verdict and `behaviorClose` is separately whether
the closing handshake went the way the case required. Both must be good
for a case to pass, because a client that echoes correctly and then leaves
the connection hanging has failed at something the suite is specifically
watching.

Three of the suite's verdicts are **not** silently folded into "pass":

  NON-STRICT    the client was acceptable but not strict. Real behaviour,
                and a regression from OK to NON-STRICT would be invisible
                if it counted as a pass — so each one must be named in
                NON_STRICT with the property it stands for.
  UNIMPLEMENTED the case did not run, because the client does not have the
                feature it tests. Honest, and not a pass: it must be named
                in EXPECTED.
  INFORMATIONAL the *case* declares itself undefined by the spec (7.1.6,
                7.13.x). That is a fact about the case, not about this
                client, so it is counted and reported and needs no entry.

# The four ways this fails

  1. no report, unparseable JSON, or no entry for the agent
  2. fewer cases than MINIMUM_CASES — a run that stopped early
  3. a case failed, or was UNIMPLEMENTED, or was NON-STRICT, without an
     entry saying so
  4. a case with an entry now passes — a stale expectation is a lie in the
     other direction, and now is the only moment anyone would notice

Every one of the four is exercised by `just autobahn-parser-selftest`
against the fixtures in `scripts/autobahn/selftest/`, so "this parser can
tell 517 passes from 0 cases run" is a thing that is checked rather than
claimed.

Usage: autobahn-report.py <report-dir> <agent>
"""

import json
import os
import sys

# The suite's own verdict names (`autobahn/testsuite/case/__init__.py`).
GOOD = {"OK", "INFORMATIONAL"}

# How many cases the report must contain. `wstest` 25.10.1 offers 517 and
# ran 517 here. A floor rather than an equality: a newer suite that adds
# cases should go red about the new cases, not about the count. A run that
# scored 3 is what this number exists for, and lowering it to make a short
# run pass is the one edit that must not happen.
MINIMUM_CASES = 517

# Cases allowed not to pass, each with the reason. A key ending in `.`
# matches a whole section.
#
# Nothing goes here because it is inconvenient. Each entry is a claim that
# the suite is testing something this client deliberately does not do, and
# the claim is checked in both directions — an entry whose case starts
# passing fails this script.
EXPECTED = {
    "12.": "permessage-deflate is not implemented. `docs/w4-upgrade-seam.md` "
    "leaves it open and the WebSocket seam's own module doc records it as "
    "absent; `hclient-tungstenite` never offers the extension in its handshake, so "
    "the suite marks every compressed case UNIMPLEMENTED rather than running "
    "it. 90 cases.",
    "13.": "permessage-deflate again: 13.x is 12.x re-run across the parameter "
    "space (window bits, no-context-takeover). Same reason, 126 cases.",
}

# Cases the suite scores NON-STRICT, each with the property that makes them
# so. Same two-way check: one that becomes OK fails this script, because
# the entry would then be describing behaviour the client no longer has.
NON_STRICT = {
    "6.4.3": "UTF-8 is validated when a frame is complete, not as its payload "
    "arrives. 6.4.1 and 6.4.2 split the bad message across *frames* and are "
    "strict OK; 6.4.3 and 6.4.4 split it into chops of one frame, and "
    "`tungstenite` decodes a frame only once it has all of it. The suite's "
    "own expectation names this outcome as acceptable ('If we timeout, we "
    "expect the connection is failed at least then').",
    "6.4.4": "Same as 6.4.3 — the two differ only in where the chop boundary "
    "falls.",
}


def reason_for(case_id, table):
    """The entry in `table` covering `case_id`, or None."""
    for key, why in table.items():
        if case_id == key or (key.endswith(".") and case_id.startswith(key)):
            return why
    return None


def die(msg):
    print(f"::error::{msg}", file=sys.stderr)
    sys.exit(1)


def load(report_dir, agent):
    index = os.path.join(report_dir, "index.json")
    if not os.path.isfile(index):
        die(
            f"{index} does not exist — the fuzzingserver never wrote a report, so "
            "there is no verdict to read. A missing report is a failed run, not a "
            "quiet one."
        )
    try:
        with open(index, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError) as e:
        die(f"{index} could not be read as JSON ({e}) — a report nobody can parse is not a pass")
    if not isinstance(data, dict):
        die(f"{index} is {type(data).__name__}, not an object keyed by agent")
    if agent not in data:
        die(
            f"{index} has no results for agent {agent!r} — it has {sorted(data)!r}. "
            "The driver did not reach the suite under the name this check reads, so "
            "nothing here was scored."
        )
    cases = data[agent]
    if not isinstance(cases, dict):
        die(f"{index}'s entry for {agent!r} is {type(cases).__name__}, not an object of cases")
    return cases


def sort_key(case_id):
    return [int(p) if p.isdigit() else 0 for p in case_id.split(".")]


def main():
    if len(sys.argv) != 3:
        die("usage: autobahn-report.py <report-dir> <agent>")
    _, report_dir, agent = sys.argv
    cases = load(report_dir, agent)

    if len(cases) < MINIMUM_CASES:
        die(
            f"the report holds {len(cases)} cases, fewer than the {MINIMUM_CASES} this "
            "check requires. A run that stopped early scores only what it reached, and "
            "a green tick over three cases is exactly the failure this fails closed "
            "against. Do not lower MINIMUM_CASES to make a short run pass."
        )

    sections = {}
    undeclared = []
    stale = []
    for case_id in sorted(cases, key=sort_key):
        r = cases[case_id]
        if not isinstance(r, dict) or "behavior" not in r or "behaviorClose" not in r:
            die(
                f"case {case_id} has no behavior/behaviorClose — this is not a report "
                "this script understands, and guessing at it would be worse than stopping"
            )
        behavior, close = r["behavior"], r["behaviorClose"]
        s = sections.setdefault(
            case_id.split(".", 1)[0],
            {"ok": 0, "informational": 0, "non-strict": 0, "unimplemented": 0, "fail": 0},
        )

        if behavior in GOOD and close in GOOD:
            s["informational" if behavior == "INFORMATIONAL" else "ok"] += 1
            for table, name in ((EXPECTED, "EXPECTED"), (NON_STRICT, "NON_STRICT")):
                if (why := reason_for(case_id, table)) is not None:
                    stale.append((case_id, behavior, close, name, why))
        elif behavior == "NON-STRICT" and close in GOOD:
            s["non-strict"] += 1
            if reason_for(case_id, NON_STRICT) is None:
                undeclared.append((case_id, behavior, close, "NON_STRICT"))
        elif behavior == "UNIMPLEMENTED":
            s["unimplemented"] += 1
            if reason_for(case_id, EXPECTED) is None:
                undeclared.append((case_id, behavior, close, "EXPECTED"))
        else:
            s["fail"] += 1
            if reason_for(case_id, EXPECTED) is None:
                undeclared.append((case_id, behavior, close, "EXPECTED"))

    cols = ["ok", "informational", "non-strict", "unimplemented", "fail"]
    print(f"Autobahn TestSuite, agent {agent}: {len(cases)} cases")
    header = "  ".join(f"{c:>13}" for c in cols)
    print(f"{'section':>8}  {header}")
    for name in sorted(sections, key=lambda n: int(n) if n.isdigit() else 0):
        row = "  ".join(f"{sections[name][c]:>13}" for c in cols)
        print(f"{name:>8}  {row}")
    totals = {c: sum(s[c] for s in sections.values()) for c in cols}
    print(f"{'total':>8}  " + "  ".join(f"{totals[c]:>13}" for c in cols))

    for case_id, behavior, close, table in undeclared:
        print(f"  UNDECLARED {case_id}: behavior={behavior} behaviorClose={close} (not in {table})")
    for case_id, behavior, close, table, why in stale:
        print(f"  STALE {case_id}: behavior={behavior} behaviorClose={close} — {table} says: {why}")

    if undeclared:
        die(
            f"{len(undeclared)} Autobahn cases did not pass and are not declared. Each is "
            "either a defect in crates/hclient-tungstenite/src/lib.rs or a decision that "
            "belongs in EXPECTED/NON_STRICT with its reason written out — not silenced."
        )
    if stale:
        die(
            f"{len(stale)} declared cases now pass. That is good news and a stale "
            "declaration at the same time: delete the entry, or this file goes on "
            "excusing something that no longer happens."
        )
    print(
        f"Autobahn: {totals['ok'] + totals['informational']} cases passed, "
        f"{totals['non-strict']} non-strict and {totals['unimplemented']} unimplemented, "
        "all declared; nothing failed."
    )


if __name__ == "__main__":
    main()
