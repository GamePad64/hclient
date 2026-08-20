#!/usr/bin/env python3
"""Mutations for the LIVE suite, in two groups that ask different questions.

`mutations.py` beside this file mutates the library and runs the hermetic
suite. This one exists because `tests/live.rs` is a harness before it is a
test, and a harness has its own way of being wrong: it can pass without
having done anything. This repository has been bitten by exactly that twice
— a `wasmtime` that was not installed, and a browser suite that stopped
running tests — so the claim "the live suite would have noticed" has to be
run rather than asserted.

**Group H — the harness cannot pass when the query never happened.** Each
entry breaks the harness in a way that leaves every assertion untouched and
simply stops the exchange from occurring, then runs `just test-doh-live`
with `HCLIENT_REQUIRE_NETWORK` set. The recipe MUST fail. A `SURVIVED` here
means a green live run proves nothing, which is worse than no live run.

**Group L — what the live suite kills that the fixtures do not, and the
reverse.** Each entry is a library mutation run TWICE: once against the
hermetic suite (`cargo nextest run -p hclient-dns-doh` with the opt-in
absent, so every live test skips) and once against the live suite alone.
The interesting rows are the asymmetric ones, in both directions — a
mutation only the live suite kills is a hole the fixtures had, and one only
the fixtures kill is a reason the fixtures stay.

Anchor counts are checked BEFORE each edit, the convention the h3 work
established: a `find` matching zero or several places is reported rather
than scored. Every edit is reverted whether the run passes or fails.

**Nothing else may touch the tree while this runs.** It edits files in
place, and a `git add -A` from another process has already cost this
project a commit containing three live mutations.

    python3 crates/hclient-dns-doh/live-mutations.py        # all of them
    python3 crates/hclient-dns-doh/live-mutations.py H      # one group
    python3 crates/hclient-dns-doh/live-mutations.py L4     # one mutation
"""

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LIVE = ROOT / "crates/hclient-dns-doh/tests/live.rs"
JUSTFILE = ROOT / "justfile"
DOH_SRC = ROOT / "crates/hclient-dns-doh/src"
SHARED = ROOT / "crates/hclient-dns/src/svcb.rs"

# label -> [(path, find, replace, expected anchor count), ...]
HARNESS = [
    (
        "H1 the gate always skips, so no query is ever made",
        [
            (
                LIVE,
                "fn live(test: &str, ep: Endpoint) -> Option<Endpoint> {\n",
                "fn live(test: &str, ep: Endpoint) -> Option<Endpoint> {\n    if true {\n        let _ = (test, ep);\n        return None;\n    }\n",
                1,
            )
        ],
    ),
    (
        "H2 the endpoints point at TEST-NET-1, which answers nothing",
        [
            (LIVE, 'uri: "https://1.1.1.1/dns-query"', 'uri: "https://192.0.2.1/dns-query"', 1),
            (LIVE, 'addr: "1.1.1.1:443"', 'addr: "192.0.2.1:443"', 1),
        ],
    ),
    (
        "H3 the receipt the recipe counts is spelled differently by the test",
        [(LIVE, 'const RECEIPT: &str = "LIVE-DOH-RAN";', 'const RECEIPT: &str = "LIVE-DOH-OK";', 1)],
    ),
    (
        # The control for H1, and the only row here whose expected verdict
        # is SURVIVED. H1 is the same skipping gate with the count still in
        # place; if this one also failed, H1's kill would have come from
        # something other than the belt it is supposed to demonstrate.
        "H4 the same skipping gate, with the recipe's receipt count disabled (control: must SURVIVE)",
        [
            (
                LIVE,
                "fn live(test: &str, ep: Endpoint) -> Option<Endpoint> {\n",
                "fn live(test: &str, ep: Endpoint) -> Option<Endpoint> {\n    if true {\n        let _ = (test, ep);\n        return None;\n    }\n",
                1,
            ),
            (JUSTFILE, '    if [ "$ran" -lt 16 ]; then', "    if false; then", 1),
        ],
    ),
    (
        # Not the same as H2: there the marker's panic is what fails the
        # run, here it is disabled, so only the receipt count is left.
        "H6 an unreachable endpoint with the REQUIRE marker's panic disabled",
        [
            (LIVE, 'uri: "https://1.1.1.1/dns-query"', 'uri: "https://192.0.2.1/dns-query"', 1),
            (LIVE, 'addr: "1.1.1.1:443"', 'addr: "192.0.2.1:443"', 1),
            (
                LIVE,
                "    let required = std::env::var_os(REQUIRE_MARKER).is_some();",
                "    let required = false;",
                1,
            ),
        ],
    ),
    (
        "H5 a test returns before its assertions but still prints its receipt",
        [
            (
                LIVE,
                "        let ours = expect_addrs(lookup_v4(ep, OWNER).await);",
                "        let ours = expect_addrs(lookup_v4(ep, OWNER).await);\n        let ours = vec![ResolvedAddr { addr: IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), ttl: Some(Duration::from_secs(1)) }; ours.len()];",
                1,
            )
        ],
    ),
]

