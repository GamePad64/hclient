# hclient

A cross-platform async HTTP client. The same application code runs on a
native socket, on `wasi:http`, on the browser's `fetch` and on Apple's
`URLSession` — the transport is a type parameter at construction, not a
`#[cfg]` inside your code.

```
cargo add hclient --features default-transport
```

```rust
let client = hclient::Client::new()?;          // needs an ambient tokio runtime
let text = client.get("https://example.com")
    .send().await?
    .collect().await?
    .text()?;
```

The same two lines work in a browser on `wasm32-unknown-unknown`, where
`Client::new()` is infallible, so there is no `?`.

`default-transport` is not a default feature. Cargo unifies features
across a dependency graph, so a default here would be a floor: a crate
that wanted a small graph would get tokio and rustls anyway because some
other crate in the tree asked for them.

## HTTP/2 and HTTP/3

```
cargo add hclient --features default-transport,default-http2,default-http3
```

Nothing else in your code changes. `default-http3` makes the default
transport pick a stack from the origin's HTTPS record, the way a browser
does: QUIC where the record advertises `h3`, TCP otherwise. It is opt-in
because a default that opens UDP sockets is a decision a caller should
make.

## What is in it

HTTP/1.1, HTTP/2 and HTTP/3; pooled connections and optional h2
multiplexing; streaming request and response bodies, with full duplex on
h3. Cookies, an RFC 9111 cache, redirects, decompression (gzip, deflate,
brotli, zstd), proxies (HTTP `CONNECT`, SOCKS5, SOCKS4/4a), digest auth,
`multipart/form-data` and server-sent events. Each of those is one
implementation shared by every backend, so the behaviour does not change
when you change transport.

WebSocket and WebTransport are separate crates behind their own traits, so
a transport that cannot do them is a compile error rather than a runtime
refusal.

Almost everything is a feature and almost nothing is on by default. The
defaults are `idn` and `public-suffix`.

## Capabilities are checked at `build()`

A transport reports what it supports, and `ClientBuilder::build()` refuses
a setting the transport cannot honour rather than ignoring it. Setting a
cookie jar on the browser backend is an error, because the browser keeps
its own and would store every `Set-Cookie` twice.

## Version

This is a pre-release. Six public types took a breaking change in the
month before the first one, so the seams are still moving and saying so is
the point. `0.1.0` follows when they stop.

`cargo add` picks the pre-release today because there is no stable release
to prefer. Once `0.1.0` exists it takes that instead.

## Related crates

Transports: `hclient-native`, `hclient-fetch`, `hclient-wasi`,
`hclient-urlsession`, `hclient-winhttp`, `hclient-mock`.
Runtimes: `hclient-rt-tokio`, `hclient-rt-smol`.
TLS: `hclient-tls-rustls`, `hclient-tls-native-tls`.
Resolvers: `hclient-dns-system`, `hclient-dns-doh`, `hclient-dns-hickory`.
Also `hclient-tower`, `hclient-tungstenite`, `hclient-webtransport`,
`hclient-otel`, and the `hc` command-line client in `hclient-cli`.

## Licence

MIT or Apache-2.0, at your option.
