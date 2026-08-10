#!/usr/bin/env python3
"""Mutation tests for `autobahn-report.py`, run without Docker.

The Autobahn run needs a container; this does not, and that is the point.
The parser is the piece that decides whether ~520 external cases passed,
and a parser that cannot tell 517 passes from 0 cases run is the exact
failure this repository has been bitten by twice. So each scenario below
mutates one field of an otherwise-passing report and asserts that the exit
code changes — the fixture is the mutant, and a scenario that stops
failing fails this script.

The baseline is the shape the real report has (`just test-autobahn`, 2026-08):
517 cases — 296 OK, 3 INFORMATIONAL, 2 NON-STRICT and 216 UNIMPLEMENTED —
generated here rather than committed, because 517 JSON objects checked in
are 517 objects nobody will read.

`SCENARIOS` is anchored: the count is asserted, so a scenario deleted
during a refactor is a red run rather than a quieter suite.
"""

import copy
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
PARSER = os.path.join(HERE, "autobahn-report.py")
AGENT = "http-ng-native"


def baseline():
    """A report in which every case passes or is declared."""
    cases = {}
    for i in range(1, 297):
        cases[f"1.1.{i}"] = {"behavior": "OK", "behaviorClose": "OK"}
    for case_id in ("7.1.6", "7.13.1", "7.13.2"):
        cases[case_id] = {"behavior": "INFORMATIONAL", "behaviorClose": "INFORMATIONAL"}
    for case_id in ("6.4.3", "6.4.4"):
        cases[case_id] = {"behavior": "NON-STRICT", "behaviorClose": "OK"}
    for i in range(1, 91):
        cases[f"12.1.{i}"] = {"behavior": "UNIMPLEMENTED", "behaviorClose": "OK"}
    for i in range(1, 127):
        cases[f"13.1.{i}"] = {"behavior": "UNIMPLEMENTED", "behaviorClose": "OK"}
    assert len(cases) == 517, len(cases)
    return {AGENT: cases}


def mutate(**changes):
    """The baseline with `changes` applied to individual cases; None deletes."""

    def build():
        report = baseline()
        for case_id, value in changes.items():
            if value is None:
                del report[AGENT][case_id]
            else:
                report[AGENT][case_id] = value
        return report

    return build


# (name, what it writes as index.json or None for "write no file", must_fail)
#
# `raw` is written verbatim when it is a string, dumped as JSON when it is
# anything else, and skipped entirely when it is the sentinel NO_FILE.
NO_FILE = object()

SCENARIOS = [
    (
        "baseline — every case passes or is declared",
        baseline,
        False,
    ),
    (
        "no index.json at all: the fuzzingserver never wrote a report",
        lambda: NO_FILE,
        True,
    ),
    (
        "index.json is not JSON",
        lambda: "{this is not json",
        True,
    ),
    (
        "index.json is a list, not an object keyed by agent",
        lambda: [],
        True,
    ),
    (
        "zero cases — the run connected and scored nothing",
        lambda: {AGENT: {}},
        True,
    ),
    (
        "one case short of the suite: 516 of 517",
        mutate(**{"1.1.1": None}),
        True,
    ),
    (
        "results are filed under a different agent name",
        lambda: {"someone-else": baseline()[AGENT]},
        True,
    ),
    (
        "one case FAILED",
        mutate(**{"1.1.1": {"behavior": "FAILED", "behaviorClose": "FAILED"}}),
        True,
    ),
    (
        "the echo was right and the close was UNCLEAN",
        mutate(**{"1.1.2": {"behavior": "OK", "behaviorClose": "UNCLEAN"}}),
        True,
    ),
    (
        "the echo was right and the close code was wrong",
        mutate(**{"1.1.3": {"behavior": "OK", "behaviorClose": "WRONG CODE"}}),
        True,
    ),
    (
        "a NON-STRICT case that NON_STRICT does not name",
        mutate(**{"1.1.4": {"behavior": "NON-STRICT", "behaviorClose": "OK"}}),
        True,
    ),
    (
        "an UNIMPLEMENTED case outside the compression sections",
        mutate(**{"1.1.5": {"behavior": "UNIMPLEMENTED", "behaviorClose": "OK"}}),
        True,
    ),
    (
        "a case EXPECTED excuses now passes — a stale declaration",
        mutate(**{"12.1.1": {"behavior": "OK", "behaviorClose": "OK"}}),
        True,
    ),
    (
        "a case NON_STRICT names is now strict — a stale declaration",
        mutate(**{"6.4.3": {"behavior": "OK", "behaviorClose": "OK"}}),
        True,
    ),
    (
        "a case with no behavior field",
        mutate(**{"1.1.6": {"duration": 3}}),
        True,
    ),
]


def main():
    # Anchored, for the reason `test-browser` anchors its test count: a
    # scenario silently dropped would leave this green while checking less.
    expected_scenarios = 15
    if len(SCENARIOS) != expected_scenarios:
        print(
            f"::error::SCENARIOS has {len(SCENARIOS)} entries, not {expected_scenarios}. "
            "If that is deliberate, change the anchor in the same commit that "
            "changes the list — a self-test that quietly checks less is the thing "
            "it exists to catch.",
            file=sys.stderr,
        )
        return 1

    bad = 0
    for name, build, must_fail in SCENARIOS:
        with tempfile.TemporaryDirectory() as d:
            raw = build()
            if raw is not NO_FILE:
                with open(os.path.join(d, "index.json"), "w", encoding="utf-8") as f:
                    f.write(raw if isinstance(raw, str) else json.dumps(raw))
            p = subprocess.run(
                [sys.executable, PARSER, d, AGENT],
                capture_output=True,
                text=True,
                check=False,
            )
        # A non-zero exit is not enough. A parser that dies with a
        # `KeyError` traceback also exits non-zero, and in CI that reads as
        # broken infrastructure rather than as a failing check — the same
        # distinction `ci-mirrors-just.py` draws about its own YAML error.
        # So a rejection must carry the `::error::` marker, which is what
        # makes it a diagnosis. Three mutations of the parser survived
        # until this line required it.
        rejected = p.returncode != 0 and "::error::" in p.stderr
        if rejected == must_fail:
            print(f"  killed  {name}" if must_fail else f"  passes  {name}")
        else:
            bad += 1
            want = "reject with ::error::" if must_fail else "pass"
            print(f"  SURVIVED  {name}: expected the parser to {want}, it returned {p.returncode}")
            print("    " + (p.stdout + p.stderr).replace("\n", "\n    ").strip())

    if bad:
        print(
            f"::error::{bad} of {len(SCENARIOS)} scenarios survived. "
            "scripts/autobahn-report.py cannot tell a bad Autobahn report from a good "
            "one, which makes the Autobahn job a check that cannot fail. Fix the parser "
            "— do not relax the scenario.",
            file=sys.stderr,
        )
        return 1
    print(f"autobahn-report.py: all {len(SCENARIOS)} scenarios behaved as required")
    return 0


if __name__ == "__main__":
    sys.exit(main())
