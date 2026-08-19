#!/usr/bin/env python3
"""Mutation run for `http-ng-quinn`.

The crate is a **move**, so the thing being checked is not new behaviour but
that the four unit tests which travelled with the code still have their teeth
from their new home. A move whose tests went green because they stopped
reaching the code would look exactly like a move that worked.

Restore is `git checkout` **plus an explicit `os.utime`**: a copy that
preserves mtime leaves cargo believing the mutated artifact is current, and
every run after the first would then score against a stale binary.

The anchor is verified before the first mutation and after the last, and one
mutation in the table is a **control** that nothing can observe. A harness
that reports "killed" unconditionally fails on the control.
"""

import os
import re
import subprocess
import sys
import time

ANSI = re.compile(r"\x1b\[[0-9;]*m")

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
LIB = "crates/http-ng-quinn/src/lib.rs"
ANCHOR = 5

# (id, file, old, new, note)
MUTATIONS = [
    (
        "M1",
        LIB,
        "        for w in taken {\n            w.wake();\n        }",
        "        for w in taken.into_iter().take(1) {\n            w.wake();\n        }",
        "the fan-out wakes the first waiter only — the failure WakeAll exists for",
    ),
    (
        "M2",
        LIB,
        "        if !waiters.iter().any(|existing| existing.will_wake(w)) {\n"
        "            waiters.push(w.clone());\n        }",
        "        waiters.push(w.clone());",
        "no `will_wake` guard, so the list grows by one clone per poll",
    ),
    (
        "M3",
        LIB,
        "    fn register(&self, w: &Waker) {",
        "    fn register(&self, w: &Waker) {\n        if true {\n            return;\n        }",
        "nothing is ever registered, so the list is always empty",
    ),
    (
        "M4",
        LIB,
        "    deadline.saturating_duration_since(Instant::now())",
        "    Instant::now().saturating_duration_since(deadline)",
        "the subtraction is backwards, so every future deadline is a zero sleep",
    ),
    (
        "M5",
        LIB,
        "    deadline.saturating_duration_since(Instant::now())",
        "    deadline - Instant::now()",
        "a plain subtraction, so an already-elapsed deadline panics",
    ),
    (
        "M6",
        LIB,
        "        http_ng_rt::EcnCodepoint::Ect0 => quinn::udp::EcnCodepoint::Ect0,\n"
        "        http_ng_rt::EcnCodepoint::Ect1 => quinn::udp::EcnCodepoint::Ect1,",
        "        http_ng_rt::EcnCodepoint::Ect0 => quinn::udp::EcnCodepoint::Ect1,\n"
        "        http_ng_rt::EcnCodepoint::Ect1 => quinn::udp::EcnCodepoint::Ect0,",
        "Ect0 and Ect1 swapped on the way out only",
    ),
    (
        "M7",
        LIB,
        "        quinn::udp::EcnCodepoint::Ect0 => http_ng_rt::EcnCodepoint::Ect0,\n"
        "        quinn::udp::EcnCodepoint::Ect1 => http_ng_rt::EcnCodepoint::Ect1,",
        "        quinn::udp::EcnCodepoint::Ect0 => http_ng_rt::EcnCodepoint::Ect1,\n"
        "        quinn::udp::EcnCodepoint::Ect1 => http_ng_rt::EcnCodepoint::Ect0,",
        "swapped on the way back too, so the round trip is clean and the wire is not",
    ),
    (
        "M8",
        LIB,
        "                self.writable.wake_all();\n                Poll::Ready(r)",
        "                Poll::Ready(r)",
        "a Ready answer keeps the inner registration for itself and strands the rest",
    ),
    (
        "M9",
        LIB,
        "        for w in taken {\n            w.wake();\n        }",
        "        for w in taken {\n            w.wake_by_ref();\n        }",
        "CONTROL — `wake_by_ref` then drop is `wake`, one clone apart",
    ),
]


def run_tests():
    r = subprocess.run(
        ["cargo", "nextest", "run", "-p", "http-ng-quinn", "--no-fail-fast"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return r.returncode, r.stdout + r.stderr


def touch(path):
    now = time.time()
    os.utime(os.path.join(ROOT, path), (now, now))


def restore(path):
    subprocess.run(["git", "checkout", "--", path], cwd=ROOT, check=True)
    touch(path)


def apply(path, old, new):
    full = os.path.join(ROOT, path)
    s = open(full).read()
    if s.count(old) != 1:
        raise SystemExit(f"pattern occurs {s.count(old)} times, not once:\n{old}")
    open(full, "w").write(s.replace(old, new))
    touch(path)


def failed_tests(out):
    names = []
    for line in out.splitlines():
        clean = ANSI.sub("", line).strip()
        if clean.startswith("FAIL ") and "]" in clean:
            names.append(clean.rsplit(" ", 1)[-1])
    return sorted(set(names))


def summary_line(out):
    for line in out.splitlines():
        if "tests run:" in line:
            return ANSI.sub("", line).strip()
    return "(no summary — build failure?)"


def main():
    code, out = run_tests()
    line = summary_line(out)
    if code != 0 or f"{ANCHOR} tests run" not in line:
        raise SystemExit(f"anchor is not {ANCHOR} green tests: {line}")
    print(f"anchor OK: {line}\n")

    results = []
    for mid, path, old, new, note in MUTATIONS:
        try:
            apply(path, old, new)
            code, out = run_tests()
        finally:
            restore(path)
        verdict = "KILLED" if code != 0 else "SURVIVED"
        results.append((mid, verdict, note, summary_line(out)))
        print(f"{mid}: {verdict:9} {note}")
        print(f"     {summary_line(out)}")
        print(f"     killed by: {', '.join(failed_tests(out)) or '(nothing)'}")

    code, out = run_tests()
    line = summary_line(out)
    if code != 0 or f"{ANCHOR} tests run" not in line:
        raise SystemExit(f"anchor did not come back: {line}")
    print(f"\nanchor restored: {line}")
    killed = sum(1 for _, v, _, _ in results if v == "KILLED")
    print(f"{killed} killed, {len(results) - killed} survived, of {len(results)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
