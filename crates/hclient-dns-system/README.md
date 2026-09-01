# hclient-dns-system

`Resolve` over the platform's own resolver.

Addresses come from `getaddrinfo`, and HTTPS/SVCB records come from
[`system-resolver`](https://crates.io/crates/system-resolver), which asks
the platform's resolver API directly. Both therefore see whatever the
machine is configured to do — a VPN's split DNS, per-interface servers,
Android's Private DNS — and use the system cache.

This is what `hclient`'s default transport uses. The SVCB records are what
let it discover HTTP/3 without waiting for an `Alt-Svc` header on a
previous response.

`supports_svcb()` answers whether this build can ask for record type 65 at
all, so a caller learns before spending a query rather than after. Not
every platform can: see `system-resolver` for which and why.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
