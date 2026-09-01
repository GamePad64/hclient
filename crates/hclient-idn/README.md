# hclient-idn

UTS 46 domain-to-ASCII, choosing its implementation by target.

On Windows it uses `icuuc.dll`, on Apple it uses Foundation, and elsewhere
it uses the `idna` crate. That keeps the Unicode tables out of the binary
on the two platforms that already ship them, without changing the answer:
the crate verifies the platform's result by round-tripping it.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
