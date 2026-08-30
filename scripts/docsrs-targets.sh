#!/usr/bin/env bash
# Every crate that declares a docs.rs `default-target`, and which one.
#
# A separate script because `just` reads `{{` and a `.` at the start of a
# line as its own syntax, and inlining python here fights both.
set -uo pipefail
cargo metadata --no-deps --format-version 1 | python3 -c '
import sys, json
for p in json.load(sys.stdin)["packages"]:
    t = ((p.get("metadata") or {}).get("docs", {}).get("rs", {}) or {}).get("default-target")
    if t:
        print(p["name"], t)
'
