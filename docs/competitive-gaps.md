# Competitive gaps: what a caller can do elsewhere, and what is refused here

Written to answer one question with evidence rather than impression: **what
would a caller who arrived from `reqwest` or `ureq` find missing here**, and
which of those absences are decisions this workspace has already made and
written down.

The second half matters more than the first. This repository carries four
acceptance documents whose *Deliberately not done* sections exist precisely
because "a bare list invites someone to 'fix' an item whose absence is the
decision" (`docs/v02-acceptance.md`). A gap analysis that reported those as
gaps would be worse than no gap analysis. So every row below is classified
as **gap**, **refused** (with the rule that refuses it), or **elsewhere**
(reachable, but not by the call a `reqwest` user would write).

---

## 0. What "checked" means in this document

Three grades, and each claim says which one it is:

- **executed** — a program was built and run, and the output is quoted.
- **read** — a specific file at a specific line was opened. Cited as
  `path:line`.
- **not checked** — said in the sentence that makes the claim, never in a
  footnote.

Competitor claims are grounded in source on disk, at the version named,
because this project has been wrong about third-party crates by reading
rather than executing before — `h3-datagram` 0.0.2's `Datagram::encode`
discards the quarter stream id it just computed, which no amount of reading
the README would have shown (AGENTS.md, the datagram section). Two of the
sharpest findings below are executions for the same reason.

Versions read, all from `~/.cargo/registry/src/index.crates.io-*/`:

| crate | version | how it got there |
|---|---|---|
| `reqwest` | **0.13.4** | already vendored; 0.12.28 also present and not used |
| `ureq` | **3.4.0** | fetched for this document |
| `ureq-proto` | 0.6.1 | ureq's sans-io half |
| `isahc` | 1.8.3 | fetched |
| `curl` | 0.4.50 | fetched |
| `curl-sys` | 0.4.90+curl-8.21.0 | fetched |
| `attohttpc` | 0.29.2 | fetched |
| `gloo-net` | 0.6.0 | fetched |
| `hyper-util` | 0.1.20 | already vendored |
| `cookie_store` | 0.22.1 | fetched, to settle one row in §2.7 |
| `publicsuffix` | 2.3.0 | same |
| `rustls-platform-verifier` | 0.7.0 | already vendored, to settle one claim in §8 |
| `web-sys` | 0.3.104 | already vendored |

**`surf` is not compared.** Its latest release is 2.3.2, published
2021-11-01, with the last push to its repository in September 2023. A
client that has not shipped in four years is not a comparator; it is
history. `attohttpc` 0.29.2 was fetched and is only lightly surveyed, for
the same reason in weaker form.

`http-ng` is this tree at `96e8b28`.

---

## 1. The two executions that decided the shape of this document

**reqwest 0.13.4 does not build for `wasm32-wasip2`.** Executed, not read:

```
$ cargo check --target wasm32-wasip2      # reqwest 0.13, default-features=false, features=["rustls"]
error: Only features sync,macros,io-util,rt,time are supported on wasm.
   --> .../tokio-1.53.1/src/lib.rs:479:1
```

reqwest's wasm branch is gated on
`cfg(all(target_arch = "wasm32", any(target_os = "unknown", target_os = "none")))`
(`reqwest-0.13.4/Cargo.toml:245`), so `wasm32-wasip2` — whose `target_os` is
`wasi` — falls into the *native* branch and drags in `hyper`, `tokio` and
`mio`. `http-ng-wasi` builds for the same target in 3.9 s, also executed.

**ureq 3.4.0 does build for `wasm32-wasip2`, and it actually works.** This
one was a surprise, and it is the single most important comparator fact in
this document. Executed:

```
$ cargo build --release --target wasm32-wasip2   # ureq 3, default-features=false, features=["rustls"]
$ wasmtime run -S inherit-network ./cmp.wasm     # GET http://127.0.0.1:18080/
OK status=200 OK
$ wasmtime run -S inherit-network -S allow-ip-name-lookup ./cmp.wasm   # GET https://example.com/
OK status=200 OK
```

wasmtime 47.0.3, a real request over a real socket, TLS included. So "reaches
WASI" is not unique to this workspace. What *is* different is the route, and
the difference is not cosmetic: ureq goes through `wasi:sockets` (a raw TCP
socket, with `rustls` + `ring` compiled to wasm doing the handshake inside
the guest), where `http-ng-wasi` goes through `wasi:http` — the host's own
HTTP client, with no socket and no TLS stack in the guest at all. A component
host that grants outbound HTTP and *not* raw sockets — which is the common
sandbox policy, and the reason `wasi:http` exists — runs `http-ng-wasi` and
cannot run ureq. **Neither was tested against a host with sockets denied**,
so that last sentence is an argument from the WIT rather than a measurement.

---

## 2. The matrix

Legend: **Y** = a caller writes one call; **Y\*** = present with a stated
restriction, named in the notes; **seam** = not a call, but reachable by
implementing a public trait; **N** = absent; **—** = not checked.

