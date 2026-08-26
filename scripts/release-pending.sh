#!/usr/bin/env bash
# Which publishable crates have changes since the version they last
# published, and which have none.
#
# **Not a CI gate, and deliberately.** It asks crates.io over the network,
# so it is flaky in a way a gate must not be, and it answers a question
# nobody needs answered on every push. Run it before a release.
#
# The anchor is a git tag. `[workspace.metadata.release]` sets
# `tag-name = "v{{version}}"`, so every release cargo-release makes leaves
# one tag naming the version it published — and with `shared-version` that
# one tag covers whichever crates went out under it. A crate whose last
# published version has no tag cannot be compared against anything, and
# this script says so rather than guessing a commit.
set -uo pipefail

UA="hclient-release-pending (+https://github.com/Shishenko/hclient)"
root="$(git rev-parse --show-toplevel)"
cd "$root" || exit 1

# The sparse index path: 1/x, 2/xx, 3/x/xxx, else aa/bb/name.
index_path() {
  local c=$1
  case ${#c} in
    1) printf '1/%s' "$c" ;;
    2) printf '2/%s' "$c" ;;
    3) printf '3/%s/%s' "${c:0:1}" "$c" ;;
    *) printf '%s/%s/%s' "${c:0:2}" "${c:2:2}" "$c" ;;
  esac
}

published_version() {
  local c=$1 body
  body=$(curl -sS --max-time 20 -A "$UA" "https://index.crates.io/$(index_path "$c")" 2>/dev/null) || return 1
  # Last non-yanked line wins; the index is append-ordered.
  printf '%s' "$body" | grep -o '"vers":"[^"]*"' | tail -1 | cut -d'"' -f4
}

changed=0 clean=0 unpublished=0 unanchored=0
pending=()

for d in crates/*/; do
  c=$(basename "$d")
  grep -q '^publish = false' "$d/Cargo.toml" 2>/dev/null && continue

  v=$(published_version "$c")
  if [ -z "$v" ]; then
    printf '  %-24s never published\n' "$c"
    unpublished=$((unpublished + 1))
    pending+=("$c")
    continue
  fi

  tag="v$v"
  if ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    printf '  %-24s published %s — NO TAG %s, cannot compare\n' "$c" "$v" "$tag"
    unanchored=$((unanchored + 1))
    continue
  fi

  if git diff --quiet "$tag" HEAD -- "$d"; then
    printf '  %-24s %s — unchanged\n' "$c" "$v"
    clean=$((clean + 1))
  else
    n=$(git diff --name-only "$tag" HEAD -- "$d" | wc -l | tr -d ' ')
    printf '  %-24s %s — CHANGED (%s files)\n' "$c" "$v" "$n"
    changed=$((changed + 1))
    pending+=("$c")
  fi
done

echo
printf 'changed %s · unchanged %s · never published %s · unanchored %s\n' \
  "$changed" "$clean" "$unpublished" "$unanchored"

if [ "$unanchored" -gt 0 ]; then
  cat <<'EOF'

No tag for a published version means there is nothing to diff against.
cargo-release writes one on every release it makes; a release made any
other way leaves none. Plant the missing tag on the commit that was
published and this answers properly from then on:

    git tag -a vX.Y.Z <commit> -m "hclient X.Y.Z" && git push origin vX.Y.Z
EOF
fi

total=$((changed + clean + unpublished))
if [ ${#pending[@]} -eq 0 ]; then
  :
elif [ ${#pending[@]} -eq "$total" ] && [ "$total" -gt 0 ]; then
  # Everything changed, which is what the policy publishes anyway.
  echo
  echo "cargo release <level>"
else
  # The selecting form, for the policy `docs/publishing.md` §5 keeps
  # rather than the one it uses. Built with a loop: `${a[*]// / -p }`
  # looks like it works and does not — it leaves the `-p` off every
  # entry but the first, and a half-right command someone pastes is
  # worse than none.
  line="cargo release"
  for c in "${pending[@]}"; do line="$line -p $c"; done
  echo
  echo "$line <level>"
fi
