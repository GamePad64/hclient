#!/usr/bin/env python3
"""Hand-applied mutations for `bare_host` and its three call sites.

Same convention as `crates/http-ng-dns-doh/mutations.py`: each entry names
a file, a literal `find`, the `replace` that mutates it, and how many
places the `find` is expected to match. **The count is checked BEFORE the
edit**, so a mutation that matched zero or several places is reported as a
mismatch rather than scored — an anchor that has gone stale (rustfmt
rewrapping a line is the usual cause) would otherwise be recorded as a
kill it never earned.

Two families of mutation, and both are needed:

  * M1-M3 remove the strip at one call site each. They say the fix is
    load-bearing where it was applied, and — because M1 and M3 are killed
    by different tests — that the two `http-ng-h3` sites are separately
    covered rather than jointly.
  * M4-M7 keep the call sites and break the function. They say the tests
    pin *what* it does, not merely that something is called: a strip that
    fires on every host, one that takes the prefix only, one that trims
    repeatedly, one that refuses an empty inner host.

The whole workspace runs for each, ~50s, so a mutation killed by a test in
some other crate is visible rather than silently attributed.

    python3 crates/http-ng-core/bare-host-mutations.py        # all
    python3 crates/http-ng-core/bare-host-mutations.py M4     # one

Nothing else may touch the tree while this runs: the script edits files in
place and restores them afterwards, so a concurrent `cargo fmt` or `git
add -A` can commit a mutant.
"""

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

HOST = "crates/http-ng-core/src/host.rs"
NATIVE = "crates/http-ng-native/src/connect.rs"
H3 = "crates/http-ng-h3/src/lib.rs"

# The function body as it stands, used as the anchor by every mutation that
# replaces the implementation.
BODY = """    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)"""

MUTATIONS = [
    (
        "M1 the TLS server name keeps the URI's brackets",
        NATIVE,
        "server_name: http_ng_core::bare_host(host),",
        "server_name: host,",
        1,
    ),
    (
        "M2 the QUIC server name keeps the URI's brackets",
        H3,
        ".connect_with(cfg, addr, http_ng_core::bare_host(&key.host))",
        ".connect_with(cfg, addr, &key.host)",
        1,
    ),
    (
        "M3 the h3 literal shortcut is asked about the bracketed host",
        H3,
        "if let Ok(ip) = http_ng_core::bare_host(host).parse::<std::net::IpAddr>() {",
        "if let Ok(ip) = host.parse::<std::net::IpAddr>() {",
        1,
    ),
    (
        "M4 the strip fires on every host, bracketed or not",
        HOST,
        BODY,
        "    host.get(1..host.len().saturating_sub(1)).unwrap_or(host)",
        1,
    ),
    (
        "M5 only the opening bracket is stripped",
        HOST,
        BODY,
        "    host.strip_prefix('[').unwrap_or(host)",
        1,
    ),
    (
        "M6 brackets are trimmed repeatedly rather than one pair",
        HOST,
        BODY,
        "    host.trim_start_matches('[').trim_end_matches(']')",
        1,
    ),
    (
        "M7 a bracketed empty host is handed back bracketed",
        HOST,
        BODY,
        """    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .filter(|inner| !inner.is_empty())
        .unwrap_or(host)""",
        1,
    ),
]


def run_suite():
    """The whole workspace: ~50s, and it makes an unexpected killer visible."""
    return subprocess.run(
        [
            "cargo",
            "nextest",
            "run",
            "--workspace",
            "--all-features",
            "--no-fail-fast",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )


def failing_tests(out):
    names = []
    for line in (out.stdout + out.stderr).splitlines():
        if "FAIL" in line and "::" in line:
            names.append(line.split()[-1].strip())
    return sorted(set(names))


def main():
    only = sys.argv[1:]
    for label, filename, find, replace, expected in MUTATIONS:
        if only and not any(label.startswith(o) for o in only):
            continue
        path = ROOT / filename
        original = path.read_text()
        count = original.count(find)
        if count != expected:
            print(f"{label}: ANCHOR MISMATCH — matched {count}, expected {expected}")
            continue
        path.write_text(original.replace(find, replace))
        try:
            out = run_suite()
            if out.returncode == 0:
                print(f"{label}: SURVIVED (anchors {count})")
            else:
                dead = failing_tests(out)
                if not dead:
                    print(f"{label}: KILLED — build failure (anchors {count})")
                else:
                    print(f"{label}: KILLED by {', '.join(dead[:6])} (anchors {count})")
        finally:
            path.write_text(original)
        sys.stdout.flush()


if __name__ == "__main__":
    main()
