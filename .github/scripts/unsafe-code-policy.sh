#!/usr/bin/env bash
#
# `unsafe` is not scanned for here. It cannot appear: the workspace sets
# `unsafe_code = "forbid"` and a `forbid` cannot be lifted by a local
# `#[allow]` — rustc refuses to compile the crate. CI's job is the one part
# rustc cannot see: that no crate quietly steps OUT of that guarantee by
# replacing `[lints] workspace = true` with a lint table of its own.
#
# Two crates do step out, deliberately, and are named below. In those two,
# every `unsafe`/`allow(unsafe_code)` site must name the spec amendment that
# justifies it.
set -uo pipefail

EXEMPT="http-ng-fetch http-ng-dns-system"   # amendments C7 and C8
fail=0

# The guarantee the other sixteen crates rest on.
if ! grep -qE '^\s*unsafe_code\s*=\s*"forbid"' Cargo.toml; then
  echo "::error::the workspace [lints.rust] table no longer sets unsafe_code = \"forbid\" — every crate inheriting it silently lost the guarantee"
  fail=1
fi

shopt -s nullglob
manifests=(crates/*/Cargo.toml)
if [ ${#manifests[@]} -eq 0 ]; then
  echo "::error::crates/*/Cargo.toml matched nothing — the workspace layout changed, or this glob is broken; the check must run, not pass"
  exit 1
fi

for m in "${manifests[@]}"; do
  crate="$(basename "$(dirname "$m")")"
  case " $EXEMPT " in *" $crate "*) continue ;; esac
  if ! grep -qE '^\s*workspace\s*=\s*true' "$m"; then
    echo "::error::$crate does not inherit the workspace lint table, so it is no longer covered by unsafe_code = \"forbid\". If it needs local unsafe, it needs a spec amendment and a place in this script's EXEMPT list — not a quiet lint table of its own."
    fail=1
  fi
done

# The exempt two: `deny` is overridable, so each site must say why it exists.
for crate in $EXEMPT; do
  d="crates/$crate/src"
  if [ ! -d "$d" ]; then
    echo "::error::$crate is in the exempt list but crates/$crate/src does not exist — a renamed crate must fail this check, not remove it"
    fail=1
    continue
  fi
  # A marker sits on the flagged line or the line below it: cargo fmt moves
  # it there for block-opening forms such as `unsafe extern "C" {`.
  # awk once per file (`-exec … \;`, not xargs): with several files in one
  # awk process NR keeps counting across them and every reported line number
  # after the first file is wrong.
  if find "$d" -name '*.rs' -type f -exec awk '
         { l[FNR] = $0 }
         END {
           for (i = 1; i <= FNR; i++) {
             s = l[i]
             if (i < FNR && l[i+1] ~ /unsafe-code-exception:/) s = s " " l[i+1]
             print FILENAME ":" i ":" s
           }
         }' {} \; |
       grep -E '\bunsafe\b|allow\(unsafe_code\)' |
       grep -vE ':[0-9]+:[[:space:]]*(//|/\*|\*)' |
       grep -vE '(deny|forbid)\(unsafe_code\)' |
       grep -vE 'unsafe-code-exception:[[:space:]]*amendment-C[78]\b'; then
    echo "::error::$crate has an unsafe site with no \`unsafe-code-exception: amendment-C7|C8\` marker. Put the marker trailing on the flagged line, or alone on the line immediately BELOW it — a marker ABOVE the line is folded onto the preceding line and does not work."
    fail=1
  fi
done

[ "$fail" -eq 0 ] && echo "unsafe-code policy: ${#manifests[@]} crates, ${EXEMPT// /, } exempt and marked"
exit "$fail"
