#!/usr/bin/env bash
#
# No `Send`/`Sync` bound declared in the core surface. Erasing a type behind
# `dyn Trait` drops auto-traits (spec amendment C1), so declaring the bound in
# the seam forces it on backends that cannot satisfy it. A site that genuinely
# needs one carries a `send-bound-exception: amendment-C…` marker naming the
# amendment that excuses it.
#
# ── why this one is still a grep, when its three neighbours are ast-grep ──
#
# It was tried. ast-grep expresses the DETECTION better than this grep does —
# `Send` in a `trait_bounds` or a `bounded_type` is a node, so prose and string
# literals stop being false positives that a second grep has to subtract. What
# it cannot express is the EXCEPTION, and the exception is the whole design.
#
# The marker is a trailing comment on the SAME LINE as the bound. ast-grep's
# rule language has relational operators over the tree — `inside`, `has`,
# `follows`, `precedes` — and no notion of a line. `precedes` was measured
# against this tree and is wrong in both directions:
#
#   - it MISSES markers on `field_declaration` and `enum_variant`, where the
#     separating `,` sits between the node and its comment, so
#     `crates/hclient-core/src/error.rs:64` and `body.rs:58` would be reported
#     as unexcused when they are marked;
#   - it EXCUSES too much when it does fire, because with `stopBy: end` any
#     ancestor followed by a marker comment excuses everything inside it —
#     measured: `caps.rs:230` came out excused by the marker belonging to
#     `caps.rs:231`, one line further down and on a different bound.
#
# ast-grep's own suppression comments (`// ast-grep-ignore: <rule>`) are
# line-oriented and would work, but they must sit on the line ABOVE the match
# and would replace a marker convention the spec amendments name by hand. That
# is a change to 38 sites across seven crates, in service of the tool rather
# than of the check.
#
# So this stays as it was, moved out of ci.yml unchanged. A scanner that is
# honest is better than a rule that is quiet.
#
# The string-literal false positive above stopped being hypothetical on
# 2026-08-28: a `#[diagnostic::on_unimplemented]` note explaining the
# `Send + Sync` box tripped it. The resolution was to reword the note —
# `Send` and `Sync` says the same thing and matches nothing — rather than to
# teach the scanner about strings. A marker there would have been a lie: it
# was not a bound, and the markers name amendments that excuse bounds.
set -uo pipefail

cd "$(dirname "$0")/.."
shopt -s nullglob

all=(crates/*/src)
[ ${#all[@]} -gt 0 ] || {
  echo "::error::crates/*/src matched no directory — the glob is broken or the layout changed"
  exit 1
}

# Excluded by name: these three are runtime/system crates where the bound is
# real, not leaked through an erased seam.
dirs=()
for d in "${all[@]}"; do
  case "$d" in
    crates/hclient-rt-tokio/src|crates/hclient-rt-smol/src|crates/hclient-dns-system/src) ;;
    *) dirs+=("$d") ;;
  esac
done
[ ${#dirs[@]} -gt 0 ] || {
  echo "::error::every crate was excluded — the exclusion set has outgrown the workspace and this check now scans nothing"
  exit 1
}

if grep -rnE '(:|\+)[[:space:]]*(Send|Sync)\b|MaybeSend' "${dirs[@]}" \
     | grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|/\*|\*)' \
     | grep -vE 'send-bound-exception:[[:space:]]*amendment-C(1|2|5|10|12|14|15|16|17)\b'; then
  echo "::error::the core declares a Send or Sync bound without a send-bound-exception marker — erasing a type behind dyn Trait drops auto-traits (amendment C1), so the bound would be forced on backends that cannot satisfy it"
  exit 1
fi
echo "no undeclared Send/Sync bounds across ${#dirs[@]} directories"
