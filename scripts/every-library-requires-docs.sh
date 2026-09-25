#!/usr/bin/env bash
# Every publishable library asks rustc for `missing_docs`.
#
# rustdoc cannot tell a doc on the wrong item from one on the right item:
# a method inserted between a doc block and the function it described
# leaves every link resolving and every page rendering, with the text on a
# neighbour. `hclient-native` shipped that way for its main type — the
# whole of `Native`'s documentation sat on a private struct below it — and
# `just docs` was green over it. What does see it is `missing_docs`, which
# finds the item left with nothing; with `just lint`'s `-D warnings` the
# attribute is a gate.
#
# The attribute and not a `[workspace.lints]` entry, because the workspace
# table reaches every target, and an integration test or an example is a
# crate of its own that would then need a crate-level doc to compile under
# `-D warnings`. What is published is the library, so the library is what
# is checked.
#
# Fails closed on examining nothing: a `cargo metadata` that returned no
# packages and a tidy tree print the same thing otherwise.
set -euo pipefail

cargo metadata --no-deps --format-version 1 | python3 -c '
import json, re, sys
pat = re.compile(r"^#!\[(warn|deny|forbid)\(missing_docs\)\]", re.M)
examined, missing = 0, []
for p in json.load(sys.stdin)["packages"]:
    if p.get("publish") == []:
        continue
    for t in p["targets"]:
        if "lib" not in t["kind"] and "rlib" not in t["kind"]:
            continue
        examined += 1
        if not pat.search(open(t["src_path"]).read()):
            missing.append(p["name"] + " (" + t["src_path"] + ")")
if examined == 0:
    sys.exit("::error::no publishable library was examined — a check over nothing is not a check")
for m in missing:
    print(f"::error::{m} has no #![warn(missing_docs)] — add it beside the crate-level attributes")
if missing:
    sys.exit(1)
print(f"every-library-requires-docs: {examined} publishable libraries ask for missing_docs")
'
