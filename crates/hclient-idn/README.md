# hclient-idn

**UTS 46 domain-to-ASCII, choosing its implementation by target.**

On Windows it is `icuuc.dll` through `windows-sys`, on Apple platforms
Foundation, and elsewhere `idna`. That is 13 crates on Windows and 15 on macOS
against 36 on Linux, all with IDN working, and it is the reason this crate
exists: `idna` pulls
in the ICU data crates, about 4 MB of vendored Unicode tables, which on a
part with 512 KB of flash is the entire budget.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
