"""`hclient-dns-system` becomes an adapter: it gains `system-resolver`,
loses `windows-sys`, and goes back to the workspace lint table."""

import re
from pathlib import Path

p = Path("crates/hclient-dns-system/Cargo.toml")
t = p.read_text()

old = 'hclient-rt   = { path = "../hclient-rt",  version = "0.1.0-alpha.2" }\nthiserror      = { workspace = true }'
new = '''hclient-rt   = { path = "../hclient-rt",  version = "0.1.0-alpha.2" }
# Every platform call this crate used to make. It owns `res_query`,
# `android_res_nquery`, `DnsQueryRaw` and `DnsQuery_UTF8`, and hands back
# records with their RDATA; what is left here is RFC 9460's client rules
# over one of those records. Not optional and not target-gated: the crate
# compiles on every target and answers `Support::None` where it has no
# backend, which is what keeps `supports_svcb` a single statement.
system-resolver = { path = "../system-resolver", version = "0.1.0-alpha.2" }
thiserror      = { workspace = true }'''
assert t.count(old) == 1
t = t.replace(old, new)

# The Windows dependency, and the paragraph explaining it, leave with the
# code that called it.
start = t.index("# Windows only.")
end = t.index("\n\n", t.index("] }", start))
t = t[:start] + t[end + 2 :]

# The lint table opt-out existed for `unsafe_code`, which this crate no
# longer has.
start = t.index("# NOT `[lints] workspace = true`")
end = t.index("\n\n", t.index("unexpected_cfgs", start))
t = t[:start] + "[lints]\nworkspace = true\n" + t[end + 1 :]

p.write_text(t)
print("manifest updated")
