# hclient-winhttp

`Transport` over Windows' WinHTTP.

Use it on Windows when you want the system HTTP stack rather than a
userspace one: WinHTTP's own proxy resolution including PAC and WPAD, the
platform certificate handling, and whatever policy the machine has.

The trade against `hclient-native` is control. WinHTTP owns the connection
pool, the timeouts and the TLS configuration, so the settings `hclient`
would otherwise apply are the platform's here. Where that means a
capability cannot be honoured, `build()` refuses rather than ignoring it.

Windows-only: on every other target the crate compiles to nothing.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
