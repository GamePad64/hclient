#!/usr/bin/env python3
"""Mutation harness for the v0.4 race.

Rules this enforces, each of which cost a previous run its result:

  * every patch must match **exactly once**, or the mutation is not run;
  * the anchor test count is verified before the first mutation and after
    every restore, so a restore that half-worked is caught;
  * nextest is run with ``--no-fail-fast``, and a run whose total is not the
    anchor is refused rather than scored;
  * restores are ``git checkout`` **plus** ``os.utime``, because a restore
    that preserves mtime leaves cargo holding the mutant.
"""

import json
import os
import re
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.abspath(__file__))
SRC = "crates/http-ng-select/src"
NEXTEST = [
    "cargo",
    "nextest",
    "run",
    "-p",
    "http-ng-select",
    "--all-features",
    "--no-fail-fast",
    "--message-format",
    "libtest-json",
]
ENV = dict(os.environ, NEXTEST_EXPERIMENTAL_LIBTEST_JSON="1")

MUTATIONS = [
    # (id, file, old, new, expectation)
    (
        "R1",
        "race.rs",
        "                self.rt.sleep(head_start).await;\n",
        "",
        "kill: the head start is not slept, so both arms start together",
    ),
    (
        "R2",
        "race.rs",
        "        if let Some(head_start) = self.hedge.filter(|_| fallback) {",
        "        if let Some(head_start) = self.hedge.or(Some(crate::DEFAULT_HEAD_START)).filter(|_| fallback) {",
        "kill: the race runs even where nobody asked for one",
    ),
    (
        "R3",
        "race.rs",
        "        if let Some(head_start) = self.hedge.filter(|_| fallback) {",
        "        if let Some(head_start) = self.hedge {",
        "kill: the race runs for a request that may not fall back",
    ),
    (
        "R4",
        "race.rs",
        "                Either::Right((Ok(_tcp_connection), _quic)) => {\n"
        "                    // The QUIC arm was abandoned rather than beaten, and that\n"
        "                    // is what the memory is told — module doc §3.\n",
        "                Either::Right((Ok(_tcp_connection), quic)) => {\n"
        "                    let _ = quic.await;\n",
        "kill: the losing arm is awaited rather than dropped",
    ),
    (
        "R5",
        "race.rs",
        "                Either::Right((Ok(_tcp_connection), _quic)) => {",
        "                Either::Right((Ok(tcp_connection), _quic)) => {\n"
        "                    std::mem::forget(tcp_connection);",
        "kill: the winning hedge's connection is never handed back to the pool",
    ),
    (
        "R6",
        "race.rs",
        "                    self.note_h3_failure(origin);\n"
        "                    self.charge(req, began);\n"
        "                    return Raced::Tcp;",
        "                    self.charge(req, began);\n"
        "                    return Raced::Tcp;",
        "kill: a QUIC arm that lost the race teaches the memory nothing",
    ),
    (
        "R7",
        "race.rs",
        "                Either::Left((Ok(_quic_connection), _hedge)) => return Raced::Quic,",
        "                Either::Left((Ok(_quic_connection), _hedge)) => {\n"
        "                    self.note_h3_failure(origin);\n"
        "                    return Raced::Quic;\n"
        "                }",
        "kill: the memory is taught whenever a race runs, won or lost",
    ),
    (
        "R8",
        "race.rs",
        "fn probe_body(like: &RequestBody) -> RequestBody {\n"
        "    match like.retry_kind() {",
        "fn probe_body(like: &RequestBody) -> RequestBody {\n"
        "    if true {\n"
        "        return RequestBody::Empty;\n"
        "    }\n"
        "    match like.retry_kind() {",
        "kill: a probe's body says nothing about the caller's retry kind",
    ),
    (
        "R9",
        "race.rs",
        "    *probe.extensions_mut() = req.extensions().clone();\n",
        "",
        "kill: a probe does not carry the caller's extensions",
    ),
    (
        "R10",
        "race.rs",
        "                Raced::Quic => {}\n"
        "                Raced::Tcp => {",
        "                Raced::Quic | Raced::Tcp => {}\n"
        "                #[allow(unreachable_patterns)]\n"
        "                Raced::Tcp => {",
        "kill: a race the hedge won still sends the request over QUIC",
    ),
    (
        "R11",
        "race.rs",
        "        spend_connect_budget(req, self.now().saturating_sub(began))",
        "        let _ = began;\n"
        "        spend_connect_budget(req, Duration::ZERO)",
        "kill: the race is not charged against Timeouts::connect",
    ),
    (
        "R12",
        "race.rs",
        "                Either::Right((Err(_), quic)) => match quic.await {\n"
        "                    Ok(_quic_connection) => return Raced::Quic,\n"
        "                    Err(refused) => refused.into_error(),\n"
        "                },",
        "                Either::Right((Err(e), _quic)) => e.into_error(),",
        "kill: a hedge that failed ends the race",
    ),
    (
        "C1",
        "lib.rs",
        '            .field("hedge", &self.hedge)\n',
        "",
        "CONTROL, must survive: `Selecting`'s Debug stops reporting the hedge",
    ),
    (
        "C2",
        "race.rs",
        "        if let Some(left) = hedge_bound {\n"
        "            set_connect(&mut hedge_probe, left);\n"
        "        }\n",
        "",
        "expected to survive: the hedge arm gets the caller's whole bound",
    ),
    (
        "C3",
        "race.rs",
        "            Room::None => return Raced::Quic,",
        "            Room::None => None,",
        "expected to survive: a head start that does not fit the bound is used anyway",
    ),
]