LIBRARY = [
    (
        "L1 the TTL is dropped",
        [
            (
                DOH_SRC / "wire.rs",
                "ttl: Some(Duration::from_secs(u64::from(a.ttl))),",
                "ttl: None,",
                2,
            )
        ],
    ),
    (
        "L2 supports_svcb is flipped to false",
        [
            (
                DOH_SRC / "lib.rs",
                "    fn supports_svcb(&self) -> bool {\n        true\n    }",
                "    fn supports_svcb(&self) -> bool {\n        false\n    }",
                1,
            )
        ],
    ),
    (
        "L3 the ECHConfigList loses RFC 9460 7.3's length prefix",
        [
            (
                SHARED,
                "                prefixed.extend_from_slice(&len.to_be_bytes());\n",
                "",
                1,
            )
        ],
    ),
    (
        "L4 RFC 9460 2.5's owner substitution for a root TargetName is dropped",
        [
            (
                SHARED,
                "        target: if binding.target.is_empty() {\n            binding.owner.clone()\n        } else {\n            binding.target.clone()\n        },",
                "        target: binding.target.clone(),",
                1,
            )
        ],
    ),
    (
        "L5 the HTTP status is not checked",
        [(DOH_SRC / "lib.rs", "        if status != http::StatusCode::OK {", "        if false {", 1)],
    ),
    (
        "L6 the response is parsed but the answer section ignored",
        [
            (
                DOH_SRC / "wire.rs",
                "    let mut answer = Answer::default();\n    for rr in &dns.answers {",
                "    let mut answer = Answer::default();\n    for rr in &[] as &[RR] {",
                1,
            )
        ],
    ),
    (
        "L7 the echoed question is not checked",
        [
            (
                DOH_SRC / "wire.rs",
                "    check_question(&dns, name, query)?;",
                "    let _ = check_question(&dns, name, query);",
                1,
            )
        ],
    ),
    (
        "L8 an error RCODE is treated as an empty answer",
        [
            (
                DOH_SRC / "wire.rs",
                "            return Err(DohError::ResponseCode { rcode: rcode as u8 });",
                "            let _ = rcode;\n            return Ok(Answer::default());",
                1,
            )
        ],
    ),
    (
        "L9 the ipv4 hints are read as the ipv6 ones",
        [
            (
                SHARED,
                "            RawParam::Ipv4Hint(hints) => endpoint.ipv4hint = hints.clone(),",
                "            RawParam::Ipv4Hint(_) => {}",
                1,
            )
        ],
    ),
    (
        "L10 NXDOMAIN becomes an error rather than an answer",
        [
            (
                DOH_SRC / "wire.rs",
                "        RCode::NXDomain => return Ok(Answer::default()),",
                "        RCode::NXDomain => return Err(DohError::ResponseCode { rcode: 3 }),",
                1,
            )
        ],
    ),
]


def hermetic():
    """The crate's own suite with the live opt-in absent: every test in
    `live.rs` prints a NOTICE and returns, so this is the fixtures alone."""
    env = {k: v for k, v in os.environ.items() if k not in ("HCLIENT_LIVE_DOH", "HCLIENT_REQUIRE_NETWORK")}
    return subprocess.run(
        ["cargo", "nextest", "run", "-p", "hclient-dns-doh", "--no-fail-fast"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        env=env,
    )


def live():
    """`just test-doh-live`, in the mode that refuses to skip."""
    env = dict(os.environ, HCLIENT_REQUIRE_NETWORK="1")
    return subprocess.run(
        ["just", "test-doh-live"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        env=env,
    )


def verdict(out):
    return "KILLED" if out.returncode != 0 else "SURVIVED"


def why(out):
    """The first thing that failed, for the table."""
    text = out.stdout + out.stderr
    for line in text.splitlines():
        if line.startswith("::error::"):
            return line[len("::error::") :][:90]
    names = [line.split()[-1] for line in text.splitlines() if "FAIL" in line and "::" in line]
    if names:
        return ", ".join(sorted(set(names))[:3])
    if "error[E" in text or "error: could not compile" in text:
        return "build failure"
    return ""


def apply(edits):
    """Returns the originals to restore, or None on an anchor mismatch."""
    originals = {}
    for path, find, _replace, expected in edits:
        text = originals.get(path, path.read_text())
        originals[path] = text
        count = text.count(find)
        if count != expected:
            return None, f"ANCHOR MISMATCH — `{find[:40]}…` matched {count}, expected {expected}"
    working = dict(originals)
    for path, find, replace, _expected in edits:
        working[path] = working[path].replace(find, replace)
    for path, text in working.items():
        path.write_text(text)
    return originals, None


def main():
    only = sys.argv[1:]
    for group, entries, runners in (
        ("H", HARNESS, [("live", live)]),
        ("L", LIBRARY, [("hermetic", hermetic), ("live", live)]),
    ):
        if only and not any(o == group or any(l.startswith(o) for l, _ in entries) for o in only):
            continue
        for label, edits in entries:
            if only and not (group in only or any(label.startswith(o) for o in only)):
                continue
            originals, problem = apply(edits)
            if problem:
                print(f"{label}: {problem}")
                continue
            try:
                results = []
                for name, run in runners:
                    out = run()
                    results.append(f"{name}={verdict(out)}" + (f" ({why(out)})" if why(out) else ""))
                print(f"{label}: " + "; ".join(results))
            finally:
                for path, text in originals.items():
                    path.write_text(text)


if __name__ == "__main__":
    main()