Columns: **ng** = `http-ng` (all features, native), **rq** = reqwest 0.13.4,
**uq** = ureq 3.4.0, **cu** = the `curl` 0.4.50 binding / `isahc` 1.8.3,
**br** = the browser story (reqwest's wasm build, `gloo-net` 0.6.0).

### 2.1 Request shaping

| capability | ng | rq | uq | cu | br | note |
|---|---|---|---|---|---|---|
| query parameters | Y | Y | Y | — | Y | ng appends and never replaces (`request.rs:195`); rq's `query` takes `Serialize` |
| urlencoded form body | Y | Y | Y | — | Y | ng hand-writes the WHATWG serialiser rather than take `form_urlencoded` |
| JSON request body | Y | Y | Y | — | Y | ng behind `json`; serialises in the builder, so a bad value is a build error |
| `multipart/form-data` | Y | Y | Y | Y | Y | see §2.2 for the streaming difference |
| Basic auth | Y | Y | Y | Y | Y | ng **refuses a colon in the username** (RFC 7617 §2) where the others encode it |
| Bearer auth | Y | Y | Y | Y | Y | |
| Digest / NTLM / Negotiate | **Digest: Y\*** | N | N | **Y** | N | **Digest closed** — RFC 7616, `RequestBuilder::digest_auth`, MD5 / SHA-256 / SHA-512-256 and their `-sess` variants, checked against the RFC's own §3.9 vectors. The only pure-Rust client with it. NTLM and Negotiate still refused: both need the platform's GSSAPI or SSPI. The `curl` crate binds all of them: `Auth` + `Easy::http_auth` over `CURLAUTH_DIGEST`, `_DIGEST_IE`, `_GSSNEGOTIATE`, `_NTLM`, `_NTLM_WB` (`curl-0.4.50/src/easy/handler.rs:565, :1340, :3733-3790`). See §3 G12 |
| client-wide default headers | Y | Y | Y | — | Y | closed — `ClientBuilder::{default_header, default_headers}`, applied per redirect hop, the caller's own header winning, and refused at `build()` where a backend forbids the name. See G2 |
| a `User-Agent` at all | Y\* | Y | Y | Y | (browser's) | `ClientBuilder::user_agent` exists; **ng still sends none by default**, deliberately — a library that names itself on every request decides for its embedder, and the embedder is who has an opinion. `ureq-3.4.0/src/config.rs:546` |
| base URL / relative URLs | Y | **N** | N | N | N | `ClientBuilder::base_url` — reqwest #988/#213 open since 2017 |
| per-request timeout override | Y | Y | Y\* | Y | Y | `RequestBuilder::timeouts` (`request.rs:341`) |
| a separate bound on name resolution | Y\* | N | Y | Y | n/a | closed — `Timeouts::resolve`. What it bounds is the wait for the **first address**, not a phase: Happy Eyeballs interleaves resolving with connecting, so there is no instant at which resolution finished. `false` on both ambient backends, honestly. See G11a; `ureq-3.4.0/src/config.rs:692` |
| per-request redirect override | Y | N | — | — | N | `RequestBuilder::redirect` (`request.rs:387`) |
| set an `http::Extensions` value from the builder | **N** | — | — | — | — | recorded as deliberate, `v03-acceptance.md:3394` — see §4 |
| `error_for_status` | Y | Y | Y | — | Y | on both `Response` and `Collected` — the second for a caller who wants the server's error text before deciding. A `3xx` is `Ok`, because reaching one means the redirect policy already handed it back. Writing it found that `Response::url()` reported the *requested* URL rather than the answering one, undocumented and untested; it is the last hop now |
| response text with charset from `Content-Type` | Y\* | Y | Y | — | — | closed: `Collected::text_with_charset` behind a `charset` feature — the name rq and uq both independently chose. **A separate method, not a smarter `text()`**: Cargo unifies features, so a feature that changed what `text()` means would make a library's behaviour depend on what an unrelated crate switched on, and the difference is silent mojibake rather than an error |

### 2.2 Bodies and streaming

| capability | ng | rq | uq | cu | br | note |
|---|---|---|---|---|---|---|
| streaming response body | Y | Y | Y | Y | Y | ng hands back an `http_body::Body`; rq adds `bytes_stream()` behind `stream` |
| streaming request body | Y | Y | Y | Y | Y\* | |
| **full duplex** | Y\* | — | N | — | Y\* | ng: `true` on `http-ng-h3` and on h2, and the capability still reports the HTTP/1.1 **floor** — see §5 |
| replay contract knowable before sending | **Y** | N | N | N | N | `RetryKind::{Free, ViaFactory, Impossible}`, and multipart derives it from its parts |
| streaming multipart | Y | Y | — | Y | — | ng: any streaming part makes the whole form `Streaming`/`Impossible` |
| response trailers reach the caller | Y | N | N | — | N | ng on h2 and h3; read via `into_parts()`, not `collect()` |
| request trailers | Y\* | N | N | — | N | sent on h1 and h2, and `Capabilities::request_trailers` understates the h2 path — a known mismatch, `v03-acceptance.md:3132` |
| a response body size limit | Y | N | **Y** | Y | N | closed — `ClientBuilder::response_limit`, counting **decompressed** bytes, which is the axis a decompression bomb lives on. Unset by default, unlike ureq: a ceiling this crate chose would fail a caller's legitimate large download. **ureq defaults to 10 MB** on `read_to_string`/`read_to_vec`/`read_json` and says so where the raw reader is handed over — *"a malicious server could send gigabytes"* (`ureq-3.4.0/src/body/mod.rs:36, :215-217`). ng has none anywhere in `http-ng`/`http-ng-core`: grepped `max_body`/`size_limit`/`body_limit`, and the only hits are the cache's own `Limits` |
| header size / count limits | Y | — | Y | Y | n/a | closed on both protocols — `Native::h1_opts` (`max_headers`, `max_buf_size`) and `H2Opts::max_header_list_size`. Neither is complete without the other: a transport that negotiates ALPN speaks whichever the server picked. `h1_opts` is **fallible** where `h2_opts` is not, because hyper panics below 8192 and a caller's number must not reach a `panic!` inside a connect. `ureq…/src/config.rs:586` |
| non-destructive body read | Y | N | — | — | — | `Collected` keeps status/headers/url after `.text()` — reqwest #1542 |

### 2.3 Protocols

| capability | ng | rq | uq | cu | br | note |
|---|---|---|---|---|---|---|
| HTTP/1.1 | Y | Y | Y | Y | (browser's) | |
| HTTP/2 | Y\* | Y | N | Y | — | ng behind `http-ng-native/http2`, off by default |
| h2 multiplexing on by default | N | Y | — | Y | — | ng: `Native::multiplexed()`, opt-in, because it needs `R: Spawn` |
| h2 tuning (window, frame size, keepalive PING, prior knowledge) | Y\* | Y | N | Y | N | closed for the settings frame — `Native::h2_opts`: both windows, `max_frame_size`, `max_header_list_size`. Keepalive, an adaptive window and prior knowledge are still absent, each for its own reason rather than as a batch. See G8; reqwest's eight are `reqwest-0.13.4/src/async_impl/client.rs:1563-1674` |
| HTTP/3 | Y | Y\*\* | N | Y\* | — | **rq requires `RUSTFLAGS='--cfg reqwest_unstable'`** (`src/lib.rs:252`) **and a per-request `.version(HTTP_3)`** — the only dispatch site matches on the request's version (`async_impl/client.rs:2638`), so `http3_prior_knowledge()` does not route anything; cu depends on the libcurl build |
| WebSocket | Y | **N** | N | **N** | Y | zero matches for `websocket` in all of `reqwest-0.13.4/src/`. And **libcurl 8.21 has a WebSocket API that the Rust binding does not expose** — zero files matching `CURLWS`/`ws_send`/`ws_recv`/`websocket` in either `curl-0.4.50/src/` or `curl-sys-0.4.90/src/`, so the capability exists in the C library and not in Rust |
| WebTransport | **Y** | N | N | N | N | `http-ng-webtransport`: sessions, bidi streams, datagrams, close capsules |
| Server-Sent Events | **Y** | **N** | N | N | Y | zero matches for `eventsource`/`text/event-stream` in `reqwest-0.13.4/src/`; ng has a decoder *and* reconnection with `Last-Event-ID` |
| `1xx` / `103 Early Hints` observable | **Y** | N | — | — | N | `Native::watching_1xx()` + `Event::Informational` |
| `Expect: 100-continue` | **Y** | N | **Y** | Y | N | `Native::expect_continue(after)`; **hyper's client does not do this**, so reqwest cannot. uq has it *and* a dedicated `timeout_await_100` (`src/config.rs:721`), which is the same "a wait ending in *proceeding*, not in failure" distinction this workspace argues for keeping out of `Timeouts` |
| demand a specific version and fail otherwise | **Y** | N\* | N | Y\* | N | `RequireVersion` is enforced before the head; rq's `http1_only`/`http2_prior_knowledge` are client-wide settings, not per-request demands |

### 2.4 Connections, sockets, resolution

| capability | ng | rq | uq | cu | br | note |
|---|---|---|---|---|---|---|
| connection pool | Y | Y | Y | Y | (browser's) | ng: `PoolConfig { idle_timeout, max_idle_per_key }` |
| a reaper that closes idle sockets | Y\* | Y | — | Y | — | ng: `Native::with_reaper`, opt-in, bounded on `R: Spawn` |
| `TCP_NODELAY` | Y | Y | — | Y | N | |
| local source address | Y | Y | — | Y | N | `TcpOpts::local_address` |
| **bind to an interface** (`SO_BINDTODEVICE`) | Y\* | Y | — | Y | N | closed — `TcpOpts::bind_device`. Not `local_address` renamed: an address binds the *source address* and the kernel still routes by its table, where this binds the interface. Linux/Android/Fuchsia only, which is why `Tokio::APPLIES` stopped being `TcpOptsSupport::ALL`. See G11 |
| TCP keepalive | Y\* | Y | — | Y | N | ng has `TcpOpts::keepalive` (one duration); rq has interval, retries and `TCP_USER_TIMEOUT` besides |
| Unix domain socket transport | **N** | N | — | **Y** | N | `Easy2::unix_socket` / `unix_socket_path` over `CURLOPT_UNIX_SOCKET_PATH` (`curl-0.4.50/src/easy/handler.rs:782, :802`) |
| static host→address override (`--resolve`) | seam | Y | — | Y | N | rq: `resolve`/`resolve_to_addrs`; here it is a `Resolve` impl, which is more work and more general |
| pluggable resolver | seam | Y | — | Y | N | rq: `dns_resolver`; ng: the `Resolve` trait, with three shipped backends |
| Happy Eyeballs (RFC 8305) | Y | Y | — | Y | (browser's) | |
| HTTPS/SVCB records consulted | **Y** | N | N | Y\* | (browser's) | asked in the same round as A/AAAA — measured, 404.6 ms → 0.8 ms |
| Alt-Svc | **Y** | **N** | N | Y | (browser's) | with RFC 7838 `ma` as the cache lifetime. Zero matches for `alt_svc`/`alt-svc`/`AltSvc` in all of `reqwest-0.13.4/src/` |
| DNS-over-HTTPS | Y | N | N | Y | N | `http-ng-dns-doh`, 22 crates, no tokio/hyper/h2 |
| choose h3 vs h1/h2 per origin | **Y** | N | N | N | (browser's) | `http-ng-select`, from the HTTPS record, with a negative cache |
| race the two stacks | **Y** | N | N | Y\* | (browser's) | off by default; `curl` has `--http3-only`/Happy-Eyeballs-for-h3 at the libcurl level |

### 2.5 TLS

| capability | ng | rq | uq | cu | br | note |
|---|---|---|---|---|---|---|
| rustls backend | Y | Y | Y | — | n/a | |
| platform TLS backend | Y | Y | Y | Y | n/a | `http-ng-tls-native-tls` |
| system trust store | Y | Y | Y | Y | n/a | `Rustls::with_platform_verifier` |
| add a root, use only supplied roots, client certs, min version, disable verification | seam | Y | Y | Y | n/a | **`Rustls::from_config(Arc<rustls::ClientConfig>)`** (`http-ng-tls-rustls/src/lib.rs:53`) makes every one of these expressible, at the cost of writing rustls directly instead of a named setter |
| ALPN reported back | Y | Y | — | Y | n/a | `TlsConnect::reports_alpn`, and h2 is only offered over a backend that answers `true` |
| 0-RTT / early data | **Y** | N | N | Y\* | n/a | admitted per request by `AllowEarlyData` and by nothing else |
| ECH | N\* | N | N | Y\* | n/a | refused deliberately: no backend here applies one, so the record's `ech_config_list` is gated behind `TlsConnect::applies_ech` |
| JA3/JA4 fingerprint control | **N** | N | N | N | n/a | refused by name in the design spec §9 — see §4 |

### 2.6 Proxies

| capability | ng | rq | uq | cu | br | note |
|---|---|---|---|---|---|---|
| HTTP `CONNECT` tunnel | Y | Y | Y | Y | (browser's) | |
| absolute-form for `http://` | Y | Y | — | Y | — | |
| SOCKS5 | Y | Y\* | Y\* | Y | — | rq behind `socks`; uq behind `socks-proxy` |
| SOCKS4/4a | Y | Y | — | Y | — | closed — one type, since 4a is signalled inside a SOCKS4 request rather than negotiated. The hostname form always, so nothing is resolved locally. See G7 |
| SOCKS remote DNS | **Y, always** | Y\* | — | Y | — | ng sends `ATYP=0x03 DOMAINNAME` unconditionally (`http-ng-native/src/proxy.rs:534`, doc at `:425` — *"a name, never an…"*), so the leak is not reachable. rq picks from the **scheme**: `socks4`/`socks5` → `DnsResolve::Local`, `socks4a`/`socks5h` → `DnsResolve::Proxy` (`src/connect.rs:540-541`), so a `socks5://` URL leaks DNS by default |
| read the proxy from the environment | **N** | Y | Y\* | Y | n/a | **refused as policy** — see §4 |
| `NO_PROXY` matching | Y\* | Y | — | Y | n/a | ng: `Proxy::bypass([..])` takes a list the caller wrote; it does not read `NO_PROXY`, and has **no CIDR** deliberately, where `hyper-util` 0.1.20's matcher does |
| a per-request/per-URL proxy rule | Y\* | Y | — | Y | n/a | closed for the per-scheme rule — an ordered list, `Proxy::only_for` and first-match-wins. No closure, and one proxy *protocol* per transport: erasing `P` would erase the IO with it. See G7 |
| PAC | **N** | N | N | N | (browser's) | nobody in Rust has this |
| system proxy (Windows registry, macOS SCF) | **N** | Y\* | Y\* | Y | (browser's) | rq behind `system-proxy`; uq behind `win-system-proxy` (Windows only) |
| proxy for QUIC (MASQUE / `CONNECT-UDP`) | N | N | N | — | n/a | refused: a different protocol against a different server |

### 2.7 Stateful client behaviour

| capability | ng | rq | uq | cu | br | note |
|---|---|---|---|---|---|---|
| redirect limit | Y | Y | Y | Y | (browser's) | |
| distinguish "do not follow" from "follow zero hops" | **Y** | Y | — | N | N | `RedirectPolicy::None` vs `Limited(0)` |
| **custom redirect predicate** | Y | Y | — | N | N | closed — `ClientBuilder::redirect_predicate`, asked **after** the policy about a hop it already approved, so it is handed the resolved target and the origin answer that drives credential stripping. Three verdicts where `reqwest-0.13.4/src/redirect.rs:102` also has three. See G9 |
| strip `Authorization` across origins | Y | Y | — | Y\* | (browser's) | ng also strips `AllowEarlyData` there |
| cookie jar | Y\* | Y\* | Y\* | Y | (browser's) | |
| **public-suffix rules in the jar** | **Y** | **N** | **N** | Y\* | (browser's) | the sharpest row in the table — see §5. ng compiles a list in (+77 KiB, measured). **Neither reqwest nor ureq rejects anything by default**, and both land there through `cookie_store` 0.22: reqwest's `Jar` is `#[derive(Default)] RwLock<cookie_store::CookieStore>` (`src/cookie.rs:30-31`) and `CookieStore`'s `Default`/`new()` leave `public_suffix_list: None` (`cookie_store-0.22.1/src/cookie_store.rs:40-48, :463, :467-473`); ureq builds its jar with `CookieStore::from_cookies(empty, true)` (`src/cookies.rs:177-180`), which sets the same `None` (`:460-464`), and takes `cookie_store` with `default-features = false` besides (`Cargo.toml:152-156`). Zero hits for `public_suffix` in `reqwest-0.13.4/src/`. `publicsuffix` 2.3.0 ships **no list at all**, only `LIST_URL` (`src/lib.rs:29`), so even a caller who wants one must fetch and refresh it |
| **a pluggable cookie store** | Y | Y | — | Y\* | n/a | closed: `ClientBuilder::cookie_jar` takes `CookieJar<P>` for any list and erases it into `AnyList` — see §G6's entry and spec amendment C12. `reqwest…client.rs:1213` takes any `CookieStore`, which is a different seam: theirs is the storage, ours is the public suffix list, and this crate's jar *is* the storage |
| RFC 9111 response cache | **Y** | N | N | N | (browser's) | `http-ng-cache`: freshness, validation, `Vary`, both sides' directives |
| **a pluggable cache store** | Y | n/a | n/a | n/a | n/a | closed: `ClientBuilder::cache` takes `HttpCache<S>` for any store and erases it into `AnyStore`, so a disk-backed or shared store reaches `Client`. Erased rather than parameterised because `S` on the cache is `S` on the public `ClientBody` alias, and because both crates are optional dependencies — a defaulted parameter needs a default type that would not exist |
| gzip | Y\* | Y\* | Y\* | Y | (browser's) | |
| brotli | Y\* | Y\* | Y\* | Y | (browser's) | |
| deflate | Y\* | Y\* | N | Y | (browser's) | **both wire formats under one token** — RFC 9110 §8.4.1.2's zlib and the raw stream its own Note records, chosen from the first two bytes rather than after a failure. No new crate: `flate2` was already here for `gzip` |
| zstd | Y\* | Y\* | N | Y | (browser's) | `ruzstd`, a decoder-first pure-Rust crate. The window is capped at RFC 8878 §3.1.1.1.2's recommended 8 MB — `ruzstd`'s own default is 100 MB — and the frame's XXH64 content checksum is compared, which `ruzstd` reads and never checks |
| request-body compression | **N** | N | N | Y | N | refused in the design spec §9 |
| automatic retry | Y\* | N | Y\* | N | n/a | ng retries a `NotSent` pooled write and replays a `425` once; neither is a general retry policy |

### 2.8 Targets, runtimes, shape

| capability | ng | rq | uq | cu | br |
|---|---|---|---|---|---|
| native async | Y | Y | N | Y (isahc) | n/a |
| **blocking API** | **N** | Y\* | Y | Y | n/a |
| runs on a single-threaded / `!Send` executor | **Y** | N | n/a | N | n/a |
| tokio-free build | Y\* | N | Y | Y | Y |
| browser (`wasm32-unknown-unknown`) | Y | Y\* | N | N | Y |
| WASI (`wasm32-wasip2`) | Y | **N** (executed) | Y (executed) | N | n/a |
| Apple `URLSession` | **Y** | N | N | N | n/a |
| embassy / `embedded-nal`-shaped runtime | Y\* | N | N | N | n/a |
| `no_std` | N | N | N | N | N |
| published on crates.io | **N** | Y | Y | Y | Y |
| a type-erased client (`Box<dyn …>`) | N\* | Y | Y | Y | Y |

### 2.9 Observability

| capability | ng | rq | uq | cu | br |
|---|---|---|---|---|---|
| connection-level events (`Connected`/`Reused`/`Closed`) | **Y** | N | — | Y (`CURLINFO_*`) | N |
| response-head event | Y | N | — | Y | N |
| `1xx` event | **Y** | N | — | — | N |
| per-phase connect timings (dns / tcp / tls) | **Y** | N | — | Y | N |
| upload/download progress callback | N | N | — | Y | N |
| a capability an over-claiming backend cannot fake | **Y** | n/a | n/a | n/a | n/a |
| middleware / interception | Y\* | N | **Y** | — | N |

On the last row: ureq ships `Middleware` in the crate
(`src/middleware.rs:16`, bounded `Send + Sync + 'static`); reqwest does not
and the ecosystem answer is the separate `reqwest-middleware` crate; here it
is `http-ng-tower`, which goes **both** ways — `TransportService` makes a
`Transport` into a `tower::Service` so `tower-http`'s stack applies, and
`ServiceTransport` makes any `tower::Service` into a `Transport`
(`crates/http-ng-tower/src/lib.rs:265`), which is how someone would put
`hyper-util`'s own client underneath this facade. The second direction has
to be told its capabilities as an argument, *"because a `Service` has none,
and this adapter must not invent them"* — the capability rule applied to a
foreign ecosystem. The costs are an `Arc`, an uncached `poll_ready` per
call, and a boxed future that is `!Send` until return type notation lands.

---

## 3. The genuine gaps, ranked

Ranked by how often an ordinary caller meets them, not by how hard they are.
Each says what it would take **here**, which of this workspace's stated rules
it has to respect, and — where it applies — which rule forbids it outright,
because a feature refused by a written rule is a better answer than a patch.

### G1. Nothing is published

Every crate says `0.1.0` and none is on crates.io. This is the gap a caller
hits first and it dwarfs every row in §2: none of what follows is reachable
by anyone who has not cloned this tree.

It is not an oversight — AGENTS.md states the position and the trigger, and
the trigger is the owner's. It is listed first anyway because a competitive
gap analysis that ranked `deflate` above "cannot be depended on" would be
measuring the wrong thing.

### G2. No `User-Agent`, and no client-wide default headers — **closed**

`ClientBuilder::{user_agent, default_header, default_headers}`. Three
decisions came out of building it, and the analysis above had predicted
the third.

**A header the caller wrote on the request wins**, because a default is a
fallback and not an override. **Applied per hop**, inside `Client::run`'s
redirect loop, so a `User-Agent` survives a redirect — a chain whose second
request looks like a different client's is stranger than one that never
sets the header. And **a default a backend forbids is refused at
`build()`**, naming the setting, which is what
`forbidden_request_headers` was for: the asymmetry with
`RequestBuilder::header` is about when the caller finds out, since a
per-request header sits beside the request that carries it while a default
applies to traffic its author may never look at again.

**There is still no `User-Agent` by default**, and that is deliberate
rather than unfinished: a library that names itself on every request makes
a decision for its embedder, and the embedder is who has an opinion.

### G3. No blocking API, and this is a refusal that has aged

Design spec §9: *"A blocking API — Out of scope by the problem statement."*
It is the rule, so this is a **refused** row, not a gap — but it is worth
naming here because it is the single largest population of callers in the
comparison. `ureq` exists entirely to serve it, and reqwest ships
`blocking` for it.

The refusal is coherent with everything else: a blocking facade needs a
runtime to block on, and picking one is exactly the choice `http-ng-rt`
exists to avoid. A caller can already write
`futures_executor::block_on(client.get(u).send())` on a bare executor —
`two_runtimes.rs` proves that path works with no reactor — so what is
missing is a facade, not a capability.

### G4. Browser callers cannot reach `mode`, `credentials`, `cache` or `redirect` — **closed for three of the four, and the fourth is refused**

`Fetch::opts(FetchOpts { .. })` — `mode`, `credentials`, `cache` and
`referrerPolicy`, four `Option`s applied to `RequestInit`, `None` meaning
*leave it to the browser*. Setters on `Fetch` and not on `Transport`,
exactly as this section predicted, and not a request extension for the
reason it gave: an extension is a channel any code that can build a
request may write to, and `credentials: "include"` decides which origins
receive the user's cookies.

Four `web-sys` feature names and **no crate**: the graph is 32 either way,
measured.

**`redirect` is refused, and the reason is a capability that would lie.**
`fetch`'s `redirect: "manual"` does not hand back a `3xx` a caller can act
on — for a cross-origin response it yields an *opaque-redirect* filtered
response, status `0`, empty header list, null body, no readable
`Location`. So `Capabilities::redirects` could not honestly move from
`Internal` to `Transparent`; it would claim a policy `Client` could act on
for exactly the case where redirects matter and the browser gives nothing.
`http-ng-urlsession` is the backend that genuinely reports `Transparent`,
which is what makes the two comparable at all. `redirect: "error"` is a
third thing — fail rather than follow — and it is `RedirectPolicy::None`
with the answer thrown away.

**Asserted without a network**, and that is the honest shape here rather
than a shortcut: `web_sys::Request` has a getter for each of the four, so
the browser itself answers whether the member arrived. What a headless run
could *not* honestly arrange is what these members are for —
`credentials: "include"` against a cross-origin server that sets a cookie,
`no-cors` producing an opaque response — both of which need a second
origin. What this crate is responsible for is that the member reaches the
request; what the browser does with it afterwards is the browser's.

**Finding this also found that the browser suite had not compiled since
2026-08-16.** `Event::Informational` landed with the `1xx` work and
`http-ng-fetch`'s `tests/hooks.rs` never gained an arm, so every browser
binary in that crate failed to build through six green merges —
`cargo nextest run --workspace --all-features` does not build for
`wasm32-unknown-unknown`, and `just test-browsers` is its own CI job.
`Event` not being `#[non_exhaustive]` is what made it a compile error
rather than silence: the design worked and the running of it did not.

### G5. `text()` is UTF-8 or an error, with no charset from `Content-Type` — **closed**

`Collected::text_with_charset`, behind a `charset` feature, off by
default. The dependency question was real and is answered by what the
method costs rather than by what it is worth: `encoding_rs` is over a
megabyte of tables, and a build that only ever meets UTF-8 pays nothing
because the feature is off.

**The shape is the interesting part, and it is the one this document
predicted.** `text()` is unchanged, and must be: Cargo unifies features
across a graph, so a `charset` feature that made `text()` charset-aware
would give a library a different answer depending on what some unrelated
crate in the graph enabled — and the difference is not an error, it is
plausible-looking mojibake. That is the same hazard the `Capabilities`
floor rule exists for, one layer up.

Four answers, each a decision rather than a default: no `charset`
parameter is UTF-8 (RFC 7231 removed RFC 2616's ISO-8859-1 default, and
sniffing is a browser's job); a label the WHATWG Encoding Standard names
is decoded with it; a label it does not name is a typed error **naming the
label**, because falling back to UTF-8 turns "the server said something we
did not understand" into mojibake with nothing to show for it; and
malformed bytes are an error rather than U+FFFD, because `text()` refuses
invalid UTF-8 rather than patching it and two policies under one name is
worse than either.

A byte order mark overrides the declared label — the Encoding Standard's
rule, inherited from `encoding_rs::Encoding::decode` and pinned by a test
rather than assumed.

### G6. No pluggable cookie or cache store, though both crates are already generic — **closed**

`ClientBuilder::cookie_jar` takes `CookieJar<P>` for any list and
`ClientBuilder::cache` takes `HttpCache<S>` for any store, erasing each
into `AnyList`/`AnyStore` on the way in. So a disk-backed cache, a store
shared between processes, and a jar over `NoList` all reach `Client`.

**Erased rather than parameterised**, and the second of the two reasons is
the one this analysis had not seen. `cached.rs` had already written down
the first: a recording body holds a cache handle, so `S` on the cache is
`S` on the public `ClientBody` alias, and the arity of a public alias must
not change with a feature Cargo may unify on. The second is that a
defaulted parameter needs a default **type**, and both crates are optional
dependencies — `Client<T, Tm, P = http_ng_cookie::BuiltinList, ..>` names
a type that does not exist without the feature, so the declaration would
fork four ways over a `Client` that is already forked once.

Both wrappers implement the seam they erase, so the jar and the cache keep
their whole API and `Client::cookies`/`Client::cache` still hand back a
guard onto the real thing. The `Send` bound this document anticipated is
real and is spec amendment C12: it sits on the two opt-in setters and
nowhere else, and it states a property `BuiltinList` and `MemoryStore`
already had — erasing without it would make every `Client` in a build with
either feature compiled in `!Send`, configured or not.

### G7. Proxy configuration from the environment, and a per-URL proxy rule — **the per-scheme half is closed**

`Native::and_proxy` builds a list, `Proxy::only_for(ProxyScheme)`
restricts an entry to one scheme, and **the first entry that serves a
request wins**. So the ordinary corporate setup — an `HTTP_PROXY` and an
`HTTPS_PROXY` at different hosts — is expressible, which one `Option`
could not hold.

**First-match-wins rather than most-specific-wins**, because a precedence
rule has to be learned where an ordered list is read off the builder chain
that wrote it. An unrestricted proxy placed first therefore shadows a
narrower one after it, which is visible at the call site and is asserted.

**One `P` per transport, and that is the limit rather than an oversight.**
A caller wanting SOCKS5 for `https` and an HTTP proxy for `http` cannot
say so: lifting it means erasing `P`, and erasing `P` erases the IO with
it — the objection `docs/proxy-design.md` already records against
`Box<dyn ProxyProtocol>`. Which makes the closure form this section
suggested a bigger change than it looks: `Fn(&Uri) -> Option<&Proxy<P>>`
would still be one `P`, so it buys arbitrary *routing* and not arbitrary
*protocols* — an ordered list already buys the routing that a caller can
name.

**A bypass belongs to the proxy that carries it**, so a bypassed host
falls through to the next proxy and only goes direct when the list runs
out. The global `NO_PROXY` reading is the worse one *because* the list
exists: a host bypassed on an `https`-only proxy would take an `http://`
request direct, past an `http` proxy that was never in the running. With
one proxy — the overwhelming majority — the two rules coincide exactly.

**SOCKS4/4a is in too**, as one type: 4a is 4's own extension, signalled
inside a SOCKS4 request by a `DSTIP` of `0.0.0.x` that cannot be a real
address, so there is no version byte to distinguish them and a second type
would be a choice the wire does not offer. This connector always sends the
hostname form, because that is what `ProxyProtocol::tunnel` is given — the
host is not resolved locally, which is the DNS leak a proxy user is often
there to avoid.

`Socks5` remains the answer unless a server forces otherwise, and the
reasons are the protocol's: **no IPv6**, since the address field is four
bytes, and **no authentication**, only a `USERID` the proxy may check
against an identd. Said where the type is, and the `USERID` is
deliberately not marked sensitive — marking it would claim a secrecy the
protocol does not have.

Still absent: **reading the environment**, which stays refused as policy.

### G8. HTTP/2 has no tuning surface — **closed for the settings frame**

`Native::h2_opts(H2Opts { .. })`, four `Option` fields forwarded to
`h2::client::Builder`: the stream window, the connection window,
`max_frame_size` and `max_header_list_size`. Every field `None` by
default, meaning *whatever h2 chooses*, on `TcpOpts`' rule — a value set
here goes on the wire, so a default of ours would change what a caller who
asked for nothing announces to every server.

The one that motivates the rest is the window, and the arithmetic is worth
stating because it is not this client's: a peer may have at most a window
in flight, so the best achievable rate is `window / RTT` — 65 535 bytes
over a 100 ms round trip is about 5 Mbit/s whatever the link can do.

**Three of reqwest's eight are deliberately still absent**, each for its
own reason rather than as a batch. An *adaptive window* is hyper's, computed
from measured RTT; `h2` has none, so it would be ours to write, and a wrong
estimator is worse than an honest constant. *Keepalive pings* need somebody
polling an idle connection, which here means `multiplexed()` and its
`Spawn` bound — a property of the driver rather than of the settings frame,
and it belongs on that constructor if it arrives. And
*`max_concurrent_streams`* governs streams the **server** opens, i.e.
server push, which `h2` does not enable and RFC 9113 §8.4 deprecates: a
knob with no subject. *Prior knowledge* is a separate axis — speaking h2
without ALPN — and is not tuning.

**Nothing is timed.** A throughput measurement on loopback would say
almost nothing, since the window bounds bytes *in flight* and a round trip
near zero refills it as fast as it drains — which is precisely why the
default only bites on a long fat pipe. What is observable without a
network is the setting itself, at the peer that has to obey it: an
`h2::server` reserving capacity is told by its own flow control how much
the client said it would accept, and it frames a large body at the
client's `max_frame_size`.

### G9. No custom redirect predicate — **closed**

`ClientBuilder::redirect_predicate(|hop| ..)`, answering
[`RedirectVerdict`]`::{Follow, Stop, Refuse}`.

**The shape this document proposed — a third `RedirectPolicy` variant
carrying a function — turned out to be wrong twice**, and finding out why
was most of the work. `RedirectPolicy` lives in `http-ng-proto`, which is
sans-io and clockless, and `redirect::decide` is a pure function of six
values; a closure variant would make it *pure except for whatever the
caller passed*. And `RedirectPolicy` is `Copy + PartialEq + Eq`, read out
of a request's extensions with `.copied()` — a boxed closure ends all
three.

So `decide` is untouched and the predicate is asked **after** it, only
about a hop it already approved. That ordering turned out to be the useful
one rather than a concession: the predicate is handed the *resolved*
target, the possibly-downgraded method, and `cross_origin` — which is the
same value that drives the credential stripping, not a second opinion
about what an origin is — and none of those exist before `decide` runs.

**Three verdicts and not two**, because `Stop` alone would make an SSRF
guard hand back a `3xx` the caller must remember to check, and a caller who
forgets gets a silent success where they asked for a refusal.

This document's rule prediction was half right. It said no `Send` bound
was needed since nothing is spawned around the decision, and that is true
of the *call* — but the closure is stored in a `Client`, which is meant to
cross a `tokio::spawn`, so an unbounded one would make **every** `Client`
`!Send`, predicate or not. The bound is on the opt-in setter alone, spec
amendment C12, which is the same answer G6 reached one gap over.

### G10. No response body size limit — **closed**

`ClientBuilder::response_limit(u64)`, and the axis is the whole decision:
`Limited` is the **outermost** body wrapper, so it counts the bytes the
caller receives, after any `Content-Encoding` has been reversed. A limit
counting wire bytes would let a decompression bomb through by definition —
small on the wire is what makes it a bomb — and the test that separates a
real bound from a plausible one is a 60-byte gzip stream expanding to a
megabyte against a 4 KiB limit.

`Deadline` sits the other way round for the mirror-image reason, and both
are now written next to each other: one counts what arrives, the other
times what is sent, and each is on the side its own threat is on.

### G11. Interface binding, TCP keepalive detail, Unix sockets — **the socket options are closed; the Unix socket is not**

`TcpOpts` gains `bind_device`, `keepalive_interval`, `keepalive_retries`
and `user_timeout`, with the matching `TcpOptsSupport` bools — the
field-per-field mirror this section correctly called *the designed-for
change*, since the error has to name the option a caller set.

**The interesting consequence is that `Tokio::APPLIES` stopped being
`TcpOptsSupport::ALL`.** `SO_BINDTODEVICE` exists on Linux, Android and
Fuchsia; `TCP_USER_TIMEOUT` on those plus Cygwin; and
`TcpKeepalive::with_retries` is absent on three others. A constant
claiming all of them everywhere would be a capability that lies on macOS
and Windows — so `APPLIES` is now a `cfg!`-computed value, and `ALL` still
means *every field* while no longer being a value a real runtime can claim
on every target it builds for. Checked by compiling for
`aarch64-apple-darwin` and `x86_64-pc-windows-msvc` as well as the host.

**`user_timeout` is the one that catches a peer which vanished
mid-transfer**, where keepalive only catches an idle one: probes are sent
when nothing is in flight, so a connection with unacknowledged data sits in
retransmission for minutes with keepalive never firing. It overlaps
`Timeouts::between_bytes` without replacing it — this one is the kernel's,
applies to a socket rather than an exchange, and is the only one of the two
a build with no `Client` above it can reach.

**A Unix-domain-socket transport is still absent**, and is the larger half
as this section said: a sibling of `TcpConnect` rather than a field, since
there is no `SocketAddr`, no Happy Eyeballs, no TLS by default and no port.

### G11a. No separate bound on name resolution — **closed**

`Timeouts::resolve`, `TimeoutSupport::resolve` and `Phase::Resolve`, all in
one change — the rule this field had to land under, and the one that kept
`connect` off `http-ng-h3` until W1.

**What it bounds is not a phase, and finding that out was the work.**
Happy Eyeballs interleaves resolution with connecting on purpose — the
resolver is a `Stream` and `http-ng-native` starts dialling the first
address while the rest are still arriving — so there is no instant at which
*resolution finished* for a bound to attach to. What is bounded is the wait
for the **first address from either family**, which is exactly the failure
the gap named: a resolver that hangs is indistinguishable from an origin
that will not answer, and only the first is worth a different retry.

Nothing is serialised by it. `attempt` cannot connect before an address
exists, so the gate waits for what the next line would wait for anyway;
what changes is the error, not the schedule.

Three things the shape decided:

- **It does not apply where the connection does not need the resolver** —
  an HTTPS record carrying address hints gives the connector somewhere to
  go with no answer at all. That skip was first written as a mutation
  *control* and is a **test**: RFC 9460 §7.3 address hints are ordinary, so
  the behaviour was reachable and untested, which is a gap.
- **It stops waiting when both families are done**, so a name that does not
  exist stays `ErrorKind::Resolve` rather than becoming a timeout. Turning
  a precise diagnosis into a vague one is the thing this feature exists to
  undo.
- **`TimeoutSupport::resolve` is honestly `false` on both ambient
  backends.** `wasi:http` 0.3's `request-options` has three timeouts and
  nothing for resolution — the host resolves — and `fetch` collapses
  everything into one `AbortController`. The field's own reason for
  existing, met on the first try.

### G12. Digest, NTLM and Negotiate authentication — **Digest closed**

`RequestBuilder::digest_auth(user, password)` behind a `digest-auth`
feature, off by default. This document's own prediction was right in full:
`Client::run` already owned the shape, and the branch is the `425` one with
a computed header — a status-code test, one resend, inside the same `total`
budget, gated on `RequestBody::retry_kind()`. No spawn, no `Send` bound.

**MD5 and SHA-2 cost nine crates, measured**, which is why the feature is
off by default and why they are taken rather than hand-written — a
departure from the rule that removed `url` and hand-wrote base64, and the
line between them is whether a wrong answer is *visible*. Base64 is twenty
lines and fails loudly; a hash is two hundred whose defects are silent.

Three things the building decided that the analysis had not asked:

- **The arithmetic is checked against RFC 7616 §3.9's own printed
  answers**, copied from the document, which is why `digest::answer` takes
  `cnonce` as a parameter instead of drawing it. A hash checked against its
  own output is green for any self-consistent mistake about what digest is.
- **The credentials do not cross an origin**, by the rule that already
  strips `Authorization` — a password-derived secret must not reach a
  server the caller never named. That is a stronger case than the one
  `AllowEarlyData` was taken off the hop for.
- **They are not in `http::Extensions`.** Extensions reach
  `Transport::execute`, so a password there would be readable by any
  transport, including one this workspace did not write.

Two deliberate absences with their reasons: `auth-int`, which hashes the
request body into `A2` and so cannot be computed for a `Streaming` one — a
server offering it *alone* gets a named refusal rather than a wrong answer
— and a nonce cache, so **every request pays one `401` round trip**.
Removing that needs per-origin state with a lifetime nobody states, the
question that made a cache dishonest for SVCB and honest for `Alt-Svc`.

**NTLM and Negotiate are unchanged and still refused**: both need the
platform's GSSAPI or SSPI, which is `http-ng-tls-native-tls`'s argument one
seam over and its own crate. Neither is a challenge/response this code
could grow into.

### G13. There is no way to name "any http-ng client"

`Transport::execute` is an `async fn` in a trait, so `dyn Transport` is not
a thing, and `Native<R, T, D, H, P>` carries five type parameters where
`reqwest::Client` carries none. A library that wants to accept a client
generically writes `fn f<T: Transport>(c: &Client<T>)` and pushes the
parameter to its own callers.

`http-ng-tower::TransportService` is the erasure route — it boxes the future
— and its own module doc records that the box is `!Send` and cannot be fixed
here until return type notation stabilises (rust-lang/rust#109417). So this
is a **gap with the cause identified upstream**, which is the most useful
kind. *Forbidden?* The `Box<dyn>`-erases-auto-traits rule (amendment C1) is
what makes the `Send` version impossible today; the `!Send` version already
exists.

---

## 4. What is refused, and by which rule

These are **not** gaps. Each is a decision already recorded, with the rule
that produced it. Reporting any of them as missing would be the main way
this document could fail.

| absence | the rule that refuses it | where |
|---|---|---|
| ~~`deflate` and `zstd` decoding~~ | **reversed, and the row is kept because the reversal is the interesting part.** The `deflate` rule — *a client must not advertise a coding it may guess wrong about* — assumed the guess had to be made after a failure, as curl's is; it is answered from the first two bytes, which is a rule with no window. The `zstd` rule was *a third dependency for a coding no server sends unasked*, and the answer is that both premises were checked rather than argued: `deflate` costs **no** crate at all, and `zstd` costs two | `http-ng/src/decompress.rs` |
| `compress`/`x-compress` | RFC 9110 §8.4.1.1's LZW: no decoder here, so it is never advertised and never matched | `http-ng/src/decompress.rs` |
| request-body compression | server support is inconsistent; a clean manual path instead | design spec §9 |
| reading `HTTP_PROXY`/`NO_PROXY` from the environment | reading the environment is *policy* and belongs to whoever builds the transport; a list the caller wrote is not policy | AGENTS.md, proxy section |
| a proxy for QUIC | `CONNECT-UDP` (RFC 9298) is a different protocol against a different server, not this feature with a wider bound | `docs/proxy-design.md` §1 |
| a blocking API | out of scope by the problem statement | design spec §9 |
| JA3/JA4/Akamai fingerprint control | rustls closed it *not planned*; `http::HeaderMap` lowercases names, so browser header casing is unreproducible anyway | design spec §9 |
| `no_std` / bare metal | `http` 1.x carries `compile_error!` for it; not this project's to reverse | AGENTS.md |
| `Ping`/`Pong` as WebSocket message variants | a browser has neither `send(ping)` nor `onping`; the variant would have no honest right-hand side | `http-ng-core/src/unversioned/websocket.rs:35` |
| permessage-deflate, subprotocol checking | left open in `docs/w4-upgrade-seam.md`; the browser negotiates extensions itself and exposes no control | same file, `:46` |
| a `RequestBuilder::extension` setter | adding one for `AllowEarlyData` and not `RequireVersion` would be arbitrary; both is a facade question | `docs/v03-acceptance.md:3394` |
| ECH | no backend here applies one, and `http-ng-tls-rustls` *refuses* a non-`None` ech — filling the field would make every ECH-publishing origin unreachable | AGENTS.md |
| RFC 6724 destination address selection | the full rule needs source address selection, i.e. a routing table, which no seam here provides; a partial one would look like compliance without being it | design spec §9 |
| `stale-while-revalidate` | it needs somewhere to run the revalidation after the response was handed over, and this client does not spawn on a caller's behalf | AGENTS.md, cache section |
| a WebSocket keep-alive by default, an h2 driver by default, an idle-socket reaper by default | a default stronger than the truth: not every `R` can `Spawn`, and a default that pings sends traffic nobody asked for | AGENTS.md, several places |
| h2 multiplexing by default | without `Spawn` nobody drives a shared connection but the in-flight futures | `docs/h2-multiplexing.md` |
| more than one session per WebTransport connection — **no longer refused** | built; the recorded blocker (`PoolKey`) turned out not to be the true one | AGENTS.md |
| WebTransport `GOAWAY` | a measured impossibility in `h3` 0.0.8: two `GOAWAY`s saying opposite things look identical to a client | AGENTS.md |

---

## 5. The reverse column: what `http-ng` has and the others do not

Each row was checked against the comparators rather than assumed.

**One `Transport` seam across five backends, and the same source builds for
three targets.** `crates/http-ng/examples/portable.rs` compiles for native,
`wasm32-wasip2` and `wasm32-unknown-unknown` from one file with **no
`#[cfg]`**, and a CI job (`portable-example-three-targets`) fails if that
stops being true. reqwest cannot do this and the reason is structural rather
than incidental: its browser build has a **different `ClientBuilder` type**
with exactly four methods — `new`, `build`, `user_agent`, `default_headers`
(`reqwest-0.13.4/src/wasm/client.rs:293-329`) — so `.timeout(..)`,
`.redirect(..)` and `.cookie_store(..)` do not compile there; and its browser
`RequestBuilder` carries `fetch_mode_no_cors` and friends that do not exist
natively. The API diverges in both directions, which is the thing this seam
was built to prevent. *This project's own version of the same divergence is
G4* — but G4 is a missing setter on one backend, not two incompatible
`ClientBuilder` types.

**A streaming request body in the browser, which `web-sys` has no setter
for.** `fetch` requires `duplex: "half"` whenever the body is a
`ReadableStream`, and `web-sys` 0.3.104's `RequestInit` has no `duplex`
setter among its fifty methods — surveyed exhaustively, not sampled. So
`gloo-net` **cannot** send a streaming request body at all (`body` takes
`impl Into<JsValue>`, `src/http/request.rs:52`, and nothing sets `duplex`),
and `http-ng-fetch`'s `js_sys::Reflect` write through an unchecked ref
(`convert.rs:519-527`) is the only route rather than a shortcut past a
proper one. Worth knowing before anyone "cleans it up".

**A cookie jar that applies public-suffix rules.** This is the one row in
§2 where the difference is a security property rather than a feature, and
it went the opposite way from the first guess: **neither reqwest nor ureq
rejects a public-suffix cookie by default.** Both use `cookie_store` 0.22,
whose `public_suffix_list` is `None` unless somebody installs one, and
neither installs one — reqwest by taking `Default`, ureq by building
through `from_cookies`. So against either default jar a server at
`shop.example.co.uk` can set a cookie scoped to `co.uk`. Nor is the fix a
feature flag: `publicsuffix` 2.3.0 ships no list, only a URL to download
one, so a caller has to fetch and refresh it themselves.

This workspace compiles a list in and charges a measured 77 KiB for it,
which is why `cookies` is off by default. That is the honest price of the
check, and it is worth saying plainly that the check is the *only* one of
its kind among the clients compared. **This was checked in both directions
before it was written**: an earlier draft of this paragraph asserted that
ureq installed a list, on nothing but plausibility, and reading
`src/cookies.rs:172-180` is what corrected it.

**A capability model that refuses at `build()` rather than ignoring at run
time.** `check_supported` (`http-ng/src/config.rs:261`) turns a cookie jar
against a jar-owning backend, a cache against a caching backend, a
`RedirectPolicy` against an internally-redirecting backend, and an
unsupported timeout into a typed `UnsupportedCapability` **before a request
is made**. No comparator has anything of the kind; the nearest is reqwest
compiling a smaller API on wasm, which catches a subset at compile time and
silently ignores nothing — but also cannot express "this backend does
decompress internally, so do not decode again", which
`DecompressionSupport::Internal` does. The rule that keeps the model honest
is that a capability reports the **floor**, and `http-ng-select` is where
that rule bites hardest: five of six disagreeing fields take the weaker
value and the rest make the constructor **refuse, naming the field**.

**Per-request timeouts, four of them, with distinct meanings.**
`connect` / `first_byte` / `between_bytes` in `Timeouts` plus `total` on the
client, settable per request (`RequestBuilder::timeouts`), and each measured
against a server that misbehaves in exactly that way
(`crates/http-ng-native/tests/timeouts.rs`, `crates/http-ng/tests/deadline.rs`).
reqwest has `timeout`, `read_timeout`, `connect_timeout` and
`pool_idle_timeout` on the **client** and one `timeout` per request.

**ureq beats this on count and it is worth conceding rather than dressing
up**: nine distinct bounds — `timeout_global`, `timeout_per_call`,
`timeout_resolve`, `timeout_connect`, `timeout_send_request`,
`timeout_await_100`, `timeout_send_body`, `timeout_recv_response`,
`timeout_recv_body` (`src/config.rs:669-745`). The three this client does
not have are `resolve`, `send_request` and `send_body`, and the first of
those is the one a caller notices, since a slow resolver currently spends
`connect`'s budget without being separable from it. Blocking IO is what
makes nine cheap there and four expensive here — each bound here has to be
a race or a body wrapper carrying a `Timer::Sleep`, and the `Expect` work
showed what a fifth costs (`docs/expect-continue.md` §7: a wrapper around
the `first_byte` race overflowed the stack in 56 tests).

**`RequireVersion`, enforced before the head.** Nobody else has a
per-request protocol demand that fails rather than downgrades. reqwest's
`http1_only` and `http2_prior_knowledge` are client-wide construction
settings.

**HTTP/3 without an unstable flag, and WebTransport at all.** reqwest's
`http3` feature refuses to compile without
`RUSTFLAGS='--cfg reqwest_unstable'` (`reqwest-0.13.4/src/lib.rs:252`) and
has done for two years. `http-ng-webtransport` — sessions, bidi streams,
datagrams, RFC 9297 close capsules — has no counterpart in any comparator;
it was checked against `wtransport` 0.7.2, which shares no code with `h3`.

**Server-Sent Events, with reconnection, in the client itself.** Zero
matches for `eventsource` or `text/event-stream` in all of
`reqwest-0.13.4/src/`; the ecosystem answer is `reqwest-eventsource` over
`eventsource-stream`, whose specific defects the design spec §4.10
enumerates. `http-ng` has the decoder *and* `Client::sse`'s reconnect with
`Last-Event-ID`.

**`Expect: 100-continue` and `1xx` observability.** hyper's client does not
implement `Expect` at all (it appears on the server side only in 1.11), so
neither reqwest nor anything else built on hyper can offer it. `Native::
expect_continue(after)` and `Event::Informational` — which is how a caller
reads `103 Early Hints` — have no comparator.

**An Apple `URLSession` backend, which no other Rust HTTP client has.**
Searched for and not found: neither reqwest, ureq nor isahc has an
Apple-native backend. What it buys is a list that a userspace stack cannot
reach at any price — background transfers that survive app suspension
(`URLSessionConfiguration.background(withIdentifier:)`, run by a separate
system process), the system proxy configuration **including PAC**, per-app
VPN with hostname fidelity (an `NEAppProxyProvider` sees the name from a
`URLSession` flow and only an address from a raw socket), the cellular /
constrained / expensive / ultra-constrained network policy flags, App
Transport Security's declarative pinning, and NTLM/Negotiate challenges.
And it reports `RedirectSupport::Transparent` where `http-ng-fetch` must
report `Internal`, because a delegate can refuse a redirect and a browser
cannot. One item of the four AGENTS.md lists is not actually on it — see
§8.

**Connection events with per-phase timings.** `Connected` carries dns / tcp
/ tls durations; `Closed` carries a reason. Nothing in reqwest or ureq does;
the only comparator is libcurl's `CURLINFO_*`, which is a different shape
(pull, after the fact, per transfer). reqwest #155 has been open since 2017.

**A connect seam a `!Send` backend can implement, and one that carries more
than a `Uri`.** The comparison here is with `hyper-util` 0.1.20, since that
is what a caller who wants to assemble their own client reaches for. Its
`Connect` is **sealed** and is an alias for `tower::Service<Uri>`; the
blanket impl demands `S: Send + 'static`, `S::Future: Unpin + Send` and
`T: Read + Write + Connection + Unpin + Send + 'static`
(`src/client/legacy/connect/mod.rs:340-345`), `Client` adds
`C: Connect + Clone + Send + Sync + 'static` (`client/legacy/client.rs:145`),
and `Sized` is required on the trait **specifically to prevent a
`dyn Connect`** (`connect/mod.rs:322-324`). A single-threaded connector is
structurally impossible there. It is also handed a `Uri` and nothing else,
which is exactly the signature `docs/proxy-design.md` §2 rejects here for
the DNS-leak reason — and the reason `Prefetch::prepare` can hand a
connector a fetched HTTPS record where `hyper-util` has no channel for one.

**Sans-io crates a third party can use without the client.** `http-ng-proto`
(RFC 3986 resolution, redirect rules, SSE decoding, the two URL encoders),
`http-ng-cookie` (RFC 6265bis, clockless), `http-ng-cache` (RFC 9111,
clockless, and not even depending on `http-ng-core`). reqwest has none.

**ureq 3 is a much closer architectural relative than expected, and the
difference between the two is worth stating exactly rather than claimed
away.** It has a sans-io crate (`ureq-proto` 0.6.1 — a real HTTP/1.1 client
*and* server state machine over `http` + `httparse`, with chunked encoding
and `100-continue`, and with *"body data transformations (charset,
compression)"* explicitly out of scope, the same boundary drawn here). It
has a `Transport` trait *and* a `Resolver` trait. It has a middleware layer
(`src/middleware.rs`). And it puts all of that in a module called
**`unversioned`**, with the same argument this workspace's `unversioned`
module makes — *"breaking changes … will NOT be reflected in a major version
bump"* (`ureq-3.4.0/src/unversioned/mod.rs:1-10`). Convergent evolution,
independently, and it is evidence that the shape is right rather than
idiosyncratic.

**The two `Transport`s are not the same seam, and that is the whole
difference.** ureq's is a *byte stream*:
`buffers()`, `transmit_output(amount, timeout)`, `await_input(timeout)`,
`is_open()`, `is_tls()` (`src/unversioned/transport/mod.rs:266-311`),
bounded `Debug + Send + Sync + 'static`. It sits where
`http_ng_rt::TcpConnect` sits, not where `http_ng_core::Transport` does, and
that placement decides two things at once: a `wasi:http` or `fetch` backend
**cannot implement it**, because neither has a byte stream to hand over, and
a single-threaded backend cannot either, because of the `Send + Sync` bound.
Both of those are exactly what `http_ng_core::Transport`'s shape — taken
from `wasi:http/client.send`, *"the poorest of the ambient APIs"*
(`transport.rs:5-11`) — was chosen to allow. ureq's seam does what ureq
needs, which is SOCKS, TLS and test doubles; it is not a smaller version of
this one.

**A build that carries no TLS, no resolver and no protocol machinery.**
`cargo tree -e normal` unique crates, measured in this tree today:

| build | crates |
|---|---|
| `http-ng`, `--no-default-features` | **18** |
| `http-ng-rt-tokio` alone | 22 |
| `http-ng-native`, `--no-default-features` | 32 |
| `http-ng` + `default-transport` (native, tokio, rustls, system DNS) | **83** |
| ureq 3.4.0, `--no-default-features --features rustls` | 23 |
| ureq 3.4.0, default | 28 |
| reqwest 0.13.4, `--no-default-features --features rustls` | 81 |
| reqwest 0.13.4, default | 91 |

**Read this table honestly: on the mainstream native path this workspace is
not smaller than reqwest.** 83 against 81 is a wash, and the reason is that
both end up carrying hyper, rustls and tokio. What the seam buys is the
*other* rows — the ambient backends with no tokio at all, and the 18-crate
floor a `NoTls` + `IpLiteralOnly` build starts from. Claiming a small
graph as a general property would be over-claiming, which is the thing this
project's own capability rule is about.

**A blocking-free single-threaded story.** `Transport` declares no `Send`,
and `two_runtimes.rs` runs the native transport on a bare
`futures_executor::block_on` with no reactor. Neither reqwest nor isahc can
do this. The claim has one honest qualifier: `Client::execute` requires
`T::Error: Send + Sync + 'static` (`client.rs:643`, amendment C1), so a
transport with an honestly `!Send` error is representable at the seam and
cannot be used with `Client`.

---

## 6. Against the project's own goal

The stated goal is not "match reqwest". It is stated twice, in two places,
and only one of them is met.

**"Powerful enough that someone else could build gRPC on it" — met, and
measured.** `docs/grpc-yardstick.md`: 21 requirements from
`grpc/doc/PROTOCOL-HTTP2.md`, 15 tests, **no library code changed**, and the
three limitations it recorded were closed by `Native::multiplexed()` — two
of them costing no code of their own. Two things the client cannot honour
are not its fault (header order, unreachable through `http::HeaderMap`) and
one is a SHOULD to servers. This is as strong an answer as an external
yardstick can give.

What the yardstick did not check, in its own words: a real gRPC server
(everything is hand-written frames), real TLS, and HTTP/3. An interop run
against `tonic` or `grpc-go` is the obvious next piece of evidence and is
not reachable from the current test suite.

**"Full-featured" as the publishing trigger — not met, and the design's own
1.0 conditions say so more sharply than §3 does.** The spec's *Conditions
for 1.0* (design spec §10) list four:

| condition | state |
|---|---|
| plugin traits validated against ≥3 backends | **met** — five: native, h3/select, wasi, fetch, urlsession |
| ≥3 runtimes | **met** — tokio, smol, embassy, and quinn's through `http-ng-rt-quinn`. **`compio` is named in decision D10 as a CI runtime and does not exist here**; grepped, one hit, in a `tree-guard` *absence* check |
| the `unversioned` quarantine is documented | not assessed here |
| **`http-ng-rmcp` and `act` in production** | `http-ng-rmcp` **does not exist in this workspace**. It is named as "the second verification loop" in the v0.2 plan and as an `rmcp` adapter in the architecture diagram. `act` is the *first* loop and is present as `examples/portable.rs` |
| **not a single foreign type remains in the public API** | **flatly contradicted by AGENTS.md**, which states that `http::{Request, Response, HeaderMap, Uri, Method}` appear in the public API of ten crates and that this is necessary. See §8 |

Two further planned crates named in the version plan are absent:
`http-ng-espidf` and `http-ng-nyquest` (both v0.4).

So the honest answer to "is it full-featured yet" is: the *hard* half was
done and the *ordinary* half was not. Every protocol capability a
demanding consumer would ask for exists — h2, h3, WebSocket,
WebTransport, SSE, trailers, duplex, 0-RTT, `1xx`, `Expect`, caching,
cookies, proxies — and what was missing was the ordinary furniture: a
`User-Agent`, a default header set, a size limit, a charset, a pluggable
store.

**Seven are closed** — G2, G5, G6, G8, G9, G10 and G12's digest half, plus
`deflate`/`zstd` from §4 — which does not retire the argument this
paragraph was making. It sharpens it. Every one was found by *writing this
document*, not by the test suite, which was green throughout and is green
now; each took under a day once named. That is the signature of the class:
cheap to fix, invisible from inside, and found only by someone trying to
use the thing. A second real consumer — `http-ng-rmcp` — is still the
argument, because whatever is next is equally invisible from here and this
document cannot be written twice by the same reader.

**What is left is a different shape, which is itself a finding.** G1 is
the owner's call, G3 is refused by the problem statement, and G13's cause
is identified upstream (rust-lang/rust#109417). Of the rest, G4 needs
browser judgement about the browser's own model, G7 and G11 are more
fields on seams that already exist, and G11a is the one that needs care —
Happy Eyeballs interleaves resolution with connecting on purpose, so
*"resolve took N ms"* is not a phase this connector has, and the honest
bound is time-to-first-address rather than a phase boundary. The furniture
is on; what remains is either somebody else's decision or a design
question.

**Four of the seven changed a rule rather than adding a feature**, which
is the return a fresh reader buys that a test suite cannot. G5 established
that a feature must never change what an existing method *means*. The
`deflate`/`zstd` work reversed two refusals whose premises turned out to
be wrong when checked instead of argued. G6 found that a defaulted type
parameter needs a default *type*, which an optional dependency does not
supply — an argument nobody had made. And G12 drew the line this project
had never had to draw about dependencies: hand-write what fails loudly,
take what fails silently.

**Two predictions in this document were wrong, and both in the same
direction — too optimistic about `Send`.** G9 said a redirect predicate
needed no bound since nothing is spawned around the decision; true of the
*call*, but the closure is stored in a `Client` meant to cross a
`tokio::spawn`, so an unbounded one would make every client `!Send`. G6
said the existing `Arc<Mutex<..>>` "already imposes `Send` in practice, so
this needs looking at rather than assuming" — the looking found the same
answer. Both landed on amendment C12, which is now the shape for *a value
the caller owns, reaching `Client` by erasure rather than by a type
parameter*.

**Two real consumers say the same thing about reqwest, and they say it in
opposite directions.** `xh` — the curl-replacement CLI built on reqwest —
carries its own reimplementations of redirects (`src/redirect.rs`),
decompression (`src/decoder.rs`), digest auth, sessions, netrc and a
middleware layer, all on top of a client that already has the first two.
`hurl` does not use reqwest at all; it is built on libcurl. Both are
evidence about which absences a consumer actually pays for, and both point
at the same three: **auth schemes beyond basic/bearer (G12), a policy hook
on redirects (G9), and enough control over decompression to override it.**
Two of those three are on this document's ranked list; the third —
overriding decompression — is already answered here, since a caller who
sets `Accept-Encoding` keeps it exactly (`grpc-yardstick.md` row 20).

---

## 7. What this did not check

- **`isahc` and `attohttpc` are surveyed much less thoroughly than reqwest,
  ureq, gloo-net and hyper-util.** Their cells in §2 carry `—` where nothing
  was read. The `curl` crate was checked directly on the five rows that
  carry an argument — auth schemes, WebSocket, Unix sockets, HTTP/3 version
  selection — and the rest of the `cu` column rests on libcurl's documented
  option set rather than on a line-by-line audit of the binding. The
  WebSocket row is a warning about doing that: libcurl 8.21 has the API and
  the Rust binding does not expose it, so "libcurl can" and "a Rust caller
  can" came apart on the first row where anyone looked.
- **isahc's maintenance state was not established.** It is the client whose
  answer would most change the shape of §2, since a dead comparator should
  be dropped the way `surf` was, and nothing here checked.
- **No comparator was benchmarked.** Every performance number in this
  document is one this repository already measured about itself.
- **reqwest's full-duplex behaviour was not established.** The `full_duplex`
  row for reqwest is `—` rather than `N`, because proving it needs a server
  fixture and reading `hyper-util` was not enough.
- **ureq on `wasm32-wasip2` was run against a host with sockets granted.**
  The claim that `wasi:http` reaches sandboxes ureq cannot is an argument
  from the WIT, not a measurement — nothing here was run against a host with
  `wasi:sockets` denied and `wasi:http` allowed.
- **Nothing here was checked on Windows or macOS.** The Apple `URLSession`
  rows rest on `http-ng-urlsession`'s own record, which states its four live
  tests are green on macOS 27.
- ~~**`Capabilities::proxy` has a producer and still no reader.**~~
  **Answered, and the answer is bigger than the field.** `proxy` is not one
  field without a reader — it is one of *eleven*, and the six this entry
  could have named (`proxy`, `client_certs`, `tls_config`, `early_data`,
  `connection_reuse`, `cancel_on_drop`) are in exactly the same state.
  Which means the question was the wrong one: `Capabilities` has **two
  kinds of field**, and the difference had never been written down.

  A **gate** guards a setting a caller made on the `Client`, and
  `build()` refuses when the transport cannot honour it — a gate with no
  branch is the *silently ignored setting* defect. A **report** states a
  fact about the transport, and nothing at the client level could refuse
  it, because the setting it describes is configured *on the transport*.
  `proxy` is a report: what it would guard is `Native::proxy`, on the
  object that would answer the question.

  It is not `upgrade`'s case either — that enum was deleted for having
  four variants encoding a distinction with one reachable side, where a
  report has both values reachable and answers a question only it can
  answer. The classification is now on the type, and enforced:
  `every_capability_is_a_gate_or_a_report` destructures the struct with no
  `..`, so a field added later is a compile error until somebody decides
  which kind it is. Verified by adding one and watching both destructures
  fail.
- **No security review was attempted.** G10 is named as a gap, not as a
  finding.

---

## 8. Five things in the repository's own record that this reading contradicts

Recorded here rather than fixed, because each is somebody's call.

**1. `README.md` is stale by three versions.** It says *"v0.1 is HTTP/1.1
over those three backends. Connection pooling, HTTP/2 and /3, streaming
request bodies and WebSocket are not built yet"*. All five have shipped, and
there are five backends rather than three. It is the first file a reader
opens.

**2. AGENTS.md does not mention four crates that exist.**
`http-ng-tower`, `http-ng-rt-embassy`, `http-ng-dns-hickory` and
`http-ng-mock` appear nowhere in it (grepped). Two of them are listed in
`docs/v01-acceptance.md`'s *Deliberately not done* as things v0.1 would not
do — *"hickory and DoH; middleware and `http-ng-tower`"* — and both now
exist. The DoH half of that sentence *was* updated (AGENTS.md has a section
on `http-ng-dns-doh`); the hickory and tower halves were not.

**3. "Microcontrollers are not reachable today" is now half true.**
AGENTS.md says so and names two obstacles, `http` 1.x and `url`. `url` is
gone and `http-ng-rt-embassy` exists — `embassy-net` sockets, `embassy-time`
clock, running under `embassy_executor::Executor`, with a CI job
(`embassy-tests-link-under-a-strict-linker`) watching it. The crate still
imports `std::time::Duration`, so the `http` 1.x obstacle is intact and a
bare-metal target is still out; a device *with* `std` — esp-idf — is not.
The sentence should distinguish the two.

**4. The design spec's 1.0 condition "not a single foreign type remains in
the public API" is contradicted by AGENTS.md's position on `http`**, which
argues that `http::{Request, Response, HeaderMap, Uri, Method}` in ten
crates' public APIs is correct and that `std` is required because of it.
These cannot both stand. AGENTS.md is the contract and the spec is the older
document, so the spec's condition is the one that should move — but it is
currently the only written statement of what 1.0 requires.

**5. The first item in `http-ng-urlsession`'s justification is already
reachable without it.** AGENTS.md says the crate *"exists for the list a
userspace stack cannot reach on an Apple platform: enterprise roots pushed
by MDM, per-app VPN, the system proxy and its PAC, background transfer."*
The **first** of those four is not on that list. `rustls-platform-verifier`
0.7.0 — which is what `Rustls::with_platform_verifier()` and therefore
`DefaultTransport` already use — evaluates through
`SecTrust::create_with_certificates` with a `SecPolicy::create_ssl`
(`src/verification/apple.rs:130-138`), which is an evaluation against the
system trust settings, MDM-installed anchors included. It even goes out of
its way to *avoid* `SecTrustSetAnchorCertificates` because that call
"disables the trusting of any other anchors" (`:176`). The other three
items stand, and so does the redirect argument that follows them in
AGENTS.md, which is the crate's strongest one anyway. One sentence wants a
correction, not the section.

---

## 9. The summary, in four sentences

The strongest thing this client lacks against its comparators is **being
published**; after that, the ordinary ergonomics a second real consumer
would have found — a `User-Agent`, default headers, a response body size
limit, a store the caller can supply.

The strongest thing it has and they do not is **one `Transport` seam that
five backends implement and three targets compile the same source against,
with a capability model that refuses at `build()` rather than ignoring at
run time**. ureq 3 is the near miss that makes the point sharp: it has a
`Transport` trait, a `Resolver` trait, a sans-io crate and an `unversioned`
quarantine of the same name — and its `Transport` is a byte stream bounded
`Send + Sync + 'static`, which no ambient backend and no single-threaded one
can implement.

The most surprising single fact found while writing this is that **neither
reqwest's nor ureq's default cookie jar applies public-suffix rules**, so
this workspace's compiled-in list is not a nicety but the only such check
among the clients compared.

And the sentence this document would most like a reader to take away is the
one it had to correct about itself: an earlier draft asserted ureq installed
that list, on plausibility alone, and reading `src/cookies.rs:172-180` is
what stopped it becoming a claim.
