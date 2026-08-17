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
| Digest / NTLM / Negotiate | **N** | N | N | Y | N | libcurl only — see §3 G7 |
| client-wide default headers | **N** | Y | Y | — | Y | `reqwest-0.13.4/src/async_impl/client.rs:1166`; ng has no `ClientBuilder` header setter at all |
| a `User-Agent` at all | **N** | Y | Y | Y | (browser's) | `ureq-3.4.0/src/config.rs:546`; **ng sends none** |
| base URL / relative URLs | Y | **N** | N | N | N | `ClientBuilder::base_url` — reqwest #988/#213 open since 2017 |
| per-request timeout override | Y | Y | Y\* | Y | Y | `RequestBuilder::timeouts` (`request.rs:341`) |
| per-request redirect override | Y | N | — | — | N | `RequestBuilder::redirect` (`request.rs:387`) |
| set an `http::Extensions` value from the builder | **N** | — | — | — | — | recorded as deliberate, `v03-acceptance.md:3394` — see §4 |
| `error_for_status` | **N** | Y | Y | — | Y | `reqwest-0.13.4/src/async_impl/response.rs:378` |
| response text with charset from `Content-Type` | **N** | Y | Y | — | — | `Collected::text` is `String::from_utf8` (`response.rs:174`); rq/uq both ship an `encoding_rs`-backed path behind a `charset` feature |

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
| a response body size limit | **N** | N | **Y** | Y | N | **ureq defaults to 10 MB** on `read_to_string`/`read_to_vec`/`read_json` and says so where the raw reader is handed over — *"a malicious server could send gigabytes"* (`ureq-3.4.0/src/body/mod.rs:36, :215-217`). ng has none anywhere in `http-ng`/`http-ng-core`: grepped `max_body`/`size_limit`/`body_limit`, and the only hits are the cache's own `Limits` |
| header size / count limits | **N** | — | Y | Y | n/a | `Config::max_response_header_size` (`ureq…/src/config.rs:586`) |
| non-destructive body read | Y | N | — | — | — | `Collected` keeps status/headers/url after `.text()` — reqwest #1542 |

### 2.3 Protocols

| capability | ng | rq | uq | cu | br | note |
|---|---|---|---|---|---|---|
| HTTP/1.1 | Y | Y | Y | Y | (browser's) | |
| HTTP/2 | Y\* | Y | N | Y | — | ng behind `http-ng-native/http2`, off by default |
| h2 multiplexing on by default | N | Y | — | Y | — | ng: `Native::multiplexed()`, opt-in, because it needs `R: Spawn` |
| h2 tuning (window, frame size, keepalive PING, prior knowledge) | **N** | Y | N | Y | N | eight setters, `reqwest-0.13.4/src/async_impl/client.rs:1563-1674` |
| HTTP/3 | Y | Y\*\* | N | Y\* | — | **rq requires `RUSTFLAGS='--cfg reqwest_unstable'`** (`src/lib.rs:252`) **and a per-request `.version(HTTP_3)`** — the only dispatch site matches on the request's version (`async_impl/client.rs:2638`), so `http3_prior_knowledge()` does not route anything; cu depends on the libcurl build |
| WebSocket | Y | **N** | N | — | Y | zero matches for `websocket` in all of `reqwest-0.13.4/src/` |
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
| **bind to an interface** (`SO_BINDTODEVICE`) | **N** | Y | — | Y | N | `reqwest…client.rs:1745`; grepped this tree for `SO_BINDTODEVICE`/`bind_device` — zero hits |
| TCP keepalive | Y\* | Y | — | Y | N | ng has `TcpOpts::keepalive` (one duration); rq has interval, retries and `TCP_USER_TIMEOUT` besides |
| Unix domain socket transport | **N** | N | — | Y | N | libcurl's `CURLOPT_UNIX_SOCKET_PATH` |
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
| SOCKS4/4a | **N** | Y | — | Y | — | `reqwest-0.13.4/src/proxy.rs:130` accepts `socks4`/`socks4a` |
| SOCKS remote DNS | **Y, always** | Y\* | — | Y | — | ng sends `ATYP=0x03 DOMAINNAME` unconditionally (`http-ng-native/src/proxy.rs:534`, doc at `:425` — *"a name, never an…"*), so the leak is not reachable. rq picks from the **scheme**: `socks4`/`socks5` → `DnsResolve::Local`, `socks4a`/`socks5h` → `DnsResolve::Proxy` (`src/connect.rs:540-541`), so a `socks5://` URL leaks DNS by default |
| read the proxy from the environment | **N** | Y | Y\* | Y | n/a | **refused as policy** — see §4 |
| `NO_PROXY` matching | Y\* | Y | — | Y | n/a | ng: `Proxy::bypass([..])` takes a list the caller wrote; it does not read `NO_PROXY`, and has **no CIDR** deliberately, where `hyper-util` 0.1.20's matcher does |
| a per-request/per-URL proxy rule | **N** | Y | — | Y | n/a | rq: `Proxy::custom(closure)`; ng has one proxy per transport |
| PAC | **N** | N | N | N | (browser's) | nobody in Rust has this |
| system proxy (Windows registry, macOS SCF) | **N** | Y\* | Y\* | Y | (browser's) | rq behind `system-proxy`; uq behind `win-system-proxy` (Windows only) |
| proxy for QUIC (MASQUE / `CONNECT-UDP`) | N | N | N | — | n/a | refused: a different protocol against a different server |

### 2.7 Stateful client behaviour

| capability | ng | rq | uq | cu | br | note |
|---|---|---|---|---|---|---|
| redirect limit | Y | Y | Y | Y | (browser's) | |
| distinguish "do not follow" from "follow zero hops" | **Y** | Y | — | N | N | `RedirectPolicy::None` vs `Limited(0)` |
| **custom redirect predicate** | **N** | Y | — | N | N | `reqwest-0.13.4/src/redirect.rs:102` |
| strip `Authorization` across origins | Y | Y | — | Y\* | (browser's) | ng also strips `AllowEarlyData` there |
| cookie jar | Y\* | Y\* | Y\* | Y | (browser's) | |
| **public-suffix rules in the jar** | **Y** | **N** | **N** | Y\* | (browser's) | the sharpest row in the table — see §5. ng compiles a list in (+77 KiB, measured). **Neither reqwest nor ureq rejects anything by default**, and both land there through `cookie_store` 0.22: reqwest's `Jar` is `#[derive(Default)] RwLock<cookie_store::CookieStore>` (`src/cookie.rs:30-31`) and `CookieStore`'s `Default`/`new()` leave `public_suffix_list: None` (`cookie_store-0.22.1/src/cookie_store.rs:40-48, :463, :467-473`); ureq builds its jar with `CookieStore::from_cookies(empty, true)` (`src/cookies.rs:177-180`), which sets the same `None` (`:460-464`), and takes `cookie_store` with `default-features = false` besides (`Cargo.toml:152-156`). Zero hits for `public_suffix` in `reqwest-0.13.4/src/`. `publicsuffix` 2.3.0 ships **no list at all**, only `LIST_URL` (`src/lib.rs:29`), so even a caller who wants one must fetch and refresh it |
| **a pluggable cookie store** | **N** | Y | — | Y\* | n/a | `reqwest…client.rs:1213` takes any `CookieStore`. Here `CookieJar<P>` *is* generic over its suffix list, and `ClientBuilder::cookie_jar` pins `P = BuiltinList` (`client.rs:244`) |
| RFC 9111 response cache | **Y** | N | N | N | (browser's) | `http-ng-cache`: freshness, validation, `Vary`, both sides' directives |
| **a pluggable cache store** | **N** | n/a | n/a | n/a | n/a | `CacheStore` is a trait (`http-ng-cache/src/store.rs:257`) and `HttpCache<S = MemoryStore>` is generic — and `ClientBuilder::cache` takes `HttpCache` with the default parameter (`client.rs:313`), so a disk-backed store cannot reach `Client` |
| gzip | Y\* | Y\* | Y\* | Y | (browser's) | |
| brotli | Y\* | Y\* | Y\* | Y | (browser's) | |
| deflate | **N** | Y\* | N | Y | (browser's) | refused — see §4 |
| zstd | **N** | Y\* | N | Y | (browser's) | refused — see §4 |
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

### G2. No `User-Agent`, and no client-wide default headers

`ClientBuilder` has `redirect`, `timeouts`, `base_url`, `total_timeout`,
`cookie_jar`, `cache` and nothing else (`client.rs:78-313`). There is no
header setter at any level but `RequestBuilder::header`, and **no default
`User-Agent` is sent at all**. reqwest has both
(`client.rs:1128`, `:1166`); ureq has `Config::user_agent`
(`config.rs:546`).

*What it takes*: two fields on `Config`, applied in `Client::execute` before
the redirect loop, so that they survive to every hop the way
`Accept-Encoding` already does. *Rules it must respect*: `forbidden_request_headers`
— `http-ng-fetch` forbids several headers the browser owns, and a default
header set that ignored that list would be a client-side setting silently
dropped, which is the shape `check_supported` exists to refuse. `User-Agent`
is on that list for `fetch`, so the honest arrangement is a default the
transport's capability can veto at `build()`, not one applied blindly.
*Forbidden by a rule?* No.

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

### G4. Browser callers cannot reach `mode`, `credentials`, `cache` or `redirect`

`http-ng-fetch`'s `RequestInit` is built with method, headers, body, signal
and (through an unchecked ref) `duplex` — and nothing else
(`crates/http-ng-fetch/src/convert.rs:500-537`). So from a browser this
client cannot send `credentials: "include"`, which is what a cross-origin
authenticated request needs; cannot select `no-cors`; and cannot set the
cache mode or the referrer policy.

reqwest's browser build has all of them, as `fetch_mode_no_cors`,
`fetch_credentials_{same_origin,include,omit}` and `fetch_cache_*` on its
wasm `RequestBuilder` — methods that **do not exist in its native build**
(`reqwest-0.13.4/src/wasm/request.rs:342-438`). So does `gloo-net` 0.6.0,
more completely and with typed `web-sys` enums passed straight through:
`mode` (`src/http/request.rs:147`), `credentials` (`:66`), `cache` (`:59`),
`referrer_policy` (`:181`), `referrer` (`:173`), `integrity` (`:123`),
`redirect` (`:165`), `abort_signal` (`:187`). It also exposes
`Response::type_()`, `redirected()` and the post-redirect `url()`
(`src/http/response.rs:36, :48, :43`). This is therefore a real absence
rather than a knob nobody bothers with: **two independent browser clients
expose all of it and this one exposes none of it.**

*What it takes*: setters on `Fetch`, not on `Transport` — the shape
`Native::multiplexed()` and `Native::expect_continue()` already have, and
the one the workspace's own rule demands, since three of five backends have
no such concept. A request extension would be the tempting alternative and
is the wrong one for the same reason `Prefetch::prepare` refuses to take an
HTTPS record from a caller: an extension is a channel any code that can
build a request can write to. *Forbidden by a rule?* No — the rule tells you
where to put it.

### G5. `text()` is UTF-8 or an error, with no charset from `Content-Type`

`Collected::text()` is `String::from_utf8` (`response.rs:174`). A response
labelled `Content-Type: text/html; charset=windows-1251` comes back as a
`Decode` error rather than as text. reqwest and ureq both ship an
`encoding_rs`-backed path behind a feature named `charset` — the same name,
independently, in both feature tables.

*What it takes*: `encoding_rs`, which is a real dependency of real size, in
a workspace that removed `url` at the cost of writing RFC 3986 §5.2 by hand
and hand-wrote base64 rather than take a crate for twenty lines. *Rules*:
the `gzip`/`brotli`/`json` precedent — one feature, off by default, because
the browser build should not link a charset table it cannot use.
*Forbidden?* No, but the dependency-resistance rule sets a high bar and
`encoding_rs` is 1–2 MB of tables. The honest answer may be to keep
`text()` as it is and document it, which is what it does not do today.

### G6. No pluggable cookie or cache store, though both crates are already generic

`CookieJar<P = BuiltinList>` (`http-ng-cookie/src/jar.rs:202`) and
`HttpCache<S = MemoryStore>` (`http-ng-cache/src/policy.rs:265`) are both
generic. `ClientBuilder::cookie_jar` and `ClientBuilder::cache` both take
the **defaulted** form (`client.rs:244`, `:313`), so the generality stops at
the facade. A caller who wants a disk-backed cache, a shared cache between
processes, or a jar with `NoList` (which is what saves the measured 77 KiB)
cannot get there through `Client`.

This is the most clearly *unintended* gap in the document: the seam exists
one crate down and is unreachable one crate up. *What it takes*: either a
type parameter on `Client` (heavy — `Client<T, Tm>` becoming
`Client<T, Tm, S, P>`) or `Box<dyn CacheStore>`, which is object-safe as
written. *Rules*: no `Send` bound may be added to reach it, which is what
rules out the obvious `Arc<dyn CacheStore + Send + Sync>`; the existing
`Arc<Mutex<..>>` already imposes `Send` in practice, so this needs looking
at rather than assuming. *Forbidden?* No.

### G7. Proxy configuration from the environment, and a per-URL proxy rule

`Proxy::bypass([..])` takes a list the caller wrote down, and reading
`HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY` is **refused as policy** — AGENTS.md
says which half is policy and why. That refusal is well-argued and it is
the right one for a library.

The gap that is *not* refused is next to it: there is one proxy per
transport (`Native::proxy` replaces `P`), so a caller cannot route
`http://` and `https://` through different proxies, which
`Proxy::http`/`Proxy::https` do in reqwest, nor supply a closure
(`Proxy::custom`). And SOCKS4/4a is absent where reqwest accepts both
schemes (`proxy.rs:130`).

*What it takes*: for the per-scheme rule, a second `Option<Proxy<P>>` and a
choice at `via`/`connect` — genuinely small. For a closure, a `Send`-free
`Fn(&Uri) -> Option<&Proxy<P>>`, which the type-parameter-not-`Box<dyn>`
rule already anticipates. *Forbidden?* Environment reading is. The rest is
not.

### G8. HTTP/2 has no tuning surface

reqwest exposes eight h2 knobs — initial stream and connection window,
adaptive window, max frame size, max header list size, keepalive interval,
keepalive timeout, keepalive-while-idle, prior knowledge
(`reqwest…client.rs:1563-1674`). `Native` exposes none: `multiplexed()`
turns the driver on and that is the whole surface.

This bites a caller carrying a large response over a long fat pipe, where
the default 64 KiB window is the throughput ceiling. `tests/grpc_shape.rs`
row 11 already carries 524 328 bytes through that window, so the path is
exercised — it just cannot be widened.

*What it takes*: fields on `Native` forwarded to `h2::client::Builder`;
mechanically small. *Rules*: the capability floor — none of these change a
`Capabilities` value, so nothing is at risk there; and no `Spawn` bound may
leak, which keepalive does not need since the ping is answered by whoever is
polling. *Forbidden?* No. It is a plain absence.

### G9. No custom redirect predicate

`RedirectPolicy` is `None | Limited(u8)` (`http-ng-proto/src/redirect.rs:24`).
reqwest has `Policy::custom(closure)`, which is how a caller refuses a
redirect to a private address, or to a different host, or stops on a
particular status.

*What it takes*: a third variant carrying a function. *Rules*: the closure
must not be bounded `Send + Sync` — the WebSocket seam's `!Send` allowance
and the `Rc`-holding hook are the precedent, and the `1xx` work shows what
it costs when an upstream crate forces the bound anyway. Since the redirect
decision is made inside `Client::run` and nothing is spawned around it, no
bound is needed. `RedirectPolicy` lives in the sans-io `http-ng-proto`,
which is the interesting constraint: a closure there is fine, but it must
stay clockless and io-free, which a redirect predicate naturally is.
*Forbidden?* No.

### G10. No response body size limit

Nothing in `http-ng` or `http-ng-core` bounds a response body. `Deadline`
bounds *time* and does so against the compressed stream deliberately, which
answers the decompression-bomb question for the time axis and not for the
byte axis. A `Content-Length: 900000000000` with a slow enough drip passes
every bound this client has.

*What it takes*: a body wrapper, the `IdleTimeout`/`Deadline` shape exactly,
and one field. *Rules*: it must wrap **outside** the decompressor if it is
counting decompressed bytes and **inside** if it is counting wire bytes, and
the `Decompressed<Deadline<B, Tm>>` ordering argument says which is which —
so this is a decision, not a wrapper. *Forbidden?* No, and it is the
cheapest real security improvement in this list.

### G11. Interface binding, TCP keepalive detail, Unix sockets

`TcpOpts` has six fields and covers `nodelay`, `keepalive` (one duration),
`local_address`, buffer sizes and `reuse_address`. It has no
`SO_BINDTODEVICE` (grepped: zero hits in this tree), no keepalive interval
or retry count, no `TCP_USER_TIMEOUT`, and there is no Unix-domain-socket
transport anywhere.

*What it takes*: three more `TcpOpts` fields and three more
`TcpOptsSupport` bools, which is exactly the shape that already exists —
and note that `TcpOptsSupport` is a field-per-field mirror *because* the
error has to name the option, so growing it is the designed-for change. A
Unix socket is a different `TcpConnect`, or rather a sibling trait, and is
the larger of the two. *Forbidden?* No.

### G11a. No separate bound on name resolution

`Timeouts::connect` covers resolve → Happy Eyeballs → TCP → TLS as one
budget. ureq separates `timeout_resolve` from `timeout_connect`
(`src/config.rs:692, :702`). A caller behind a resolver that hangs cannot
distinguish "DNS is broken" from "the origin is unreachable", and cannot
give the two different budgets.

*What it takes*: a fourth `Timeouts` field, a fourth `TimeoutSupport` bool,
and a race around the resolver call. *Rules*: the declaration and the
enforcement must land in one change — the rule that kept `connect` off
`http-ng-h3` until W1 and `first_byte`/`between_bytes` off native until v0.2
W4. It is also the field most likely to be honestly `false` on the ambient
backends, which is what `TimeoutSupport` is for. *Forbidden?* No.

### G12. Digest, NTLM and Negotiate authentication

Nobody in pure Rust has these; libcurl does. A caller who needs to talk to a
corporate intranet reaches for `curl` and finds nothing else. This is
recorded as a gap rather than ranked higher because the population that
needs it is narrow and it is a whole-ecosystem absence rather than this
project's.

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
| `deflate` and `zstd` decoding | a client must not advertise a coding it may guess wrong about; `zstd` is a third dependency for a coding no server sends unasked | `http-ng/src/decompress.rs:44` |
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

So the honest answer to "is it full-featured yet" is: the *hard* half is
done and the *ordinary* half is not. Every protocol capability a demanding
consumer would ask for exists — h2, h3, WebSocket, WebTransport, SSE,
trailers, duplex, 0-RTT, `1xx`, `Expect`, caching, cookies, proxies. What
is missing is the second verification loop that would find the ordinary
things: a `User-Agent`, a default header set, a size limit, a charset, a
pluggable store. **G2, G5, G6 and G10 are exactly the class of defect a
second real consumer finds and a test suite does not**, which is an
argument for `http-ng-rmcp` before an argument for any individual row in
§3.

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

- **`isahc`, `curl` and `attohttpc` are surveyed less thoroughly than
  reqwest, ureq, gloo-net and hyper-util.** Their columns in §2 carry `—`
  where nothing was read, and the libcurl rows lean on libcurl's documented
  option set rather than on a line-by-line audit of what the `curl` crate
  binds. Treat the `cu` column as indicative.
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
- **`Capabilities::proxy` has a producer and still no reader.**
  `Native::proxy` sets it (`http-ng-native/src/lib.rs:744`), and grepping
  `crates/http-ng/src/` for a read of it returns nothing. So the field is no
  longer inert in the sense `docs/v04-acceptance.md` recorded — it can be
  `true` — but the second half of that entry's demand, *"make the field mean
  something"*, is unmet. Whether it should have a reader at all is a
  question this document raises and does not answer.
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
