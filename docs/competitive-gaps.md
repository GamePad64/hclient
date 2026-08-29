# Competitive gaps: what a caller can do elsewhere, and what is refused here

Written to answer one question with evidence rather than impression: **what
would a caller who arrived from `reqwest`, `ureq` or `niquests` find missing
here**, and which of those absences are decisions this workspace has already
made and written down.

The second half matters more than the first. This repository carries four
acceptance documents whose *Deliberately not done* sections exist precisely
because "a bare list invites someone to 'fix' an item whose absence is the
decision". A gap analysis that reported those as
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
| `isahc` | **2.0.1** | re-fetched 2026-08-19; was 1.8.3 |
| `curl` | 0.4.50 | fetched |
| `curl-sys` | 0.4.90+curl-8.21.0 | fetched |
| `attohttpc` | **0.31.0** | re-fetched 2026-08-19; was 0.29.2 |
| `gloo-net` | **0.7.0** | re-fetched 2026-08-19; was 0.6.0 |
| `hyper-util` | 0.1.20 | already vendored |
| `cookie_store` | 0.22.1 | fetched, to settle one row in §2.7 |
| `publicsuffix` | 2.3.0 | same |
| `rustls-platform-verifier` | 0.7.0 | already vendored, to settle one claim in §8 |
| `web-sys` | 0.3.104 | already vendored |

And one comparator that is not a crate:

| package | version | how it got there |
|---|---|---|
| **`niquests`** | **3.21.1** | `uv pip install niquests` into a throwaway 3.14.4 venv, 2026-08-30 |
| `urllib3-future` | 2.24.905 | its transport, pulled as a hard dependency |
| `qh3` | 1.9.4 | its QUIC/HTTP-3 and TLS-1.3 stack, **also a hard dependency** on this platform |
| `wassima` | 2.1.4 | its OS trust store |

**niquests is compared because it is the closest peer by ambition, not
because it is in Rust.** It is a Python client with the same three
protocols in one client, the same bet that a browser-shaped feature set
belongs inside rather than in a satellite crate, and — this is the part
that makes it a peer rather than a curiosity — a `wasi:http` backend and a
browser backend beside the native one. Every other comparator here answers
a narrower question. Being in another language costs the comparison the two
rows that are about Rust (`Send`, the crate graph) and nothing else: a
capability is a capability.

**It is also the comparator whose surface was exercised live rather than
only read.** reqwest and ureq each contributed one execution (§1) and were
otherwise read at a path and a line; niquests was installed, imported, and
asked — signatures off `inspect`, values off live objects, and four claims
off a program that ran. Where a claim below says *executed* against
niquests, a program in that venv produced the quoted output. That is a
higher grade of evidence than the rest of this document holds itself to,
and §7 says what it still does not cover.

**`surf` is not compared.** Its latest release is 2.3.2, published
2021-11-01, with the last push to its repository in September 2023. A
client that has not shipped in four years is not a comparator; it is
history. `attohttpc` 0.29.2 was fetched and is only lightly surveyed, for
the same reason in weaker form.

**Re-surveyed on 2026-08-19, and three of the eight comparators had
moved.** `reqwest` 0.13.4, `ureq` 3.4.0, `curl` 0.4.50 and `cookie_store`
0.22.1 are still the latest published, so every row citing them stands as
written and was not re-read. What changed:

- **`isahc` 1.8.3 → 2.0.1**, which answers §7's open question about its
  maintenance state in the strongest available way: a major release, on
  edition 2024 with `rust-version = "1.85"`, and TLS turned into a choice
  (`rustls-tls`, `native-tls`, `trust-webpki-roots`, `tls-insecure` —
  none of which 1.8.3 had). It is not `surf`. Its column was mostly `—`
  and is now read; six rows below change because of it, and one of them
  is the sharpest row in the document.
- **`attohttpc` 0.29.2 → 0.31.0** and **`gloo-net` 0.6.0 → 0.7.0**, and
  neither moved a row: `gloo-net`'s `http::{Request, Response}` surfaces
  are identical between the two (diffed, `pub fn` for `pub fn`), and
  `attohttpc`'s feature table gains only rustls plumbing
  (`__rustls-ring`) over the same set of capabilities.

**A version bump is not evidence of change and an unchanged version is not
evidence of staleness** — both had to be checked, and the cheap half is
diffing the public surface rather than reading the changelog.

`hclient` was this tree at `96e8b28` when the document was written, and
the `ng` column has been kept current since — ten of the thirteen ranked
gaps closed between then and `5dc452d`, each row updated with the change
that closed it. The niquests pass re-read every `ng` cell against
`fabc7cb`.

**That re-read is the reason to add a column even where the new column is
mostly `Y` on both sides: it forced eleven `ng` cells and cannot be
skimmed.** The ranked gaps in §3 were kept current because closing one is
an event somebody writes down. The *matrix* was not, because a row nobody
is arguing about is a row nobody re-reads. Eleven were wrong, all in the
same direction — the client had grown past them:

| row | said | is |
|---|---|---|
| §2.3 h2 keepalive PING | absent | `Native::h2_keep_alive`, `lib.rs:2019` |
| §2.4 static host→address override | `seam` | `hclient_dns::Overrides<D>`, `overrides.rs:61` |
| §2.4 TCP keepalive | one duration | interval, retries and `user_timeout` (G11) |
| §2.5 client certs | `seam` | `Rustls::with_identity`, per request, `lib.rs:183` |
| §2.5 disable verification | `seam` | `dangerous-insecure`, a constructor per backend |
| §2.6 proxy from the environment | **refused as policy** | **reversed** — `system-proxy`, in `default` |
| §2.6 system proxy (registry, SCF) | absent | `Native::system_proxy`, `lib.rs:1179` |
| §2.7 custom redirect predicate | `ClientBuilder::redirect_predicate` | the name is gone; it is a `RedirectPolicy` trait |
| §2.7 automatic retry | *"neither is a general retry policy"* | `ClientBuilder::retry`, `client.rs:217` |
| §2.8 published on crates.io | `N` | `0.1.0-alpha.2`, which §3 G1 already said |
| §2.8 a type-erased client | `N*` | `Client` names no parameters, which §3 G13 already said |

The last two are the sharpest, because **the document contradicted itself
and stayed green**: §3 recorded both as closed, at length, and the matrix
two screens above went on saying `N`. A summary and its table drift apart
in exactly the way this workspace's own rule about checks predicts — the
half somebody edits stays true and the half nobody reads does not.

---

## 1. The executions that decided the shape of this document

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
`mio`. `hclient-wasi` builds for the same target in 3.9 s, also executed.

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
the guest), where `hclient-wasi` goes through `wasi:http` — the host's own
HTTP client, with no socket and no TLS stack in the guest at all. A component
host that grants outbound HTTP and *not* raw sockets — which is the common
sandbox policy, and the reason `wasi:http` exists — runs `hclient-wasi` and
cannot run ureq. **Neither was tested against a host with sockets denied**,
so that last sentence is an argument from the WIT rather than a measurement.

**niquests reaches HTTP/3 on the third line of a plain install, and this
workspace does not.** Executed against the venv above — three requests on one
session, nothing configured:

```
$ python probe.py                 # niquests 3.21.1, plain install, no extras
0 200 http_version= 20 conn_info.http_version= HttpVersion.h2
1 200 http_version= 30 conn_info.http_version= HttpVersion.h3
2 200 http_version= 30 conn_info.http_version= HttpVersion.h3
tls_version: 772 cipher: TLS_AES_128_GCM_SHA256
dest: ('8.6.112.6', 443)
ocsp_verified: True
```

Three `*_latency` lines are elided from that output and the reason they are
elided is itself a fact worth having: they read `0:00:00`, because the
program printed `conn_info` after the **third** request and the third
request reused the second's connection. Timings that are zero on a reused
connection are the same thing `Event::Reused` says here.

The first request negotiated h2 over TCP, read the origin's `Alt-Svc`, and the
second went out over QUIC. `qh3` is in that graph because `urllib3-future`
requires it **unconditionally** on CPython on Linux, macOS and Windows on the
mainstream architectures — read off its `METADATA` environment marker — so the
`http3` extra exists only for the platforms that marker excludes. Here the same
three requests need `hclient-native`'s `http3` feature switched on, **and**
`Native::http3()` called, **and** the caller to have reached past
`Client::new()`, whose `DefaultTransport` is HTTP/1.1 with h2 behind a feature.

Two things that comparison does **not** say, and both matter. It is not a
protocol-support gap: this client has HTTP/3, WebTransport, and an h3-vs-h1
chooser fed by the HTTPS record that no comparator has. And the last line of
that output is the other half of the trade — `ocsp_verified: True` means a
second network dependency was consulted on the way, which §4 records as
something this workspace declines to grow. What the comparison is about is
**defaults**, and G14 is where this client's defaults stop being merely
different.

---

## 2. The matrix

Legend: **Y** = a caller writes one call; **Y\*** = present with a stated
restriction, named in the notes; **seam** = not a call, but reachable by
implementing a public trait; **N** = absent; **—** = not checked.

Columns: **ng** = `hclient` (all features, native), **rq** = reqwest 0.13.4,
**uq** = ureq 3.4.0, **cu** = the `curl` 0.4.50 binding / `isahc` 2.0.1,
**nq** = niquests 3.21.1, **br** = the browser story (reqwest's wasm build,
`gloo-net` 0.7.0).

The `nq` column follows the same legend, with one addition: **Y (extra)**
means present but behind a `pip install niquests[..]` extra, which is that
ecosystem's word for a Cargo feature and is treated the same way the `Y*`
of an off-by-default feature is treated in the `ng` column.

### 2.1 Request shaping

