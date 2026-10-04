#!/usr/bin/env bash
# A doc comment is written for whoever reads docs.rs; a maintainer's note
# is a `//` comment.
#
# The two audiences are both real, and this repository serves the second
# at length — AGENTS.md, `.notes/`, and `//` blocks headed
# `// Maintainer notes (not rendered):` beside the items they explain. What
# this check keeps apart is the *place*: a doc comment that sends its
# reader to `.notes/`, AGENTS.md, an amendment number, a commit hash, a
# work-item label or a task number sends them somewhere a docs.rs reader
# cannot follow. Those markers are the unambiguous half of the rule — a
# history sentence or a measurement can be right in a doc when it tells a
# caller what to expect, so they are a matter of review, and only what can
# never be followed is gated.
#
# Doc comments on private items count too. They are not rendered, but
# `--document-private-items` renders them, and a rule with an exception
# for visibility is one a refactor that widens an item breaks silently.
#
# Tests, examples and benches are out of scope: they are not documentation
# anyone installs.
#
# Fails closed when it scanned no file, because a glob that matched
# nothing and a clean tree print the same thing.
set -euo pipefail

if ! command -v rg >/dev/null 2>&1; then
    echo "::error::ripgrep (rg) is not on PATH — the scan cannot run, and reporting a pass over a scan that did not run is the defect this script exists to prevent"
    exit 1
fi

pattern='^\s*//[/!].*(\.notes/|AGENTS\.md|CLAUDE\.md|amendment[- ]C[0-9]+|`[0-9a-f]{7,10}`|\bv0\.[0-9]+ W[0-9]+\b|\bW[0-9]+\b|\b[Vv]ertical [0-9]\b|\bTask [0-9]+\b)'

files=$(find crates -path '*/src/*.rs' -not -path '*/target/*' | sort)
count=$(printf '%s\n' "$files" | grep -c . || true)
if [ "$count" -eq 0 ]; then
    echo "::error::no source file was scanned — a check over nothing is not a check"
    exit 1
fi

set +e
# The file list goes in as arguments, not through xargs: xargs answers
# exit 123 for "a child exited 1-125" and cannot tell "no match" from
# "error", which would turn the error path below into a false pass.
# The whitespace model is the one `find` output already had.
hits=$(rg -n "$pattern" $files)
status=$?
set -e

if [ "$status" -gt 1 ]; then
    echo "::error::ripgrep itself failed (exit $status) — reporting a pass over a scan that errored is the defect this script exists to prevent"
    echo "$hits" >&2
    exit 1
fi

if [ "$status" -eq 0 ]; then
    printf '%s\n' "$hits" | while IFS= read -r h; do
        echo "::error::${h%%:*}: a doc comment names something a docs.rs reader cannot follow — move it to a \`//\` maintainer note: ${h#*:}"
    done
    exit 1
fi
echo "doc-comments-speak-to-readers: $count source files, no doc comment points past docs.rs"
