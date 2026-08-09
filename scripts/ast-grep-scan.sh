#!/usr/bin/env bash
#
# `ast-grep` over `scripts/ast-grep/rules`, plus the two things ast-grep does
# not do for us.
#
# **It does not fail closed.** A rule whose `files:` glob matches nothing
# scans nothing and reports success — the exact defect docs/ci.md's first rule
# exists to prevent, and the one every `tree-guard.sh` call already guards
# against. So each rule's globs are expanded here first, and a glob that
# matches no file is an error even though ast-grep would have been happy.
#
# **Its rules are code and have their own tests.** `ast-grep test` runs the
# corpus in `scripts/ast-grep/rule-tests` — the inputs each rule was accepted
# against, recorded when it replaced a hand-written scanner. Running it before
# the scan means a rule that has quietly stopped matching what it was written
# for reports THAT, rather than reporting a clean tree.
set -uo pipefail

cd "$(dirname "$0")/.."
shopt -s nullglob globstar

command -v ast-grep >/dev/null || {
  echo "::error::ast-grep is not on PATH (cargo binstall ast-grep). Note that /usr/bin/sg is util-linux's set-group, not this."
  exit 1
}

rules=(scripts/ast-grep/rules/*.yml)
[ ${#rules[@]} -gt 0 ] || {
  echo "::error::scripts/ast-grep/rules/*.yml matched nothing — the rules moved or the glob is broken; the check must run, not pass"
  exit 1
}

fail=0
for rule in "${rules[@]}"; do
  # The `files:` block is a flat list of globs, ours, one per line. A rule
  # without one would scan the whole workspace; that is never what these
  # rules mean, so it is an error rather than a default.
  globs="$(awk '/^files:/ {inlist=1; next} inlist && /^  - / {sub(/^  - /, ""); print; next} inlist {exit}' "$rule")"
  if [ -z "$globs" ]; then
    echo "::error::$rule declares no \`files:\` globs — it would scan the whole workspace; say what it is about"
    fail=1
    continue
  fi
  while IFS= read -r g; do
    [ -n "$g" ] || continue
    matched=($g)
    if [ ${#matched[@]} -eq 0 ]; then
      echo "::error::$rule scopes itself to \`$g\`, which matches no file — a renamed crate or moved module must fail this check, not silently empty it"
      fail=1
    fi
  done <<EOF
$globs
EOF
done
[ "$fail" -eq 0 ] || exit 1

ast-grep test || exit 1
ast-grep scan || exit 1
echo "ast-grep: ${#rules[@]} rules, all scoped to files that exist"
