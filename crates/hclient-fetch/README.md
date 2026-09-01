# hclient-fetch

`Transport` over the browser's `fetch`, for `wasm32-unknown-unknown`.

The browser attaches cookies, follows redirects and caches responses
itself, so `hclient`'s own cookie jar, redirect policy and cache are
refused at `build()` rather than being applied twice. `Fetch::opts` sets
`mode`, `credentials`, `cache` and `referrerPolicy`.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
