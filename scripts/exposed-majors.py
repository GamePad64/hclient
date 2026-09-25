#!/usr/bin/env python3
"""A stable crate must not change which major of a dependency it exposes
without changing its own.

`cargo semver-checks` compares a crate's own shape, and a type re-exported
or named from a dependency keeps its shape while becoming another type when
that dependency moves major. So `hclient-rt` re-exporting `hclient_core::Timer`
looks unchanged when `hclient-core` goes from 0.2 to 0.3, and a caller who
holds `hclient-core = "0.2"` beside `hclient-rt = "0.1"` stops compiling on a
patch release: *multiple different versions of `hclient_core`*. release-plz
proposed exactly those patch bumps for three crates when core 0.3 was on the
table. This is the check that would have said so.

For every publishable library whose working version is stable and whose
newest stable release on crates.io is semver-compatible with it (so no major
step is being taken on purpose):

1. rustdoc's JSON output (nightly) names every external crate the public API
   reaches: re-exports, signatures, fields, variants, trait impls and the
   associated types inside them;
2. of those, the ones that are direct normal dependencies are the exposed
   ones;
3. the requirement on each, in the working manifest and in the published
   release, must land in the same compatible range (`^0.2.1` and `^0.2.0`
   agree; `^0.3` does not).

It fails closed: on a crate whose API came back empty, on a requirement it
cannot read, and on a run that examined nothing.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import urllib.request
from pathlib import Path

USER_AGENT = "hclient exposed-majors gate (gamepad64@gmail.com)"
STD = {"std", "core", "alloc", "proc_macro", "test"}


def fail(msg: str) -> None:
    print(f"::error::{msg}")
    sys.exit(1)


def get(url: str) -> dict:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def parse_version(v: str) -> tuple[int, int, int, str]:
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?", v)
    if not m:
        fail(f"cannot read version {v!r}")
    return int(m[1]), int(m[2]), int(m[3]), m[4] or ""


def compat(major: int, minor: int, patch: int) -> str:
    """The range `^x.y.z` covers: below 1.0 the leftmost non-zero part is
    the major one."""
    if major > 0:
        return f"{major}"
    if minor > 0:
        return f"0.{minor}"
    return f"0.0.{patch}"


def req_compat(req: str, dep: str, crate: str) -> str:
    parts = [p.strip() for p in req.split(",") if p.strip()]
    if len(parts) != 1:
        fail(f"{crate}: cannot read the requirement {req!r} on {dep} (a range with several comparators)")
    m = re.fullmatch(r"(?:\^|=|~)?\s*(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-[0-9A-Za-z.-]+)?", parts[0])
    if not m:
        fail(f"{crate}: cannot read the requirement {req!r} on {dep}")
    return compat(int(m[1]), int(m[2] or 0), int(m[3] or 0))


def exposed_crates(doc: Path) -> set[str]:
    """External crates the public API of this rustdoc JSON reaches."""
    d = json.loads(doc.read_text())
    idx, paths, ext = d["index"], d["paths"], d["external_crates"]

    def crate_of(i) -> str | None:
        p = paths.get(str(i))
        if not p or p["crate_id"] == 0:
            return None
        return ext[str(p["crate_id"])]["name"]

    found: set[str] = set()

    def scan(t) -> None:
        if isinstance(t, dict):
            r = t.get("resolved_path")
            if r is None and "id" in t and isinstance(t.get("path"), str):
                r = t
            if r is not None:
                c = crate_of(r["id"])
                if c:
                    found.add(c)
            for v in t.values():
                scan(v)
        elif isinstance(t, list):
            for v in t:
                scan(v)

    seen: set = set()
    public_items = 0

    def visit(i) -> None:
        nonlocal public_items
        if i in seen:
            return
        seen.add(i)
        it = idx.get(str(i))
        if it is None:
            c = crate_of(i)
            if c:
                found.add(c)
            return
        kind = next(iter(it["inner"]))
        body = it["inner"][kind]
        if not isinstance(body, dict):
            return
        if kind == "module":
            for c in body["items"]:
                visit(c)
            return
        if kind == "use":
            target = body.get("id")
            if target is None:
                return
            if str(target) not in idx:
                c = crate_of(target)
                if c:
                    found.add(c)
                return
            visit(target)
            return
        if it["visibility"] != "public":
            return
        public_items += 1
        scan({k: v for k, v in body.items() if k not in ("impls", "items", "fields", "variants", "kind")})
        children = list(body.get("items", [])) + list(body.get("variants", []))
        kd = body.get("kind")
        if isinstance(kd, dict):
            children += kd.get("plain", {}).get("fields", []) if "plain" in kd else []
            children += [f for f in kd.get("tuple", []) if f is not None] if "tuple" in kd else []
        for c in children:
            ci = idx.get(str(c))
            if ci and ci["visibility"] in ("public", "default"):
                scan(ci["inner"])
        for im in body.get("impls", []):
            ib = idx[str(im)]["inner"]["impl"]
            if ib.get("is_synthetic") or ib.get("blanket_impl"):
                continue
            tr = ib.get("trait")
            if tr:
                c = crate_of(tr["id"])
                if c:
                    found.add(c)
            for m in ib["items"]:
                mi = idx[str(m)]
                if tr or mi["visibility"] == "public":
                    scan(mi["inner"])

    visit(d["root"])
    if public_items == 0:
        fail(f"{doc.name}: the public API came back empty, so nothing about it was checked — "
             "is the crate compiled out on this target? Give it a docs.rs `default-target`")
    return found - STD


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    scratch = Path(os.environ.get("EXPOSED_MAJORS_TARGET_DIR", root / "target" / "exposed-majors"))
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        check=True, capture_output=True, text=True, cwd=root).stdout)

    examined = 0
    # Every offender, not the first: a core major moves five crates at once,
    # and a gate that names one of them sends somebody round the loop five
    # times.
    problems: list[str] = []
    for pkg in sorted(meta["packages"], key=lambda p: p["name"]):
        name, version = pkg["name"], pkg["version"]
        if pkg.get("publish") == []:
            continue
        if not any(k in ("lib", "rlib") for t in pkg["targets"] for k in t["kind"]):
            print(f"  skip {name}: no library target, so no API")
            continue
        wmaj, wmin, wpat, wpre = parse_version(version)
        if wpre:
            print(f"  skip {name} {version}: a pre-release promises nothing")
            continue
        info = get(f"https://crates.io/api/v1/crates/{name}/versions?per_page=100")
        stable = [v["num"] for v in info.get("versions", [])
                  if not v["yanked"] and not parse_version(v["num"])[3]]
        if not stable:
            print(f"  skip {name} {version}: no stable release to compare against")
            continue
        published = max(stable, key=lambda v: parse_version(v)[:3])
        pmaj, pmin, ppat, _ = parse_version(published)
        if compat(pmaj, pmin, ppat) != compat(wmaj, wmin, wpat):
            print(f"  skip {name} {version}: a deliberate major step from {published}")
            continue

        target = ((pkg.get("metadata") or {}).get("docs", {}).get("rs", {}) or {}).get("default-target")
        cmd = ["cargo", "+nightly", "rustdoc", "-q", "-p", name, "--lib", "--all-features"]
        if target:
            cmd += ["--target", target]
        cmd += ["--", "-Z", "unstable-options", "--output-format", "json"]
        # Both directories, because `CARGO_TARGET_DIR` does not override a
        # `[build] build-dir` set in the user's cargo config: without the
        # second, this run shares intermediate artefacts (and their lock)
        # with every other build on the machine — the defect the mutation
        # recipe records.
        env = dict(os.environ, CARGO_TARGET_DIR=str(scratch / "target"),
                   CARGO_BUILD_BUILD_DIR=str(scratch / "build"))
        env.pop("RUSTUP_TOOLCHAIN", None)
        run = subprocess.run(cmd, cwd=root, env=env, capture_output=True, text=True)
        if run.returncode != 0:
            print(run.stderr[-3000:])
            fail(f"{name}: rustdoc JSON failed (needs a nightly toolchain)")
        doc_dir = scratch / "target" / target / "doc" if target else scratch / "target" / "doc"
        doc = doc_dir / (pkg["targets"][0]["name"].replace("-", "_") + ".json")
        for t in pkg["targets"]:
            if any(k in ("lib", "rlib") for k in t["kind"]):
                doc = doc_dir / (t["name"].replace("-", "_") + ".json")
        if not doc.exists():
            fail(f"{name}: rustdoc wrote no {doc}")
        exposed = exposed_crates(doc)

        # Direct normal dependencies, by the name the code uses for them.
        deps = {}
        for dep in pkg["dependencies"]:
            if dep["kind"] not in (None, "normal"):
                continue
            deps[(dep.get("rename") or dep["name"]).replace("-", "_")] = dep
        published_deps = {d["crate_id"]: d for d in
                          get(f"https://crates.io/api/v1/crates/{name}/{published}/dependencies")["dependencies"]
                          if d["kind"] == "normal"}

        rows = []
        for c in sorted(exposed):
            dep = deps.get(c)
            if dep is None:
                continue  # reached through a dependency's own re-export; that crate answers for it
            now = req_compat(dep["req"], dep["name"], name)
            then_dep = published_deps.get(dep["name"])
            if then_dep is None:
                rows.append(f"{dep['name']} {now} (new since {published})")
                continue
            then = req_compat(then_dep["req"], dep["name"], name)
            if now != then:
                problems.append(f"{name} {version} exposes {dep['name']} and moves it from the {then} range "
                     f"({then_dep['req']} in {published}) to {now} ({dep['req']}) without a major step "
                     f"of its own. A caller holding both versions gets two {c} crates. "
                     f"Give {name} a major step, or stop exposing {dep['name']}.")
                continue
            rows.append(f"{dep['name']} {now}")
        examined += 1
        print(f"  ok   {name} {version} (against {published}): exposes "
              + (", ".join(rows) if rows else "no dependency"))

    if problems:
        for p in problems:
            print(f"::error::{p}")
        sys.exit(1)
    if examined == 0:
        fail("no stable, published, non-major-stepping crate was examined — a green run over nothing")
    print(f"exposed-majors: {examined} stable crate(s) keep the majors they expose")


if __name__ == "__main__":
    main()
