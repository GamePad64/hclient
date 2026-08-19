# http-ng

**An HTTP client complete enough to build a new curl on — or a browser.**

The same application code runs on a native socket, on `wasi:http`, on the
browser's own `fetch` and on Apple's `URLSession`. The transport is swapped
out, not buried under `#[cfg]`.

```rust
let client = http_ng::Client::new()?;          // needs an ambient tokio runtime
let text = client.get("https://example.com")
    .send().await?
    .collect().await?
    .text()?;
```

The same two lines in a browser, on `wasm32-unknown-unknown`. `Client::new()`
is infallible there, so there is no `?` on it — that is the only difference.

## No backend may claim a capability it does not have

This is enforced rather than asked for. A cookie jar configured against a
transport that keeps its own — the browser does — is an error at `build()`,
not a value quietly dropped, and the same holds for a redirect policy
against a backend that follows redirects internally, for response
decompression, for a response cache, and for each timeout separately.
`Capabilities` is read from the component that knows, never from a
constant.

## What is behind the seams

HTTP/1.1, HTTP/2 and HTTP/3 are all spoken; connections are pooled and h2
can be multiplexed on request; request and response bodies stream, with
real full duplex on h3. Cookies, an RFC 9111 cache, redirects,
decompression (gzip, deflate, brotli, zstd), proxies (HTTP `CONNECT`,
SOCKS5, SOCKS4/4a), digest auth, `multipart/form-data` and SSE are each one
implementation shared by every backend, which is what makes "the same
answers everywhere" more than a slogan.

WebSocket and WebTransport are their own crates behind their own traits,
deliberately: a transport that cannot do them should be a compile error
rather than a runtime refusal.

Almost everything is a feature and almost nothing is on by default — a
build should carry nothing it did not ask for. `default` here is `idn`
alone.

## Related crates

`http-ng-native` (TCP, TLS, h1/h2), `http-ng-h3` (QUIC),
`http-ng-select` (chooses between the two from the origin's HTTPS record),
`http-ng-fetch`, `http-ng-wasi`, `http-ng-urlsession`; runtimes
`http-ng-rt-tokio`, `-smol`, `-embassy`; TLS `http-ng-tls-rustls`,
`-native-tls`; resolvers `http-ng-dns-system`, `-doh`, `-hickory`.

See the [repository](https://github.com/actcore/http-ng), and `AGENTS.md`
in it for why each piece is there — every seam records the argument that
produced it and the measurement that settled it.

## Licence

MIT or Apache-2.0, at your option.
