#!/usr/bin/env python3
"""Mutation run for `http-ng-webtransport`.

Restore is `git checkout` **plus an explicit `os.utime`**: a copy that
preserves mtime leaves cargo believing the mutated artifact is current, and
every run after the first would then score against a stale binary. Six runs
elsewhere in this session were mis-scored exactly that way.

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
LIB = "crates/http-ng-webtransport/src/lib.rs"
ANCHOR = 7

# (id, file, old, new, note)
MUTATIONS = [
    (
        "M1",
        LIB,
        ".extension(h3::ext::Protocol::WEB_TRANSPORT)",
        ".extension(h3::ext::Protocol::CONNECT_UDP)",
        "the :protocol value is connect-udp, not webtransport",
    ),
    (
        "M2",
        LIB,
        ".method(http::Method::CONNECT)",
        ".method(http::Method::GET)",
        "not a CONNECT, so h3 drops :protocol entirely",
    ),
    (
        "M3",
        LIB,
        "const WEBTRANSPORT_STREAM: u64 = 0x41;",
        "const WEBTRANSPORT_STREAM: u64 = 0x42;",
        "the stream signal value is one off",
    ),
    (
        "M4",
        LIB,
        "id: SessionId(stream.id().into_inner()),",
        "id: SessionId(stream.id().index()),",
        "the session id is h3's `index()` — the ID without its two type bits",
    ),
    (
        "M5",
        LIB,
        "    if v < (1 << 6) {",
        "    if v < (1 << 7) {",
        "the varint short branch takes one value too many, so 0x41 is one byte",
    ),
    (
        "M6",
        LIB,
        "    if announced.webtransport && announced.extended_connect {",
        "    if true {",
        "the settings gate is gone: the CONNECT goes to anyone",
    ),
    (
        "M7",
        LIB,
        "    if announced.webtransport && announced.extended_connect {",
        "    if announced.webtransport || announced.extended_connect {",
        "either setting is enough, rather than both",
    ),
    (
        "M8",
        LIB,
        """    match poll_fn(|cx| inner.poll_control(cx)).await {
        Ok(Frame::Settings(_)) => {}
        // Unreachable rather than impossible, and typed rather than
        // `unwrap`ed: `h3` turns any other first frame into
        // `H3_MISSING_SETTINGS` on the line above, so this arm exists for
        // the version of `h3` that stops doing so.
        Ok(other) => {
            return Err(Error::new(
                ErrorKind::Connect,
                std::io::Error::other(format!("first control frame was {other:?}, not SETTINGS")),
            ));
        }
        Err(e) => return Err(connect_error(e)),
    }
""",
        "    let _ = inner;\n",
        "the peer's SETTINGS are never awaited, so the defaults are read as the answer",
    ),
    (
        "M9",
        LIB,
        "        if !resp.status().is_success() {",
        "        if false {",
        "any status establishes the session",
    ),
    (
        "M10",
        LIB,
        "            .enable_extended_connect(true)",
        "            .enable_extended_connect(false)",
        "our own SETTINGS no longer announce extended CONNECT",
    ),
    (
        "M11",
        LIB,
        "        let mut header = Vec::with_capacity(16);",
        "        let mut header = Vec::new();",
        "CONTROL — an allocation hint, observable by nothing",
    ),
]


def run_tests():
    r = subprocess.run(
        ["cargo", "nextest", "run", "-p", "http-ng-webtransport", "--no-fail-fast"],
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
