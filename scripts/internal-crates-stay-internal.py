#!/usr/bin/env python3
"""No crate exposes a crate marked internal.

An internal crate is published only because its dependents need it on
crates.io, and it promises nothing: `hclient-proto` moves its minor version
whenever the transports need it to. That is safe exactly as long as no
other crate hands its types to a caller — the moment one does, a proto
release becomes a breaking change of that crate too, and nothing would say
so. So every publishable library other than the internal ones has its
public API read out of rustdoc's JSON, with the same scanner
`exposed-majors.py` uses, and any path into an internal crate fails.

A crate is internal when its manifest says so:

    [package.metadata.hclient]
    internal = true

— on the crate itself, so the list cannot drift from the crates it names.

Fails closed: on no internal crate found (the marker was renamed and the
check would pass over nothing), on no library examined, and on a library
whose API came back empty (the scanner's own check).
"""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("exposed_majors", root / "scripts" / "exposed-majors.py")
em = importlib.util.module_from_spec(spec)
spec.loader.exec_module(em)


def main() -> None:
    scratch = Path(os.environ.get("EXPOSED_MAJORS_TARGET_DIR", root / "target" / "exposed-majors"))
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        check=True, capture_output=True, text=True, cwd=root).stdout)
    packages = [p for p in meta["packages"] if p.get("publish") != []]

    internal = {p["name"].replace("-", "_") for p in packages
                if ((p.get("metadata") or {}).get("hclient") or {}).get("internal") is True}
    if not internal:
        em.fail("no crate is marked `[package.metadata.hclient] internal = true` — "
                "the marker was renamed or removed, and this check would pass over nothing")

    examined = 0
    problems: list[str] = []
    for pkg in sorted(packages, key=lambda p: p["name"]):
        name = pkg["name"]
        if name.replace("-", "_") in internal:
            continue
        libs = [t for t in pkg["targets"] if any(k in ("lib", "rlib") for k in t["kind"])]
        if not libs:
            continue
        target = ((pkg.get("metadata") or {}).get("docs", {}).get("rs", {}) or {}).get("default-target")
        cmd = ["cargo", "+nightly", "rustdoc", "-q", "-p", name, "--lib", "--all-features"]
        if target:
            cmd += ["--target", target]
        cmd += ["--", "-Z", "unstable-options", "--output-format", "json"]
        env = dict(os.environ, CARGO_TARGET_DIR=str(scratch / "target"),
                   CARGO_BUILD_BUILD_DIR=str(scratch / "build"))
        env.pop("RUSTUP_TOOLCHAIN", None)
        run = subprocess.run(cmd, cwd=root, env=env, capture_output=True, text=True)
        if run.returncode != 0:
            print(run.stderr[-3000:])
            em.fail(f"{name}: rustdoc JSON failed (needs a nightly toolchain"
                    + (f" with the {target} standard library" if target else "") + ")")
        doc_dir = scratch / "target" / target / "doc" if target else scratch / "target" / "doc"
        doc = doc_dir / (libs[0]["name"].replace("-", "_") + ".json")
        if not doc.exists():
            em.fail(f"{name}: rustdoc wrote no {doc}")
        leaked = sorted(em.exposed_crates(doc) & internal)
        examined += 1
        if leaked:
            problems.append(f"{name} exposes {', '.join(leaked)}, which is internal: "
                            "declare the type in this crate instead of naming or re-exporting it")
        else:
            print(f"  ok   {name}")

    for p in problems:
        print(f"::error::{p}")
    if problems:
        sys.exit(1)
    if examined == 0:
        em.fail("no publishable library was examined — a green run over nothing")
    print(f"internal-crates-stay-internal: {examined} libraries, none exposes "
          + ", ".join(sorted(internal)))


if __name__ == "__main__":
    main()
