#!/usr/bin/env bash
# Every error type lives in a file named `error.rs`.
#
# The rule is about the *file name* rather than about one file per crate,
# which is what lets `hclient-native/src/http2/error.rs` and
# `hclient/src/cookie/error.rs` be legal: a self-contained subsystem may
# keep its own, and the boundary is written in each crate's module doc.
# What it forbids is an error type sitting in `lib.rs` or beside the code
# that raises it, which is the state the convention ended.
#
# Detection is deliberately two greps, not one. The survey that sized this
# work looked for `thiserror::Error` alone and undercounted: three crates
# write `impl Display` + `impl Error` by hand, and those are exactly the
# types nobody had looked at recently.
#
# A `#[cfg(test)] mod` is exempt, and that is about who reads the type
# rather than about leniency: the convention exists so a caller looking
# for what a crate can refuse finds one file, and a fixture that exists to
# make one test fail has no such reader. `hclient-fetch`'s `Torn` is the
# subject. The exemption is by brace counting rather than by "after the
# first `#[cfg(test)]`", so a test module in the middle of a file does not
# blind the check to everything below it.
set -uo pipefail

# Blank out `#[cfg(test)]` modules, keeping the line count so that the
# numbers this check prints are the file's own.
strip_test_mods() {
  awk '
    /^#\[cfg\(test\)\]/ && !depth { pending = 1; print ""; next }
    pending && /\{/ { pending = 0; depth = 1; print ""; next }
    depth {
      n = gsub(/\{/, "{"); m = gsub(/\}/, "}")
      depth += n - m
      print ""
      next
    }
    { print }
  ' "$1"
}

bad=0
found=0
while IFS= read -r f; do
  case "$(basename "$f")" in error.rs) continue ;; esac
  hits=$(strip_test_mods "$f" | grep -nE '#\[derive\([^)]*thiserror::Error|^impl (std::error::)?Error for ' || true)
  [ -z "$hits" ] && continue
  found=1
  bad=1
  echo "::error::$f defines an error type outside an \`error.rs\`:"
  printf '%s\n' "$hits" | sed 's/^/    /'
done < <(find crates -path '*/src/*' -name '*.rs' -not -path '*/tests/*')

if [ "$bad" -eq 0 ]; then
  # Fail closed on the search itself: a `find` that matched nothing, or a
  # `grep` that never ran, would otherwise be indistinguishable from a
  # clean tree — this workspace's own recurring defect.
  n=$(find crates -path '*/src/*' -name 'error.rs' | wc -l)
  if [ "$n" -lt 15 ]; then
    echo "::error::only $n \`error.rs\` files found — the check looked in the wrong place rather than finding a tidy tree"
    exit 1
  fi
  echo "errors-live-in-error-rs: $n error modules, none defined elsewhere"
fi
exit "$bad"