| capability | ng | rq | uq | cu | nq | br | note |
|---|---|---|---|---|---|---|---|
| query parameters | Y | Y | Y | — | Y | Y | ng appends and never replaces (`request.rs:195`); rq's `query` takes `Serialize` |
| urlencoded form body | Y | Y | Y | — | Y | Y | ng hand-writes the WHATWG serialiser rather than take `form_urlencoded` |
| JSON request body | Y | Y | Y | — | Y | Y | ng behind `json`; serialises in the builder, so a bad value is a build error |
| `multipart/form-data` | Y | Y | Y | Y | Y | Y | see §2.2 for the streaming difference |
| Basic auth | Y | Y | Y | Y | Y | Y | ng **refuses a colon in the username** (RFC 7617 §2) where the others encode it |
| Bearer auth | Y | Y | Y | Y | Y | Y | |
| Digest / NTLM / Negotiate | **Digest: Y\*** | N | N | **Y** | Digest: Y | N | **Digest closed** — RFC 7616, `RequestBuilder::digest_auth`, MD5 / SHA-256 / SHA-512-256 and their `-sess` variants, checked against the RFC's own §3.9 vectors. The only pure-Rust client with it. NTLM and Negotiate still refused: both need the platform's GSSAPI or SSPI. The `curl` crate binds all of them: `Auth` + `Easy::http_auth` over `CURLAUTH_DIGEST`, `_DIGEST_IE`, `_GSSNEGOTIATE`, `_NTLM`, `_NTLM_WB` (`curl-0.4.50/src/easy/handler.rs:565, :1340, :3733-3790`), and **`isahc` 2.0.1 surfaces two of them in Rust** — `Authentication::digest()` and `::negotiate()` (`src/auth.rs:100, :120`), the second behind its `spnego` feature. So the `cu` column is not only the raw binding here. See §3 G12 |
| client-wide default headers | Y | Y | Y | — | Y | Y | closed — `ClientBuilder::{default_header, default_headers}`, applied per redirect hop, the caller's own header winning, and refused at `build()` where a backend forbids the name. See G2 |
| a `User-Agent` at all | Y\* | Y | Y | Y | Y | (browser's) | `ClientBuilder::user_agent` exists; **ng still sends none by default**, deliberately — a library that names itself on every request decides for its embedder, and the embedder is who has an opinion. `ureq-3.4.0/src/config.rs:546` |
| base URL / relative URLs | Y | **N** | N | N | Y | N | `ClientBuilder::base_url` — reqwest #988/#213 open since 2017 |
| per-request timeout override | Y | Y | Y\* | Y | Y | Y | `RequestBuilder::timeouts` (`request.rs:341`) |
| a separate bound on name resolution | Y\* | N | Y | Y | N | n/a | closed — `Timeouts::resolve`. What it bounds is the wait for the **first address**, not a phase: Happy Eyeballs interleaves resolving with connecting, so there is no instant at which resolution finished. `false` on both ambient backends, honestly. See G11a; `ureq-3.4.0/src/config.rs:692` |
| per-request redirect override | Y | N | — | — | Y\* | N | `RequestBuilder::redirect` (`request.rs:387`) |
| set an `http::Extensions` value from the builder | **N** | — | — | — | n/a | — | recorded as deliberate, `v03-acceptance.md:3394` — see §4 |
| `error_for_status` | Y | Y | Y | — | Y | Y | on both `Response` and `Collected` — the second for a caller who wants the server's error text before deciding. A `3xx` is `Ok`, because reaching one means the redirect policy already handed it back. Writing it found that `Response::url()` reported the *requested* URL rather than the answering one, undocumented and untested; it is the last hop now |
| response text with charset from `Content-Type` | Y\* | Y | Y | Y | Y\* | — | closed: `Collected::text_with_charset` behind a `charset` feature — the name rq and uq both independently chose. **A separate method, not a smarter `text()`**: Cargo unifies features, so a feature that changed what `text()` means would make a library's behaviour depend on what an unrelated crate switched on, and the difference is silent mojibake rather than an error. **`isahc` 2.0.1 is the live counter-example to the other half of the decision**: its `text-decoding` is *in `default`*, and an unknown label falls back to UTF-8 with a `tracing::warn!` (`src/text.rs:64-70`) — where ng makes it a typed error naming the label, because a warning nobody reads and mojibake handed back are the same outcome for the caller |

### 2.2 Bodies and streaming

| capability | ng | rq | uq | cu | nq | br | note |
|---|---|---|---|---|---|---|---|
| streaming response body | Y | Y | Y | Y | Y | Y | ng hands back an `http_body::Body`; rq adds `bytes_stream()` behind `stream` |
| streaming request body | Y | Y | Y | Y | Y | Y\* | |
| **full duplex** | Y\* | — | N | — | — | Y\* | ng: `true` on `hclient-h3` and on h2, and the capability still reports the HTTP/1.1 **floor** — see §5 |
| replay contract knowable before sending | **Y** | N | N | N | N | N | `RetryKind::{Free, ViaFactory, Impossible}`, and multipart derives it from its parts |
| streaming multipart | Y | Y | — | Y | — | — | ng: any streaming part makes the whole form `Streaming`/`Impossible` |
| response trailers reach the caller | Y | N | N | **Y** | Y | N | ng on h2 and h3; read via `into_parts()`, not `collect()`. `isahc` 2.0.1 has a `Trailer` handle with `try_get`, `wait` and `wait_timeout` (`src/trailer.rs:26-107`) — a *blocking* read, which is the shape a curl-backed client can offer and an async one cannot |
| request trailers | Y\* | N | N | — | N | N | sent on h1 and h2, and `Capabilities::request_trailers` understates the h2 path — a known mismatch, `v03-acceptance.md:3132` |
| a response body size limit | Y | N | **Y** | Y | N | N | closed — `ClientBuilder::response_limit`, counting **decompressed** bytes, which is the axis a decompression bomb lives on. Unset by default, unlike ureq: a ceiling this crate chose would fail a caller's legitimate large download. **ureq defaults to 10 MB** on `read_to_string`/`read_to_vec`/`read_json` and says so where the raw reader is handed over — *"a malicious server could send gigabytes"* (`ureq-3.4.0/src/body/mod.rs:36, :215-217`). ng has none anywhere in `hclient`/`hclient-core`: grepped `max_body`/`size_limit`/`body_limit`, and the only hits are the cache's own `Limits` |
| header size / count limits | Y | — | Y | Y | — | n/a | closed on both protocols — `Native::h1_opts` (`max_headers`, `max_buf_size`) and `H2Opts::max_header_list_size`. Neither is complete without the other: a transport that negotiates ALPN speaks whichever the server picked. `h1_opts` is **fallible** where `h2_opts` is not, because hyper panics below 8192 and a caller's number must not reach a `panic!` inside a connect. `ureq…/src/config.rs:586` |
| non-destructive body read | Y | N | — | — | Y | — | `Collected` keeps status/headers/url after `.text()` — reqwest #1542 |

### 2.3 Protocols

| capability | ng | rq | uq | cu | nq | br | note |
|---|---|---|---|---|---|---|---|
| HTTP/1.1 | Y | Y | Y | Y | Y | (browser's) | |
| HTTP/2 | Y\* | Y | N | Y | Y | — | ng behind `hclient-native/http2`, off by default |
| h2 multiplexing on by default | N | Y | — | Y | Y | — | ng: `Native::multiplexed()`, opt-in, because it needs `R: Spawn` |
| h2 tuning (window, frame size, keepalive PING, prior knowledge) | Y\* | Y | N | Y | N | N | closed for the settings frame — `Native::h2_opts`: both windows, `max_frame_size`, `max_header_list_size`. **Keepalive is closed too, and not as an `H2Opts` field** — `Native::h2_keep_alive`, on the opt-in `multiplexed()` constructor, because a `SETTINGS` field is written once at handshake where a `PING` needs a driver. An adaptive window and prior knowledge remain absent, each for its own reason rather than as a batch. **nq exposes no knob and picks values anyway**, which is the opposite of this crate's rule rather than a smaller version of it: `contrib/hface/protocols/http2/_h2.py:72-80` hard-codes `INITIAL_WINDOW_SIZE: 6291456`, `HEADER_TABLE_SIZE: 65536`, `MAX_HEADER_LIST_SIZE: 262144` and `ENABLE_PUSH: 0` into the `SETTINGS` frame, and nothing in `niquests/` lets a caller move them (grepped for all three names, zero hits). `H2Opts`' every-field-`None` default exists precisely so that a caller who asked for nothing announces what `h2` chose rather than what this workspace chose; niquests announces 6 MiB to every server on the caller's behalf. Which of the two is right is a genuine argument — 6 MiB is roughly Chrome's number and 64 KiB is a bandwidth ceiling on a long fat pipe (§G8) — and it is an argument this client hands to the caller and that one does not. See G8; reqwest's eight are `reqwest-0.13.4/src/async_impl/client.rs:1563-1674` |
| HTTP/3 | Y | Y\*\* | N | Y\* | Y | — | **rq requires `RUSTFLAGS='--cfg reqwest_unstable'`** (`src/lib.rs:252`) **and a per-request `.version(HTTP_3)`** — the only dispatch site matches on the request's version (`async_impl/client.rs:2638`), so `http3_prior_knowledge()` does not route anything; cu depends on the libcurl build — and `isahc` 2.0.1 makes that dependence readable rather than a build-time surprise: `VersionNegotiation::http3()` exists and `info.rs:47` asks `curl_info().feature_http3()` at run time, so a caller can find out whether the linked libcurl has it. **nq is the one that needs nothing at all** — see §1's execution, where the second request on a fresh install went out over QUIC |
| WebSocket | Y | **N** | N | **N** | Y (extra) | Y | zero matches for `websocket` in all of `reqwest-0.13.4/src/`. And **libcurl 8.21 has a WebSocket API that the Rust binding does not expose** — zero files matching `CURLWS`/`ws_send`/`ws_recv`/`websocket` in either `curl-0.4.50/src/` or `curl-sys-0.4.90/src/`, so the capability exists in the C library and not in Rust. **nq has it over all three protocols and this client has it over one** — `WebSocketExtensionFromMultiplexedHTTP.supported_protocols()` answers `{h11, h2, h3}` and the h2/h3 arm writes `:protocol: websocket` with `:method: CONNECT`, i.e. RFC 8441 extended CONNECT (`contrib/webextensions/ws.py:100-104, :292`, read). Here `hclient_native::Upgrading` is an `http1::Connection` and nothing else, so a `ws://` over an h2 connection is not expressible. See G16 |
| WebTransport | **Y** | N | N | N | N | N | `hclient-webtransport`: sessions, bidi streams, datagrams, close capsules |
| Server-Sent Events | **Y** | **N** | N | N | Y | Y | zero matches for `eventsource`/`text/event-stream` in `reqwest-0.13.4/src/`; ng has a decoder *and* reconnection with `Last-Event-ID` |
| `1xx` / `103 Early Hints` observable | **Y** | N | — | — | Y | N | `Native::watching_1xx()` + `Event::Informational`. nq has it too and over all three protocols — the `early_response` hook, dispatched from `sessions.py:1518` off urllib3-future's `EarlyHeadersReceived`. So this row is no longer one where the field is empty besides curl |
| `Expect: 100-continue` | **Y** | N | **Y** | Y | N | N | `Native::expect_continue(after)`; **hyper's client does not do this**, so reqwest cannot. uq has it *and* a dedicated `timeout_await_100` (`src/config.rs:721`), which is the same "a wait ending in *proceeding*, not in failure" distinction this workspace argues for keeping out of `Timeouts`. **`isahc` 2.0.1 does it by default** and lets a caller turn it off (`ExpectContinue`, `src/config/mod.rs:209`) — the opposite default from ng's, where a default that waited would be a default that hangs against a server ignoring `Expect`. **nq is `N` and the near-miss is worth naming**: it *observes* a `100` through the same `early_response` hook that reads `103`, and it never *sends* `Expect` — grepped both packages for the header, and the one textual hit outside a header-name table is the English verb in `_base.py:666`'s *"Expect protocol handshake to be done here"*, the same trap this workspace records for `embedded-nal-async`'s three `Send`s. Observing the answer and gating the body on it are different features |
| demand a specific version and fail otherwise | **Y** | N\* | N | Y\* | Y\* | N | `RequireVersion` is enforced before the head; rq's `http1_only`/`http2_prior_knowledge` are client-wide settings, not per-request demands |

### 2.4 Connections, sockets, resolution

| capability | ng | rq | uq | cu | nq | br | note |
|---|---|---|---|---|---|---|---|
| connection pool | Y | Y | Y | Y | Y | (browser's) | ng: `PoolConfig { idle_timeout, max_idle_per_key }` |
| per-request timing a caller can read | Y\* | N | N | **Y** | Y | N | **A row this document did not have, and `isahc` 2.0.1 is why it should.** Its `Metrics` gives `name_lookup_time`, `connect_time`, `secure_connect_time`, `transfer_start_time`, `transfer_time`, `total_time` and upload/download progress and speed (`src/metrics.rs:58-142`), behind one `metrics(true)` switch. ng has `ConnectTiming` on the `Connected` hook event with `dns`/`tcp`/`tls`/`total` and `Head::elapsed` — the same four phases, but through a `Hooks` impl rather than off the response, which is `Y*` rather than `Y`: a caller who wants one request's numbers must write a hook and correlate by `ConnectionId`. Neither reqwest nor ureq has either — **and nq has the shape isahc has rather than the shape this client has**, which is what makes `Y*` the honest cell here rather than pedantry. `response.conn_info` is a `ConnectionInfo` carrying `resolution_latency`, `established_latency`, `tls_handshake_latency`, `request_sent_latency`, `http_version`, `cipher`, `tls_version`, `destination_address` and **both peer and issuer certificates, parsed** (`urllib3_future/backend/_base.py:41-95`, read; and executed above). Reaching it is an attribute access on the response a caller already holds. Reaching the same four phases here means writing a `Hooks` impl and correlating by `ConnectionId`, because `Connected` is an *event about a connection* and a response is not where a connection's history lives. That is a real design difference and not obviously the wrong one — a pooled connection serves many responses, so hanging its handshake timings off one of them is a choice about which response gets them — but the caller who wants *this request's* numbers pays for it here and does not there |
| a reaper that closes idle sockets | Y\* | Y | — | Y | Y\* | — | ng: `Native::with_reaper`, opt-in, bounded on `R: Spawn` |
| `TCP_NODELAY` | Y | Y | — | Y | Y | N | |
| local source address | Y | Y | — | Y | Y | N | `TcpOpts::local_address` |
| **bind to an interface** (`SO_BINDTODEVICE`) | Y\* | Y | — | Y | seam | N | closed — `TcpOpts::bind_device`. `isahc` 2.0.1 has `interface(..)` taking a name, an address or `Any` (`src/config/mod.rs:393`, `src/net/interface.rs`), which is libcurl's `CURLOPT_INTERFACE` and covers both what ng splits into `bind_device` and `local_address`. Not `local_address` renamed: an address binds the *source address* and the kernel still routes by its table, where this binds the interface. Linux/Android/Fuchsia only, which is why `Tokio::APPLIES` stopped being `TcpOptsSupport::ALL`. See G11 |
| TCP keepalive | Y | Y | — | Y | seam | N | **closed since this row said *one duration*** — `TcpOpts` has `keepalive`, `keepalive_interval`, `keepalive_retries` and `user_timeout`, which is rq's set exactly. Setting any of the three enables `SO_KEEPALIVE`, so a caller who sets only the interval has switched keepalive on with the OS's idle time; asserted, because it reads as a surprise otherwise. nq reaches it through urllib3's `socket_options`, i.e. by naming the constant. See G11 |
| Unix domain socket transport | Y | N | N | **Y** | Y | N | closed — `Native::unix_socket(path)`, curl's `--unix-socket` exactly. `isahc` 2.0.1 has it too and spells it as a *dialer*: `Dialer::unix_socket("/var/run/docker.sock")`, or the URI form `"unix:/path/to/my.sock".parse::<Dialer>()` (`src/net/dial.rs:41, :95`) — which is a nicer shape than a transport-wide setting for a caller who wants one request over a socket. **Not a sibling trait**, which is what this document expected: a second trait would have to produce `TcpConnect::Stream` anyway, so it is a defaulted method on that seam — `reports_alpn`'s shape. See G11 |
| static host→address override (`--resolve`) | Y | Y | — | Y | Y | N | **the `seam` this row used to say is closed**: `hclient_dns::Overrides<D>` (`overrides.rs:61`) answers from a table and hands the rest to `D`, so it composes over the system resolver, over DoH, over anything. Host-wide rather than curl's `host:port:addr`, because `Resolve` is asked for a name and a family and never sees a port. nq spells it as a resolver URL, `in-memory://` |
| pluggable resolver | seam | Y | — | Y | Y | N | rq: `dns_resolver`; ng: the `Resolve` trait, with three shipped backends |
| Happy Eyeballs (RFC 8305) | Y | Y | — | Y | Y\* | (browser's) | |
| HTTPS/SVCB records consulted | **Y** | N | N | Y\* | Y | (browser's) | asked in the same round as A/AAAA — measured, 404.6 ms → 0.8 ms |
| Alt-Svc | **Y** | **N** | N | Y | Y | (browser's) | with RFC 7838 `ma` as the cache lifetime. Zero matches for `alt_svc`/`alt-svc`/`AltSvc` in all of `reqwest-0.13.4/src/`, and `isahc` 2.0.1 says so about itself — its version-negotiation doc reads *"In the future, headers such as `Alt-Svc` will be used"* (`src/config/mod.rs:788`), which is a comparator naming its own absence. **nq's is the one that is reachable**, and that is G15: `session.quic_cache_layer` is a public `QuicSharedCache` with `add_domain(host)` and `exclude_domain(host)`, replaceable at construction through `Session(quic_cache_layer=..)`. Executed: `add_domain("cloudflare.com")` puts `('cloudflare.com', 443)` in the store, and `exclude_domain` writes an entry `__setitem__` then refuses to overwrite. Here `Native`'s `alt_svc` and `h3_failures` are **private fields with no accessor** — `network_changed()` clears both and is the only public entry point |
| DNS-over-HTTPS | Y | N | N | Y | Y | N | `hclient-dns-doh`, 22 crates, no tokio/hyper/h2 — and what makes it a resolver rather than a client is that it is bootstrapped by which constructor compiles, `Doh::pinned` taking an IP literal and `Doh::bootstrapped` a name. **nq has four transports where this has one**: `ProtocolResolver` names `doh`, `dot`, `doq`, `dou`, plus `in-memory`, `null` and `custom` (`contrib/resolver/protocols.py:38-59`, read), reached as a URL — `Session(resolver="doh+cloudflare://")` — and a resolver may carry host patterns, so different names take different resolvers. Executed: `ResolverDescription.from_url` accepts `doh+google://`, `dot+google://`, `doq://`, `dou://`, `system://`, `in-memory://` and `null://` and answers the matching `ProtocolResolver`. DoT and DoQ are an open question here rather than a refusal — see §4.1 |
| choose h3 vs h1/h2 per origin | **Y** | N | N | N | Y | (browser's) | `hclient-native`, from the HTTPS record — the *fast* tier, at resolution time — with a negative cache. nq chooses from the `Alt-Svc` cache alone, i.e. the slow tier: a first request to an unknown origin is never h3 unless the caller seeded the domain |
| race the two stacks | **Y** | N | N | Y\* | N | (browser's) | off by default; `curl` has `--http3-only`/Happy-Eyeballs-for-h3 at the libcurl level |

### 2.5 TLS

| capability | ng | rq | uq | cu | nq | br | note |
|---|---|---|---|---|---|---|---|
| rustls backend | Y | Y | Y | — | Y (extra) | n/a | |
| platform TLS backend | Y | Y | Y | Y | N | n/a | `hclient-tls-native-tls` |
| system trust store | Y | Y | Y | Y | Y | n/a | `Rustls::with_platform_verifier` |
| add a root, use only supplied roots, client certs, min version, disable verification | Y\*/seam | Y | Y | Y | Y | n/a | **`Rustls::from_config(Arc<rustls::ClientConfig>)`** (`lib.rs:53`) makes every one expressible, at the cost of writing rustls directly instead of a named setter — and **two of the five have stopped being only that**, which this row said for three verticals. Client certificates are `Rustls::with_identity(name, cfg)` plus `ClientIdentity` in the request's extensions, chosen **per request** and isolated by the pool key. Turning verification off is a constructor on each backend behind a `dangerous-insecure` feature — a feature rather than a plain method for auditability, since `cargo tree -f "{p} {f}"` then answers whether a build contains the path at all. nq does all five |
| ALPN reported back | Y | Y | — | Y | Y | n/a | `TlsConnect::reports_alpn`, and h2 is only offered over a backend that answers `true` |
| 0-RTT / early data | **Y** | N | N | Y\* | — | n/a | admitted per request by `AllowEarlyData` and by nothing else |
| ECH | N\* | N | N | Y\* | Y (extra) | n/a | refused deliberately: no backend here applies one, so the record's `ech_config_list` is gated behind `TlsConnect::applies_ech`. **nq is the live counter-example and it built the same gate** — read: the `echconfig` (SvcParamKey 5) comes off the HTTPS record in `contrib/resolver/utils.py:242`, travels as `ech_config_list`, and is applied by `util/ssl_.py`'s `if ech_config_list and hasattr(context, "set_ech_configs")`. Verified that the guard is real: `hasattr(ssl.SSLContext, 'set_ech_configs')` is `False` on CPython 3.14, so a plain install carries the record to a backend that silently cannot use it and only `[rtls]`/`[utls]` applies one. That `hasattr` is `applies_ech` duck-typed — the same design, with the last link connected, and the reason the last link is not connected here is one crate below the seam |
| JA3/JA4 fingerprint control | **N** | N | N | N | Y (extra) | n/a | refused by name in the design spec §9 — see §4, where the refusal now has a shape it did not have. **nq is the first comparator that can do it**, and its route is the one §4's second reason rules out for rustls rather than the one it rules out for us: a *second TLS stack*. `TLSConfiguration(backend="utls")` selects BoringSSL, and `urllib3-future`'s `contrib/anytls` resolves `rtls` (rustls + AWS-LC) → `utls` (BoringSSL) → stdlib `ssl` at import, so the fingerprint follows the backend rather than being configured on one |

### 2.6 Proxies

| capability | ng | rq | uq | cu | nq | br | note |
|---|---|---|---|---|---|---|---|
| HTTP `CONNECT` tunnel | Y | Y | Y | Y | Y | (browser's) | |
| absolute-form for `http://` | Y | Y | — | Y | — | — | |
| SOCKS5 | Y | Y\* | Y\* | Y | Y (extra) | — | rq behind `socks`; uq behind `socks-proxy` |
| SOCKS4/4a | Y | Y | — | Y | Y (extra) | — | closed — one type, since 4a is signalled inside a SOCKS4 request rather than negotiated. The hostname form always, so nothing is resolved locally. `isahc` 2.0.1 takes all four as proxy URI schemes — `socks4`, `socks4a`, `socks5`, `socks5h` (`src/config/mod.rs:471-474`) — which is libcurl's spelling and makes the remote-DNS choice a scheme rather than a decision. See G7 |
| SOCKS remote DNS | **Y, always** | Y\* | — | Y | Y\* | — | ng sends `ATYP=0x03 DOMAINNAME` unconditionally (`hclient-native/src/proxy.rs:534`, doc at `:425` — *"a name, never an…"*), so the leak is not reachable. rq picks from the **scheme**: `socks4`/`socks5` → `DnsResolve::Local`, `socks4a`/`socks5h` → `DnsResolve::Proxy` (`src/connect.rs:540-541`), so a `socks5://` URL leaks DNS by default. **nq inherits the same scheme-driven choice** — `socks4`/`socks5` resolve locally, `socks4a`/`socks5h` at the proxy (`contrib/socks.py:10-13`) — with its own module recommending the `h`/`a` forms, i.e. a documented default that is the wrong one. That is three of four comparators leaking by default and one that structurally cannot |
| read the proxy from the environment | **Y** | Y | Y\* | Y | Y | n/a | **this row said *refused as policy* and the refusal has been reversed**, which is the most consequential of the eleven stale cells in §0. `hclient-proxy`'s `system` feature reads `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`, `hclient`'s `system-proxy` feature is **in `default`**, and `Client::new()` therefore honours them — because a convenience constructor that ignored `HTTPS_PROXY` would be the one program on the machine that does. The rule survives one level down: `default_transport()` reads nothing, so the seam for *configuring* a transport stays free of ambient state, and the feature is spelled `hclient-native?/system-proxy` with the question mark, so a build without a transport pays no crate. `just graph-default-has-no-transport` asserts both directions |
| `NO_PROXY` matching | Y\* | Y | — | Y | Y | n/a | ng: `Proxy::bypass([..])` takes a list the caller wrote; it does not read `NO_PROXY`, and has **no CIDR** deliberately, where `hyper-util` 0.1.20's matcher does |
| a per-request/per-URL proxy rule | Y\* | Y | — | Y | Y | n/a | closed for the per-scheme rule — an ordered list, `Proxy::only_for` and first-match-wins. No closure, and one proxy *protocol* per transport: erasing `P` would erase the IO with it. See G7 |
| PAC | **N\*** | N | N | N | N | (browser's) | nobody in Rust has this, and nobody in the comparison runs one: curl does not, reqwest does not (grepped, zero hits), and nq does not. Here the engine was **written, measured and withdrawn** — `viperjs` ran a real PAC file in 2 crates and 1,563 KiB — because the feature had no consumer and the script arrives from the network by WPAD. What stayed is the half that was missing: `SystemProxies::pac()` reports the URL, so a PAC machine is a named refusal pointing at `hclient-urlsession` instead of going *silently direct* |
| system proxy (Windows registry, macOS SCF) | **Y** | Y\* | Y\* | Y | Y\* | (browser's) | also closed since this row was written — `Native::system_proxy()` (`lib.rs:1179`), over `windows-registry` and `system-configuration`, both of which expose safe APIs. Nothing on Linux, +4 crates on Windows, +6 on macOS, against `proxy_cfg`'s 28 with `url` and ICU among them. Everything ambiguous is a **named refusal** rather than a quiet narrowing — a PAC script, a SOCKS proxy beside an HTTP one, a bypass pattern the matcher cannot state exactly. rq behind `system-proxy`; uq behind `win-system-proxy` (Windows only); nq gets it from `urllib.request.getproxies()`, which reads the registry on Windows and SystemConfiguration on macOS |
| proxy for QUIC (MASQUE / `CONNECT-UDP`) | N | N | N | — | N | n/a | refused: a different protocol against a different server |

### 2.7 Stateful client behaviour

| capability | ng | rq | uq | cu | nq | br | note |
|---|---|---|---|---|---|---|---|
| redirect limit | Y | Y | Y | Y | Y | (browser's) | |
| distinguish "do not follow" from "follow zero hops" | **Y** | Y | — | N | Y | N | `RedirectPolicy::None` vs `Limited(0)` |
| **custom redirect predicate** | Y | Y | — | N | N | N | closed — **and the name in this cell no longer exists**, which is the sixth of §0's eleven. `RedirectPolicy` and `RedirectPredicate` were two things for one job; there is one trait now, `fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict`, with `Forbid`, `Limit`, `SameOriginOnly`, `HttpsOnly`, `FromFn`, `All` and `.and(..)`, set by `ClientBuilder::redirect`. The predicate is still asked about a hop the mechanism resolved, so it is handed the resolved target and the origin answer that drives credential stripping. Three verdicts where `reqwest-0.13.4/src/redirect.rs:102` also has three. nq has the requests inheritance — `allow_redirects` is a boolean and `max_redirects` a count, with no hook between them. See G9 |
| strip `Authorization` across origins | Y | Y | — | Y\* | Y | (browser's) | ng also strips `AllowEarlyData` there |
| cookie jar | Y\* | Y\* | Y\* | Y | Y | (browser's) | |
| **public-suffix rules in the jar** | **Y** | **N** | **N** | **Y\*, and a third answer** | **N** | (browser's) | the sharpest row in the table — see §5. ng compiles a list in (+77 KiB, measured). **Neither reqwest nor ureq rejects anything by default**, and both land there through `cookie_store` 0.22: reqwest's `Jar` is `#[derive(Default)] RwLock<cookie_store::CookieStore>` (`src/cookie.rs:30-31`) and `CookieStore`'s `Default`/`new()` leave `public_suffix_list: None` (`cookie_store-0.22.1/src/cookie_store.rs:40-48, :463, :467-473`); ureq builds its jar with `CookieStore::from_cookies(empty, true)` (`src/cookies.rs:177-180`), which sets the same `None` (`:460-464`), and takes `cookie_store` with `default-features = false` besides (`Cargo.toml:152-156`). Zero hits for `public_suffix` in `reqwest-0.13.4/src/`. `publicsuffix` 2.3.0 ships **no list at all**, only `LIST_URL` (`src/lib.rs:29`), so even a caller who wants one must fetch and refresh it. **`isahc` 2.0.1 does exactly that, and it is the third answer to this question rather than a fourth vote for one of the two above.** Behind its `psl` feature (not in `default`) it carries *both* a compiled-in list, through the `psl` crate, *and* a copy downloaded from `publicsuffix::LIST_URL` on a 24-hour TTL, refreshed on a background thread and falling back to the compiled-in one when the network fails (`src/cookies/psl.rs:33-137`). Its module doc gives the argument against this workspace's choice in as many words: *"HTTP clients tend to be used in a much different way and are often embedded into long-lived software without frequent (or any) updates, [so] it is better for us to download a fresh copy from the Internet every once in a while"* — and then answers itself, *"a stale list is better than no list at all"*. ng reaches the same end differently: the list is a seam (`CookieJar<P>`), and since G6 a caller can hand a fresher one through `Client`, so what isahc does by default is here a decision the caller makes and pays for. **nq is a fourth vote for `N` and the only one that was *executed* rather than read**: its jar is `http.cookiejar.CookieJar` under a `CookiePolicyLocalhostBypass`, and Python's `DefaultCookiePolicy` has no public suffix list at all (checked: no attribute of it names one). Run against `shop.example.co.uk`, `set_ok_domain` for a cookie scoped to `.co.uk` answers **`True`**; the controls separate, since `.com` from `a.b.com` answers `False` — the stdlib's two-dot Netscape heuristic, which `co.uk` walks straight past. Four clients, four different routes to the same hole |
| **a pluggable cookie store** | Y | Y | — | Y\* | Y | n/a | closed: `ClientBuilder::cookie_jar` takes `CookieJar<P>` for any list and erases it into `AnyList` — see §G6's entry and spec amendment C12. `reqwest…client.rs:1213` takes any `CookieStore`, which is a different seam: theirs is the storage, ours is the public suffix list, and this crate's jar *is* the storage |
| RFC 9111 response cache | **Y** | N | N | N | N | (browser's) | `hclient`'s `cache` feature: freshness, validation, `Vary`, both sides' directives |
| **a pluggable cache store** | Y | n/a | n/a | n/a | n/a | n/a | closed: `ClientBuilder::cache` takes `HttpCache<S>` for any store and erases it into `AnyStore`, so a disk-backed or shared store reaches `Client`. Erased rather than parameterised because `S` on the cache is `S` on the public `ClientBody` alias, and because both crates are optional dependencies — a defaulted parameter needs a default type that would not exist |
| gzip | Y\* | Y\* | Y\* | Y | Y | (browser's) | |
| brotli | Y\* | Y\* | Y\* | Y | Y (extra) | (browser's) | |
| deflate | Y\* | Y\* | N | Y | Y | (browser's) | **both wire formats under one token** — RFC 9110 §8.4.1.2's zlib and the raw stream its own Note records, chosen from the first two bytes rather than after a failure. No new crate: `flate2` was already here for `gzip` |
| zstd | Y\* | Y\* | N | Y | Y (extra) | (browser's) | `ruzstd`, a decoder-first pure-Rust crate. The window is capped at RFC 8878 §3.1.1.1.2's recommended 8 MB — `ruzstd`'s own default is 100 MB — and the frame's XXH64 content checksum is compared, which `ruzstd` reads and never checks |
| request-body compression | **N** | N | N | Y | N | N | refused in the design spec §9 |
| automatic retry | **Y** | N | Y\* | N | Y | n/a | this cell said *"neither is a general retry policy"* and there is one: `ClientBuilder::retry(timer, policy)` (`client.rs:217`) over a `RetryPolicy` trait in `hclient-proto`, with `Standard`, `SafeMethodsOnly`, `Never`, `RetryAll`, `RetryFromFn` and `.and(..)`. **The signature takes a clock because the alternative hangs** — without `default-transport`, `DefaultClock` is `NoClock`, whose `Sleep` is `std::future::Pending`, so the version that used the client's own timer compiled everywhere and waited for ever at the first backoff. nq's is urllib3's `Retry`, whose `allowed_methods` defaults to the same safe-and-idempotent set `SafeMethodsOnly` names, with `status_forcelist`, `backoff_jitter` and `respect_retry_after_header`; it is off by default (`retries=0`) as this one is. **What it cannot have is the distinction this one is built on**: a policy above the transport cannot tell a request that never left from one a server acted on, and `Error::is_unsent()` is a claim a transport makes at a site where it knows |

### 2.8 Targets, runtimes, shape

| capability | ng | rq | uq | cu | nq | br |
|---|---|---|---|---|---|---|
| native async | Y | Y | N | Y (isahc) | Y | n/a |
| **blocking API** | **N** | Y\* | Y | Y | Y | n/a |
| runs on a single-threaded / `!Send` executor | **Y** | N | n/a | N | n/a | n/a |
| tokio-free build | Y\* | N | Y | Y | n/a | Y |
| browser (`wasm32-unknown-unknown`) | Y | Y\* | N | N | Y\* | Y |
| WASI (`wasm32-wasip2`) | Y | **N** (executed) | Y (executed) | N | Y\* | n/a |
| Apple `URLSession` | **Y** | N | N | N | N | n/a |
| embassy / `embedded-nal`-shaped runtime | Y\* | N | N | N | N | n/a |
| `no_std` | N | N | N | N | n/a | N |
| published on crates.io | **Y\*** | Y | Y | Y | Y | Y |
| a type-erased client (`Box<dyn …>`) | **Y** | Y | Y | Y | n/a | Y |

Three cells in that table need their footnote, and one of them was wrong for
as long as this document has existed. **`published on crates.io` and
`a type-erased client` both said `N` while §3 G1 and §3 G13 recorded them
closed, at length, two screens below** — `0.1.0-alpha.2` is on crates.io and
`hclient::Client` names no type parameters. A summary contradicting its own
table is the failure mode this document is otherwise built to avoid, and it
is recorded rather than quietly repaired.

**nq's two wasm cells are `Y*` for opposite reasons and neither target is
ours.** Its browser story is Pyodide — `wasm32-unknown-emscripten`, an
interpreter compiled to wasm, where `Response.conn_info` is documented as
unset — not `wasm32-unknown-unknown`, where a Rust `fetch` backend runs. Its
WASI story is the closer one and it is genuinely close: `niquests/extensions/
wasi/_capabilities.py` probes for `wasi:http` **0.2 and 0.3** bindings *and*
for `wasi:sockets`, and falls back between them — so it covers both the route
`hclient-wasi` takes and the route ureq takes, in one package, chosen at
import. Nothing here was executed under a WASI host, so this is `read` and
not `executed`; what can be said is that the WIT-level design question this
document opened in §1 — host HTTP against guest sockets — is one somebody
else has answered by taking both.

### 2.9 Observability

| capability | ng | rq | uq | cu | nq | br |
|---|---|---|---|---|---|---|
| connection-level events (`Connected`/`Reused`/`Closed`) | **Y** | N | — | Y (`CURLINFO_*`) | Y\* | N |
| response-head event | Y | N | — | Y | Y | N |
| `1xx` event | **Y** | N | — | — | Y | N |
| per-phase connect timings (dns / tcp / tls) | **Y** | N | — | Y | Y | N |
| upload/download progress callback | N | N | — | Y | Y\* | N |
| a capability an over-claiming backend cannot fake | **Y** | n/a | n/a | n/a | N | n/a |
| middleware / interception | Y\* | N | **Y** | — | Y | N |

On the last row: ureq ships `Middleware` in the crate
(`src/middleware.rs:16`, bounded `Send + Sync + 'static`); reqwest does not
and the ecosystem answer is the separate `reqwest-middleware` crate; here it
is `hclient-tower`, which goes **both** ways — `TransportService` makes a
`Transport` into a `tower::Service` so `tower-http`'s stack applies, and
`ServiceTransport` makes any `tower::Service` into a `Transport`
(`crates/hclient-tower/src/lib.rs:265`), which is how someone would put
`hyper-util`'s own client underneath this facade. The second direction has
to be told its capabilities as an argument, *"because a `Service` has none,
and this adapter must not invent them"* — the capability rule applied to a
foreign ecosystem. The costs are an `Arc`, an uncached `poll_ready` per
call, and a boxed future that is `!Send` until return type notation lands.
nq's `LifeCycleHook`/`AsyncLifeCycleHook` is the same idea with the events
named on the type — `pre_request`, `pre_send`, `early_response`, `response`,
`on_upload` — and it is what its rate limiters are built on
(`LeakyBucketLimiter`, `TokenBucketLimiter`), which is a shape nothing here
ships and nothing here forbids: it is `tower`'s territory through
`hclient-tower`, unwritten rather than refused.

**The one row where the `nq` column is the weaker one is the one this whole
project is about.** *A capability an over-claiming backend cannot fake* is
`N` for niquests in the strong sense — not that it lies, but that there is
no place where it could be caught. Its backends are chosen at import by
`hasattr` probes (`hasattr(context, "set_ech_configs")` decides ECH,
`_HAS_NATIVE_SOCKET_SUPPORT` decides the WASI route), which is duck typing
doing the work `Capabilities` does here — and duck typing answers *does this
object have that method*, never *and will it honour the setting*. The ECH
row in §2.5 is that difference made concrete: a plain install carries a
`ech_config_list` to a backend that cannot use it and says nothing, where
this workspace's version of the same omission is a `TlsConnect::applies_ech`
that defaults to the understating value and a connector that does not fill
the field. Both end with no ECH; only one of them can be asked about it.

**`upload/download progress` is `Y*` for nq and `N` here, and the asymmetry
inside that cell is the interesting half.** Its `on_upload` hook fires per
transmitted block and `PreparedRequest.upload_progress` carries percentage,
content length and completion — but there is no matching *download* callback,
because a streamed response is already a loop the caller writes. That is
exactly the shape a progress feature would take here too: the download half
is `poll_frame`, and the upload half has no event at all. See G17.

---

## 3. The genuine gaps, ranked

Ranked by how often an ordinary caller meets them, not by how hard they are.
Each says what it would take **here**, which of this workspace's stated rules
it has to respect, and — where it applies — which rule forbids it outright,
because a feature refused by a written rule is a better answer than a patch.

**The numbering is chronological and the ranking is not**, which is worth
one sentence because eleven of the seventeen are now closed and a reader
skimming for what is live will otherwise read G1 as the top of a list. The
order the entries were *written* in is G1..G13 from the first reading and
G14..G17 from the niquests one; the order that matters today is **G14, G15,
G16, G17**, then G3, which is refused, and G1, which is the owner's. G14 is
first because it is the only one whose absence costs a caller a hang rather
than a feature.

### G1. ~~Nothing is published~~ — **closed, as a pre-release**

`0.1.0-alpha.2` is on crates.io, all 25 publishable crates at one version.
It was the gap that dwarfed every row in §2 — none of what follows was
reachable by anyone who had not cloned this tree — and the ranking that put
it above `deflate` was right for exactly as long as it stood.

**What is claimed is narrower than "published", and the pre-release is the
claim.** A caller writing `cargo add hclient` gets *nothing*: cargo will not
select a pre-release without being asked, so the name is taken and no
promise is made. That is the point — the week before the first upload moved
six public surfaces, and `0.1.0` would have frozen twenty-nine of them at
the moment they were last seen moving. `--version 0.1.0-alpha.2` is the
incantation, and both READMEs carry it.

So the gap this section should rank first is **not** publication. This
paragraph then said it was G3 — the blocking API, refused by the problem
statement — and a refusal is not a ranking either. It is **G14**: the
default timeout, which is the only entry on this list that is neither
closed nor refused nor somebody else's call.

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
runtime to block on, and picking one is exactly the choice `hclient-rt`
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
`hclient-urlsession` is the backend that genuinely reports `Transparent`,
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
`hclient-fetch`'s `tests/hooks.rs` never gained an arm, so every browser
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
dependencies — `Client<T, Tm, P = hclient_cookie::BuiltinList, ..>` names
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
it — the objection already recorded against
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
was most of the work. `RedirectPolicy` lives in `hclient-proto`, which is
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

### G11. Interface binding, TCP keepalive detail, Unix sockets — **closed**

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

**The Unix-domain socket is in too, and it is not the sibling trait this
section expected.** A second trait would have to produce
`TcpConnect::Stream` — `Native`'s IO type *is* that associated type — at
which point it is `TcpConnect` with an extra method; and `R: UnixConnect`
on `Native` would tax every runtime with no file descriptors. The
`fn`-pointer trick that keeps `Spawn` off `Native`'s signature does not
work either, because `spawn` returns `()` where this returns a future and
boxing it drops auto traits (amendment C1).

So it is `TcpConnect::connect_unix`, a **defaulted method** whose default
is a refusal, beside `SUPPORTS_UNIX` defaulted to `false` — `reports_alpn`
and `applies_ech`'s shape one seam over: a constant defaulted to the
understating value, read by the layer above to decide whether to *ask*.
`Native::unix_socket` refuses where the runtime says it cannot, at the call
that configures it.

It replaces the whole resolve → discovery → Happy Eyeballs → connect block,
which is `Proxy`'s slot exactly — and a proxy and a socket together are a
**refusal**, because both answer *where does this connection go* and a
precedence rule between them would be one nobody could guess.

`Connected::remote` became `Option<SocketAddr>` for it, and that is the
sharper half: there is no address, and a fabricated `0.0.0.0:0` would give
a hook a *wrong* answer where the absence gives it a missing one — the
argument `Head::version` already settled one event over. Emitting no
`Connected` at all was the alternative and is worse: the `Closed` that
follows would announce the end of a connection whose beginning was never
announced.

### G11a. No separate bound on name resolution — **closed**

`Timeouts::resolve`, `TimeoutSupport::resolve` and `Phase::Resolve`, all in
one change — the rule this field had to land under, and the one that kept
`connect` off `hclient-h3` until W1.

**What it bounds is not a phase, and finding that out was the work.**
Happy Eyeballs interleaves resolution with connecting on purpose — the
resolver is a `Stream` and `hclient-native` starts dialling the first
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
platform's GSSAPI or SSPI, which is `hclient-tls-native-tls`'s argument one
seam over and its own crate. Neither is a challenge/response this code
could grow into.

### G13. ~~There is no way to name "any hclient client"~~ — closed

**`hclient::Client` names no type parameters.** `Client` is one concrete
type, `Clone` is an `Arc` bump, and a library writes `fn f(c: &Client)`
where this section said it must write `fn f<T: Transport>(c: &Client<T>)`
and push the parameter to its own callers.

The row is kept, and at length, because **the reason recorded here was
wrong three times while the conclusion was right twice** — which is the
shape this document exists to make visible.

**Wrong once: `Timer::Instant: Copy` is not a permanent blocker.** `Copy` on
a trait object is indeed not a thing, and fixing `Instant` to a concrete
type is indeed impossible for the reason given: the three shipped clocks
disagree (`tokio::time::Instant`, `std::time::Instant`, and `NoClock`'s
`()`). The way out is neither. `ErasedInstant` answers *how long ago was
this* and nothing else, so the instant never leaves the clock that made it
and `Copy` is asked of nothing erased.

**Wrong twice: `Transport`'s RPITIT is not a wall.** Return type notation is
still `E0658` on 1.98, and it is not on the path: the boxed future declares
no `Send`, so there is nothing to prove and `BoxedTransport` takes a
**blanket impl** over every `Transport`. No backend author writes anything —
which is better than the per-backend trait this section proposed as the
buildable `Send` version.

**Wrong three times, and this is the interesting one: the `!Send`
`Native::execute` future does not stop it either.** That was recorded here
as the real cause after the first two were cleared, and it is a fact about
spawning a *request*, which the erased client simply does not offer. It
never needed to: `Native`'s future was already `!Send`, so no caller had it
to lose.

**Right twice, about what an erased client cannot be.** The two halves this
section ruled out stay ruled out. A `Send` erasure of the *future* would
need the bound on seven seam methods and would exclude
`hclient-rt-embassy`, whose `connect` future holds
`RefCell<embassy_net::Inner>` because embassy's executor is
single-threaded — so the erased `Client` does not have one. And the
`!Send` erasure's price — *no `tokio::spawn`, no `axum`* — is exactly half
paid: **`Client` is `Send + Sync`** and lives in application state, while
nothing a request *produces* is `Send`.

**What decided that last point is the browser, and it was measured rather
than argued.** One `ClientBody` serves every backend, and `hclient-fetch`'s
body holds a `dyn Stream` with no auto trait — so `Send` on the erased body
does not weaken the browser backend, it **excludes** it:
`Client::builder(Fetch::new())` stops compiling.

**The exclusion is real and the reason given for it was not.** Asked on
`wasm32-unknown-unknown`, `JsValue`, `js_sys::Promise` and
`web_sys::ReadableStreamDefaultReader` are all `Send`, `Fetch::execute`'s
future is `Send`, and `hclient-wasi` is `Send` throughout. The one `!Send`
type is `js_sys::JsFuture`, whose `Rc<RefCell<Inner<T>>>` reaches the body
through `wasm_streams::readable::IntoStream`. So this is one read loop
rather than a property of the target — and it is closed now:
`hclient_fetch::Body` is `Send`, because `body::pump` keeps the stream on
the thread that owns it and hands `Bytes` across a channel. So every
backend here produces a `Send` body — so the declaration has been made:
`erased::{BoxBody, BoxSleep, BoxInstant}` carry `Send` (amendment C14),
and `tokio::spawn` of a response body works through `Client` again. The
*request* future is still `!Send` on purpose, because bounding
`BoxExchange` is the thing that excludes `hclient-rt-embassy`. So the
half-paid price this section describes is now paid the other way round:
`Client` is `Send + Sync` **and** what a request produces crosses a
thread; what does not is the act of making one.

**`+atomics` is supported, and what is not supported there is `Send`.**
Measured: `cargo check -p hclient-fetch --target wasm32-unknown-unknown
-Zbuild-std` under `-Ctarget-feature=+atomics,+bulk-memory` **succeeds**
for the library. What fails is `--tests`, deliberately — the
`fetch-must-fail-under-atomics` job requires that failure, with `E0277`
about `Send` specifically, because `SingleThreaded<T>`'s soundness
argument is single-threadedness and the `cfg` must strip it. Nothing
holding a JS handle can be `Send` under wasm threads by anyone's hand:
wasm-bindgen's own `unsafe impl Send for JsValue` is `#[cfg(not(
target_feature = "atomics"))]`. The cost of the other
direction is that a response body no longer crosses a `tokio::spawn`, which
worked on `hclient-native`; a caller who needs it reaches past the facade
with `Client::transport_as::<Native<..>>()`.

**Reaching past the facade buys more than it did when that was written.**
`Native::execute`'s future is `Send` now — not by a bound on any seam, but
because `connect.rs` stopped discarding its resolver stream's type behind a
`dyn` that declared no auto traits. So a caller holding the transport
concretely can `tokio::spawn` the *request*, which the erased `Client`
still cannot hand them. The seam is untouched and stays untouched: the
conversion that would let `Client` promise it was built and measured, and
it costs `hclient-dns-doh` entirely. See CLAUDE.md, *A `dyn` that declares
no auto traits does not hide `Send`*.

Two smaller costs, both recorded where they land: the embedded target has no
`Client` at all (`RefCell<embassy_net::Inner>` is not `Sync`, and
`hclient-rt-embassy`'s live TAP scenarios use `Transport` directly now), and
a `!Send` hook can be watched at the transport but not through the facade.

**Decision D6 is obeyed and is why this works at all**: *"all the machinery
was a consequence of type erasure in middleware; remove the erasure from the
built-in stages and the machinery disappears entirely"*. `dynosaur`,
`trait_variant` and a `BoxFuture` alias remain refused by name, and so —
newly — is a `#[cfg]` that would make `BoxBody` `Send` off wasm: it hides
the symptom rather than removing the cause. What was built is two traits
beside the seam with blanket impls, no proc macro and no cfg alias.

`hclient-tower::
TransportService` is unchanged and is still the route for a caller who
wants a `tower::Service`.

---

### G14. There is no default timeout anywhere, and that is not written down as a decision

**Measured, at `fabc7cb`.** `hclient_core::Timeouts` derives `Default`
(`caps.rs:333`), so `resolve`, `connect`, `first_byte` and `between_bytes`
are all `None`; `hclient::Config` derives `Default` (`config.rs:24`), so
`total` is `None`. Nothing in the workspace sets any of the five. A caller
who writes the two lines this project's front page opens with gets a client
that **can wait for ever**, on a resolver that hangs, a connect that never
answers, a head that never arrives, or a body that stops mid-stream.

niquests is the sharp contrast because it made the opposite choice
explicitly and by method: `READ_DEFAULT_TIMEOUT = 30` for `GET`, `HEAD` and
`OPTIONS` and `WRITE_DEFAULT_TIMEOUT = 120` for `DELETE`, `PUT`, `PATCH` and
`POST` (`niquests/_constant.py`, read), each named in the signature of the
verb it applies to. The split is an argument rather than a number: a read is
a thing you retry and an upload is a thing you do not want cut in half.

**This is the one row in the document where the absence is not obviously
defensible**, and the reason to rank it first is that every other gap here
costs a caller a feature while this one costs them a hang. The two
arguments *for* the current state are real and neither is quite enough:

- **A library that picks a number picks it for its embedder**, which is the
  same argument that keeps `User-Agent` unset (G2) and is the right shape.
  But a `User-Agent` left unset costs nothing, and a timeout left unset
  costs the caller the one failure mode they cannot recover from without
  restarting the process. The two are not the same kind of default.
- **`Timeouts` is in `hclient-core` and is read by transports out of the
  request's extensions**, so a default there would be a default for
  `wasi:http` and `fetch` too — and both of those already collapse the
  model differently (`fetch` into one `AbortController`, `wasi:http` into
  three fields with nothing for resolution). A default that cannot be
  honoured is the *capability that lies* defect one layer down.

**The second argument is the one that says where a default belongs, and
it points at exactly one field.** It is not `Timeouts` — `TimeoutSupport`
has a bool per field and a transport that cannot honour one makes `build()`
refuse, so a default there is a default that some backend rejects. It is
`Config::total`, which `check_supported` deliberately does **not** gate,
and the comment beside it says why: *"no transport enforces a whole-
operation bound … so there is no capability to check it against. What it
needs instead — a clock — is guaranteed by the client's type"*
(`config.rs:335-340`, read). `total` is the one bound this client enforces
entirely by itself, in `Deadline`, needing nothing from any backend — and
since `Client::new()` exists only where `DefaultTransport` does, the clock
it needs is present by construction at exactly the constructor where a
default would go.

So the shape is narrow: a `total` on `Client::new()` **alone** — not on
`ClientBuilder`, not on `Timeouts`, not on any transport — which leaves
`Client::builder(t).build()` untouched for the caller who is configuring
deliberately, and leaves the seam free of a number nobody at that layer
could have chosen. That is `system-proxy`'s resolution one field over: the
convenience constructor behaves the way a program on a machine is expected
to behave, and the seam stays ambient-free.

What is **not** proposed is a number. Picking one is the owner's, and the
comparators do not agree: niquests 30/120 by method; ureq nine separable
bounds and — read, because it was worth checking rather than assuming from
the count — `timeout_global` *"Defaults to `None`"* like the other eight,
so ureq is in the same position this client is; reqwest none; curl none.
**So the majority is on this client's side and the majority is not the
argument** — niquests is one client out of five and it is the one whose
default a caller cannot be surprised by, because it is printed in the
signature of every verb. What this entry
asserts is only that the current state is an *accident* rather than a
decision — grep finds no sentence anywhere in this workspace saying "no
default timeout, and here is why" — and this project's own standard is that
an absence with a reason is a decision and an absence without one is a gap.

### G15. The Alt-Svc cache and the HTTP/3 failure memory cannot be seeded, excluded or replaced

`Native`'s `alt_svc: AltSvcCache` and `h3_failures: H3Failures` are
**private fields with no accessor** (`hclient-native/src/lib.rs:516, :524`,
read). Both types are `pub` and both carry `note`, so the machinery is
public and the instance is not; `Native::network_changed()` is the only
public entry point and it *clears*. So a caller cannot say *this origin
speaks h3, do not spend a round trip finding out*, cannot say *never try
h3 here*, and cannot hand in a cache shared between transports or
persisted across a run.

niquests has all three, and it is one attribute: `session.quic_cache_layer`
is a `QuicSharedCache` with `add_domain(host, port=None)` and
`exclude_domain(..)`, and `Session(quic_cache_layer=..)` replaces it
wholesale. Executed — `add_domain("cloudflare.com")` leaves
`{('cloudflare.com', 443): ('cloudflare.com', 443)}` in the store, and
`__setitem__` consults an exclusion store before writing, so an excluded
domain can never be learned either (`niquests/structures.py:263-296`, read
and run). Its bound is 12,288 entries.

**Two of the three are cheap and the third is the one to think about.**
Seeding and excluding are a pair of methods on `Native` forwarding to the
`note`/`suppressed` that already exist. Replacing the cache is not: it
would put a type parameter or a `dyn` on `Native` for a value that is
consulted on the connect path, and this workspace has a written position on
what a `dyn` declaring no auto traits costs. The seeding half is also the
half with a real use — a caller who *knows* their origin speaks h3 pays one
h2 round trip per process today for a fact they could have stated.

**The scope rule is the thing not to lose.** `network_changed()` exists
because RFC 7838 §2.2 conditions its own SHOULD on network state being
knowable, and it is not knowable to a `Transport`. A seeded entry is a
different fact from a learned one — the caller asserted it, and a network
change does not make the caller wrong — so a seed that `network_changed()`
clears and a seed that survives are two features, and shipping the wrong
one silently is how a laptop that moved networks keeps dialling UDP into a
wall. niquests does not answer this; its cache has no notion of a network
change at all.

### G16. WebSocket is HTTP/1 only

`hclient_native::Upgrading` holds an `http1::Connection`
(`upgrade.rs:139-145`, read) and nothing else, so `hclient-tungstenite`
frames a socket that was upgraded by a `101`. RFC 8441's extended CONNECT —
`:protocol: websocket` on an h2 or h3 stream — is not expressible, and on
h3 the workspace has the parts: `hclient-webtransport` already sends an
extended CONNECT over `hclient-h3`, which is the same mechanism with a
different `:protocol` value.

niquests does all three from one call
(`contrib/webextensions/ws.py:100-104`, `:292`, read): the h2/h3 arm sets
`:protocol` and `:method: CONNECT` where the h1 arm sends the `Upgrade`
head, and `Response.extension` is the same object either way.

**What this costs is a multiplexing property, not a feature.** A WebSocket
over h1 owns its TCP connection for its whole life; over h2 or h3 it is one
stream beside ordinary requests. A caller holding a socket open to an
origin they are also making requests of pays a second connection here and
does not there — and on h3 that is a second QUIC handshake, which is the
expensive kind.

**The blocker is which seam it lands on rather than the framing.** The
`WebSocketConnect`/`WebSocket` pair is protocol-agnostic already — that is
what let the browser implement it unchanged — and `Tungstenite` borrows a
`Native` because it needs the *upgraded byte stream*, which is an h1 idea.
An h2 arm would borrow a stream pair instead, and `hclient-h3`'s
WebTransport work is the evidence that the h3 arm is reachable; what
neither has is a place for `tungstenite`'s framing to sit that is not the
h1 upgrade. That is a design question, not a missing method.

### G17. Six pieces of ordinary furniture, and the sixth is a type

The class G6 and G10 belonged to, found the same way: by reading a peer's
surface rather than by using this one. Each is small, each is genuinely
absent at `fabc7cb`, and they are ranked together because none of them is
worth a section of its own.

| what | nq's spelling | here |
|---|---|---|
| **`.netrc`** | automatic; `~/.netrc`, `~/_netrc`, `$NETRC` (`utils.py:196-207`) | nothing — grepped the workspace, the only hit is this document |
| **`Link:` header** | `response.links['next']['url']` (`models.py:1577`) | nothing on `Response`; a parser is being written in `hclient-proto` in the working tree as this is filed, which is not the same thing until something on `Response` reads it |
| **a `lines()` adapter over a body** | `iter_lines()`, sync and async | nothing; `hclient_proto::sse` has a line decoder and it is SSE's, not a body's |
| **certificate fingerprint pinning** | `verify="sha256_<hex>"` (`adapters.py:571-573`) | nothing; expressible through `Rustls::from_config` with a custom verifier, which is the *seam* answer and a long way from one string |
| **upload progress** | the `on_upload` hook plus `PreparedRequest.upload_progress` | no event exists; `Event` has `Connected`, `Reused`, `Closed`, `Head` and `Informational`, all of which are about a connection or a head |

**The one that is a type rather than a method is `local_address`.** It is
`Option<IpAddr>` (`hclient-rt/src/caps.rs:101`) where niquests'
`source_address` is `tuple[str, int]` — an address *and a port*. Binding a
source port is what a caller behind a firewall rule keyed on one needs, and
it is not reachable through `TcpOpts` at any price, because the field
cannot carry it. The escape hatch is the usual one and it is a large one:
`TcpConnect` is a public trait, so a caller who needs a source port writes
a runtime. That is the difference between a seam and a setter, and it is
the wrong side of it for a two-line change.
Widening it to `SocketAddr` is a breaking change to a public struct that is
deliberately not `#[non_exhaustive]`, which is exactly the class of change
this workspace has been spending freely and stops being able to spend at
`0.1.0`. So this one is cheap *now* and expensive later, which is the only
reason it is on a ranked list at all.

**Two of the six have a rule that would refuse them and four do not.**
`.netrc` is *reading the environment*, and §4 records that as policy
belonging to whoever builds the transport — except that the rule was
reversed for proxies this week, and the reversal's argument (a
convenience constructor that ignores what every other program on the
machine honours is the odd one out) transfers verbatim. Fingerprint
pinning is a verification decision, and this workspace has a written
position that the way to reach one is `from_config`. `Link:`, `iter_lines`,
upload progress and the source port are absences with nothing written
against them.

---

## 4. What is refused, and by which rule

These are **not** gaps. Each is a decision already recorded, with the rule
that produced it. Reporting any of them as missing would be the main way
this document could fail.

| absence | the rule that refuses it | where |
|---|---|---|
| ~~`deflate` and `zstd` decoding~~ | **reversed, and the row is kept because the reversal is the interesting part.** The `deflate` rule — *a client must not advertise a coding it may guess wrong about* — assumed the guess had to be made after a failure, as curl's is; it is answered from the first two bytes, which is a rule with no window. The `zstd` rule was *a third dependency for a coding no server sends unasked*, and the answer is that both premises were checked rather than argued: `deflate` costs **no** crate at all, and `zstd` costs two | `hclient/src/decompress.rs` |
| `compress`/`x-compress` | RFC 9110 §8.4.1.1's LZW: no decoder here, so it is never advertised and never matched | `hclient/src/decompress.rs` |
| request-body compression | server support is inconsistent; a clean manual path instead | design spec §9 |
| ~~reading `HTTP_PROXY`/`NO_PROXY` from the environment~~ | **reversed, and kept for the same reason the `deflate` row is.** The rule — *reading the environment is policy and belongs to whoever builds the transport* — was true of the seam and wrong about the audience: it left `Client::new()` as the one program on the machine that ignores `HTTPS_PROXY`. The resolution keeps both halves rather than picking one — the *constructor* reads (`system-proxy`, in `default`), the *transport seam* (`default_transport()`) still reads nothing — and the shape that made it affordable is `hclient-native?/system-proxy`, the weak form, so a build with no transport pays no crate | AGENTS.md, proxy section |
| a proxy for QUIC | `CONNECT-UDP` (RFC 9298) is a different protocol against a different server, not this feature with a wider bound |
| a blocking API | out of scope by the problem statement | design spec §9 |
| JA3/JA4/Akamai fingerprint control | rustls closed it *not planned*; `http::HeaderMap` lowercases names, so browser header casing is unreproducible anyway. **The first half is now an open question rather than a closed one**, because niquests answered it without asking rustls: it ships a *second TLS stack* (`utls`, BoringSSL) selected at import, so the fingerprint is a property of the backend. That route is open here too — `TlsConnect` is a seam and a third backend beside `hclient-tls-rustls` and `hclient-tls-native-tls` would need no change above it. The second half is unchanged and is the harder one: header **order and casing** are as much of a fingerprint as the ClientHello, and `http::HeaderMap` gives up both, which is the same foreign-type constraint that keeps this workspace on `std` | design spec §9 |
| `no_std` / bare metal | `http` 1.x carries `compile_error!` for it; not this project's to reverse | AGENTS.md |
| `Ping`/`Pong` as WebSocket message variants | a browser has neither `send(ping)` nor `onping`; the variant would have no honest right-hand side | `hclient-core/src/unversioned/websocket.rs:35` |
| permessage-deflate, subprotocol checking | left open; the browser negotiates extensions itself and exposes no control | same file, `:46` |
| a `RequestBuilder::extension` setter | adding one for `AllowEarlyData` and not `RequireVersion` would be arbitrary; both is a facade question |
| ECH | no backend here applies one, and `hclient-tls-rustls` *refuses* a non-`None` ech — filling the field would make every ECH-publishing origin unreachable | AGENTS.md |
| RFC 6724 destination address selection | the full rule needs source address selection, i.e. a routing table, which no seam here provides; a partial one would look like compliance without being it | design spec §9 |
| `stale-while-revalidate` | it needs somewhere to run the revalidation after the response was handed over, and this client does not spawn on a caller's behalf | AGENTS.md, cache section |
| a WebSocket keep-alive by default, an h2 driver by default, an idle-socket reaper by default | a default stronger than the truth: not every `R` can `Spawn`, and a default that pings sends traffic nobody asked for | AGENTS.md, several places |
| h2 multiplexing by default | without `Spawn` nobody drives a shared connection but the in-flight futures |
| more than one session per WebTransport connection — **no longer refused** | built; the recorded blocker (`PoolKey`) turned out not to be the true one | AGENTS.md |
| WebTransport `GOAWAY` | a measured impossibility in `h3` 0.0.8: two `GOAWAY`s saying opposite things look identical to a client | AGENTS.md |

---

### 4.1 Three things niquests has that are neither gaps nor refusals

The classification this document uses has two boxes and the niquests
comparison needed a third. A **gap** is something to do; a **refusal** is
something decided against with a rule. These three are neither: each is a
capability whose price is a dependency or a provider this workspace has
already declined **for a reason written down somewhere else**, so the
decision exists but was never made about *this* feature. Naming them as
gaps would be wrong and leaving them out would be a comparison that only
lists what is convenient.

**Post-quantum key exchange — a cost not paid, and the bill is already
itemised.** niquests offers X25519MLKEM768 on its QUIC path by default;
read in `qh3` 1.9.4, whose `quic/configuration.py:123` says *"By default
only X25519MLKEM768 + X25519 key shares are offered"*, with the exchange
implemented in `tls.py`. Here it is not a decision about post-quantum at
all — it is a fact about the crypto provider. Measured in rustls 0.23.43:
`src/crypto/aws_lc_rs/` exports `X25519MLKEM768`, `SECP256R1MLKEM768`,
`MLKEM768` and `MLKEM1024`; `src/crypto/ring/kx.rs` declares exactly three
groups, `X25519`, `SECP256R1` and `SECP384R1`, and **zero** hits for
`MLKEM` anywhere under `src/crypto/ring/`. So reaching it means adding
`aws-lc-rs`, and the reason not to is written in this workspace already —
in `hclient-tls-rustls`'s ECH refusal, which names the identical blocker
for the identical reason: *"Honouring ECH begins with adding `aws-lc-rs`: a
C toolchain in the build, on every target this crate claims."* One provider
swap buys ECH and post-quantum together, and costs a C toolchain on the
wasm and embedded targets this seam exists to reach. That is the trade;
it has not been taken and it has not been refused either.

**Revocation checking — a decision, and the industry moved under the
question.** niquests verifies OCSP by default and reports it on every
response (`ocsp_verified: True` in §1's execution), with a
`RevocationConfiguration(strategy=PREFER_OCSP|PREFER_CRL|CHECK_ALL)` to
change it. Listing OCSP as something this client lacks would be wrong, and
the evidence is not an opinion here: Let's Encrypt removed OCSP URLs from
its certificates on **7 May 2025** and turned off its responders on
**6 August 2025**, citing privacy — *"CAs immediately become aware of which
website is being visited from that visitor's particular IP address"* — and
naming CRLs as the replacement; Chromium's own CRLSets page states that
*"Online (i.e. OCSP and CRL) checks are not generally performed by
Chrome."* Both fetched rather than recalled. niquests itself is the
strongest witness: its `3.15.0` release added CRL *because* of Let's
Encrypt's withdrawal, so the comparator's own history is the argument for
not copying its default. What this leaves open is not OCSP but **revocation
at all**, and the honest statement of where that stands here is narrow:
nothing in this workspace performs any revocation check of its own —
grepped, zero hits for `with_crls` or `CertRevocationList` — while rustls
0.23 offers `WebPkiServerVerifier::with_crls` and a platform verifier
delegates to an OS that may do its own. CRL and Certificate Transparency
are being designed separately and are out of this document's scope; the
row that would have been *"OCSP: N"* is deleted rather than answered.

**DNS over TLS and DNS over QUIC — an open question, and this is what it
would cost.** niquests names four transports on one seam
(`ProtocolResolver`: `doh`, `dot`, `doq`, `dou`), reached as a URL and
selectable per host pattern. Here `Resolve` is the same seam and carries
three implementations, none of which is DoT or DoQ. Two things are worth
knowing before anyone rules on it. The **codec is already shared** — SVCB
and wire-format parsing moved up into `hclient-dns` behind a `codec`
feature precisely so an `IpLiteralOnly` build carries no decoder — so a
fourth backend reuses the half that took the work. And the **bootstrap
question they raise is the one `hclient-dns-doh` already answered**:
`Doh::pinned` takes an IP literal and refuses a name, `Doh::bootstrapped`
takes a name and refuses a literal, and failing closed is visible in the
type. What a DoQ backend adds that DoH did not is a QUIC endpoint on the
resolver's side of a client that already has one for HTTP/3 — which is
either a reason it is nearly free or a reason it drags `quinn` into every
graph that resolves a name, and nobody has measured which.

---

## 5. The reverse column: what `hclient` has and the others do not

Each row was checked against the comparators rather than assumed.

**One `Transport` seam across five backends, and the same source builds for
three targets.** `crates/hclient/examples/portable.rs` compiles for native,
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
and `hclient-fetch`'s `js_sys::Reflect` write through an unchecked ref
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
time.** `check_supported` (`hclient/src/config.rs:261`) turns a cookie jar
against a jar-owning backend, a cache against a caching backend, a
`RedirectPolicy` against an internally-redirecting backend, and an
unsupported timeout into a typed `UnsupportedCapability` **before a request
is made**. No comparator has anything of the kind; the nearest is reqwest
compiling a smaller API on wasm, which catches a subset at compile time and
silently ignores nothing — but also cannot express "this backend does
decompress internally, so do not decode again", which
`DecompressionSupport::Internal` does. The rule that keeps the model honest
is that a capability reports the **floor**, and `Native::http3` is where
that rule bites hardest: five of six disagreeing fields take the weaker
value and the rest make the call **refuse, naming the field**.

**Per-request timeouts, five of them, with distinct meanings** — this
paragraph said *four* and was written before G11a landed. `resolve` /
`connect` / `first_byte` / `between_bytes` in `Timeouts` plus `total` on the
client, settable per request (`RequestBuilder::timeouts`), and each measured
against a server that misbehaves in exactly that way
(`crates/hclient-native/tests/timeouts.rs`, `crates/hclient/tests/deadline.rs`).
reqwest has `timeout`, `read_timeout`, `connect_timeout` and
`pool_idle_timeout` on the **client** and one `timeout` per request.
niquests has **three** and they are urllib3's — `Timeout(total, connect,
read)`, where `read` is a per-socket-operation bound rather than a bound on
the exchange, which its own quickstart says in as many words: *"A scalar
`timeout` applies to socket connection and read operations; it is not a
wall-clock limit on the entire response download."* So `total` there means
something narrower than `total` here.

**And the count is the wrong axis, which G14 is about**: five separable
bounds all defaulting to `None` is a worse answer for the caller who wrote
two lines than three bounds with a number on them.

**ureq beats this on count and it is worth conceding rather than dressing
up**: nine distinct bounds — `timeout_global`, `timeout_per_call`,
`timeout_resolve`, `timeout_connect`, `timeout_send_request`,
`timeout_await_100`, `timeout_send_body`, `timeout_recv_response`,
`timeout_recv_body` (`src/config.rs:669-745`). **The three this paragraph
listed as missing are two**: `resolve` landed with G11a, and what remains is
`send_request` and `send_body` — neither of which is the one a caller
notices, which was the argument for ranking `resolve` first and is now
spent. Blocking IO is what
makes nine cheap there and four expensive here — each bound here has to be
a race or a body wrapper carrying a `Timer::Sleep`, and the `Expect` work
showed what a fifth costs (a wrapper around
the `first_byte` race overflowed the stack in 56 tests).

**`RequireVersion`, enforced before the head.** Nobody else has a
per-request protocol demand that fails rather than downgrades. reqwest's
`http1_only` and `http2_prior_knowledge` are client-wide construction
settings.

**HTTP/3 without an unstable flag, and WebTransport at all.** reqwest's
`http3` feature refuses to compile without
`RUSTFLAGS='--cfg reqwest_unstable'` (`reqwest-0.13.4/src/lib.rs:252`) and
has done for two years. `hclient-webtransport` — sessions, bidi streams,
datagrams, RFC 9297 close capsules — has no counterpart in any comparator;
it was checked against `wtransport` 0.7.2, which shares no code with `h3`.

**Server-Sent Events, with reconnection, in the client itself.** Zero
matches for `eventsource` or `text/event-stream` in all of
`reqwest-0.13.4/src/`; the ecosystem answer is `reqwest-eventsource` over
`eventsource-stream`, whose specific defects the design spec §4.10
enumerates. `hclient` has the decoder *and* `Client::sse`'s reconnect with
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
And it reports `RedirectSupport::Transparent` where `hclient-fetch` must
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
which is exactly the signature rejected here for
the DNS-leak reason — and the reason `Prefetch::prepare` can hand a
connector a fetched HTTPS record where `hyper-util` has no channel for one.

**Sans-io crates a third party can use without the client.**
`hclient-proto` — RFC 3986 resolution, redirect rules, SSE decoding, the
two URL encoders. reqwest has none.

That list was two entries longer until the jar (RFC 6265bis) and the
cache (RFC 9111) became modules of `hclient` behind its `cookies` and
`cache` features. **They are still sans-io and still clockless — what
they are no longer is separately usable**, and that is the whole of what
the merge cost: `cargo tree -i` had named exactly one consumer for each,
so the boundary was being kept for a third party who did not exist.

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
`hclient_rt::TcpConnect` sits, not where `hclient_core::Transport` does, and
that placement decides two things at once: a `wasi:http` or `fetch` backend
**cannot implement it**, because neither has a byte stream to hand over, and
a single-threaded backend cannot either, because of the `Send + Sync` bound.
Both of those are exactly what `hclient_core::Transport`'s shape — taken
from `wasi:http/client.send`, *"the poorest of the ambient APIs"*
(`transport.rs:5-11`) — was chosen to allow. ureq's seam does what ureq
needs, which is SOCKS, TLS and test doubles; it is not a smaller version of
this one.

**A build that carries no TLS, no resolver and no protocol machinery.**
`cargo tree -e normal` unique crates, measured in this tree today:

| build | crates |
|---|---|
| `hclient`, `--no-default-features` | **18** |
| `hclient-rt-tokio` alone | 22 |
| `hclient-native`, `--no-default-features` | 32 |
| `hclient` + `default-transport` (native, tokio, rustls, system DNS) | **83** |
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

### 5.1 Against niquests specifically, because it is the one that has most of the rest

Six of the rows above are answered by niquests and are not claims against
it: it has HTTP/3, SSE, WebSocket, a resolver seam, per-phase timings and a
`wasi:http` backend. So the reverse column against the *closest* peer is
shorter than the general one and is worth stating separately rather than
letting the long list imply more than it should.

**A capability model, and it is the one thing on this list that is
structural rather than a feature.** §2.9 has the argument; the short form is
that niquests decides what a backend can do with `hasattr`, so the question
*will this setting be honoured* has no place to be asked. The ECH row is the
demonstration and it is not hypothetical: on a plain install a record's
`ech_config_list` is fetched, carried and dropped, silently, because the
active TLS backend has no `set_ech_configs`. Here the identical omission is
a defaulted-`false` constant that a connector reads before it fills the
field. Both do nothing; only one can say so.

**WebTransport, `Expect: 100-continue`, request trailers, an RFC 9111
cache, a response body size limit, and public-suffix rules in the jar.**
Each was checked against the installed package rather than assumed: zero
hits for `webtransport`/`masque`/`connect-udp`; no `Expect` emission; a
`trailers` attribute on the response and none on the request; no
`Cache-Control` freshness logic anywhere in `niquests/`; no body ceiling
(`enforce_content_length` is a `Content-Length` agreement check, not a
limit); and the executed cookie probe in §2.7.

**A retry that knows whether the request left.** niquests' retry is
urllib3's `Retry`, which is a good one — the same safe-method default this
workspace arrived at independently — and it sits above the transport, so
*never sent* and *sent and the server acted on it* are the same event to
it. `Error::is_unsent()` is a claim made at the site that knows, and it is
the difference between a retry that is safe by construction and one that is
safe by convention. This is the row where being in Rust is irrelevant and
being *below* the transport is the whole thing.

**A pluggable cache store, and a cache at all.** niquests has neither, which
is unusual for a client this complete and is probably the same reason as
here-before-v0.4: a cache needs a store, a store needs a lifetime, and
nobody wants to pick one for somebody else.

**One `Transport` seam that five backends implement, with the same source
compiling for three targets.** This is the row where niquests is the closest
of any comparator and still different in kind. It genuinely has three
backends — native, Pyodide, WASI — and its WASI half probes for
`wasi:http` 0.2, `wasi:http` 0.3 *and* `wasi:sockets`, which is more ground
than `hclient-wasi` covers. But they are selected by `sys.platform` and
import-time probes inside one package, not by a trait a third party can
implement: there is a `BaseAdapter` a caller can subclass and mount, and it
is an *adapter over urllib3*, not a seam the client is written against. The
practical difference is the one §5 opened with — `crates/hclient/examples/
portable.rs` compiles unchanged for three targets and a CI job fails if that
stops being true, and the equivalent question does not arise in a language
where the same source always compiles.

**What niquests has that nothing here does, said plainly, because a reverse
column that only flatters is not evidence.** Its HTTP/3 needs no flag and
no call (§1). Its Alt-Svc memory is seedable (G15). Its WebSocket runs on
all three protocols (G16). Its timings come off the response rather than
through a hook (§2.4). It has a second TLS stack for fingerprint control
(§2.5), four DNS transports (§2.4), `.netrc`, `Link:`, `iter_lines`,
fingerprint pinning and upload progress (G17) — and a default timeout
(G14). That is a longer list than this document has had to write about any
comparator, and it is the reason the column was added.

---

## 6. Against the project's own goal

The stated goal is not "match reqwest". It is stated twice, in two places,
and only one of them is met.

**"Powerful enough that someone else could build gRPC on it" — met, and
measured.** 21 requirements from
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
| ≥3 runtimes | **met** — tokio, smol, embassy, and quinn's through `hclient-quinn`. **`compio` is named in decision D10 as a CI runtime and does not exist here**; grepped, one hit, in a `tree-guard` *absence* check |
| the `unversioned` quarantine is documented | not assessed here |
| **`hclient-rmcp` and `act` in production** | `hclient-rmcp` **does not exist in this workspace**. It is named as "the second verification loop" in the v0.2 plan and as an `rmcp` adapter in the architecture diagram. `act` is the *first* loop and is present as `examples/portable.rs` |
| **not a single foreign type remains in the public API** | **flatly contradicted by AGENTS.md**, which states that `http::{Request, Response, HeaderMap, Uri, Method}` appear in the public API of ten crates and that this is necessary. See §8 |

Two further planned crates named in the version plan are absent:
`hclient-espidf` and `hclient-nyquest` (both v0.4).

So the honest answer to "is it full-featured yet" is: the *hard* half was
done and the *ordinary* half was not. Every protocol capability a
demanding consumer would ask for exists — h2, h3, WebSocket,
WebTransport, SSE, trailers, duplex, 0-RTT, `1xx`, `Expect`, caching,
cookies, proxies — and what was missing was the ordinary furniture: a
`User-Agent`, a default header set, a size limit, a charset, a pluggable
store.

**Eight are closed** — G2, G5, G6, G8, G9, G10, G13 and G12's digest half,
plus `deflate`/`zstd` from §4 — which does not retire the argument this
paragraph was making. It sharpens it. Every one was found by *writing this
document*, not by the test suite, which was green throughout and is green
now; each took under a day once named. That is the signature of the class:
cheap to fix, invisible from inside, and found only by someone trying to
use the thing. A second real consumer — `hclient-rmcp` — is still the
argument, because whatever is next is equally invisible from here and this
document cannot be written twice by the same reader.

**What is left is a different shape, which is itself a finding.** G1 is
the owner's call and G3 is refused by the problem statement. G13 was the
one whose cause this document said was upstream
(rust-lang/rust#109417) — it is closed, and the stabilisation never
arrived: what closed it was noticing that the erased client does not need
the bound the upstream feature would have proven. Of the rest, G4 needs
browser judgement about the browser's own model, G7 and G11 are more
fields on seams that already exist, and G11a is the one that needs care —
Happy Eyeballs interleaves resolution with connecting on purpose, so
*"resolve took N ms"* is not a phase this connector has, and the honest
bound is time-to-first-address rather than a phase boundary. The furniture
is on; what remains is either somebody else's decision or a design
question.

**And then a second reader arrived and the shape changed again, which is
the finding's own point being made about itself.** G14 through G17 came out
of reading niquests, and they are the same class as G2, G5 and G10 — cheap,
invisible from inside, found only by looking at what somebody else thought
worth having. The paragraph above says *the furniture is on*; four more
pieces of it turned out to be missing the moment a fifth comparator was
opened, and one of them (G14) is not furniture at all. So the sentence to
keep is not *the furniture is on* but the one this section already makes:
**this document cannot be written twice by the same reader**, and the
evidence for that is now two readings apart rather than one.

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

- ~~**`isahc` and `attohttpc` are surveyed much less thoroughly than
  reqwest, ureq, gloo-net and hyper-util.**~~ **Half answered.** `isahc`
  2.0.1 was read on 2026-08-19 and seven rows changed because of it —
  digest and Negotiate, Unix sockets, response trailers, `Expect:
  100-continue`, SOCKS4/4a, HTTP/3 detection, interface binding — plus one
  row this document did not have (per-request timing) and a **third
  answer** on the public-suffix row. `attohttpc` 0.31.0 is still only
  lightly surveyed, and now deliberately: its feature table is unchanged
  in every capability this document compares. The `curl` crate was checked directly on the five rows that
  carry an argument — auth schemes, WebSocket, Unix sockets, HTTP/3 version
  selection — and the rest of the `cu` column rests on libcurl's documented
  option set rather than on a line-by-line audit of the binding. The
  WebSocket row is a warning about doing that: libcurl 8.21 has the API and
  the Rust binding does not expose it, so "libcurl can" and "a Rust caller
  can" came apart on the first row where anyone looked.
- ~~**isahc's maintenance state was not established.**~~ **Answered: it is
  alive.** `isahc` 2.0.1 is a major release on edition 2024 with
  `rust-version = "1.85"`, and it turned TLS into a choice — `rustls-tls`,
  `native-tls`, `trust-webpki-roots`, `tls-insecure`, none of which 1.8.3
  had. The prediction that its answer "would most change the shape of §2"
  was right: it moved more rows than any other single re-read in this
  document's history.
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
  rows rest on `hclient-urlsession`'s own record, which states its four live
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
- **niquests was executed but never against a misbehaving server.** Every
  `nq` cell rests on reading the installed package, on `inspect` against
  live objects, or — for the four claims marked *executed* — on running it
  against a real origin or a real object. What was **not** done is what
  makes the `ng` column trustworthy: no fixture, no server that answers
  wrongly on purpose, no mutation. So a row saying niquests *has* a feature
  means the code path exists and, where marked, ran; it does not mean the
  feature is correct. The `full duplex`, `streaming multipart`,
  `absolute-form`, `0-RTT` and `header size limits` cells are `—` for that
  reason rather than `N`.
- **Only two of niquests' three TLS backends were exercised, and the two
  that matter most were not.** The venv resolved to stdlib `ssl`
  (`urllib3_future.contrib.anytls.BACKEND == 'ssl'`, executed), so every
  claim about `rtls` (rustls + AWS-LC) and `utls` (BoringSSL) — the ECH
  application, the fingerprint control, the post-quantum path on the *TCP*
  side — is read from the dispatch code and from the extras table, not run.
  The QUIC path's post-quantum default is read in `qh3`, which *was*
  installed and did serve the HTTP/3 requests in §1.
- **niquests' docs and its source disagreed nowhere, and that was checked
  rather than assumed.** Every claim below that cites a doc page was also
  found in the installed package, which is the discipline §0 states for
  Rust comparators applied to a Python one. The one place the docs say more
  than the code could be made to show is the index page's `DNSSEC!` bullet,
  for which no mechanism was located in either — so no row claims it.

---

## 8. Five things in the repository's own record that this reading contradicts

Recorded here rather than fixed, because each is somebody's call.

**1. `README.md` is stale by three versions.** It says *"v0.1 is HTTP/1.1
over those three backends. Connection pooling, HTTP/2 and /3, streaming
request bodies and WebSocket are not built yet"*. All five have shipped, and
there are five backends rather than three. It is the first file a reader
opens.

**2. AGENTS.md does not mention four crates that exist.**
`hclient-tower`, `hclient-rt-embassy`, `hclient-dns-hickory` and
`hclient-mock` appear nowhere in it (grepped). Two of them are listed in
the v0.1 *deliberately not done* list as things it would not
do — *"hickory and DoH; middleware and `hclient-tower`"* — and both now
exist. The DoH half of that sentence *was* updated (AGENTS.md has a section
on `hclient-dns-doh`); the hickory and tower halves were not.

**3. "Microcontrollers are not reachable today" is now half true.**
AGENTS.md says so and names two obstacles, `http` 1.x and `url`. `url` is
gone and `hclient-rt-embassy` exists — `embassy-net` sockets, `embassy-time`
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

**5. The first item in `hclient-urlsession`'s justification is already
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

## 9. The summary, in five sentences

The strongest thing this client lacks against its comparators is **a
default timeout** — G14, and the sentence that used to stand here said
*being published*, which stopped being true at `0.1.0-alpha.2`; after that,
the ordinary ergonomics a fresh reader finds and a test suite cannot, which
this document has now produced twice over from two different comparators
and has no reason to think it has stopped producing.

The strongest thing it has and they do not is **one `Transport` seam that
five backends implement and three targets compile the same source against,
with a capability model that refuses at `build()` rather than ignoring at
run time**. ureq 3 is the near miss that makes the point sharp: it has a
`Transport` trait, a `Resolver` trait, a sans-io crate and an `unversioned`
quarantine of the same name — and its `Transport` is a byte stream bounded
`Send + Sync + 'static`, which no ambient backend and no single-threaded one
can implement.

The most surprising single fact found while writing this is that **no
comparator's default cookie jar applies public-suffix rules** — reqwest's
and ureq's by reading `cookie_store`, niquests' by *running* it, where a
cookie scoped to `.co.uk` set by `shop.example.co.uk` is accepted — so this
workspace's compiled-in list is not a nicety but the only such check among
four clients that otherwise agree about almost everything.

The closest peer is **niquests**, and it is close enough that the
interesting differences are all about *where a decision lives*: it reaches
HTTP/3 with no flag and this one needs a feature and a constructor, it
seeds its Alt-Svc memory through a public attribute and this one keeps
that memory private, it decides what a backend can do with `hasattr` and
this one refuses at `build()` — and only the last of those three is a
difference this document would defend.

And the sentence this document would most like a reader to take away is the
one it had to correct about itself, twice. An earlier draft asserted ureq
installed that list, on plausibility alone, and reading
`src/cookies.rs:172-180` is what stopped it becoming a claim. Then the
matrix went on saying `N` for *published* and *type-erased client* while §3
said `closed` two screens below, for as long as nobody re-read a row they
were not arguing about — which is this workspace's rule about checks,
turned on the document that keeps stating it.