def sh(cmd, **kw):
    return subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, **kw)


def run_tests():
    """-> (total, failed_names) or (None, reason)."""
    r = subprocess.run(NEXTEST, cwd=ROOT, capture_output=True, text=True, env=ENV)
    total, failed = 0, []
    for line in r.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        if ev.get("type") != "test":
            continue
        ev_kind = ev.get("event")
        if ev_kind in ("ok", "failed", "ignored"):
            if ev_kind != "ignored":
                total += 1
            if ev_kind == "failed":
                failed.append(ev.get("name", "?"))
    if total == 0:
        tail = (r.stderr or r.stdout)[-2000:]
        return None, "no tests ran:\n" + tail
    return total, failed


def restore():
    sh(["git", "checkout", "--", SRC])
    now = time.time()
    for root, _dirs, files in os.walk(os.path.join(ROOT, SRC)):
        for f in files:
            os.utime(os.path.join(root, f), (now, now))


def apply(fname, old, new):
    path = os.path.join(ROOT, SRC, fname)
    with open(path) as fh:
        s = fh.read()
    if s.count(old) != 1:
        return f"patch matched {s.count(old)} times, not once"
    with open(path, "w") as fh:
        fh.write(s.replace(old, new))
    now = time.time()
    os.utime(path, (now, now))
    return None


def main():
    only = set(sys.argv[1:])
    restore()
    anchor, failed = run_tests()
    if anchor is None:
        print("anchor run failed:", failed)
        return 1
    if failed:
        print("anchor is red:", failed)
        return 1
    print(f"anchor = {anchor} tests, all green\n")

    rows = []
    for mid, fname, old, new, expectation in MUTATIONS:
        if only and mid not in only:
            continue
        err = apply(fname, old, new)
        if err:
            restore()
            print(f"{mid}: NOT RUN — {err}")
            rows.append((mid, expectation, "not run", []))
            continue
        total, failed = run_tests()
        restore()
        if total is None:
            print(f"{mid}: did not build / did not run — {failed[:400]}")
            rows.append((mid, expectation, "did not build", []))
            continue
        if total != anchor:
            print(f"{mid}: UNSCORABLE — {total} tests ran, anchor is {anchor}")
            rows.append((mid, expectation, f"unscorable ({total})", failed))
            continue
        verdict = "killed" if failed else "survived"
        print(f"{mid}: {verdict} ({len(failed)}) — {expectation}")
        for name in failed:
            print(f"      {name}")
        rows.append((mid, expectation, verdict, failed))

    # A restore that half-worked is caught here rather than in the next run.
    total, failed = run_tests()
    print(f"\nafter restore: {total} tests, {len(failed or [])} failing")
    if total != anchor or failed:
        print("RESTORE IS NOT CLEAN")
        return 1

    print("\n| # | mutation | verdict | killed by |")
    print("|---|---|---|---|")
    for mid, expectation, verdict, failed in rows:
        names = "<br>".join(f"`{n}`" for n in failed) or "—"
        print(f"| **{mid}** | {expectation} | **{verdict}** ({len(failed)}) | {names} |")
    return 0


if __name__ == "__main__":
    sys.exit(main())
