#!/usr/bin/env bash
#
# The shared shape of every `cargo tree` invariant in ci.yml. All of them
# have the same three outcomes, and only the last is the invariant:
#
#   - cargo tree itself failed   -> a check that cannot run must not pass
#                                   (this also covers a renamed or deleted
#                                   crate: `-p missing` exits non-zero)
#   - cargo tree printed nothing -> an empty tree satisfies every
#                                   "must not contain" trivially
#   - the pattern (mis)matched   -> the thing we actually care about
#
# The first two are why this is a script and not a `cargo tree | grep`
# pipeline: written inline, they were forgotten more often than not, and a
# silently green check is the defect this repository keeps finding.
#
# usage: tree-guard.sh absent|present <extended-regex> <message> -- <cargo tree args…>
set -uo pipefail

mode="$1"
pattern="$2"
message="$3"
shift 3
[ "${1:-}" = "--" ] && shift

out="$(cargo tree "$@" -e normal --prefix none 2>&1)"
rc=$?
if [ "$rc" -ne 0 ]; then
  echo "::error::cargo tree failed (exit $rc) for [$*] — a check that cannot run must not pass"
  printf '%s\n' "$out"
  exit 1
fi
if [ -z "$out" ]; then
  echo "::error::cargo tree produced no output for [$*] — an empty dependency tree means the check did not run, not that it passed"
  exit 1
fi

case "$mode" in
  absent)
    if printf '%s\n' "$out" | grep -qE "$pattern"; then
      echo "::error::$message"
      printf '%s\n' "$out" | grep -E "$pattern"
      exit 1
    fi
    echo "OK — nothing matching /$pattern/ in [$*]"
    ;;
  present)
    if ! printf '%s\n' "$out" | grep -qE "$pattern"; then
      echo "::error::$message"
      printf '%s\n' "$out"
      exit 1
    fi
    echo "OK — /$pattern/ present in [$*]"
    ;;
  *)
    echo "::error::tree-guard.sh: unknown mode '$mode' (expected absent|present)"
    exit 1
    ;;
esac
