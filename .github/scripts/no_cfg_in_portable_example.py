import re
import sys
from pathlib import Path

src = Path("crates/http-ng/examples/portable.rs").read_text(encoding="utf-8")
# Comment lines first, then string literals over what is left --
# in that order, and the string pass over the WHOLE text rather
# than line by line. The example's `main` prints a string that
# mentions `#[cfg]` and wraps it across two lines with a trailing
# backslash, so a per-line pass sees neither of the two quotes
# paired and leaves the mention standing. `re.S` is what lets
# `\\.` step over that escaped newline. Measured: without either,
# this scan goes red on the honest file.
code = "\n".join(
    line for line in src.splitlines() if not line.lstrip().startswith("//")
)
code = re.sub(r'"(?:[^"\\]|\\.)*"', '""', code, flags=re.S)

hits = [
    m.group(0)
    for m in re.finditer(r"#!?\[\s*cfg[_a-z]*|\bcfg!\s*\(", code)
]
if hits:
    print(
        "::error file=crates/http-ng/examples/portable.rs::"
        f"the acceptance example must contain no cfg switch, found: {hits}. "
        "If it needs one to build, the Transport shape is wrong and that is "
        "the finding — do not add the cfg."
    )
    sys.exit(1)
print("OK: no cfg switch in the acceptance example")
