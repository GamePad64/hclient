# hclient-dns-doh

`Resolve` over DNS-over-HTTPS.

The DoH server's own name has to be resolved somehow, and which way is
visible in the type: `Doh::pinned` takes an IP literal and refuses a name,
`Doh::bootstrapped` takes a name and refuses a literal. Failing closed is
the default.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
