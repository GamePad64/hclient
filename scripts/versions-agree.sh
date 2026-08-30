#!/usr/bin/env bash
# Every in-workspace dependency requirement names the workspace version.
#
# Cargo offers no way to write `version.workspace = true` *inside* a
# dependency requirement, so `[workspace.package].version` is one number
# and the requirements beside it are dozens of literal copies. Nothing
# checked that they agreed, and they did not: after the alpha.2 release
# every requirement still read `0.1.0-alpha.1`, because
# `dependent-version = "fix"` only rewrites one when the new version stops
# satisfying it — and `^0.1.0-alpha.1` goes on satisfying alpha.2.
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
set -uo pipefail

want=$(grep -m1 '^version *= *"' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
[ -n "$want" ] || { echo "::error::no [workspace.package] version in Cargo.toml"; exit 1; }

bad=0
seen=0
while IFS= read -r line; do
  f=${line%%:*}
  rest=${line#*:}
  n=${rest%%:*}
  text=${rest#*:}
  # An in-workspace requirement is the one carrying a `path` to a sibling.
  case "$text" in *'path = "../'*|*'path = "crates/'*) ;; *) continue ;; esac
  seen=$((seen + 1))
  got=$(printf '%s' "$text" | sed -n 's/.*version = "\([^"]*\)".*/\1/p')
  [ -z "$got" ] && continue
  if [ "$got" != "$want" ]; then
    bad=1
    echo "::error::$f:$n requires $got where the workspace is at $want"
    printf '    %s\n' "$text"
  fi
done < <(grep -n 'path = "' Cargo.toml crates/*/Cargo.toml)

# Fail closed on the search itself: a `grep` that matched nothing and a
# tidy tree print the same thing otherwise — this workspace's own
# recurring defect.
if [ "$seen" -lt 40 ]; then
  echo "::error::only $seen in-workspace requirements found — the check looked in the wrong place rather than finding a tidy tree"
  exit 1
fi

[ "$bad" -eq 0 ] && echo "versions-agree: $seen in-workspace requirements, all at $want"
exit "$bad"
