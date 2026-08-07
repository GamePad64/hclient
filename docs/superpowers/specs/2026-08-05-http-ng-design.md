# http-ng — design

**Date:** 2026-08-05
**Status:** approved for implementation
**Verified on:** rustc 1.97.1 (2026-07-14), crates.io as of 2026-08-05

Async HTTP client in Rust: the same application code builds for
native, browser and WASI, because the transport is swapped, not buried under
`#[cfg]`.

**The implementation target for this spec is v0.1 (§10).** The other versions are
described here so v0.1's decisions don't close off paths later; a separate
implementation plan is written for v0.1 only.

---

## 1. Motivation and positioning

**The driving force is cross-platform reach.** Write an application once and build
it for server, browser (WASM), and WASI. Hence the priorities: ambient backends and
a single API matter more than full parity with reqwest.

**The niche.** reqwest is modularizing, but along the *middleware* axis, not the
*runtime* and *backend* axes:

- [seanmonstar/reqwest#2585](https://github.com/seanmonstar/reqwest/issues/2585) —
  "Meta: more modular pieces," breaking the pool, proxy, redirects and decompression
  out into tower Services.
- [#557](https://github.com/seanmonstar/reqwest/issues/557) "Allow setting Hyper
  executor" — open since 2019-07-04, still unresolved.
- [discussion #2486](https://github.com/seanmonstar/reqwest/discussions/2486) —
  a user asks to plug in their own backend Service (they have wasip1/Extism);
  the maintainer's answer — "use `tower::Service`" — but only in the reverse
  direction (Client *as* a Service, not a Service *under* Client).

In other words, a pluggable transport and runtime neutrality aren't coming to
reqwest. That's our niche.

**A historical warning.** The graveyard in this niche is made up entirely of
projects that promised N runtimes and tested one: `hreq` ("set out with the
ambition to be runtime agnostic… in practice that was not a viable route"),
`surf`/`http-client` (7.4M downloads, dead since 2022-06-20 along with async-std,
officially discontinued 2025-03-01). Mitigation in §11.

---

## 2. Decision log

Each row is a fork in the road where we could have gone another way.

| # | Decision | Why |
|---|---|---|
| D1 | Core **doesn't depend on hyper**; hyper is one of the transports | In the browser, hyper is physically not involved: `fetch` gives no access to connection bytes. Otherwise fetch/wasi would pull in hyper+h2+tokio for nothing |
| D2 | `Transport` is a **public trait**, backends are separate crates | Third-party backends (esp, URLSession, curl, BoringSSL) get written without a PR to us. Side effect: no mutually exclusive cargo features arise by construction |
| D3 | Our own minimal `Transport`, tower as an **adapter** in a separate crate | Backends don't need `poll_ready` (reqwest hardcodes it to `Poll::Ready(Ok(()))`); tower 0.x in the public API would tie us to its major version |
| D4 | Narrow portable core + **extension traits** per backend | Differences *between* backends get closed at compile time, honestly |
| D5 | Plus a **runtime capability registry, `Capabilities`**, and a typed error on `build()` | One wasm binary works in both Chrome (streaming request body since 131) and Safari (no). `cfg` can't express that |
| D6 | **Not a single `Send`/`Box`/`cfg` alias in the core** | All the machinery was a consequence of type erasure in middleware. Remove the erasure from the built-in stages and the machinery disappears entirely (§4.2) |
| D7 | Built-in capabilities are **stages configured by data**, not layers | Otherwise the client type becomes `Decompression<FollowRedirect<Cookie<Retry<…>>>>` (literally reqwest 0.13's guts) with sealed `Unnameable`/`Conn` types, which make connection metrics impossible ([reqwest#2955](https://github.com/seanmonstar/reqwest/issues/2955)) |
| D8 | Core has only `Timer`; networking and spawn belong to the transport | The core has no I/O at all. Its only obligation is sleep, for timeouts and backoff |
| D9 | **embassy/no_std is crossed off**; "embedded" = ESP-IDF as an ambient backend | The blocker isn't hyper, it's `http` 1.5.0: `#[cfg(not(feature = "std"))] compile_error!`. ESP-IDF, meanwhile, doesn't need hyper at all |
| D10 | CI runtimes: **tokio + smol + compio** | You can only claim neutrality about what's in CI |
| D11 | tokio in the dependency graph of hyper builds **is accepted and documented** | [hyper#3428](https://github.com/hyperium/hyper/pull/3428) (exactly this fix via `futures-channel`) was rejected, [#3767](https://github.com/hyperium/hyper/issues/3767) closed as *not planned* (§10) |
| D12 | Split `http-ng-core` (plugin contract) / `http-ng` (user-facing surface) | Only this way can `http-ng` depend on `http-ng-hyper`, which depends on the contract, without a cycle. Gives `Client<T = DefaultTransport>` |
| D13 | **h1/h2/h3 inside one native transport**, negotiation transparent | The user shouldn't have to know they're on h3. Composing `AltSvc<H2,H3>` would make the version choice visible in the type |
| D14 | Native transport **does not declare `Send`**; CI asserts stand in for a bound | `Send` in a trait is contagious: `TcpConnect::Stream: Send` would force compio to repeat cyper's `SendWrapper` hack, which panics on a cross-thread drop |
| D15 | The QUIC engine is an **implementation detail**, swappable in a patch release | `h3::quic::*` and quinn types aren't in the public API (§9.2) |
| D16 | **sans-io** as a binding rule, enforced by the dependency graph | §8 |
| D17 | **`wasi-fetch` gets absorbed** and becomes `http-ng-wasi`; **p3-only** in v0.1 | It's our crate, 571 lines, already works; §7.1 |

---

## 3. Architecture

Three tiers, not one ladder. A tier is defined by what's physically available.

```
Tier A — portable. No hyper, no tokio, no sockets.
┌──────────────────────────────────────────────────────────────────┐
│ http-ng-proto   pure state machines: SSE decoder, Alt-Svc,       │
│                 redirect/retry/cookie logic, Happy Eyeballs      │
│                 scheduler, multipart, URL. Zero async.           │
│ http-ng-core    Transport, Capabilities, RequestBody, Error,     │
│                 **Timer**. ~500 lines. `unversioned` quarantine. │
│ http-ng         Client<T = DefaultTransport>, builder, stages,   │
│                 SSE stream, sugar.                               │
└──────────────────────────────────────────────────────────────────┘
        ▲                     ▲                        ▲
Tier B — socket tier          │        Tier C — ambient (depend
(hyper, Send de facto)        │        only on Tier A)
┌──────────────────────────┐  │        ┌─────────────────────────┐
│ http-ng-rt   Spawn,      │  │        │ http-ng-wasi  (p3)      │
│   Timer, TcpConnect,     │  │        │ http-ng-fetch           │
│   TcpAdoptStd, Blocking, │  │        │ http-ng-espidf   (v0.4) │
│   + FuturesIo shim       │  │        │ http-ng-nyquest  (v0.4) │
│ http-ng-rt-{tokio,smol,  │  │        └─────────────────────────┘
│              compio}     │  │
│ http-ng-tls  +-rustls    │  │        On the side:
│              +-native    │  │        http-ng-tower   adapter
│ http-ng-dns  +-system    │  │        http-ng-ws      message API
│              +-hickory   │  │        http-ng-wt      hook
│              +-doh       │  │        http-ng-rmcp    adapter
│ http-ng-native  h1/h2/h3 │──┘        wasi-fetch      facade compat
│ http-ng-h3   (engine)    │
└──────────────────────────┘
```

**Invariant:** `http-ng` does not depend on hyper. `http-ng-h3` is an engine inside
`http-ng-native`, not a user-facing `Transport`.

---

## 4. Core (Tier A)

### 4.1 `Transport`

The shape is taken not from hyper, but from `wasi:http/client.send` — the poorest
of the ambient APIs. Anything richer degrades to it cleanly; the reverse doesn't
hold.

```rust
// http_ng_core::unversioned
pub trait Transport {
    type Body: http_body::Body<Data = Bytes>;
    type Error: std::error::Error + 'static;

    async fn execute(&self, req: http::Request<RequestBody>)
        -> Result<http::Response<Self::Body>, Self::Error>;

    fn capabilities(&self) -> &Capabilities;
}
```

No `poll_ready`, no `&mut self`, no `Send`. Per-request configuration goes into
`req.extensions()`, **not** a separate `Context` parameter: rama kept a `Context`
for 16 months and ripped it out in 0.3.0 in favor of extensions (PR #711/#714).

**The shape was arrived at independently three times:** the `wasi:http` 0.3.0
spec; `rmcp::transport::OAuthHttpClient::execute` (`http::Request<Vec<u8>>` →
`http::Response<Vec<u8>>`); `wasi_fetch::send_raw` (`http::Request<Bytes>` →
`http::Response<Body>`). And `act-cli` implements the same signature from the
*host* side of the boundary (`wasmtime-wasi-http` outgoing handler), while
`wasi-fetch` does it from the guest side.

### 4.2 Why the core has no `Send`, `Box`, or cfg

The chain we broke: built-in stages as layers → type explosion → need erasure →
need `dyn` → `dyn` requires declaring `Send` → doesn't exist on wasm → need a
cfg-switched `MaybeSend`. Five levels of machinery because of the first step.

We break the first link (D7): built-in capabilities are stages configured by
data. Then `Send` is never declared anywhere, and auto-traits leak through on
their own via `impl Future`.

**Verified on rustc 1.97.1:**

| technique | status |
|---|---|
| `where B::execute(..): Send` (RTN) | ❌ unstable, [rust#109417](https://github.com/rust-lang/rust/issues/109417) |
| `type Fut = impl Future` (ATPIT) | ❌ unstable, [rust#63063](https://github.com/rust-lang/rust/issues/63063) |
| `async fn` in a trait, no Send | ✅ |
| `-> impl Future + Send` in a trait | ✅ (but forces Send) |

RTN is only needed by someone writing **generic, unerased** code that spawns. No
such case remains: **Send-ness and erasure are one axis**. Whoever erases takes
`BoxTransport`, which is `Send` by construction; whoever stays monomorphic gets
auto-traits for free.

Erasure is explicit and optional — two named types, ~40 lines of hand-written glue
each, **without** `dynosaur` or `trait_variant`:

```rust
let c: Client<BoxTransport>   = builder.build().boxed();        // Send + Sync
let c: Client<LocalTransport> = builder.build().boxed_local();  // !Send
```

Gone from the dependencies: `cfg_aliases`, `dynosaur`, `trait-variant`,
`async-trait`.

### 4.3 `!Send` is a build-configuration property, not a platform one

Verified by building for `wasm32-unknown-unknown`:

| type | Send? |
|---|---|
| `wasm_bindgen::JsValue` | ✅ |
| `js_sys::Promise` | ✅ |
| `web_sys::{Request, Response, ReadableStream}` | ✅ |
| `wasm_bindgen_futures::JsFuture` | ❌ — `Rc<RefCell<futures::Inner>>` |

wasm-bindgen 0.2.126 declares this itself:

```rust
pub struct JsValue { idx: u32, _marker: PhantomData<*mut u8> /* not at all threadsafe */ }
#[cfg(not(target_feature = "atomics"))] unsafe impl Send for JsValue {}
#[cfg(not(target_feature = "atomics"))] unsafe impl Sync for JsValue {}
```

The reason: `JsValue` is an index into a table owned by the JS glue ("*A
`JsValue` doesn't actually live in Rust right now but actually in a table owned
by the wasm-bindgen generated JS glue code*"). With `+atomics`, each worker gets
its own table — the index becomes unsafe, and the compiler catches it.

The only blocker on the default target is the `Rc` inside `JsFuture`. Our own
promise adapter on `Arc<Mutex<..>>` (~50 lines) **is verified to build without
atomics and to be correctly rejected with atomics**. So:

- `http-ng-fetch` will be `Send` on the default browser target;
- `!Send` remains only for builds with wasm threads — a deliberate opt-in;
- **no cfg appears in our own code**; the only `#[cfg(not(target_feature =
  "atomics"))] unsafe impl Send` is on a single type in `http-ng-fetch` that
  mirrors upstream.

In retrospect this vindicates D6 even more strongly: the originally proposed
`MaybeSend` keyed off `target_family = "wasm"` — **the wrong axis**.

### 4.4 `RequestBody` with an explicit replay contract

```rust
pub enum RequestBody {
    Empty,
    Full(Bytes),                              // replay is free
    Rewindable(Arc<dyn Fn() -> BodyStream>),  // replay via a factory
    Streaming(BodyStream),                    // replay impossible
}

impl RequestBody {
    pub fn retry_kind(&self) -> RetryKind;              // before sending, not after
    pub fn buffer_for_retry(self, max: usize) -> Self;  // Streaming → Rewindable
}
```

Closes off the root of two holes: `reqwest::Request::try_clone() -> None`
silently disables retry; `reqwest-retry` on a streaming body fails with
`Error::Middleware("Request object is not cloneable")` **before the first
attempt**. And it removes the "streaming or redirects, pick one" choice that
makes `wasi-fetch` hold its body as `Bytes`.

### 4.5 Timeouts — a triple

```rust
pub struct Timeouts {
    pub connect:       Option<Duration>,
    pub first_byte:    Option<Duration>,
    pub between_bytes: Option<Duration>,
}
```

The `wasi:http` shape is the richest of the ambient models. In fetch it collapses
into a single `AbortController`; in hyper it spreads across connector / awaiting
response / body idle.

Two live proofs that a single `Duration` isn't enough:
`act-cli/src/runtime/http_client.rs` does
`tokio::time::timeout(config.connect_timeout + config.first_byte_timeout, ..)` —
**adding two timeouts together**, because reqwest only accepts one; `wasi-fetch`
sets `set_connect_timeout(ns)` and `set_first_byte_timeout(ns)` from a single
value and separately hacked off a `between_bytes_timeout` for SSE.

### 4.6 `Capabilities` — runtime, registry, typed error

```rust
#[non_exhaustive]
pub struct Capabilities {
    pub streaming_request_body: bool,   // Chrome 131+ yes, Safari no — in one binary
    pub full_duplex: bool,
    pub request_trailers: bool, pub response_trailers: bool,
    pub redirects: RedirectSupport,     // Internal | Configurable | Inspectable | None
    pub tls_config: TlsSupport,         // None | ServerTrustCallbackOnly | Full
    pub client_certs: bool, pub proxy: bool,
    pub owns_cookie_jar: bool, pub owns_cache: bool,
    pub version_select: bool, pub version_reported: bool,
    pub timeouts: TimeoutSupport,       // separately for each of the three
    pub informational_1xx: bool,
    pub upgrade: UpgradeSupport,        // None | H1 | ExtendedConnect | Both
    pub forbidden_request_headers: &'static [HeaderName],  // ~25 for fetch
}
```

An unsupported setting is `Err(UnsupportedCapability)` at `build()`, **never a
silent no-op**. The model is `wasi:http` itself: its setters return
`result<_, request-options-error::not-supported>`.

A live counterexample: `wasi-fetch/src/request.rs` has **seven** `let _ =` on
such `Result`s (`set_connect_timeout`, `set_first_byte_timeout`,
`set_between_bytes_timeout`, `set_method`, `set_scheme`, `set_authority`,
`set_path_with_query`). If the host doesn't support a timeout, the guest
silently ends up without one.

**Invariant:** `Capabilities` must not exceed ~25 fields (§11, stop criterion).

### 4.7 `Error`

```rust
#[non_exhaustive]
pub enum ErrorKind { Resolve, Connect, Tls, Redirect, Timeout(Phase), Body, Decode, Status, .. }
```

`Clone` via `Arc<dyn Error>` (no `Send` bound — auto-trait transparency reaches
errors too). Keep the `is_*` predicates as a convenience.

The motivation isn't aesthetics. In `act`:

- the guest flattens the `wasip3 ErrorCode` into a string:
  `Error::Transport(format!("{e:?}"))`;
- the host reconstructs it by substring-matching across the whole `source()`
  chain: `error_chain_contains(&err, &["deny cidr", "failed to lookup",
  "dns"])`, with the comment "*reqwest wraps DNS resolver errors through
  multiple layers … so a single `.source()` hop isn't enough*";

A full circle: structure → string → structure → string. Both losses trace back
to an opaque `Error` ([reqwest#1053](https://github.com/seanmonstar/reqwest/issues/1053)).

### 4.8 `Client` and stages

```rust
pub struct Client<T = DefaultTransport> { transport: T, config: Config }

#[cfg(not(target_family = "wasm"))]
pub type DefaultTransport = http_ng_native::Native<Tokio, Rustls, SystemDns>;
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub type DefaultTransport = http_ng_fetch::Fetch;
#[cfg(target_os = "wasi")]
pub type DefaultTransport = http_ng_wasi::WasiHttp;
```

Three levels of user:

```rust
// 1. Just a client — zero generics, one piece of code on three targets
let text = http_ng::Client::new().get(url).send().await?.text().await?;

// 2. Configuration — still zero generics
let client = http_ng::Client::builder()
    .redirect(Redirect::limited(10))
    .timeouts(Timeouts { connect: Some(secs(3)), ..default() })
    .retry(Retry::idempotent())
    .build()?;

// 3. A different backend / your own middleware — this is where generics show up
let client = Client::builder_with(Native::new(Smol, Rustls, hickory))
    .layer(Signing::new(key))
    .build()?;                       // Client<Signing<Native<…>>>
```

The default is an opinion, not a restriction: `Client` with no parameter means
`Client<DefaultTransport>`; `Client<Whatever>` works the same way. No mutually
exclusive features arise — the target does the choosing.

**Stages** (the order is fixed in code and correct by construction; reqwest
arrived at it empirically):

```
decompression → redirect → retry → transport
```

**Middleware** is the only generic axis; it only grows with what the user adds.
The types are ours and public, not `Unnameable`:

```rust
pub trait Middleware<T> { type Output: Transport; fn wrap(self, inner: T) -> Self::Output; }
```

Two insertion points, which reqwest+reqwest-middleware can't offer (their
`FollowRedirect` sits outside everything):

- `.layer_outer(..)` — one logical req/resp pair: auth, tracing, cache;
- `.layer_inner(..)` — **every hop**, including redirects and retries: request
  signing, per-hop policy, per-hop metrics.

Connection-level middleware exists **only** on the native path — write that down
in the docs, or users will write a layer that silently does nothing on iOS.

### 4.9 What we fix from reqwest, because we're designing from scratch

| Hole | Reactions | Fix |
|---|---|---|
| Base URL / relative URLs (#988, #213, since 2017 and 2020) | 104 | `ClientBuilder::base_url()` |
| Event hooks (#155, since 2017) | 48 | public `ConnectRequest`/`Connected` (URI, resolved addr, ALPN, peer certs) + observable connection `Drop` |
| Per-request config (#2641) | 16 | `http::Extensions`, "request-first, client-fallback" lookup — **build it in from day one**, or it's a breaking change later |
| Non-destructive body reads (#1542) | 18 | `into_parts() -> (Parts, Body)` + `Collected` that keeps status/headers/url after `.text()` |
| `Error` with no kind enum, not `Clone` (#1053) | — | §4.7 |
| Synchronous `CookieStore` | — | async trait with `&self` |

### 4.10 SSE

We write our own, because every existing one is broken in specific ways:

- `eventsource-stream` 0.2.3 (17.2M downloads, backs `reqwest-eventsource`): in
  `dispatch()` it **loses `retry:`** in blocks with no `data`; **discards
  comment lines** → you can't build a keep-alive detector; it parses `retry` via
  `u64::from_str`, accepting `+5000` despite "ASCII digits only".
- `reqwest-eventsource` 0.6.0: **doesn't implement the HTTP 204 rule** → keeps
  reconnecting forever against a server that said "stop"; sends an empty
  `Last-Event-ID`; `try_clone().unwrap()` → panics on a streaming body.
- LaunchDarkly's `eventsource-client` 0.17.5: the best parser, but **doesn't
  check `Content-Type`**, has a `TODO … (e.g. 204)`, and as of 0.17.x pulls in
  the proprietary `launchdarkly-sdk-transport`.
- `sse-stream` 0.2.5 (used by rmcp): no comment variant; `Error` has
  `DuplicatedEventLine`/`DuplicatedIdLine`/`DuplicatedRetry` — **gets it wrong**
  exactly where WHATWG says "last value wins".
- **Not one** of them jitters on reconnect.

**Two layers** (rmcp confirmed the split is needed — it only wants the decoder):

- `SseDecoder` — in `http-ng-proto`. Byte-at-a-time, a BOM state machine that
  survives the BOM being split across chunks; no knowledge of HTTP; **a
  mandatory size limit on the raw event**, and exceeding it is fatal and not
  retried (rmcp's requirement: `DEFAULT_MAX_SSE_EVENT_SIZE = 16 MiB`, applied
  "*at the raw byte layer, before SSE parsing*").
- `SseStream` — in `http-ng`. Reconnect, `Last-Event-ID`, backoff with jitter.

Events are an enum, `{ Message, Comment, Retry, Open }`: making comment
first-class gives you a keep-alive detector, making retry first-class fixes the
retry-only-block bug at the type level.

The normative WHATWG rules to honor: strip **one** BOM; three line terminators
(CRLF/LF/CR); `:` = comment; split on the first `:`, stripping one space; `id`
is ignored on NUL; `retry` only on pure ASCII digits; on an empty data buffer —
reset without dispatch, but the last-event-ID is **not** reset; trailing LF
trimmed; `Last-Event-ID` sent only if non-empty; fail on status ≠ 200 or
Content-Type ≠ `text/event-stream`; **204 = stop forever**; 301/307 — follow.

We build on top of an ordinary `Client`, not the browser's `EventSource` — that
one can't do headers, POST, or auth.

---

## 5. Transport tier (Tier B)

`Send` isn't declared anywhere; it shows up in three places, and all three are
requirements from someone else's code: hyper (`B::Data: Send`, `Upgraded`,
`Sleep: Send + Sync`), quinn (`Runtime: Send + Sync + 'static`), hickory
(`RuntimeProvider: Send + Sync`).

### 5.1 `http-ng-rt` — separate capabilities, not one Runtime

```rust
// The Spawn shape is deliberately copied from hyper::rt::Executor: generic over the future,
// zero bounds in the trait — Send comes from the impl, not the declaration.
pub trait Spawn<F: Future<Output = ()>> { fn spawn(&self, f: F); }
impl<F: Future<Output=()> + Send + 'static> Spawn<F> for Tokio {}
impl<F: Future<Output=()> + 'static>        Spawn<F> for TokioLocal {}

// NOTE: `Timer` is defined ONCE — in `http-ng-core` (D8), because the core
// needs it for timeouts and backoff. `http-ng-rt` depends on `http-ng-core` and
// only re-exports it alongside its own capabilities. There are not two Timers.
pub use http_ng_core::Timer;
// pub trait Timer {
//     fn sleep(&self, d: Duration) -> impl Future<Output = ()>;  // not Pin<Box<dyn Sleep>>
//     fn now(&self) -> Self::Instant;                            // not std::time::Instant
// }

pub trait TcpConnect {
    type Stream: hyper::rt::Read + hyper::rt::Write + Unpin;
    async fn connect(&self, addr: SocketAddr, opts: &TcpOpts) -> io::Result<Self::Stream>;
}

/// A separate trait, not a method: it doesn't exist on wasm, and that should be
/// a compile error, not an unimplemented!() at runtime.
pub trait Blocking { async fn run<T>(&self, f: impl FnOnce() -> T) -> T; }

pub trait TcpAdoptStd: TcpConnect {          // fd platforms only
    fn adopt(&self, std: std::net::TcpStream) -> io::Result<Self::Stream>;
}
```

Our own `Timer`, not `hyper::rt::Timer`: theirs has `Sleep: Send + Sync`
**unconditionally**, `sleep()` returns `Pin<Box<dyn Sleep>>` (an allocation on
every sleep), and `now()` is typed to `std::time::Instant`, which **panics** on
`wasm32-unknown-unknown`. `impl hyper::rt::Timer for Tokio` lives in
`http-ng-rt-tokio` and doesn't leak anywhere else.

`TcpAdoptStd` exists because the whole set of socket options (nodelay,
keepalive+interval+retries, send/recv buffer size, local_address,
local_addresses(v4,v6), connect_timeout, happy_eyeballs_timeout, reuse_address,
`SO_BINDTODEVICE`, `TCP_USER_TIMEOUT`) gets applied on a `socket2::Socket` —
that's the cleanest seam on fd platforms. The options live in http-ng exactly
once; the runtime just adopts the descriptor.

**Ours to write:** a bridge from `futures_io::{AsyncRead,AsyncWrite}` to
`hyper::rt::{Read,Write}`. hyper-util only has `TokioIo`; `smol-hyper` 0.1.1 has
been dead since 2023-12-29 **and** implements the direction backwards. ~200
lines; without them, no smol/compio backend exists.

**The h2 trap:** as of hyper 1.8.0 (2025-11-11, marked breaking in the
CHANGELOG), `Http2ClientConnExec` requires `Clone` and "the executor must be
able to spawn itself," the trait is sealed, `H2ClientFuture` sits in a private
`mod proto`. The only way to satisfy it is a blanket `impl<F: Future<Output=()>
+ 'static> Executor<F>`.

**Convenient for v0.1:** the h1 handshake needs neither an executor nor a timer
— `Connection` is polled inline via `select`. The first client will run on a
bare `futures` executor with zero ability to spawn.

### 5.2 TLS

The adapter is written **directly against `hyper::rt::Read/Write`**, not against
futures-io or tokio-io. Consequence: per-runtime TLS glue simply doesn't exist —
one adapter for every runtime.

```rust
pub trait TlsConnect {
    type Stream<S>: hyper::rt::Read + hyper::rt::Write + Unpin;
    async fn connect<S>(&self, io: S, req: TlsRequest<'_>) -> Result<(Self::Stream<S>, TlsInfo)>;
}

pub struct TlsRequest<'a> {
    pub server_name: ServerName<'a>,
    pub alpn: &'a [&'a [u8]],                     // per connection, not per config
    pub ech: Option<EchConfigListBytes<'a>>,      // from day one
}
```

- **ALPN per connection**: version pinning and h2 prior-knowledge require
  different ALPN sets for different connections to the same origin. Inside
  `http-ng-tls-rustls`: an `Arc<ClientConfig>` cache keyed by ALPN set.
- **`ech` from day one, even unimplemented**: ECH is **RFC 9849** (Proposed
  Standard, March 2026), `EchConfigList` comes from an HTTPS/SVCB record. Had we
  pinned the resolver and the TLS request to `SocketAddr`, ECH would be closed
  off forever without a breaking change.
- **`TlsInfo` — every method is `Option`**: native-tls only gives you the leaf
  certificate, ALPN, and tls-server-end-point.

We build on the surface that's been stable since rustls 0.20: `process_new_packets`
+ `wants_read`/`wants_write` + poll wrappers around `read_tls`/`write_tls`.
**Not** on `unbuffered` — that's already been removed on rustls main (PR #2905,
2026-02-06). Pin `rustls = "0.23"` (currently 0.23.43, 2026-07-29); rustls is
**not in the public API**. Expected in 0.24: the `std` feature removed,
providers split out into `rustls-ring`/`rustls-aws-lc-rs`, MSRV 1.85, edition
2024 — one rewritten crate is budgeted for.

Trust: `rustls-platform-verifier` 0.7 (`new_with_extra_roots`) by default,
`webpki-roots` 1.0.9 for wasm/wasi, `rustls-native-certs` 0.8.4 optionally. Plus
an escape hatch — "bring your own `ClientConfig`" — which gives ECH, keylog,
FIPS, Graviola/SymCrypt/OpenSSL for free.

### 5.3 DNS

```rust
pub trait Resolve {
    fn lookup_ipv4(&self, name: &Name) -> impl Stream<Item = Result<ResolvedAddr>>;
    fn lookup_ipv6(&self, name: &Name) -> impl Stream<Item = Result<ResolvedAddr>>;

    /// The default returns empty — so getaddrinfo, wasi and embedded
    /// satisfy the trait trivially.
    fn lookup_svcb(&self, _: &Name) -> impl Stream<Item = Result<SvcbEndpoint>> { empty() }
}
pub struct ResolvedAddr { pub addr: IpAddr, pub ttl: Option<Duration> }
pub struct SvcbEndpoint {
    pub priority: u16, pub target: Name, pub alpn: Vec<Vec<u8>>, pub port: Option<u16>,
    pub ipv4hint: Vec<Ipv4Addr>, pub ipv6hint: Vec<Ipv6Addr>,
    pub ech_config_list: Option<Bytes>,
}
```

Separate **streams**, not a `Vec<SocketAddr>`: RFC 8305 says you should start
connecting on AAAA without waiting for A.

- **The system resolver requires `Blocking`** — `getaddrinfo` is blocking
  everywhere. **Two slots, not one**: curl 8.20 runs v4 and v6 on separate
  threads so partial results can kick off Happy Eyeballs sooner.
- **`getaddrinfo` will never return HTTPS/SVCB** → the system path structurally
  can't give you ECH or h3 discovery. The only escape hatches are
  platform-specific: Apple's `DNSServiceQueryRecord`, Android's
  `DnsResolver.rawQuery` (API 29+).
- **hickory is honestly tokio-only.** `RuntimeProvider` moved to a new crate,
  `hickory-net` 0.26.1 (2026-05-01, MSRV 1.88); encrypted DNS is locked to
  `__tls = ["dep:rustls", "dep:tokio-rustls", "tokio"]`; issue #3304, "Support
  smol runtime in the resolver," has been open since 2025-10-10 with no
  assignee. The only non-tokio `RuntimeProvider` in the wild is
  `cyper-hickory` 0.1.0, and it's forced to implement TLS/HTTPS/H3 itself.
  `LookupIpStrategy::Ipv6AndIpv4` is **mandatory** — the default
  `Ipv4thenIpv6` is sequential and IPv4-first, which contradicts RFC 8305 §3.
- **Runtime-neutral encrypted DNS is only DoH on top of http-ng itself**, on
  `hickory-proto` as a pure codec (it's `#![no_std]` + alloc, with CI on
  `aarch64-unknown-none`). Needs a bootstrap resolver and a cycle guard.

### 5.4 Happy Eyeballs — we write it ourselves, RFC 8305

Nothing off the shelf: `happy-eyeballs` 0.2.1 has been dead since 2023-05;
`happyeyeballs` declares itself non-RFC-compliant; hyper-util implements
**RFC 6555** (two family groups + a single `sleep(300ms)`, at most two attempts
in parallel, sequential within a group) behind a sealed trait.

Ours: interleaving with First Address Family Count = 1, Resolution Delay 50ms,
Connection Attempt Delay 250ms (clamped 10ms…2s), on `FuturesUnordered` +
`select`, **no spawn** (spawn would require `Send + 'static`). The three
constants are public config. The scheduler is a pure state machine in
`http-ng-proto` that takes `now` as a parameter: the constants get tested
without a single `sleep`.

An own RFC 6724 Destination Address Selection was assumed here to probably be
needed ("no maintained crate exists") — Task 11 (vertical 2,
`http-ng-native::connect`) checked before implementing and closed the
question: we don't do it, see §9.

### 5.5 The pool

First we try `hyper_util::client::pool` (feature `client-pool`, 0.1.19 from
2025-12-03, ~1860 lines): `cache`/`map`/`negotiate`/`singleton` layers, where
`negotiate` is ALPN h2-with-fallback-to-h1, battle-tested by reqwest.

The blocker is a one-liner: the `client` feature pulls `tokio/net`
unconditionally → the wasm build fails. `client-pool` itself is `["client",
"dep:futures-util", "dep:tower-layer", "tokio/sync"]`, with no `net` or `rt`.
**File the upstream PR immediately.**

Needs finishing regardless:

- **Idle eviction.** `cache.rs` literally has `// todo: on_idle`; the only API
  is `Cache::retain(..)`. Design `Spawn` and `Timer` as **`Option`**: without
  them, lazy eviction on checkout (the single-threaded WASM mode).
- **Draining an unread body with a deadline.** rmcp disables the pool entirely
  (`pool_max_idle_per_host(0)`) because of "*~40 ms stalls caused by TCP
  Delayed ACK on Linux when the previous response body was not fully consumed
  before the pool attempts to reuse the connection*," and separately does
  `tokio::time::timeout(50ms, ..)` to finish reading the tail of an SSE
  stream. Not an optimization: a real consumer turns the pool off because of
  this.
- **H2 stream accounting** ([hyper#3623](https://github.com/hyperium/hyper/issues/3623),
  open since 2024-04-05): `SendRequest` is unbounded, `poll_ready` is always
  Ready → a connection with `MAX_CONCURRENT_STREAMS` exhausted is counted as
  healthy.
- The pool's types are deliberately unnameable (`pub` only under
  `#[cfg(docsrs)]`) → you can't hold a `Cache` in your own field without
  boxing it.

h1 eviction after an upgrade is already correct: `Drop for Pooled` checks
`is_open()` **before** inserting.

The pool key is `(scheme, authority, protocol)`.

### 5.6 h1/h2/h3 negotiation inside one transport

```
candidates(origin):
  svcb = resolver.lookup_svcb(host)              // RFC 9460
  if svcb.alpn ∋ "h3"                  → H3(ipv4hint/ipv6hint, svcb.port)
  if altsvc_cache is fresh and not broken → H3(...)               // RFC 7838, with ma=
  always                                → TCP(A/AAAA via HE, ALPN=[h2, http/1.1])

selection:
  H3 candidate exists → QUIC handshake races TCP with a delay, take the first success
  otherwise           → TCP only

on h3 failure:
  altsvc_cache.mark_broken(origin)     // 30s → 60s → 120s, exponential
  continue over TCP; don't surface the error to the user
```

The broken backoff is mandatory: UDP/443 is blocked on ~2–5% of networks.
Without it, the client keeps trying h3 forever and pays a timeout on every
request (Chrome's model).

**Consequence for DNS:** `getaddrinfo` won't return SVCB, so with the system
resolver the first request is always TCP, and h3 only kicks in from the second
request on via `Alt-Svc`. h3-from-the-first-packet requires `-dns-hickory` or
`-doh` — write that in the docs, and it bumps hickory's priority up to v0.2.

Management and observability ("may not know" ≠ "cannot know"):

```rust
.http3(Http3::Auto)            // default: SVCB + Alt-Svc + race + fallback
.http3(Http3::Disabled)
.http3(Http3::PriorKnowledge)  // straight to QUIC, no fallback
.version_pin(Version::HTTP_11) // part of the pool key and the ALPN offer, NOT a request field
```

`Response::version()` always tells the truth; the `on_connect` hook hands back
the negotiated protocol, the resolved address, and the ALPN.

**Why the version pin can't be a request field:** ALPN overrides it —
`reqwest::RequestBuilder::version()` **doesn't work** for exactly this reason
([reqwest#2116](https://github.com/seanmonstar/reqwest/issues/2116), open),
and `reqwest-websocket` keeps a dedicated error for it with the comment "*this
could be the case because reqwest silently upgraded the connection to http2*".

### 5.7 Upgrade and WebSocket-over-h2

hyper 1.11 **already supports** client-side RFC 8441 extended CONNECT, and
nobody uses it: `reqwest-websocket` 0.6.0 returns `UnsupportedHttpVersion` on
`Version::HTTP_2`, and the one h2-WS client that exists has 59 downloads. The
mechanics: `proto/h2/client.rs:731` carries `hyper::ext::Protocol` into the h2
extensions; h2 0.4.15 keeps `:scheme`/`:path` when protocol is non-empty;
`ResponseFutMap::poll`, on a status of **exactly 200**, puts `OnUpgrade` into
the extensions.

Three traps, which is why we need our own h2 connection wrapper:

1. Neither hyper nor h2 **check `SETTINGS_ENABLE_CONNECT_PROTOCOL`** before
   sending `:protocol` → a stream error (a violation of RFC 8441 §3). The
   `is_extended_connect_protocol_enabled()` flag lives only on `Connection`,
   and hyper-util spawns it into a task and loses the flag. It needs to be
   threaded through into the pool entry.
2. **The tunnel's lifetime is tied to the pool**: hyper-util drops `Pooled`
   right after the headers; a live tunnel is kept alive by a `SendRequest`
   clone in the pool; `pool_idle_timeout` (default 90s) or eviction tears it
   down.
3. ALPN silently breaks h1 upgrade (see §5.6).

The public shape of the seam:

```rust
pub struct Upgraded<S> { pub io: S, pub read_buf: Bytes, pub version: Version }
```

Not `hyper::upgrade::Upgraded`: that one holds a `Rewind<Box<dyn Io + Send>>`,
and `with_upgrades()` requires `T: Read + Write + Unpin + Send + 'static` —
`!Send` IO simply can't be represented through it, and it would leak hyper
into every downstream crate. The `!Send` path: `Connection::without_shutdown()
-> Parts<T> { io, read_buf }`, bounded only by `T: Read + Write + Unpin`. The
catch: on an upgrade, a plain `Connection` returns `Poll::Ready(Ok(()))`, **not
an error** — detect 101 by status. `Parts` is `#[non_exhaustive]`.

**The public WS/WT API is message-oriented:**

```rust
pub trait WebSocket: Stream<Item = Result<Message>> + Sink<Message> {}
```

Not a matter of taste: in the browser `WebSocket` is a separate global,
unreachable through fetch; on Apple it's `NSURLSessionWebSocketTask`,
**message-framed**. Had we exposed `impl Read + Write`, wasm and iOS would
become impossible. Raw duplex stays a native-only detail inside `http-ng-ws`.
Framing: `async-tungstenite` 0.35.0 (2026-07-28, on `futures_io`,
runtime-neutral); handshake verification:
`tungstenite::handshake::client::{generate_key, derive_accept_key}`.

**We don't write WebTransport:** `h3-webtransport` 0.1.2 is **server-only**
(`src/` only has `lib.rs`/`server.rs`/`stream.rs`); the working clients
(`wtransport` 0.7.1, `web-transport-quinn` 0.11.12) carry their own HTTP/3 and
are nailed to `quinn/runtime-tokio`. We delegate to `web-transport` 0.10.9,
which cfg-switches native/wasm on its own.

---

## 6. HTTP/3

hyper gives us **nothing** here: no `client::conn::http3`, no `hyper::rt::quic`,
no h3/quinn in Cargo.toml; the roadmap (last statement 2024-12-10) lists three
items that never started. h3 is a separate stack: `h3` + `h3-quinn` + `quinn`.

**The good news:** quinn 0.11.11 (2026-06-22) ships three in-tree runtimes
(Tokio, Smol, AsyncStd); the smol impl is ~25 lines plus ~60 lines of shared
wrapper, because all the platform hell (GSO/GRO/ECN/recvmmsg/DF) lives in the
runtime-independent `quinn-udp`. **We don't grow `http-ng-rt` to cover UDP** —
`http-ng-h3` takes a `quinn::Runtime` directly.

**The bad news (why this is v0.3–v0.4):**

- `h3` 0.0.8 was released **2025-05-06**; 13 commits to master in 15 months, 3
  merged PRs in all of 2026. What we need sits unreleased on master: RFC 9220
  `:protocol` (#236), connect-ip (#273), the CONNECT fix (#322), 0-RTT (#323).
  A git dependency means we can't publish to crates.io.
- quinn **0.12.0** on main breaks exactly the traits our adapters would be
  written against: `UdpPoller` → `UdpSender`, `create_io_poller(Arc<Self>)` →
  `create_sender(&self)`, `try_send` removed, `poll_recv` now takes `&mut
  self`, `wrap_udp_socket` returns a `Box`, `runtime-async-std` removed.
  `h3-quinn` 0.0.10 requires quinn `^0.11.7`.

**Limitations we write into the docs up front:**

1. HTTP/3 **will never be `!Send`**: `quinn::Runtime: Send + Sync + Debug +
   'static`, `spawn(Pin<Box<dyn Future + Send>>)`, `quinn-proto` is std-only.
2. **Pluggable TLS doesn't extend to h3**: the only implementation of
   `quinn_proto::crypto::Session` is rustls. The statement: "HTTP/1.1 and
   HTTP/2 — native-tls / rustls / others; HTTP/3 — rustls only".
3. WebTransport won't run on top of our h3 (§5.7).

**Pins:** `h3 = "=0.0.8"` (exact: a caret gives no `0.0.x` compatibility
guarantee); `quinn-proto` — the current 0.11.16 already covers the floor of
RUSTSEC-2026-0037 (CVSS 8.7, `>= 0.11.14`); `h3::quic::*` and quinn's types are
**not in the public API**.

### 6.1 Swapping the QUIC engine — documented emergency exits

quinn's `Send` isn't in the QUIC code — it's entirely in the async wrapper.
Below that layer everything is already sans-io (measured):

| crate | dependencies |
|---|---|
| `quinn-proto` 0.11.16 | bytes, fastbloom, lru-slab, rand, ring, rustc-hash, rustls — **neither tokio nor async** |
| `quinn-udp` 0.6.1 | libc, socket2, tracing — **no runtime** |
| `quinn` 0.11.11 | this is where `Runtime: Send + Sync + 'static` lives |

Size of the wrapper: `wc -l quinn-0.11.11/src` = 5347, of which `tests.rs` is
1111 → **~4200 lines**; client-only ≈ 3000–3500. The hidden cost: we'd have to
**grow `http-ng-rt` to cover UDP**.

The alternative engine is `quiche` 0.29.3: **zero `async fn` and zero
`tokio::`** in `src/`, and it has its own `src/h3/` with qpack → the
frozen-`h3` problem disappears entirely. The price: a dependency on `boring`
4.22 (BoringSSL) — a C toolchain, cmake, a second TLS implementation in the
graph.

Upstream isn't discussing a `!Send` quinn: searching quinn-rs/quinn issue
titles for "Send" gives 98 hits, all about sending data.

**Triggers for reconsidering:**

| trigger | move | cost |
|---|---|---|
| compio-with-h3 is genuinely needed | drive `quinn-proto` ourselves, keeping `quinn-udp` | ~3000 lines + UDP in the public runtime contract |
| `h3` stays frozen, need WS-over-h3 / connect-ip | switch to `quiche` | BoringSSL in the build, a second TLS stack |

The swap is possible **in a patch release**, because the engine is an
implementation detail of a single crate (D15).

---

## 7. Ambient backends (Tier C)

### 7.1 `http-ng-wasi` — absorbing `wasi-fetch`

`wasi-fetch` 0.2.0 (our crate, `github.com/actcore/wasi-fetch`) — 571 lines,
dependencies `http`, `wasip3 0.7.0`, `futures`, `serde`, `http-body`, `bytes`,
`url`; **neither tokio nor reqwest**. It's already `http-ng-wasi` plus a bit
of facade.

| `wasi-fetch` today | in http-ng |
|---|---|
| `Client` + `get/post/put/delete/patch/head/query/request` | `http_ng::Client<T>` |
| `RequestBuilder::{header, headers, body, json}` | `http_ng::RequestBuilder` |
| `timeout` (sets connect **and** first_byte) + `between_bytes_timeout` | `Timeouts` (§4.5), per-request via `Extensions` |
| `redirect_limit` + a ~60-line loop | the `Redirect` stage |
| `send_raw`: http↔wasi conversion, `Fields`, `BodyWriter`, `join!`, `to_wasi_method` | **stays** → `impl Transport for WasiHttp` |
| `Body::{Incoming, Buffered, Done}` | `Incoming` → `WasiHttp::Body`; `chunk/bytes/text/json` → `http_ng::Response` methods |
| `Error::{Url, Transport(String), Utf8, Json}` | `http_ng::Error` with `ErrorKind` (§4.7) |
| seven `let _ =` on setters | `Capabilities` (§4.6) |

What's left of `http-ng-wasi` is ≈ **250–300 lines out of 571**.

We keep this as-is: `Body::chunk()` skips trailer frames; `poll_frame` returns
them. The convenience layer loses fidelity; the full one doesn't.

**p3-only in v0.1.** wasmtime 46+ supports WASI 0.3 (ratified 2026-06-11);
`act-cli` already runs on p3; `wasip3::http_compat` gives `impl
http_body::Body for IncomingBody` (`Data = Bytes`, `Error = ErrorCode`) for
free. The umbrella `wasi` crate is still at 0.14.7+wasi-0.2.4 — can't be used.
The build targets `wasm32-wasip2` (MSRV 1.90); `wasm32-wasip3` is Tier 3. p2
becomes a separate arm later, if a consumer shows up for it.

**Fate of the name:** `wasi-fetch` 0.3 becomes a thin facade (~40 lines) over
`http_ng::Client<WasiHttp>` with the old names — the crate stays findable, and
old users migrate with one line.

**`wasi:http` 0.3.0 limitations that need to show up in `Capabilities`:** all
per-request configuration is three timeouts, each of which can return
`not-supported`; `request.get-options` hands back an **immutable** handle
(options are fixed at `request.new`); **no notion of redirect at all** (the
host may or may not follow one, and you have no way to know); **no
upgrade/CONNECT** (only the `HTTP-upgrade-failed` error code) → WebSocket over
wasi:http is impossible; no TLS/proxy/version/cookie/pool. On the other hand,
**full duplex and trailers in both directions** — richer than native.

### 7.2 `http-ng-fetch`

Not deferred, because fetch is the **only** backend where capabilities differ
at runtime, and is therefore the only test of decision D5.

What fetch physically can't do (this is exactly the content of `Capabilities`):

- **Streaming request body — Chromium only**: BCD `api.Request.duplex` —
  chrome/edge/webview 131 (2024-11-12), Firefox `false`
  ([bugzil.la/1792434](https://bugzil.la/1792434)), Safari/iOS `false`
  ([webkit.org/b/245671](https://webkit.org/b/245671)). Even in Chrome:
  rejected over HTTP/1.x, `duplex:"half"` is mandatory, any redirect other
  than 303 kills the request, always preflighted, `no-cors` is forbidden.
- **web-sys 0.3.103 has no `set_duplex`/`set_keepalive`/`set_priority`** →
  only via `js_sys::Reflect::set`; `wasm-streams` 0.6.0 for Rust Stream →
  ReadableStream.
- ~25 forbidden headers (Host, Connection, Content-Length, Cookie, Origin,
  Transfer-Encoding, TE, Upgrade, `Proxy-*`, `Sec-*`…).
- No trailers in either direction
  ([whatwg/fetch#772](https://github.com/whatwg/fetch/issues/772) proposes
  removing the API); no 1xx; no choosing or observing the HTTP version; no
  TLS config, cert pinning, client certs, proxy; cookies are ambient; no pool.
- Timeout only via `AbortSignal` — one deadline for everything.
- `redirect: manual` gives you `opaqueredirect` with status 0, no headers, no
  body.
- No upgrade → **WebSocket is unreachable through fetch** (a separate global).
- `keepalive: true` — a 64 KiB ceiling, incompatible with ReadableStream.

The fallback when duplex is unavailable is buffering the body, **documented
and switchable off**, not silent.

### 7.3 Later

`http-ng-espidf` (v0.4): `esp-idf-svc` 0.52.1 only gives you a **blocking**
`EspHttpConnection` (zero `async` in `src/http/client.rs`); the wrapper is a
blocking C API on a dedicated FreeRTOS task plus a channel. `esp_http_client`
handles HTTP/1.1+2 via ALPN, mbedTLS, redirects, chunked, Basic+Digest; no h3,
no WebSocket. **hyper isn't needed here at all.**

`http-ng-nyquest` (v0.4): native mobile stacks. The motive for URLSession on
iOS isn't speed — it's App Transport Security, the system trust store, MDM
CAs, per-app VPN, the system proxy/PAC, background transfer. Precedents:
Mozilla's `viaduct` (buffered `Vec<u8>`, config =
`{timeout, redirect_limit, ohttp_channel, user_agent}`; their iOS runs on
hyper anyway), `frakt` 0.1.0 (a push-based `mpsc::Receiver<Bytes>` for
response bodies — NSURLSession/Cronet hand back bytes in delegates, which
can't be polled). There's no dedicated Rust crate for Cronet/OkHttp;
`objc2-foundation` 0.3.2 covers NSURLSession entirely.

---

## 8. Cross-cutting principle: sans-io

**The rule.** Every piece of logic we write ourselves is shaped as a pure state
machine: no I/O, no `async`, no runtime, no clock. Anything time-dependent
takes `now` as a parameter. The async wrapper is a separate thin type that
makes no decisions.

The rule is enforced by the **dependency graph**, not discipline: `http-ng-proto`
has neither `tokio`, `futures-*`, nor `async-*` in its graph.

This is already our de facto pattern: the rustls adapter against `hyper::rt`
instead of `tokio-rustls`; DoH on `hickory-proto` instead of
`hickory-resolver`; `quinn-proto` + `quinn-udp` as the substrate (which is
exactly what makes swapping the engine possible); `SseDecoder` kept separate
from `SseStream` (which rmcp confirmed the need for).

**Where sans-io stops:** hyper's h1 is typed against `hyper::rt::Read/Write`;
`h2` 0.4.15 is io-coupled to `tokio::io` + `tokio-util::codec`, and a rewrite
there isn't on the table (0 issues). So the declaration reads:

> All the protocol code that http-ng writes itself is sans-io. Where we depend
> on someone else's, we take their sans-io core (rustls, quinn-proto,
> hickory-proto), and write the io wrapper ourselves, thin. The only
> non-sans-io dependencies are hyper and h2, and that's a deliberate price
> paid for mature h1/h2.

**CI check:** `cargo tree` for `http-ng-proto` fails the moment an async
dependency shows up; `grep -rn "async fn" http-ng-proto/src` → empty;
`SseDecoder` and the Alt-Svc parser go into `cargo-fuzz` from day one (both
parse untrusted input, and both are exactly the class of code where other
crates have broken).

**A side benefit:** `http-ng-embedded` on top of reqwless as a separate
product (§9, embassy) keeps that option open for free — "pure logic" is now a
real crate.

---

## 9. What we deliberately don't do

| Not doing | Evidence |
|---|---|
| **embassy / bare-metal no_std** | `http` 1.5.0: `#[cfg(not(feature = "std"))] compile_error!`. Issue #551 has been open since 2022-05-10, PR #740 since 2025-01-02 with no maintainer response, and even it only gets you no_std **+ alloc**. Plus `embedded-nal-async::TcpConnect` borrows the stack via a GAT (incompatible with the pool), `embedded-tls` 0.19 gives no ALPN → h2 is impossible |
| **TLS/H2 fingerprinting (JA3/JA4/Akamai)** | rustls #1932 → duplicate #2498 → closed as *not planned*. `wreq` 6.0.0-rc.29 threw out hyper entirely for this (switched to a fork of h2 + BoringSSL). Plus `http::HeaderMap` normalizes names to lowercase, so browser header casing can't be reproduced. Compensation: the public `Transport` |
| **RustCrypto as a TLS backend** | `rustls-rustcrypto` has been stuck at 0.0.2-alpha since 2024-04-24, its README says "DO NOT USE THIS IN PRODUCTION," and it requires std |
| **An abstraction over QUIC backends** | Exactly one is viable. s2n-quic hard-pulls s2n-quic-platform/tokio-runtime and has no published h3 bridge; quiche goes through tokio-quiche; neqo isn't on crates.io and requires NSS |
| **async-std** | Discontinued 2025-03-01; quinn 0.12 removes `runtime-async-std` |
| **Request body compression** | Server support is inconsistent; provide a clean manual path instead |
| **`hyper::upgrade::Upgraded` in the public API** | Leaks hyper into every downstream crate |
| **RFC 6724 Destination Address Selection (v0.2)** | §5.4 called it probably needed, with no crate available. Task 11 (vertical 2, `http-ng-native::connect`) checked before implementing: the full rule requires Source Address Selection (Rule 1 onward) — knowing which local address the OS would actually connect out from to a given destination, i.e. a routing table, which none of this vertical's traits (`Resolve`, `TcpConnect`, `Timer`) provide. A partial implementation (the rules alone, without Source Address Selection) would look like RFC 6724 compliance without being it — the same principle that split `RedirectSupport::None`/`Transparent` apart. Today, addresses of each family go into `Scheduler::offer_v4`/`offer_v6` in the order the resolver hands them over (`http-ng-dns::Resolve`, `http-ng-native::connect` — both documented). Open to revisiting if a dedicated Source Address Selection capability shows up |
| **Connection-level middleware on ambient backends** | Physically impossible |
| **A blocking API** | Out of scope by the problem statement |
| **HTTP/3 as a 1.0 blocker** | reqwest has kept it behind `--cfg reqwest_unstable` for two years |

### 9.1 tokio in the graph

Measured with `cargo tree -e normal` for an external consumer:

| build | tokio |
|---|---|
| ambient-only (`http-ng` + `-fetch`/`-wasi`) | **none at all** |
| hyper, HTTP/1 only | present, but with the `sync` + `default` features; its whole dep tree is `pin-project-lite`. No mio, no libc, no socket2, no tokio-macros |
| hyper + HTTP/2 | real: `h2` pulls `tokio` with `io-util`+`bytes` and `tokio-util` with `codec` → and **`libc`**, plus `tracing`, `indexmap`, `slab`, `fnv`, `once_cell` |

There are exactly three uses in hyper's source on the h1 path:
`tokio::sync::oneshot` (`upgrade.rs`), `tokio::sync::{mpsc, oneshot}`
(`client/dispatch.rs`), `tokio::pin!` (`common/task.rs`). The `Compat` bridge
to `tokio::io` is already gated behind `#[cfg(feature = "http2")]`.

Upstream isn't going to fix this:
[hyper#3428](https://github.com/hyperium/hyper/pull/3428) (exactly this swap
for futures-channel) was rejected — "*As of 1.0, we are going to be very
careful about adding new dependencies to the public API… it "exposes" a crate
feature that we could never remove*";
[hyper#3767](https://github.com/hyperium/hyper/issues/3767) closed as *not
planned*.

**Decision:** accept it and document it precisely with the table above in the
README, with links as justification.

### 9.2 On `Send` for the native transport

We don't declare it (D14), but in practice:

| configuration | Send? | who declared it |
|---|---|---|
| default (tokio + h3) | yes | quinn |
| tokio, `http3` off | yes, via auto-traits | nobody, inferred |
| smol, `http3` off | yes, via auto-traits | nobody |
| compio, h1/h2 | honestly no | nobody |

Our guarantee is an assert in tests, not a bound:

```rust
#[test] fn default_stack_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Native<Tokio, Rustls, SystemDns>>();
    assert_send::<Client<Native<Tokio, Rustls, SystemDns>>>();
}
```

Reversible: the runtime traits are in the `unversioned` quarantine, so
breaking changes there ship in a minor version.

---

## 10. Version plan

### v0.1 — architecture proven (not a product)

| Claim | Proof |
|---|---|
| The runtime seam is real | h1 on **tokio and smol**, both in CI, zero `#[cfg]` in the shared code |
| The delegation seam is real | `wasi:http` p3 — no socket exists there at all |
| The capability model degrades honestly | **fetch** is the only backend with runtime differences (Chrome/Safari duplex) |
| The `Transport` shape was guessed correctly | `components/http-client` from `act` builds against http-ng **with no logic changes** |

Contents: `http-ng-proto`, `http-ng-core`, `http-ng`, `http-ng-rt`, `-rt-tokio`,
`-rt-smol`, `http-ng-native` (**h1 only**, trivial pool), `http-ng-tls` +
`-tls-rustls`, `http-ng-dns` + `-dns-system`, `http-ng-wasi` (p3),
`http-ng-fetch`, `wasi-fetch` 0.3 as a facade.

Plus: our own SSE decoder, the `futures_io → hyper::rt` shim, a `Negotiate`
module with one arm, a pool key that already includes the protocol, two fuzz
targets.

**The `Redirect` stage is part of v0.1** (pulled forward from v0.2): otherwise
migrating `components/http-client` would be a regression. It ships with three
fixes over `wasi-fetch`'s current loop right away: don't follow **304/305**;
strip `Authorization`/`Cookie` on a host **or** scheme change; downgrade
301/302 POST→GET the same as 303.

Why h1-only: the h1 handshake needs neither an executor nor a timer — the
client will run on a bare `futures` executor with zero ability to spawn.

**Verification tasks (currently unverified):**

1. Whether the resource handles in the `wasip3` 0.7.0 bindings are `Send`.
2. Whether `cargo tree` for `http-ng` + `http-ng-fetch` really contains no
   tokio.
3. Whether the one-line PR to hyper-util lands (`tokio/net` → moved into
   `client-legacy`). **File it immediately** — ~2000 lines in v0.2 depend on
   the answer.

### v0.2 — the product becomes a product

h2 via ALPN, with the executor and Timer as **builder typestate**
(`Client::builder()` gives you h1-only, `.executor(e)` unlocks `.http2()`) —
that way `keep_alive_interval` without a timer is impossible at the type
level, instead of panicking out of hyper with "You must supply a timer."

The pool (draining, idle eviction, `Spawn`/`Timer` as `Option`). `AltSvcCache`
gets written and tested here, even though h3 doesn't exist yet. Decompression,
cookies (async `CookieStore` with `&self`), retry with a typed replayable
body. Middleware + `http-ng-tower`. `http-ng-dns-hickory` → SVCB.
`http-ng-tls-native`. Multipart, proxy, base URL.

`http-ng-rmcp` — the second verification loop.

### v0.3 — what nobody else has

WebSocket with a unified upgrade seam for H1 + h2 extended CONNECT (ownership
of the h2 connection wrapper — **decide this before the pool architecture is
frozen**). An h3 arm behind `feature = "http3"`, **off** by default: race,
fallback, broken backoff, SVCB-first-flight. `http-ng-dns-doh`. ECH via SVCB.
A compio backend. Event hooks and connection observability.

### v0.4 — `http3` becomes default

Once `h3` and quinn 0.12 settle down. Plus `http-ng-espidf`, `http-ng-nyquest`,
the WebTransport hook in `web-transport` 0.10.9.

### Conditions for 1.0

Plugin traits validated against **≥3 backends** (native/wasi/fetch) and **≥3
runtimes**; the `unversioned` quarantine is documented; `http-ng-rmcp` and
`act` are in production; not a single foreign type remains in the public API.

---

## 11. Risks

| # | Risk | Mitigation |
|---|---|---|
| 1 | **Scope.** The requirements list is bigger than any of our dead predecessors managed | v0.1 proves four claims and **does nothing else** |
| 2 | **`h3` frozen / a quinn 0.12 break** | `http3` default-off until v0.4; two documented emergency exits (§6.1) |
| 3 | **rustls 0.24 — a guaranteed rewrite** | We build on the surface stable since 0.20; rustls not in the public API; one rewritten crate is budgeted |
| 4 | **The hyper-util pool**: unnameable types, `// todo: on_idle`, the PR might not land | File the PR immediately; gate the pool design on the response; fallback path ~2000 lines |
| 5 | **The graveyard of runtime neutrality** | Every runtime is a leaf crate with its **own CI job**; **never** prop up smol via `async-compat` (it silently spins up a second runtime); the formula: "tokio first-class, smol/compio in CI, the surface is 4 traits" |
| 6 | **Extended CONNECT traps** | Decide in v0.2, before the pool is frozen |
| 7 | **Foreign types leaking into the API** (`Upgraded`, `h3::quic::*`, rustls, quinn) | A `cargo public-api` CI check that fails the moment a foreign type appears |

**Criteria for stopping and reconsidering:**

- v0.1 doesn't build under smol **without** `#[cfg]` in the shared code → the
  runtime seam is decorative; fix `http-ng-rt`, don't move forward.
- `Capabilities` grows past ~25 fields by v0.2 → the model is wrong, go back
  to discussing typestate.
- `http-ng-rmcp` or `act` required changes in `http-ng-core` → the `Transport`
  shape was guessed wrong; better to find that out at v0.2 than after 1.0.
- `http-ng-proto` needed an `async fn` → the layer boundary was drawn wrong.

---

## 12. Verification loop: `act`

`act` becomes the first consumer **on both sides of the boundary**, which
makes it the best test of the `Transport` shape:

- **Host:** `act-cli/src/runtime/http_client.rs` (872 lines) — implements the
  `wasi:http` outgoing handler on top of reqwest. Moves to `impl Transport` +
  policy as a **`Resolve` decorator** and **`Middleware`**. What that fixes:
  - `tokio::time::timeout(connect_timeout + first_byte_timeout, ..)` → the
    timeout triple;
  - `error_chain_contains(&err, &["deny cidr", "failed to lookup", "dns"])` →
    a typed `ErrorKind::Resolve`;
  - lost request trailers (`reqwest::Body::wrap` requires `Send + Sync`, and
    `UnsyncBoxBody` is `!Sync` → they go through `wrap_stream`, where
    trailers get dropped) → no conversion needed at all;
  - the `StreamBody::is_end_stream()` bug, always `false`, which causes
    "*wasi-fetch guests trap mid-read on HTTP/2 responses*" → a shared
    implementation with the guest side;
  - one `reqwest::Client` per component call (because policy gets baked into
    the constructor) → one shared pool + per-request policy via
    `Extensions`;
  - "*the redirect callback is sync and can't prompt… Per-hop
    ask-prompting is a later phase*" → `layer_inner` is async and gets
    called on every hop;
  - the resolver returns `Box<dyn Iterator<Item=SocketAddr>>` with no TTL and
    no SVCB → streams + `ResolvedAddr` + `lookup_svcb`.
- **Guest:** `wasi-fetch` → `http-ng-wasi` (§7.1); `components/http-client`
  builds with no logic changes and gets a native and a browser build for
  free.

`act_policy::net::decide` is already a pure function with no I/O — it fits the
layout in §8 with no changes needed.

---

## 13. Fixed technical decisions

- `default = []` in every crate. Backends are **separate crates, not
  features**.
- Private join features prefixed `__` (reqwest's pattern), `?/` feature
  propagation, `[lints.rust] unexpected_cfgs.check-cfg`.
- `#![cfg_attr(docsrs, feature(doc_cfg))]` + `[package.metadata.docs.rs]
  all-features = true, rustdoc-args = ["--cfg","docsrs"]` —
  `#[doc(auto_cfg)]` is still unstable on 1.97.1.
- **MSRV: 1.85 for the core, 1.88 for `-dns-hickory` and `-h3`**, 1.90 for
  `-wasi` (per-crate MSRV in the workspace). Floors: quinn 1.85, hickory 1.88,
  wasip3 1.90.
- Plugin traits (`Transport`, `Resolve`, `TlsConnect`, the runtime traits) go
  into an `unversioned` module with an explicit policy (ureq's pattern):
  breaking changes there ship in **minor**, not major. Without this, 1.0
  can't ship.
- CI matrix: `{tokio, smol} × {linux, macos, windows}` +
  `wasm32-unknown-unknown` + `wasm32-wasip2`, plus `Send` assert tests and
  `cargo public-api`.

---

## 14. Next actions

1. PR to hyper-util: move `tokio/net` out of the `client` feature and into
   `client-legacy`.
2. Spike: whether `wasip3` 0.7.0 is `Send`; whether a skeleton builds for
   `wasm32-wasip2`.
3. Freeze `http-ng-core` (`Transport`, `Capabilities`, `RequestBody`,
   `Error`) — on paper, against three backends, before writing code.
4. Workspace skeleton + the CI matrix from §13.

---

## Design amendments

Every exception to the invariant "we never declare `Send`/`Sync`" in the code
must cite one of this section's amendments by an ASCII token —
`amendment-C1`, `amendment-C2`, `amendment-C3`, `amendment-C4` or
`amendment-C5` — in a `send-bound-exception: amendment-CN` comment. CI
(`no-declared-send`) checks exceptions against exactly these tokens, so the
citation and its justification turn up in the same search.

### C1. `Error` requires `Send + Sync` from its source

**What turned out to be wrong.** §4.7 and decision D6 claimed that
"auto-trait transparency reaches errors too": `Error` holds an `Arc<dyn Error
+ 'static>` with no `Send`, so an error from a `!Send` transport supposedly
still works, while one from a `Send` transport stays `Send`. The second half
is wrong. Erasure into a `dyn Trait` **never** lets auto-traits through unless
the trait object itself is bounded. Verified by compiling: even with a ZST
source, trivially `Send + Sync`, the wrapper is not `Send`:

```
error[E0277]: `(dyn std::error::Error + 'static)` cannot be sent between threads safely
   = note: required for `Arc<(dyn std::error::Error + 'static)>` to implement `Send`
```

The consequence was load-bearing: `Client::execute` wraps the transport error
in `Error`, so the future `client.get(u).send()` turned out to be `!Send`
**always**, and `tokio::spawn` wouldn't compile for any transport at all. My
spike didn't catch this, because it used a concrete `MockError` as
`Transport::Error` and never once went through the core's erased error.

**Fix.** The source gets bounded: `Arc<dyn Error + Send + Sync + 'static>`,
and `Error::new<E: Error + Send + Sync + 'static>`.

**Why this doesn't dilute D6.** Empirically, from this same session: all three
v0.1 backends have `Send` errors. `JsValue` and the `web_sys` types are
`Send` without `target_feature = "atomics"` (§4.3, measured), `wasip3`'s
resource handles are `Send` (measured), hyper/quinn/hickory are `Send` by
declaration. So the requirement isn't a restriction — it's a statement of
fact.

**The refined statement of the invariant**, replacing "we never declare
Send":

> The seam traits — `Transport`, `Timer`, middleware — don't declare
> `Send`/`Sync`. The one exception: `http_ng_core::Error` requires `Send +
> Sync` from its source, and `Client::execute` carries `T::Error: Send +
> Sync + 'static` in its where clause — not in the trait. A transport with a
> `!Send` error remains representable; it just can't make use of wrapping
> into `Error`.

**The price, said out loud.** A wasm build with `+atomics` (wasm threads),
where `JsValue` is honestly `!Send`, loses the path through `Client`. That's
the same configuration §4.2 already prescribes `.boxed_local()` for, and it
stays out of scope for v0.1.

### C2. `RequestBody` also has to bound its trait objects

The same class of bug as C1, and found before implementation — by a compile
check, not a review. §4.4 specified:

```rust
Rewindable(Arc<dyn Fn() -> BodyStream>),
Streaming(BodyStream),                    // Box<dyn Body + Unpin>
```

Both trait objects have no `Send`, which means `RequestBody` is `!Send`,
which means `http::Request<RequestBody>` is `!Send`, which means the
`Transport::execute` future is `!Send`. Fixing C1 alone wouldn't have saved
us: spawning would still be impossible, just for a different reason.

**Fix.**

```rust
Rewindable(Arc<dyn Fn() -> RequestBody + Send + Sync>),
Streaming(Box<dyn http_body::Body<Data = Bytes, Error = Error> + Unpin + Send>),
```

`Sync` is only needed on the `Arc`: `Arc<T>: Send` requires `T: Send + Sync`,
because `Arc` is shared; `Box<T>: Send` requires only `T: Send`. Verified by
compiling: with these bounds, `RequestBody: Send` and
`http::Request<RequestBody>: Send`, and `Sync` on `RequestBody` is neither
achieved nor needed — the request moves into `execute` by value.

**A general takeaway worth keeping in mind through the rest of v0.1.**
Whenever a trait object lands in a type on the `Client -> Transport` path,
the auto-traits on it get cut off. Before adding any `dyn` to this path: a
compile-time `assert_send` check, not reasoning about it.

### C3. Convention: `Send`/`Sync` assertions live in `tests/`

The `no-declared-send` check only scans `crates/*/src`, so an ordinary `fn
assert_send_sync<T: Send + Sync>() {}` inside `src` breaks the check with its
own text. The workaround via `impl Send` in argument position works and
proves exactly the same thing (verified by mutation: adding an `Rc<()>` field
breaks compilation both ways), but it needs six lines of comment explaining
why the test is written oddly.

**Convention, from Task 9 onward:** such assertions get written in
`crates/<crate>/tests/`, in the ordinary generic form. The grep doesn't see
them there; they sit right at the public API boundary — exactly where a real
consumer would be looking at the type — and the exception list keeps its
meaning of "a justified exception in production code," not "an awkward
test."

### C4. `&'static [HeaderName]` can't be filled from a static slice

A trap found before anyone fell into it. The field
`Capabilities::forbidden_request_headers` is declared as `&'static
[HeaderName]`, and on `Capabilities::none()` it's `&[]` — an empty literal,
no promoted temporaries involved. But filling it in isn't trivial: on stable,
with `http` 1.5

```rust
static FORBIDDEN: &[HeaderName] = &[http::header::HOST /* ... */];
```

gives `E0492: interior mutable shared borrows of temporaries` — for **any**
header, including `host` and `content-length`. rustc's promotion check works
by type, and the `HeaderName` type contains a `Custom` variant on top of
`Bytes` with an `AtomicPtr`, regardless of which variant is actually live.

What works: a single `static X: HeaderName = ...`, a `const` array, or
`Box::leak(vec![..].into_boxed_slice())` / `OnceLock` for the slice. This
concerns `http-ng-fetch`, where the forbidden-header list runs to about
twenty-five entries. The implementer should verify the shape before writing
the list, not after.

### C5. `http-ng-rt::Blocking` declares `Send` right in the capability trait

A different class of case than C1/C2: there, the bound arose not in the
trait declaration but at the point of erasure (`dyn Error`, `dyn Fn`, `dyn
Body`) on the `Client -> Transport` path, and that's exactly why it was
unexpected — §4.7 promised auto-trait transparency, and erasure into a trait
object breaks it. Here there's no surprise: `Blocking` is a runtime
capability trait (`http-ng-rt`, vertical 2), not a core seam trait, and
`Send` in its signature is a deliberate design decision, not a discovery
made after the fact.

**Why the bound is mandatory, not optional.** `Blocking::run` is a bridge to
a blocking thread pool: `getaddrinfo`, file I/O, any operation that can't be
polled without blocking the executor. The only two backends that implement
this capability at all set `Send + 'static` on the input not by http-ng's
choice, but by their own API's contract:

- `tokio::task::spawn_blocking<F, R>(f: F) -> JoinHandle<R> where F:
  FnOnce() -> R + Send + 'static, R: Send + 'static`;
- `blocking::unblock<T, F>(f: F) -> Task<T> where F: FnOnce() -> T + Send +
  'static, T: Send + 'static` (the `blocking` crate, which `smol::unblock`
  is built on).

Both runtimes hand the closure off to another thread and wait for the result
back — precisely the definition `Send` exists for. This bound can't be
declared any weaker: it isn't http-ng's choice, it's a condition without
which the runtime primitive itself doesn't compile for either of the two
backends vertical 2 is obligated to support with no `#[cfg]` in shared code
(the vertical's Global Constraints).

**Why this doesn't infect the portable core.** The `Blocking` capability
simply doesn't exist on wasm — there's no thread pool there to hand a
blocking call off to, and that absence shows up as a compile error (no trait
impl), not as `unimplemented!()` at runtime (see the trait's doc comment: "a
separate trait, not a method"). So there's nothing for the bound to infect
beyond `http-ng-rt` and its native backends (`http-ng-rt-tokio`,
`http-ng-rt-smol`) — the portable core (`http-ng-core`, `http-ng-proto`) and
the wasm transport (`http-ng-wasi`) don't depend on `http-ng-rt` and never
see this trait. `Spawn`, `TcpConnect`, `TcpAdoptStd` and `Timer` in this same
crate don't declare `Send` — only `Blocking` does, and only because the
source of the bound is someone else's API, not ours.

**How this differs from C1/C2 in marker mechanics.** Both `no-declared-send`
and this rule require the `send-bound-exception: amendment-CN` marker **on
the same line** where the bound is declared — per-line scope, not per-file
(see the justification in the CI job itself). In `Blocking::run`, both
bounds (`T: Send + 'static` and `F: FnOnce() -> T + Send + 'static`) are
pulled out into their own `where` clause, rather than the method's generic
parameter list, precisely so each gets its own line and its own marker — a
single comment after `fn run<T: Send + …>(…)` would only have covered the
last line of the declaration, not both.

### C6. Completeness of a `#[non_exhaustive]` type is only checkable inside the crate that defines it

Not about exceptions to the `Send`/`Sync` invariant (that class is closed by
C1–C5) — about a separate, previously unstated fact about
`#[non_exhaustive]`, found by Task 13 of vertical 2 (review fix round 1)
while trying to prove that a test on `Capabilities` outside `http-ng-core`
can do the same thing `Capabilities::none_is_the_conservative_base` does
inside it.

**Found by measurement, not by reasoning.**
`Capabilities::none_is_the_conservative_base` (`http-ng-core/src/caps.rs`)
destructures `Capabilities` WITHOUT `..`: a new field added to the struct
and not mentioned in the test is a compile error naming the field. Task 13's
review built exactly this scenario: it added a seventeenth field to
`Capabilities` — the `http-ng-core` test failed to compile, exactly as
intended. The same technique, written in
`http-ng-native/tests/transport.rs` (a consumer crate, not the crate that
owns the type), is forced to bring in `..` — `#[non_exhaustive]` requires it
for any destructuring outside the defining crate (`E0638` without it) — and
that `..` silently swallows the new field: the test from `http-ng-native`,
on that same seventeenth field, compiled and stayed green, never noticing
it.

**The rule.** A check that "type A's structure hasn't changed without
explicit acknowledgment," over a `#[non_exhaustive]` type (destructuring
without `..`, which only compiles on an exact match of the field set), can
only live inside the crate that defines that type — the place where
`#[non_exhaustive]` doesn't apply to the crate's own code. A test on the
same type from outside can verify that the fields it LISTS have the
expected values (a valuable check, but a different one), and must name and
document itself accordingly — not as a completeness check over the field
set, which it structurally cannot be.

**The consequence for `Capabilities` specifically.** The only completeness
check is `Capabilities::none_is_the_conservative_base` in `http-ng-core`.
Any future backend crate (`http-ng-native`, `http-ng-wasi`, `http-ng-fetch`,
and beyond) that needs to confirm it hasn't turned on a capability by
mistake writes a test of the form "the listed fields' values are today's
conservative defaults" (see
`http-ng-native/tests/transport.rs::undeclared_capability_fields_match_their_conservative_defaults_today`),
not a test of the form "this is all the fields there are" — it can't write
that one.

**Difference from C3.** C3 is about WHERE the assert lives (`tests/`, not
`src`, because `no-declared-send` only scans `src`). C6 is about WHAT an
assert over a `#[non_exhaustive]` type is even capable of checking,
regardless of which file it's written in: even in `tests/` outside the
defining crate, destructuring without `..` doesn't compile, so completeness
is structurally unprovable there, not merely "inconvenient" to write. Don't
reuse one token for both — a past mistake in Task 13's review (citing
`amendment-C3` for this very rule) was found and fixed precisely because
Task 13's implementer checked the citation against the spec text before
writing it into the code, rather than inheriting it as-is.

### C7. `http-ng-fetch` carries the project's one `unsafe impl`, and `deny` — not `forbid` — is correct there

Not about the `Send`/`Sync` invariant either (that class is still closed by
C1–C5, and this amendment's token is never cited as a `send-bound-exception`
marker — see below). About the separate, narrower invariant "no crate writes
`unsafe` code," which vertical 2 enforces at two layers: `#![forbid(unsafe_code)]`
in every crate's `src/lib.rs`, and the `no-unsafe-code` CI job as a backstop
for the case where that line itself goes missing (see the job's own comment
in `.github/workflows/ci.yml`, "REVISED in Task 6, fix round 1").

**Exactly one exception exists, and it is load-bearing, not incidental.**
`wasm_bindgen_futures::JsFuture` (as resolved into this workspace: a
re-export of `js_sys::futures::JsFuture`) holds an `Rc<RefCell<..>>`
internally and is therefore `!Send` — an implementation choice, not a
platform property, since `JsValue`, `js_sys::Promise` and the `web_sys`
types used here are all `Send` on the default target. Without a `Send`
replacement, `Client::execute`'s future would be `!Send` in the browser and
C1's whole argument (`Client -> Transport` stays spawnable) would not hold
there. `http-ng-fetch/src/promise.rs` supplies that replacement:
`SendJsFuture`, built on a `js_sys::Promise` callback pair plus a hand-rolled
waker, wrapping its two `wasm_bindgen::Closure` values in

```rust
#[repr(transparent)]
pub(crate) struct SingleThreaded<T>(pub(crate) T);

#[allow(unsafe_code, reason = "…")]
#[cfg(not(target_feature = "atomics"))]
unsafe impl<T> Send for SingleThreaded<T> {}
```

**Why this mirrors wasm-bindgen's own reasoning, not a new risk.**
`wasm-bindgen` itself declares, for the exact same reason and under the
exact same `cfg`:

```rust
// wasm-bindgen-0.2.126/src/lib.rs:173-176
#[cfg(not(target_feature = "atomics"))]
unsafe impl Send for JsValue {}
#[cfg(not(target_feature = "atomics"))]
unsafe impl Sync for JsValue {}
```

Without `target_feature = "atomics"` (wasm threads), a wasm module has one
linear memory, one instance, and no threads by construction — there is
nothing for a `Send` bound to protect against, and `JsValue`'s own
`PhantomData<*mut u8>` marker (there purely to opt the type out of the
auto-trait, not because it holds a real pointer with thread-unsafe access
patterns) is deliberately overridden on that basis. `SingleThreaded<T>`
makes the identical argument about the two `Closure` values it wraps: they
are `!Send` only because their internals route through the same kind of
non-atomic JS-table index, and only a single-instance, single-thread module
ever touches that index. `#[cfg(not(target_feature = "atomics"))]` on the
`unsafe impl` is what makes the claim true rather than assumed — with
`+atomics`, the `cfg` strips the impl and the compiler is left to enforce
`!Send` on its own.

**Verified, not asserted, in both directions.** `cargo check -p
http-ng-fetch --target wasm32-unknown-unknown --tests` passes without
`target_feature = "atomics"`. With it:

```
RUSTFLAGS="-Ctarget-feature=+atomics,+bulk-memory" cargo +nightly check \
  -p http-ng-fetch --target wasm32-unknown-unknown --tests \
  -Zbuild-std=std,panic_abort
```

fails with `` `(dyn FnMut(JsValue) + 'static)` cannot be sent between
threads safely `` (this wasm-bindgen version routes a closure's `!Send`ness
through `Box<dyn FnMut>` inside `ScopedClosure`, not through a bare `*mut
u8`, so the exact type named in the diagnostic differs from older
wasm-bindgen releases — the conclusion, "the compiler rejects it," is what
carries the weight, not the literal wording of the error).

**`--tests` is not optional in that command, and its absence is a
false-negative trap.** `cargo check -p http-ng-fetch --target
wasm32-unknown-unknown` (lib target only, no `--tests`) succeeds under
`+atomics` too — nothing in the library itself demands `SendJsFuture:
Send`; only `tests/promise.rs`'s `assert_send::<..SendJsFutureAlias>()`
does. A verification command that drops `--tests` looks like it re-confirms
the safety argument and actually confirms nothing at all. Anyone extending
this check (a future MSRV bump, a CI job, a new spike) must run it with
`--tests` or the check is theater.

**Why `deny`, not `forbid`, is correct here — and only here.** Every other
crate in the workspace inherits `[lints] workspace = true`, which sets
`unsafe_code = "forbid"`; `forbid` cannot be relaxed by a local `#[allow]`
from inside the crate (`E0453`), which is exactly the point of using it
everywhere the crate has no legitimate `unsafe`. `http-ng-fetch` is the one
crate that does, so it opts out of `[lints] workspace = true` entirely and
declares its own `[lints.rust]` with `unsafe_code = "deny"` — restating
`missing_debug_implementations = "warn"` and `unexpected_cfgs = { level =
"warn", check-cfg = [] }` explicitly in the same table, since opting out of
the workspace table drops those two along with `unsafe_code` and losing them
silently would be exactly the kind of drift this project spends review
effort on elsewhere (see the `no-unsafe-code` job's own "HISTORY" comment on
list-based coverage rotting). Verified by probe: the same crate keeping
`[lints] workspace = true` plus a local `#[allow(unsafe_code)]` next to the
`unsafe impl` fails with two `E0453`s and does not build at all; dropping
the inherited table and declaring `#![deny(unsafe_code)]` in `src/lib.rs`
instead (the crate-level doc comment on `SendJsFuture`'s module explains
this) lets the scoped `#[allow]` do its job. `forbid` remains correct in
every other crate — this crate is the sole, deliberate exception, and the
exception is why C1's "one table, no threads" premise needed restating
rather than silently assumed.

**A separate marker from `send-bound-exception`, on the same mechanical
principle.** `no-declared-send` accepts `send-bound-exception:
amendment-CN` for `C1`, `C2`, `C3`, `C5` or `C6` — that's the Send/Sync
invariant. `no-unsafe-code` is a different job over a different invariant
("no crate writes `unsafe`"), so it gets its own token,
`unsafe-code-exception: amendment-C7`, rather than overloading
`send-bound-exception` for an unrelated check. Per-line, not per-file, for
the same reason C5 gives: the job's grep would otherwise treat the whole
file as exempt, hiding any *unrelated* `unsafe` block added to
`promise.rs` later. Both lines the job would flag —
`#[allow(unsafe_code, ...)]`'s `unsafe_code,` and the `unsafe impl` line
itself — carry the marker; `#[cfg(...)]` and the doc comments around them
don't contain the literal text `unsafe` in a way the job's comment filter
doesn't already drop. The job accepts `amendment-C7` and nothing else: a
marker citing any other token (a typo, a made-up amendment, or a real but
unrelated one like `C1`) is left unmatched and still fails the check —
verified by both a positive probe (the real marked lines pass) and two
negative ones (an unmarked `unsafe` anywhere in any crate still fails; a
marker citing a nonexistent amendment, e.g. `amendment-C9`, still fails).

**Two corrections to the task brief that this crate's implementer is
recorded as having found, not inherited.** First, `pub(crate) struct
SendJsFuture` (as literally specified) cannot be re-exported through
`#[doc(hidden)] pub mod testing { pub use crate::promise::SendJsFuture as
SendJsFutureAlias; }` — `tests/promise.rs` is compiled as a separate,
external crate, and `E0365` (private type re-exported from a public module)
follows immediately. The type is `pub` instead, with the `promise` module
itself staying private (`mod promise;`, not `pub mod promise;`) — the type
has no path to it from outside the crate except through the `testing`
re-export, so it is still not part of the crate's advertised API despite
the `pub` keyword. Second, the brief's citation for `JsFuture`'s
`Rc<RefCell<..>>` — `js-sys-0.3.103/src/futures/mod.rs:118` — names the
right crate: as of the versions this workspace actually resolves
(`wasm-bindgen-futures` 0.4.76, `js-sys` 0.3.103, confirmed via `cargo
tree -p http-ng-fetch -e normal` and by reading both crates' vendored
sources), `wasm-bindgen-futures` is a thin re-export shim over
`js_sys::futures`, and `JsFuture` is defined in `js-sys` itself — a
refactor from older releases (`wasm-bindgen-futures` 0.4.42 still defines
`JsFuture` directly, at its own `src/lib.rs:101-102`), not a mistake in the
brief's crate attribution. The one-line-off part was the line number: `118`
is `pub struct JsFuture<T = JsValue> {`, and the `Rc<RefCell<Inner<T>>>`
field itself is `119`.
