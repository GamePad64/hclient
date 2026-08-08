#!/usr/bin/env bash
#
# `unsafe` is not scanned for here. It cannot appear: every crate root
# carries `#![forbid(unsafe_code)]`, and a `forbid` cannot be lifted by a
# local `#[allow]` — rustc refuses to compile the crate.
#
# The attribute, not the manifest, is what this checks, and that is a
# deliberate change of target. The workspace `[lints.rust]` table sets the
# same lint, but a crate only inherits it by opting in with `[lints]
# workspace = true`, and a crate that drops that line loses the guarantee
# with nothing in the source to show it. The attribute in `lib.rs` is
# enforced by the compiler on every build, on every machine, whether or not
# CI runs — so CI's remaining job is only to notice if someone deletes it.
# The manifest table stays as it is: harmless, and it also covers targets
# without a crate root of their own.
#
# Two crates deliberately stand outside, named below. In those two, every
# `unsafe`/`allow(unsafe_code)` site must name the spec amendment that
# justifies it.
set -uo pipefail

EXEMPT="http-ng-fetch http-ng-dns-system"   # amendments C7 and C8
fail=0

shopt -s nullglob
roots=(crates/*/src/lib.rs)
if [ ${#roots[@]} -eq 0 ]; then
  echo "::error::crates/*/src/lib.rs matched nothing — the workspace layout changed, or this glob is broken; the check must run, not pass"
  exit 1
fi

for r in "${roots[@]}"; do
  crate="$(basename "$(dirname "$(dirname "$r")")")"
  case " $EXEMPT " in *" $crate "*) continue ;; esac
  if ! grep -qF '#![forbid(unsafe_code)]' "$r"; then
    echo "::error::$r has no \`#![forbid(unsafe_code)]\`, so rustc no longer refuses \`unsafe\` in $crate. If it genuinely needs unsafe, it needs a spec amendment and a place in this script's EXEMPT list — not a quiet deletion."
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

[ "$fail" -eq 0 ] && echo "unsafe-code policy: ${#roots[@]} crate roots, ${EXEMPT// /, } exempt and marked"
exit "$fail"
