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
# Three crates deliberately stand outside, named below. In those three,
# every `unsafe`/`allow(unsafe_code)` site must name the spec amendment
# that justifies it — and must do so in a file that amendment names, which
# is what the ALLOWED map below is for.
set -uo pipefail

EXEMPT="http-ng-fetch http-ng-dns-system http-ng-idn"   # amendments C7, C8 and C9

# The one file each amendment excuses, and the token it must be cited by.
# A directory is never enough: "a marker that excuses a directory excuses
# everything a future reviewer forgets to look at" (amendment C8), and the
# tokens are not interchangeable — C7's marker in a C8 file excuses
# nothing. This mapping is what makes both of those true; before it existed
# the check accepted `amendment-C[78]` anywhere under an exempt crate's
# `src`, which the spec already described it as not doing.
ALLOWED="crates/http-ng-fetch/src/promise.rs:amendment-C7
crates/http-ng-dns-system/src/sys/res_query.rs:amendment-C8
crates/http-ng-dns-system/src/sys/windows.rs:amendment-C8
crates/http-ng-idn/src/icu/windows.rs:amendment-C9"
fail=0

# A renamed or deleted excused file must fail this check, not quietly stop
# excusing anything.
while IFS=: read -r allowed_file allowed_token; do
  [ -n "$allowed_file" ] || continue
  if [ ! -f "$allowed_file" ]; then
    echo "::error::$allowed_file is named by $allowed_token but does not exist — a renamed file must fail this check, not remove it"
    fail=1
  fi
done <<EOF
$ALLOWED
EOF

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

# The exempt three: `deny` is overridable, so each site must say why it
# exists, and in which file it is allowed to.
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
  flagged="$(find "$d" -name '*.rs' -type f -exec awk '
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
       grep -vE '(deny|forbid)\(unsafe_code\)')"

  # A line survives only if its OWN file is excused and it cites that
  # file's OWN token. Both halves matter: the file half stops a correct
  # marker from excusing a sibling module, the token half stops one
  # amendment from standing in for another.
  unexcused=""
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    file="${line%%:*}"
    token="$(printf '%s\n' "$ALLOWED" | awk -F: -v f="$file" '$1 == f { print $2 }')"
    if [ -n "$token" ] && printf '%s\n' "$line" | grep -qE "unsafe-code-exception:[[:space:]]*$token\b"; then
      continue
    fi
    unexcused="$unexcused$line
"
  done <<EOF
$flagged
EOF

  if [ -n "$unexcused" ]; then
    printf '%s' "$unexcused"
    echo "::error::$crate has an unsafe site that is not excused. Each one needs an \`unsafe-code-exception: amendment-CN\` marker naming the amendment for THAT FILE (see the ALLOWED map in this script), trailing on the flagged line or alone on the line immediately BELOW it — a marker ABOVE the line is folded onto the preceding line and does not work, and a marker in a file no amendment names excuses nothing."
    fail=1
  fi
done

[ "$fail" -eq 0 ] && echo "unsafe-code policy: ${#roots[@]} crate roots, ${EXEMPT// /, } exempt and marked"
exit "$fail"
