# hclient-dns

The `Resolve` trait for `hclient`, and the SVCB/HTTPS record types.

Implement `Resolve` to give the client a name resolver of your own. The
`codec` feature adds decoding of SVCB parameters from wire format; without
it the crate carries no DNS decoder at all.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
