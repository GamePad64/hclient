#!/usr/bin/env python3
"""Read the Autobahn TestSuite's own verdict and decide whether this build passes.

The suite writes `index.json` from `/updateReports`. This script is what
turns that into an exit code, and the whole of its job is to **fail
closed**: a report it cannot find, cannot parse, or that contains no cases
is a failure, not a pass. That is not a hypothetical — this repository has
twice shipped a green check that ran nothing, which is why
`test-workspace` greps for nextest's Summary line and why the browser
recipe asserts a minimum test count.

So there are four independent ways to fail here, and each one is exercised
by `just test-autobahn-parser-selftest` rather than argued for:

  1. no report, or one that is not JSON, or one with no entry for the agent
  2. fewer cases than MINIMUM_CASES — a run that stopped early
  3. a case failed that is not in EXPECTED
  4. a case in EXPECTED passed — a stale expectation is a lie in the other
     direction, and the only moment anyone would notice is now

Usage: autobahn-report.py <report-dir> <agent>
"""

import json
import os
import sys

# The suite's own vocabulary (`autobahn/testsuite/case/__init__.py`).
# `behavior` is the case's verdict; `behaviorClose` is separately whether
# the closing handshake went the way the case required.
PASSING = {"OK", "NON-STRICT", "INFORMATIONAL", "UNIMPLEMENTED"}

# How many cases the suite must have run for this check to mean anything.
# `wstest` 25.10.1 offers 522. A floor rather than an equality so that a
# newer suite adding cases is a red run about the new cases, not about the
# count; a suite that ran 3 is the failure this number exists for.
MINIMUM_CASES = 500

# Every case allowed to fail, with the reason it is allowed to.
#
# A prefix ending in `.` matches a whole section. Nothing may be added here
# because it is inconvenient: each entry is a claim that the suite is
# testing something this client deliberately does not do, and the claim is
# checked in the other direction too — an entry that stops failing fails
# this script.
EXPECTED = {
    "12.": "permessage-deflate: not implemented, and recorded as absent in "
    "docs/w4-upgrade-seam.md and the WebSocket seam's own module doc. "
    "The client never offers the extension, so the suite's compressed "
    "cases have nothing to run against.",
    "13.": "permessage-deflate, again: 13.x is 12.x re-run across the "
    "parameter space (window bits, no-context-takeover). Same reason.",
}


def expectation(case_id):
    """The reason `case_id` is allowed to fail, or None."""
    for prefix, why in EXPECTED.items():
        if case_id == prefix or (prefix.endswith(".") and case_id.startswith(prefix)):
            return why
    return None


def section_of(case_id):
    return case_id.split(".", 1)[0]


def die(msg):
    print(f"::error::{msg}", file=sys.stderr)
    sys.exit(1)


def load(report_dir, agent):
    index = os.path.join(report_dir, "index.json")
    if not os.path.isfile(index):
        die(
            f"{index} does not exist — the fuzzingserver never wrote a report, so "
            "there is no verdict to read. A missing report is a failed run."
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
            f"{index} has no results for agent {agent!r} (it has "
            f"{sorted(data)!r}) — the driver did not reach the suite under the "
            "name this check reads"
        )
    cases = data[agent]
    if not isinstance(cases, dict):
        die(f"{index}'s entry for {agent!r} is {type(cases).__name__}, not an object of cases")
    return cases


def main():
    if len(sys.argv) != 3:
        die("usage: autobahn-report.py <report-dir> <agent>")
    _, report_dir, agent = sys.argv
    cases = load(report_dir, agent)

    if len(cases) < MINIMUM_CASES:
        die(
            f"the report has {len(cases)} cases, fewer than the {MINIMUM_CASES} this "
            "check requires. A run that stopped early scores only what it reached, "
            "and a green tick over 3 cases is the exact failure this fails closed "
            "against. Do not lower MINIMUM_CASES to make a short run pass."
        )

    sections = {}
    unexpected = []
    stale = []
    for case_id in sorted(cases, key=lambda c: [int(p) if p.isdigit() else p for p in c.split(".")]):
        r = cases[case_id]
        behavior = r.get("behavior") if isinstance(r, dict) else None
        close = r.get("behaviorClose") if isinstance(r, dict) else None
        if behavior is None:
            die(f"case {case_id} has no `behavior` field — this report is not one this script understands")
        ok = behavior in PASSING and close in PASSING
        why = expectation(case_id)
        s = sections.setdefault(section_of(case_id), {"pass": 0, "fail": 0, "expected": 0})
        if ok:
            s["pass"] += 1
            if why:
                stale.append((case_id, behavior, close, why))
        elif why:
            s["expected"] += 1
        else:
            s["fail"] += 1
            unexpected.append((case_id, behavior, close))

    print(f"Autobahn TestSuite, agent {agent}: {len(cases)} cases")
    print(f"{'section':>8}  {'pass':>5}  {'fail':>5}  {'expected-fail':>13}")
    for name in sorted(sections, key=lambda n: int(n) if n.isdigit() else 0):
        s = sections[name]
        print(f"{name:>8}  {s['pass']:>5}  {s['fail']:>5}  {s['expected']:>13}")
    total_pass = sum(s["pass"] for s in sections.values())
    total_fail = sum(s["fail"] for s in sections.values())
    total_exp = sum(s["expected"] for s in sections.values())
    print(f"{'total':>8}  {total_pass:>5}  {total_fail:>5}  {total_exp:>13}")

    for case_id, behavior, close in unexpected:
        print(f"  FAIL {case_id}: behavior={behavior} behaviorClose={close}")
    for case_id, behavior, close, why in stale:
        print(f"  STALE {case_id}: behavior={behavior} behaviorClose={close} — expected to fail because: {why}")

    if unexpected:
        die(
            f"{len(unexpected)} Autobahn cases failed that are not in EXPECTED. Each is "
            "either a defect in crates/http-ng-native/src/websocket.rs or a decision "
            "that has to be written down in EXPECTED with its reason — not silenced."
        )
    if stale:
        die(
            f"{len(stale)} cases in EXPECTED now pass. That is good news and a stale "
            "expectation at the same time: remove the entry, or this file goes on "
            "excusing a failure that no longer happens."
        )
    print("Autobahn: every case either passed or is a recorded, deliberate absence.")


if __name__ == "__main__":
    main()
