# hclient

**An HTTP client complete enough to build a new curl on — or a browser.**

The same application code runs on a native socket, on `wasi:http`, on the
browser's own `fetch` and on Apple's `URLSession`. The transport is swapped
out, not buried under `#[cfg]`.

```
cargo add hclient@0.1.0-alpha.1 --features default-transport
```

**HTTP/2 and HTTP/3 are two more words in the same place**, and nothing
else changes — `Client::new()` and every line after it are identical:

```
cargo add hclient@0.1.0-alpha.1 --features default-transport,default-http2,default-http3
```

`default-http3` makes the default transport route by the origin's HTTPS
record, the way a browser does: QUIC where the record advertises `h3`, TCP
otherwise. It is off unless asked because Cargo unifies features — a
default here would open UDP sockets for a caller who never said so.

The version is explicit because this is a **pre-release**, which `cargo
add` will not select on its own — deliberately: six public types took a
breaking change in the month before it, so the seams are young and saying
so is the point. `0.1.0` follows when they stop moving.

That feature is what `Client::new()` needs, and it is **not** on by default
on purpose: Cargo unifies features across a graph, so a default here would
be a floor — a caller who asked for a small graph would get tokio and
rustls anyway, because something else in the graph asked for them. The flag
is the cost of not doing that to them. With it, this compiles as written:

```rust
let client = hclient::Client::new()?;          // needs an ambient tokio runtime
let text = client.get("https://example.com")
    .send().await?
    .collect().await?
    .text()?;
```

The same two lines in a browser, on `wasm32-unknown-unknown`. `Client::new()`
is infallible there, so there is no `?` on it — that is the only difference.

## `Client` names no type parameters

```rust
fn refresh(client: &hclient::Client) { /* .. */ }
```

That is the whole signature. The transport and the clock live behind an
`Arc` inside, so a library taking a client does not restate its callee's
bounds — this crate's own portable example lost four `where` lines and a
type parameter to it. `Client` is `Clone` with `Arc` semantics and is
`Send + Sync`.

The price is one thing and it is stated here rather than discovered: **a
response body is not `Send`**, so it cannot be moved into a
`tokio::spawn`. One body type has to serve every backend, and the browser's
holds a `dyn Stream` with no auto trait — declaring `Send` would not weaken
the browser backend, it would exclude it. A caller who needs the concrete
transport back, to spawn a body or to lend a `Native` to a WebSocket
connector, asks with `Client::transport_as::<T>()`.

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
and `public-suffix`.

## `Client::new()` honours the machine's proxy

`HTTP_PROXY` and `HTTPS_PROXY` where the environment names them, the
registry on Windows, the dynamic store on macOS — read once, at
construction, with no feature to turn on and no call to make. A client
that ignored them would be the one program on the machine that does, and
a port from curl or reqwest would silently start going direct.

**`default_transport()` does not**, and the asymmetry is deliberate: it
is the seam for *configuring* a transport, and a step that silently
installed a proxy would change what the calls after it do — `unix_socket`
refuses when a proxy is configured, so the same chain would fail on
machines that happen to have an `HTTP_PROXY` and not on others. The
convenience constructor is the good citizen; the seam does exactly what it
is told. A caller who wants both writes it:

```rust
let transport = hclient::default_transport()?.system_proxy()?;
```

The difference between those two lines and the constructor is what
happens to a configuration this client cannot express in full — a PAC
script, or a machine naming a SOCKS proxy as well. `system_proxy()`
**refuses**, naming it, because its caller asked and can decide.
`Client::new()` **installs what it can**, because a constructor that
refused would be a client that will not start on a network with WPAD,
which is a worse answer than proxying what we can. A machine with a PAC
script and a static proxy beside it gets the static one — WinINET's own
fallback, not an invention of ours.

Proxying explicitly needs the `proxy` feature; reading the machine comes
with `default-transport`.

## Related crates

`hclient-native` (TCP, TLS, h1/h2, and h3 behind a feature),
`hclient-fetch`, `hclient-wasi`, `hclient-urlsession`; `hclient-proxy`
(the proxy protocols, sans-io, and the OS's own settings); runtimes
`hclient-rt-tokio`, `-smol`, `-embassy`; TLS `hclient-tls-rustls`,
`-native-tls`; resolvers `hclient-dns-system`, `-doh`, `-hickory`.

See the [repository](https://github.com/GamePad64/hclient), and `AGENTS.md`
in it for why each piece is there — every seam records the argument that
produced it and the measurement that settled it.

## Licence

MIT or Apache-2.0, at your option.
