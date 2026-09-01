#!/usr/bin/env bash
# Every in-workspace dependency requirement names the version of the crate
# it points at.
#
# Cargo offers no way to write `version.workspace = true` *inside* a
# dependency requirement, so a version is one number and the requirements
# beside it are dozens of literal copies. Nothing checked that they
# agreed, and they did not: after the alpha.2 release every requirement
# still read `0.1.0-alpha.1`, because `dependent-version = "fix"` only
# rewrites one when the new version stops satisfying it — and
# `^0.1.0-alpha.1` goes on satisfying alpha.2.
#
# What that costs is a requirement that lies. Measured on the published
# crates: `hclient` 0.1.0-alpha.2 with `hclient-core` pinned to
# `=0.1.0-alpha.1` resolves, and then fails to compile — `Reduced` arrived
# in alpha.2. A pre-release promises nothing between alphas, so a
# requirement spanning two of them offers a compatibility nobody made.
#
# The setting is `upgrade` now. This checks the result rather than the
# setting, because a release run from an older checkout, a hand-edited
# manifest and a merge all bypass the setting and none bypass this.
#
# ── what changed when one crate left the shared version ────────────────
#
# This used to compare every requirement against `[workspace.package]`'s
# version, which was the same statement only while every crate shared it.
# `system-resolver` has its own now — a shared version cannot leave
# pre-release, and inside a pre-release `cargo semver-checks` skips all of
# its lints — so *the workspace's number* and *the target's number* stopped
# being one fact.
#
# The check is stronger for it rather than weaker: it resolves each
# requirement to the crate it names and compares against **that** crate's
# version. The old rule is what this one implies for every crate still in
# the shared group, and the exception needs no entry in a list here —
# which matters, because a list of exceptions is a second place to
# remember, and the one that rots.
#
# It reads `cargo metadata` rather than the manifests, so a requirement
# cannot hide behind formatting, and dev- and build-dependencies are seen
# for free — the previous grep found any line carrying a `path`, which
# happened to cover them and did not promise to.
set -uo pipefail

meta=$(cargo metadata --format-version 1 --no-deps 2>&1) || {
  echo "::error::cargo metadata failed, so this check could not run — and a check that could not run must not pass"
  printf '%s\n' "$meta" | tail -5
  exit 1
}

printf '%s' "$meta" | python3 -c '
import json, sys

meta = json.load(sys.stdin)
root = meta["workspace_root"]
# name -> the version that crate actually is
version_of = {p["name"]: p["version"] for p in meta["packages"]}

bad = 0
checked = 0
# A path dependency with no version at all is legitimate here and is
# skipped rather than flagged: `hclient-fetch` and `hclient-native`
# dev-depend on `hclient` that way on purpose, because a version-carrying
# dev-dependency makes a cycle cargo refuses at package time.
unversioned = 0

for pkg in meta["packages"]:
    for dep in pkg["dependencies"]:
        if not dep.get("path"):
            continue
        target = version_of.get(dep["name"])
        if target is None:
            continue                      # a path outside this workspace
        req = dep["req"]
        if req == "*":
            unversioned += 1
            continue
        checked += 1
        # The convention here is a literal copy of the target version, which
        # cargo normalises to a caret requirement. Anything else — a range, a
        # pin, a wildcard — is a deliberate statement somebody should have to
        # write down, so it is reported rather than accepted.
        if req != "^" + target:
            bad = 1
            where = pkg["manifest_path"]
            if where.startswith(root + "/"):
                where = where[len(root) + 1:]
            print("::error::%s requires %s of %s, which is at %s"
                  % (where, req, dep["name"], target))

# Fail closed on the search itself: a query that matched nothing and a tidy
# tree print the same thing otherwise — this workspace own recurring defect.
if checked < 40:
    print("::error::only %d in-workspace requirements found — the check looked in the "
          "wrong place rather than finding a tidy tree" % checked)
    sys.exit(1)

if not bad:
    groups = sorted({version_of[n] for n in version_of})
    print("versions-agree: %d in-workspace requirements, all naming their target; "
          "%d path-only by design; versions in use: %s"
          % (checked, unversioned, ", ".join(groups)))
sys.exit(bad)
'
