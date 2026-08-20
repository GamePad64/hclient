# hclient — design

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
| D12 | Split `hclient-core` (plugin contract) / `hclient` (user-facing surface) | Only this way can `hclient` depend on `hclient-hyper`, which depends on the contract, without a cycle. Gives `Client<T = DefaultTransport>` |
| D13 | **h1/h2/h3 inside one native transport**, negotiation transparent | The user shouldn't have to know they're on h3. Composing `AltSvc<H2,H3>` would make the version choice visible in the type |
| D14 | Native transport **does not declare `Send`**; CI asserts stand in for a bound | `Send` in a trait is contagious: `TcpConnect::Stream: Send` would force compio to repeat cyper's `SendWrapper` hack, which panics on a cross-thread drop |
| D15 | The QUIC engine is an **implementation detail**, swappable in a patch release | `h3::quic::*` and quinn types aren't in the public API (§9.2) |
| D16 | **sans-io** as a binding rule, enforced by the dependency graph | §8 |
| D17 | **`wasi-fetch` gets absorbed** and becomes `hclient-wasi`; **p3-only** in v0.1 | It's our crate, 571 lines, already works; §7.1 |

---

## 3. Architecture

Three tiers, not one ladder. A tier is defined by what's physically available.

```
Tier A — portable. No hyper, no tokio, no sockets.
┌──────────────────────────────────────────────────────────────────┐
│ hclient-proto   pure state machines: SSE decoder, Alt-Svc,       │
│                 redirect/retry/cookie logic, Happy Eyeballs      │
│                 scheduler, multipart, URL. Zero async.           │
│ hclient-core    Transport, Capabilities, RequestBody, Error,     │
│                 **Timer**. ~500 lines. `unversioned` quarantine. │
│ hclient         Client<T = DefaultTransport>, builder, stages,   │
│                 SSE stream, sugar.                               │
└──────────────────────────────────────────────────────────────────┘
        ▲                     ▲                        ▲
Tier B — socket tier          │        Tier C — ambient (depend
(hyper, Send de facto)        │        only on Tier A)
┌──────────────────────────┐  │        ┌─────────────────────────┐
│ hclient-rt   Spawn,      │  │        │ hclient-wasi  (p3)      │
│   Timer, TcpConnect,     │  │        │ hclient-fetch           │
│   TcpAdoptStd, Blocking, │  │        │ hclient-espidf   (v0.4) │
│   + FuturesIo shim       │  │        │ hclient-nyquest  (v0.4) │
│ hclient-rt-{tokio,smol,  │  │        └─────────────────────────┘
│              compio}     │  │
│ hclient-tls  +-rustls    │  │        On the side:
│              +-native    │  │        hclient-tower   adapter
│ hclient-dns  +-system    │  │        hclient-ws      message API
│              +-hickory   │  │        hclient-wt      hook
│              +-doh       │  │        hclient-rmcp    adapter
│ hclient-native  h1/h2/h3 │──┘        wasi-fetch      facade compat
│ hclient-h3   (engine)    │
└──────────────────────────┘
```

**Invariant:** `hclient` does not depend on hyper. `hclient-h3` is an engine inside
`hclient-native`, not a user-facing `Transport`.

---

## 4. Core (Tier A)

### 4.1 `Transport`

The shape is taken not from hyper, but from `wasi:http/client.send` — the poorest
of the ambient APIs. Anything richer degrades to it cleanly; the reverse doesn't
hold.

```rust
// hclient_core::unversioned
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

- `hclient-fetch` will be `Send` on the default browser target;
- `!Send` remains only for builds with wasm threads — a deliberate opt-in;
- **no cfg appears in our own code**; the only `#[cfg(not(target_feature =
  "atomics"))] unsafe impl Send` is on a single type in `hclient-fetch` that
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
pub type DefaultTransport = hclient_native::Native<Tokio, Rustls, SystemDns>;
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub type DefaultTransport = hclient_fetch::Fetch;
#[cfg(target_os = "wasi")]
pub type DefaultTransport = hclient_wasi::WasiHttp;
```

Three levels of user:

```rust
// 1. Just a client — zero generics, one piece of code on three targets
let text = hclient::Client::new().get(url).send().await?.text().await?;

// 2. Configuration — still zero generics
let client = hclient::Client::builder()
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

- `SseDecoder` — in `hclient-proto`. Byte-at-a-time, a BOM state machine that
  survives the BOM being split across chunks; no knowledge of HTTP; **a
  mandatory size limit on the raw event**, and exceeding it is fatal and not
  retried (rmcp's requirement: `DEFAULT_MAX_SSE_EVENT_SIZE = 16 MiB`, applied
  "*at the raw byte layer, before SSE parsing*").
- `SseStream` — in `hclient`. Reconnect, `Last-Event-ID`, backoff with jitter.

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

### 5.1 `hclient-rt` — separate capabilities, not one Runtime

```rust
// The Spawn shape is deliberately copied from hyper::rt::Executor: generic over the future,
// zero bounds in the trait — Send comes from the impl, not the declaration.
pub trait Spawn<F: Future<Output = ()>> { fn spawn(&self, f: F); }
impl<F: Future<Output=()> + Send + 'static> Spawn<F> for Tokio {}
impl<F: Future<Output=()> + 'static>        Spawn<F> for TokioLocal {}

// NOTE: `Timer` is defined ONCE — in `hclient-core` (D8), because the core
// needs it for timeouts and backoff. `hclient-rt` depends on `hclient-core` and
// only re-exports it alongside its own capabilities. There are not two Timers.
pub use hclient_core::Timer;
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
`hclient-rt-tokio` and doesn't leak anywhere else.

`TcpAdoptStd` exists because the whole set of socket options (nodelay,
keepalive+interval+retries, send/recv buffer size, local_address,
local_addresses(v4,v6), connect_timeout, happy_eyeballs_timeout, reuse_address,
`SO_BINDTODEVICE`, `TCP_USER_TIMEOUT`) gets applied on a `socket2::Socket` —
that's the cleanest seam on fd platforms. The options live in hclient exactly
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
  `hclient-tls-rustls`: an `Arc<ClientConfig>` cache keyed by ALPN set.
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
  **Superseded in vertical 3 — the first clause still holds, the conclusion
  does not.** `getaddrinfo` indeed cannot return an HTTPS record and never
  will, but "the system path" is not the same thing as "`getaddrinfo`":
  `res_query(3)` is in the same libc, answers RR type 65, and is neither
  Apple-specific nor Android-specific. `hclient-dns-system` uses it, parses
  the raw response itself, and reports `supports_svcb() == true` on Linux
  (glibc/musl) and Apple — see amendment C8, which also records why this
  costs the project its second `unsafe` file and what the remaining gap
  (Windows) is.
- **hickory is honestly tokio-only.** `RuntimeProvider` moved to a new crate,
  `hickory-net` 0.26.1 (2026-05-01, MSRV 1.88); encrypted DNS is locked to
  `__tls = ["dep:rustls", "dep:tokio-rustls", "tokio"]`; issue #3304, "Support
  smol runtime in the resolver," has been open since 2025-10-10 with no
  assignee. The only non-tokio `RuntimeProvider` in the wild is
  `cyper-hickory` 0.1.0, and it's forced to implement TLS/HTTPS/H3 itself.
  `LookupIpStrategy::Ipv6AndIpv4` is **mandatory** — the default
  `Ipv4thenIpv6` is sequential and IPv4-first, which contradicts RFC 8305 §3.
- **Runtime-neutral encrypted DNS is only DoH on top of hclient itself**, on
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
`hclient-proto` that takes `now` as a parameter: the constants get tested
without a single `sleep`.

An own RFC 6724 Destination Address Selection was assumed here to probably be
needed ("no maintained crate exists") — Task 11 (vertical 2,
`hclient-native::connect`) checked before implementing and closed the
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
**No longer true wherever `supports_svcb()` answers `true`** (vertical 3,
amendment C8): the system resolver reaches SVCB through `res_query(3)` on Unix
and `DnsQueryRaw` on Windows 11 / Server 2025, not through `getaddrinfo`, so
h3-from-the-first-packet is available there without hickory or DoH. It remains
true on older Windows and on every other target — and a caller must ask that
method rather than assume, which is exactly what it is for.

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
become impossible. Raw duplex stays a native-only detail inside `hclient-ws`.
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
runtime-independent `quinn-udp`. **We don't grow `hclient-rt` to cover UDP** —
`hclient-h3` takes a `quinn::Runtime` directly.

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
**grow `hclient-rt` to cover UDP**.

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

### 7.1 `hclient-wasi` — absorbing `wasi-fetch`

`wasi-fetch` 0.2.0 (our crate, `github.com/actcore/wasi-fetch`) — 571 lines,
dependencies `http`, `wasip3 0.7.0`, `futures`, `serde`, `http-body`, `bytes`,
`url`; **neither tokio nor reqwest**. It's already `hclient-wasi` plus a bit
of facade.

| `wasi-fetch` today | in hclient |
|---|---|
| `Client` + `get/post/put/delete/patch/head/query/request` | `hclient::Client<T>` |
| `RequestBuilder::{header, headers, body, json}` | `hclient::RequestBuilder` |
| `timeout` (sets connect **and** first_byte) + `between_bytes_timeout` | `Timeouts` (§4.5), per-request via `Extensions` |
| `redirect_limit` + a ~60-line loop | the `Redirect` stage |
| `send_raw`: http↔wasi conversion, `Fields`, `BodyWriter`, `join!`, `to_wasi_method` | **stays** → `impl Transport for WasiHttp` |
| `Body::{Incoming, Buffered, Done}` | `Incoming` → `WasiHttp::Body`; `chunk/bytes/text/json` → `hclient::Response` methods |
| `Error::{Url, Transport(String), Utf8, Json}` | `hclient::Error` with `ErrorKind` (§4.7) |
| seven `let _ =` on setters | `Capabilities` (§4.6) |

What's left of `hclient-wasi` is ≈ **250–300 lines out of 571**.

We keep this as-is: `Body::chunk()` skips trailer frames; `poll_frame` returns
them. The convenience layer loses fidelity; the full one doesn't.

**p3-only in v0.1.** wasmtime 46+ supports WASI 0.3 (ratified 2026-06-11);
`act-cli` already runs on p3; `wasip3::http_compat` gives `impl
http_body::Body for IncomingBody` (`Data = Bytes`, `Error = ErrorCode`) for
free. The umbrella `wasi` crate is still at 0.14.7+wasi-0.2.4 — can't be used.
The build targets `wasm32-wasip2` (MSRV 1.90); `wasm32-wasip3` is Tier 3. p2
becomes a separate arm later, if a consumer shows up for it.

**Fate of the name:** `wasi-fetch` 0.3 becomes a thin facade (~40 lines) over
`hclient::Client<WasiHttp>` with the old names — the crate stays findable, and
old users migrate with one line.

**`wasi:http` 0.3.0 limitations that need to show up in `Capabilities`:** all
per-request configuration is three timeouts, each of which can return
`not-supported`; `request.get-options` hands back an **immutable** handle
(options are fixed at `request.new`); **no notion of redirect at all** (the
host may or may not follow one, and you have no way to know); **no
upgrade/CONNECT** (only the `HTTP-upgrade-failed` error code) → WebSocket over
wasi:http is impossible; no TLS/proxy/version/cookie/pool. On the other hand,
**full duplex and trailers in both directions** — richer than native.

### 7.2 `hclient-fetch`

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

`hclient-espidf` (v0.4): `esp-idf-svc` 0.52.1 only gives you a **blocking**
`EspHttpConnection` (zero `async` in `src/http/client.rs`); the wrapper is a
blocking C API on a dedicated FreeRTOS task plus a channel. `esp_http_client`
handles HTTP/1.1+2 via ALPN, mbedTLS, redirects, chunked, Basic+Digest; no h3,
no WebSocket. **hyper isn't needed here at all.**

`hclient-nyquest` (v0.4): native mobile stacks. The motive for URLSession on
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

The rule is enforced by the **dependency graph**, not discipline: `hclient-proto`
has neither `tokio`, `futures-*`, nor `async-*` in its graph.

This is already our de facto pattern: the rustls adapter against `hyper::rt`
instead of `tokio-rustls`; DoH on `hickory-proto` instead of
`hickory-resolver`; `quinn-proto` + `quinn-udp` as the substrate (which is
exactly what makes swapping the engine possible); `SseDecoder` kept separate
from `SseStream` (which rmcp confirmed the need for).

**Where sans-io stops:** hyper's h1 is typed against `hyper::rt::Read/Write`;
`h2` 0.4.15 is io-coupled to `tokio::io` + `tokio-util::codec`, and a rewrite
there isn't on the table (0 issues). So the declaration reads:

> All the protocol code that hclient writes itself is sans-io. Where we depend
> on someone else's, we take their sans-io core (rustls, quinn-proto,
> hickory-proto), and write the io wrapper ourselves, thin. The only
> non-sans-io dependencies are hyper and h2, and that's a deliberate price
> paid for mature h1/h2.

**CI check:** `cargo tree` for `hclient-proto` fails the moment an async
dependency shows up; `grep -rn "async fn" hclient-proto/src` → empty;
`SseDecoder` and the Alt-Svc parser go into `cargo-fuzz` from day one (both
parse untrusted input, and both are exactly the class of code where other
crates have broken).

**A side benefit:** `hclient-embedded` on top of reqwless as a separate
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
| **RFC 6724 Destination Address Selection (v0.2)** | §5.4 called it probably needed, with no crate available. Task 11 (vertical 2, `hclient-native::connect`) checked before implementing: the full rule requires Source Address Selection (Rule 1 onward) — knowing which local address the OS would actually connect out from to a given destination, i.e. a routing table, which none of this vertical's traits (`Resolve`, `TcpConnect`, `Timer`) provide. A partial implementation (the rules alone, without Source Address Selection) would look like RFC 6724 compliance without being it — the same principle that split `RedirectSupport::None`/`Transparent` apart. Today, addresses of each family go into `Scheduler::offer_v4`/`offer_v6` in the order the resolver hands them over (`hclient-dns::Resolve`, `hclient-native::connect` — both documented). Open to revisiting if a dedicated Source Address Selection capability shows up |
| **Connection-level middleware on ambient backends** | Physically impossible |
| **A blocking API** | Out of scope by the problem statement |
| **HTTP/3 as a 1.0 blocker** | reqwest has kept it behind `--cfg reqwest_unstable` for two years |

### 9.1 tokio in the graph

Measured with `cargo tree -e normal` for an external consumer:

| build | tokio |
|---|---|
| ambient-only (`hclient` + `-fetch`/`-wasi`) | **none at all** |
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
| The `Transport` shape was guessed correctly | `components/http-client` from `act` builds against hclient **with no logic changes** |

Contents: `hclient-proto`, `hclient-core`, `hclient`, `hclient-rt`, `-rt-tokio`,
`-rt-smol`, `hclient-native` (**h1 only**, trivial pool), `hclient-tls` +
`-tls-rustls`, `hclient-dns` + `-dns-system`, `hclient-wasi` (p3),
`hclient-fetch`, `wasi-fetch` 0.3 as a facade.

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
2. Whether `cargo tree` for `hclient` + `hclient-fetch` really contains no
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
body. Middleware + `hclient-tower`. `hclient-dns-hickory` → SVCB.
`hclient-tls-native`. Multipart, proxy, base URL.

`hclient-rmcp` — the second verification loop.

### v0.3 — what nobody else has

WebSocket with a unified upgrade seam for H1 + h2 extended CONNECT (ownership
of the h2 connection wrapper — **decide this before the pool architecture is
frozen**). An h3 arm behind `feature = "http3"`, **off** by default: race,
fallback, broken backoff, SVCB-first-flight. `hclient-dns-doh`. ECH via SVCB.
A compio backend. Event hooks and connection observability.

### v0.4 — `http3` becomes default

Once `h3` and quinn 0.12 settle down. Plus `hclient-espidf`, `hclient-nyquest`,
the WebTransport hook in `web-transport` 0.10.9.

### Conditions for 1.0

Plugin traits validated against **≥3 backends** (native/wasi/fetch) and **≥3
runtimes**; the `unversioned` quarantine is documented; `hclient-rmcp` and
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
  runtime seam is decorative; fix `hclient-rt`, don't move forward.
- `Capabilities` grows past ~25 fields by v0.2 → the model is wrong, go back
  to discussing typestate.
- `hclient-rmcp` or `act` required changes in `hclient-core` → the `Transport`
  shape was guessed wrong; better to find that out at v0.2 than after 1.0.
- `hclient-proto` needed an `async fn` → the layer boundary was drawn wrong.

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
- **Guest:** `wasi-fetch` → `hclient-wasi` (§7.1); `components/http-client`
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
3. Freeze `hclient-core` (`Transport`, `Capabilities`, `RequestBody`,
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

A second, separate family of amendments covers a different invariant — "no
crate writes `unsafe`" — under its own token, `unsafe-code-exception:
amendment-CN`, checked by its own CI job (`no-unsafe-code`). Those are **C7**
(`hclient-fetch/src/promise.rs`), **C8**
(`hclient-dns-system/src/sys/res_query.rs` and
`hclient-dns-system/src/sys/windows.rs`) and **C9**
(`hclient-idn/src/icu/windows.rs`), and the two families are never
interchangeable: each job matches only its own token, and each
`unsafe-code-exception` marker is additionally pinned to the file paths its
amendment names.

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
> `Send`/`Sync`. The one exception: `hclient_core::Error` requires `Send +
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
concerns `hclient-fetch`, where the forbidden-header list runs to about
twenty-five entries. The implementer should verify the shape before writing
the list, not after.

### C5. `hclient-rt::Blocking` declares `Send` right in the capability trait

A different class of case than C1/C2: there, the bound arose not in the
trait declaration but at the point of erasure (`dyn Error`, `dyn Fn`, `dyn
Body`) on the `Client -> Transport` path, and that's exactly why it was
unexpected — §4.7 promised auto-trait transparency, and erasure into a trait
object breaks it. Here there's no surprise: `Blocking` is a runtime
capability trait (`hclient-rt`, vertical 2), not a core seam trait, and
`Send` in its signature is a deliberate design decision, not a discovery
made after the fact.

**Why the bound is mandatory, not optional.** `Blocking::run` is a bridge to
a blocking thread pool: `getaddrinfo`, file I/O, any operation that can't be
polled without blocking the executor. The only two backends that implement
this capability at all set `Send + 'static` on the input not by hclient's
choice, but by their own API's contract:

- `tokio::task::spawn_blocking<F, R>(f: F) -> JoinHandle<R> where F:
  FnOnce() -> R + Send + 'static, R: Send + 'static`;
- `blocking::unblock<T, F>(f: F) -> Task<T> where F: FnOnce() -> T + Send +
  'static, T: Send + 'static` (the `blocking` crate, which `smol::unblock`
  is built on).

Both runtimes hand the closure off to another thread and wait for the result
back — precisely the definition `Send` exists for. This bound can't be
declared any weaker: it isn't hclient's choice, it's a condition without
which the runtime primitive itself doesn't compile for either of the two
backends vertical 2 is obligated to support with no `#[cfg]` in shared code
(the vertical's Global Constraints).

**Why this doesn't infect the portable core.** The `Blocking` capability
simply doesn't exist on wasm — there's no thread pool there to hand a
blocking call off to, and that absence shows up as a compile error (no trait
impl), not as `unimplemented!()` at runtime (see the trait's doc comment: "a
separate trait, not a method"). So there's nothing for the bound to infect
beyond `hclient-rt` and its native backends (`hclient-rt-tokio`,
`hclient-rt-smol`) — the portable core (`hclient-core`, `hclient-proto`) and
the wasm transport (`hclient-wasi`) don't depend on `hclient-rt` and never
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
while trying to prove that a test on `Capabilities` outside `hclient-core`
can do the same thing `Capabilities::none_is_the_conservative_base` does
inside it.

**Found by measurement, not by reasoning.**
`Capabilities::none_is_the_conservative_base` (`hclient-core/src/caps.rs`)
destructures `Capabilities` WITHOUT `..`: a new field added to the struct
and not mentioned in the test is a compile error naming the field. Task 13's
review built exactly this scenario: it added a seventeenth field to
`Capabilities` — the `hclient-core` test failed to compile, exactly as
intended. The same technique, written in
`hclient-native/tests/transport.rs` (a consumer crate, not the crate that
owns the type), is forced to bring in `..` — `#[non_exhaustive]` requires it
for any destructuring outside the defining crate (`E0638` without it) — and
that `..` silently swallows the new field: the test from `hclient-native`,
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
check is `Capabilities::none_is_the_conservative_base` in `hclient-core`.
Any future backend crate (`hclient-native`, `hclient-wasi`, `hclient-fetch`,
and beyond) that needs to confirm it hasn't turned on a capability by
mistake writes a test of the form "the listed fields' values are today's
conservative defaults" (see
`hclient-native/tests/transport.rs::undeclared_capability_fields_match_their_conservative_defaults_today`),
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

### C7. `hclient-fetch` carries the project's one `unsafe impl`, and `deny` — not `forbid` — is correct there

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
there. `hclient-fetch/src/promise.rs` supplies that replacement:
`SendJsFuture`, built on a `js_sys::Promise` callback pair plus a hand-rolled
waker, wrapping its two `wasm_bindgen::Closure` values in

```rust
#[repr(transparent)]
pub(crate) struct SingleThreaded<T>(pub(crate) T);

#[allow(unsafe_code, reason = "…")]
#[cfg(not(target_feature = "atomics"))]
unsafe impl<T> Send for SingleThreaded<T> {}
```

**Why this mirrors wasm-bindgen's own REASONING — not its shipped SCOPE, and
that distinction had to be corrected once (fix round 1, finding 1).** The
first draft of this amendment said the technique "mirrors what wasm-bindgen
applies to `JsValue` itself," true of the underlying argument but stated in
a way that implied more precedent than actually exists. Checked precisely,
not restated from memory:

*What IS shipped, unconditionally, in every released version this crate
depends on:*

```rust
// wasm-bindgen-0.2.126/src/lib.rs:173-176
#[cfg(not(target_feature = "atomics"))]
unsafe impl Send for JsValue {}
#[cfg(not(target_feature = "atomics"))]
unsafe impl Sync for JsValue {}
```

*What is NOT shipped anywhere:* no released `wasm-bindgen` gives
`Closure<T>` or `JsFuture` a `Send`/`Sync` impl under any `cfg`. The only
place upstream does this at all is an UNMERGED branch,
`unsafe-send-sync` on `wasm-bindgen/wasm-bindgen` (commits `a7e0c944`,
2025-10-30, and `0c0a8a8e`, 2025-10-31 — fetched from the real repository
and read directly for this correction, not taken on the review's word),
which adds exactly:

```rust
#[cfg(unsafe_single_threaded_traits)]
unsafe impl Send for JsValue {}                  // + Sync
#[cfg(unsafe_single_threaded_traits)]
unsafe impl<T: ?Sized> Send for Closure<T> {}     // + Sync
#[cfg(unsafe_single_threaded_traits)]
unsafe impl Send for JsFuture {}                  // + Sync
```

gated behind an explicit OPT-IN cfg, `unsafe_single_threaded_traits`
(`RUSTFLAGS="--cfg unsafe_single_threaded_traits"`) — deliberately **not**
automatic under `!atomics` the way the shipped `JsValue` impl is, and the
branch adds its own `compile_error!` if that cfg is combined with
`target_feature = "atomics"`. Upstream converged on the same underlying
argument this amendment makes (no atomics implies no threads implies one
table) without ever shipping it unconditionally for `Closure`/`JsFuture`
the way it does for `JsValue`.

Without `target_feature = "atomics"` (wasm threads), a wasm module has one
linear memory, one instance, and no threads by construction — there is
nothing for a `Send` bound to protect against, and `JsValue`'s own
`PhantomData<*mut u8>` marker (there purely to opt the type out of the
auto-trait, not because it holds a real pointer with thread-unsafe access
patterns) is deliberately overridden on that basis. `SingleThreaded<T>`
makes the identical argument about the two `Closure` values it wraps: they
are `!Send` only because their internals route through the same kind of
non-atomic JS-table index, and only a single-instance, single-thread module
ever touches that index — but this is a claim `hclient-fetch` is making
itself, on the same evidence upstream already accepted for `JsValue` and an
unmerged branch accepted for `Closure`/`JsFuture` too (under a stricter,
explicit opt-in), not something copied from a released, audited surface.
`#[cfg(not(target_feature = "atomics"))]` on the `unsafe impl` is what
makes the claim true rather than assumed — with `+atomics`, the `cfg`
strips the impl and the compiler is left to enforce `!Send` on its own.

**Verified, not asserted, in both directions.** `cargo check -p
hclient-fetch --target wasm32-unknown-unknown --tests` passes without
`target_feature = "atomics"`. With it:

```
RUSTFLAGS="-Ctarget-feature=+atomics,+bulk-memory" cargo +nightly check \
  -p hclient-fetch --target wasm32-unknown-unknown --tests \
  -Zbuild-std=std,panic_abort
```

fails with `` `(dyn FnMut(JsValue) + 'static)` cannot be sent between
threads safely `` (this wasm-bindgen version routes a closure's `!Send`ness
through `Box<dyn FnMut>` inside `ScopedClosure`, not through a bare `*mut
u8`, so the exact type named in the diagnostic differs from older
wasm-bindgen releases — the conclusion, "the compiler rejects it," is what
carries the weight, not the literal wording of the error).

**`--tests` is not optional in that command, and its absence is a
false-negative trap.** `cargo check -p hclient-fetch --target
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
everywhere the crate has no legitimate `unsafe`. `hclient-fetch` is the one
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
tree -p hclient-fetch -e normal` and by reading both crates' vendored
sources), `wasm-bindgen-futures` is a thin re-export shim over
`js_sys::futures`, and `JsFuture` is defined in `js-sys` itself — a
refactor from older releases (`wasm-bindgen-futures` 0.4.42 still defines
`JsFuture` directly, at its own `src/lib.rs:101-102`), not a mistake in the
brief's crate attribution. The one-line-off part was the line number: `118`
is `pub struct JsFuture<T = JsValue> {`, and the `Rc<RefCell<Inner<T>>>`
field itself is `119`.

**Fix round 1, finding 2: the callbacks used to live in the wrong place,
and the fix is the same one upstream already uses.** The first version of
`SendJsFuture` held its two `Closure`s as a field sibling to `state`, both
dropped together as soon as the future itself was. That's unsound in
exactly the way `js_sys::futures::JsFuture::from`'s own comment warns
about (`futures/mod.rs:149-158`): "we'd have no way of cancelling the
callbacks getting invoked... one of the callbacks is likely always going to
be invoked... they have to be self-contained." A `Closure` dropped BEFORE
the promise it's registered on settles throws when JS later invokes it
(`ScopedClosure::drop` invalidates the JS-side function first), and since
`SendJsFuture::new` discards the promise `.then2()` returns, nothing
observed that throw directly — it surfaced only as a browser-level
unhandled promise rejection whenever an already-pending, already-abandoned
promise finally settled.

The fix mirrors js-sys's own `Inner::callbacks` / `finish` (`futures/mod.rs
:101-108`, `:159-211`) exactly: the callbacks now live INSIDE `State`
(behind the same `Arc<Mutex<..>>` `SendJsFuture.state` also holds), and
each callback's own captured clone of that `Arc` keeps `State` — and hence
itself — alive independently of the outer future handle. Whichever callback
actually fires drops BOTH (`state.callbacks = None`) from within its own
invocation, which is sound only because a JS `Promise` invokes at most one
of resolve/reject, ever, so the sibling callback is guaranteed to never
fire once its partner already has.

A first regression test tried to observe this via a real global
`unhandledrejection` listener. Abandoned after a demonstrated false
negative AND false positive in the same run: `wasm-bindgen-test`'s browser
runner executes every test in one shared page/JS realm back to back, and a
deliberately-unhandled rejection created to sanity-check the listener
wasn't observed within its OWN test's window, then bled into a LATER
test's window and was misattributed there — real timing, but not
deterministic enough to trust. The test that shipped instead
(`tests/promise.rs::dropping_a_pending_future_does_not_drop_its_still_needed_callbacks`)
checks the retention mechanism directly: a `Weak` handle to the same `Arc`
(`SendJsFuture::downgrade_state`, exposed via `testing` for exactly this),
checked with `Weak::upgrade` strictly AFTER the future itself is dropped —
deterministic, synchronous, no promise scheduling or browser events
involved. Mutation-checked by reverting to the sibling-field design: the
test goes red.

**Fix round 1, finding 3: the future wasn't fused, and that's a silent
hang — the exact failure mode this project has spent two verticals
removing.** Polling `SendJsFuture` again after it had already returned
`Poll::Ready` used to silently return `Poll::Pending` forever: `result` had
already been `take()`n by the first `Ready` and nothing ever refilled it.
Out of `Future`'s own contract (a `Future` must not be polled again after
completion), and exactly the "no test name, no message" class of defect
most recently fixed by rewriting the F1 connect-timeout watchdog in
vertical 2. Fixed with a `completed: bool` flag, checked at the top of
`poll` and asserted false, turning the silent hang into an immediate, loud
panic naming the type. Mutation-checked by removing the guard: the
regression test
(`tests/promise.rs::polling_after_ready_panics_loudly_instead_of_hanging_silently`,
`#[should_panic]`) doesn't cleanly fail "didn't panic" — it HANGS and times
out, which is itself the demonstration of why the guard matters.

**Fix round 1, finding 4: the `unsafe-code-exception` marker had to be
scoped to its one legitimate file.** As first shipped, the `no-unsafe-code`
job's marker filter matched the text `unsafe-code-exception:
amendment-C7` anywhere in `crates/*/src`, with no check on which file. A
reviewer probe confirmed the gap directly: planting that exact marker text
next to an unrelated `unsafe` block in `hclient-core/src/lib.rs` passed the
job. `send-bound-exception` can't be fixed the same way — it legitimately
appears across many files, by design (see its own preamble at the top of
this section) — but C7 has exactly one legitimate location in the whole
project, and that location is knowable ahead of time. The job's filter now
also requires the marker's line to have the path
`crates/hclient-fetch/src/promise.rs`; the same marker text anywhere else
no longer excuses anything. Verified in all four directions: the two real
marked lines pass; an unmarked `unsafe` anywhere still fails; a marker
citing a nonexistent amendment still fails; the correct marker, planted in
a different crate's `src`, now ALSO fails (the missing direction before
this fix).

### C8. `unsafe` is permitted at a foreign-function boundary, in the files the amendment names

The second exception to "no crate writes `unsafe`", and the first that is not
about wasm. C7 established the mechanism — `#![deny(unsafe_code)]` in place of
`forbid` for one crate, a per-line `unsafe-code-exception` marker, and a
`no-unsafe-code` CI job that path-scopes that marker to the one file the
amendment names. C8 reuses that mechanism — with one correction forced by `cargo fmt`, below —
under its own token `unsafe-code-exception: amendment-C8`, for
`hclient-dns-system/src/sys/res_query.rs` and
`hclient-dns-system/src/sys/windows.rs`: one file per platform backend, and no
`unsafe` anywhere else in that crate.

**The rule, stated once so it does not have to be re-derived per task.**
`unsafe` is permitted where Rust has no other way to reach a platform API this
project needs, and only there. It is not permitted for performance, for
convenience, or to avoid a bound. Each site gets its own amendment, its own
token, and an explicit file path in the CI job — never a widened existing one,
and never a directory, because a marker that excuses a directory excuses
everything a future reviewer forgets to look at. C8 naming two files is not an
exception to that: they are the two platform halves of one boundary, both
enumerated by name, and `sys/mod.rs` sitting between them is deliberately not
among them.

**Why a system SVCB lookup cannot be written in safe Rust.** `getaddrinfo` —
everything `hclient-dns-system` used before this — cannot return an HTTPS/SVCB
record in principle: its result type is a list of `sockaddr`s. §5.3 concluded
from that that "the system path structurally can't give you ECH or h3
discovery," which conflated `getaddrinfo` with the system resolver;
`res_query(3)` is in the same libc and answers RR type 65. It is not wrapped
by anything in this workspace's dependency graph: checked against the vendored
sources, `res_query` is absent from `rustix` 1.1.4 and from every `libc`
release from 0.2.182 through 0.2.189. So the alternative to declaring the
foreign function ourselves is not "the same thing in safe Rust" — it is no ECH
and no first-request h3 on any native target, which is what
`supports_svcb() == false` used to mean here.

**The scope, and why it is this small — and it got smaller than first
written.** `res_query.rs` declares one foreign function, calls it into a
buffer it sized itself, and returns an owned `Vec<u8>` or a copied `[u8; 12]`.
`windows.rs` is the same shape on the other platform: it resolves two symbols,
drives one asynchronous call to completion, copies the response out, and frees
what `dnsapi` allocated. Neither parses anything, and neither hands a borrowed
pointer upward. The result is the property that matters: **the code that reads
untrusted input is safe Rust, and the code that is not safe Rust reads
nothing.**

Three decisions were made specifically to keep it that way, and all three moved
logic *out* of the unsafe files:

- the RCODE/`QR` classification of a failed call lives in
  `svcb::endpoints_from_answer`, so `RawAnswer` has no "no response" variant
  for a backend to choose between — it always returns the header, and safe code
  decides what it means;
- the buffer-length decision lives in `sys::classify_written`, so the one bound
  governing whether a length reported by C reaches a slice index is ordinary
  safe code with four tests around it;
- **the RFC 9460 wire parsing is not ours at all.** The first version of this
  work hand-wrote it — roughly 600 lines of bounds checks over
  attacker-chosen bytes, with its own name-decompression termination proof.
  That code was deleted in favour of `dns-message-parser` 0.9, chosen after
  reading its source rather than its README: no `unsafe` anywhere in its
  `src`, `DecodeResult` on every path rather than panics, and name
  decompression that terminates by recording visited offsets in a `HashSet`
  (`decode/domain_name.rs`, `EndlessRecursion` / `MaxRecursion`) instead of
  relying on a hand-argued rule. Deleting the riskiest code in a change is
  strictly better than writing it carefully; the adversarial byte vectors it
  was tested with were kept and now test the seam instead — that a decoder
  refusal becomes an `Err` here rather than a panic, a hang, or a silently
  empty result.

What remains hand-written is the twelve-byte header (`svcb::read_header`), and
it has to be: `res_query`'s failure path returns no length, so only the
fixed-size header can be trusted, and `Dns::decode` cannot read one on its own
— measured, given exactly twelve bytes it fails with "not enough bytes ...
offset 13" because it goes on to look for the question section.

**Two things the decoder does that the RFC does not, both found by keeping the
round-trip assertions.** Neither is a reason to drop the crate; both are
recorded so the next reader does not rediscover them as bugs.

1. *It strips the ECHConfigList's length prefix.* RFC 9460 §7.3 defines the
   `ech` SvcParamValue as an ECHConfigList "including the redundant length
   prefix", and that prefixed form is what rustls parses — an ECHConfigList is
   a TLS vector, so its codec reads a `u16` length first. The decoder
   validates the prefix and returns the payload without it. The mapping puts
   it back, because `SvcbEndpoint::ech_config_list` exists to feed
   `rustls::EchConfig` directly and would otherwise hold something rustls
   cannot parse — a field that looks populated and fails far from here. Caught
   by writing the ECH test as a round-trip rather than as "is it non-empty".
2. *It rejects an AliasMode record that carries SvcParams.* It reads
   SvcParams only when `priority != 0`, so such a record leaves bytes
   unconsumed and fails the whole message with `TooManyBytes`. RFC 9460 §2.4.1
   says recipients MUST *ignore* those params, i.e. the record stays usable.
   Accepted as a documented divergence: it only triggers for a server already
   violating the same section's "SHOULD be empty", and it fails in the safe
   direction — the RRSet is rejected and the caller falls back to non-SVCB
   rather than acting on a record nobody agrees about. Pinned by a test that
   will fail if upstream ever starts honouring §2.4.1.

**A trap in the dependency's public API, worth naming because it makes tests
lie silently.** `ServiceParameter`'s `PartialEq` and `Hash` compare **only the
SvcParamKey number** — `ALPN{["h2"]} == ALPN{["h3"]}` is `true`. It exists so a
`BTreeSet` can hold one parameter per key, but the consequence for a caller is
that `assert_eq!(param, ServiceParameter::ALPN { alpn_ids: vec!["h2"] })`
passes **without comparing a single value**: it looks like it checks the ALPN
list and actually checks the number `1`. Every assertion in this crate
therefore reaches into the extracted `SvcbEndpoint` fields, which have ordinary
equality, and a characterisation test asserts the upstream behaviour directly
so that the day it changes, the reason for the style is revisited deliberately
rather than found again by accident. Mutation-checked: a mutant that emits the
right ALPN *key* with the wrong *values* is killed by the real-answer test.

**Why `res_query` and not `res_nquery`.** `res_nquery` takes a caller-owned
`res_state` and is normally the better answer where a process-global resolver
state would be a hazard. It is rejected here because `struct __res_state` is a
libc-private layout that neither `libc` nor `rustix` declares and that differs
between glibc, musl and Darwin. A hand-written `#[repr(C)]` guess at it is not
a safer version of this file; it is memory corruption waiting for a libc point
release. What is relied on instead is that `res_query`'s state is per-thread on
all three supported libcs — glibc and Darwin reach it through `__res_state()`,
and musl's `res_query` holds no resolver state at all, re-reading
`/etc/resolv.conf` per call — which is what makes calling it from an arbitrary
`Blocking`-pool thread sound.

**Four platform facts, each established by measurement rather than by
convention.** They are recorded here because each one would otherwise be
re-guessed by the next person to touch this file, and three of the four
contradict the obvious guess.

1. *Where the symbol lives.* glibc 2.34 and later export `res_query` from
   `libc.so.6` (`res_query@@GLIBC_2.34`) and leave `libresolv.so.2` an empty
   stub — read out of the installed libraries with `nm -D`, not assumed.
   Linking `resolv` is still correct there (harmless) and required before
   2.34. musl is the opposite: `res_query` is a strong symbol inside `libc.a`,
   and Rust's self-contained musl sysroot ships **no `libresolv.a` at all**, so
   a `#[link(name = "resolv")]` on musl would fail to link. The attribute is
   therefore `target_env = "gnu"`-scoped, not `target_os = "linux"`-scoped.
2. *Apple exports only the BIND9-prefixed name.* `libresolv.9.tbd` lists
   `_res_9_query` and no plain `_res_query`; C code reaches it through
   `#define res_query res_9_query` in `<resolv.h>`. Rust has no preprocessor,
   so the mapping is spelled out with `#[link_name = "res_9_query"]`. Without
   this the macOS leg of the CI matrix fails at link time, not at run time.
3. *`h_errno` is not usable and is not used.* `res_query` reports the ordinary
   case "this name exists but has no HTTPS record" as **failure**, with the
   reason in `h_errno`. That value is a process-global on Darwin (`netdb.h`
   declares a plain `extern int h_errno;`, not a per-thread accessor), and it
   is sticky: measured on glibc 2.43, a *successful* call left `h_errno` at
   `4` (`NO_DATA`) from the previous failing one. What is used instead was
   measured directly: on the `-1` path the response has already been written
   into the answer buffer, the buffer is zeroed before the call, and every DNS
   response has `QR` set — so `QR` separates "a response arrived and the call
   still failed" from "nothing arrived", and the RCODE on the wire classifies
   the rest.

   ```text
   ftp.gnu.org     type=65 -> ret=-1  qr=1 rcode=0 ancount=0   (no HTTPS record)
   zzz9.invalid    type=65 -> ret=-1  qr=1 rcode=3 ancount=0   (NXDOMAIN)
   cloudflare.com  type=65 -> ret=116 qr=1 rcode=0 ancount=1   (an answer)
   cloudflare.com  in an empty network namespace:
                              ret=-1  qr=0                     (nothing arrived)
   ```

   This matters beyond tidiness: "no HTTPS record" is the *commonest* outcome
   of an HTTPS query, and reporting it as an error would tell every caller its
   DNS was broken for every host that simply does not publish one.
4. *The return value is not a length.* Given a 20-byte buffer for a 116-byte
   answer, glibc's `res_query` returns **20** — the buffer's size, with no
   indication that anything was lost. A return that reaches the buffer's end is
   therefore indistinguishable from a silent truncation and must be retried at
   65535 (the width of the length field that frames a DNS message over TCP),
   never truncated to. `sys::classify_written` is that rule, and it also
   handles the opposite convention (a libc reporting the size it *needed*)
   identically, because both must be kept away from `buf[..n]`.

**Windows: the OS has already parsed the record, and the first two attempts
at this both missed that.** The route finally taken is `DnsQuery_UTF8` with
`DNS_TYPE_HTTPS`, reading the `DNS_SVCB_DATA` the DNS Client service fills in.
No DNS bytes are parsed on Windows at all. The two abandoned routes are kept
here because each is a day of work to rediscover, and because the *reason*
the second was abandoned turned out to be wrong — which is the more useful
half of the record.

*Dead end 1: `DNS_QUERY_RETURN_MESSAGE`.* It hands back a
`DNS_MESSAGE_BUFFER`, which is a `DNS_HEADER` plus `MessageBody: [i8; 1]` and
**no length field**, while `DnsExtractRecordsFromMessage_W` demands the length
from its caller. A parser without a buffer end cannot detect an overrun,
because the read that would reveal it is the read that is already out of
bounds. This one is a genuine dead end and stays one.

*Dead end 2, and the mistake worth naming: "the union is untagged, so a
structured Windows path is unsafe in principle."* `DNS_RECORDW.Data` is indeed
an untagged union, and for a while that reading stopped this work twice — once
into "Windows is out of scope, `supports_svcb()` is `false` there", and once
into an elaborate `DnsQueryRaw` backend fetched through `GetProcAddress` so the
raw wire bytes could go through the shared decoder. Both were solving a problem
that does not exist. `wType == DNS_TYPE_HTTPS` **is** the tag for the outer
union, and one level down `DNS_SVCB_PARAM` carries `wSvcParamKey`, which is the
discriminator for its own union (`pAlpn`, `pIpv4Hints`, `pIpv6Hints`,
`pMandatory`, `wPort`, `pszDohPath`, `pUnknown`). The error was looking at one
union, finding no tag *for that union*, and generalising. The lesson that
belongs in this spec is narrower than "check the metadata": **an untagged union
in a foreign API is a question about which field selects the member, not a
verdict that none does.**

What that buys is not marginal. `DnsQueryRaw` exists only on Windows 11 /
Server 2025; `DnsQuery_UTF8` has been there since Windows 2000, and the SVCB
parsing behind it is present from Windows 10 — so coverage goes from a minority
of machines to nearly all of them, the link is static, and the
`GetProcAddress` machinery disappears along with the run-time capability it
forced. **`SUPPORTS_SVCB` is a `const bool` again on every backend**: the
run-time form was honest only while the answer genuinely depended on the
machine, and a function that always returns a constant is machinery with no
honesty to show for it. The capability and the lookup still come from the same
`#[cfg]`-selected module, so they cannot drift.

**Why `DnsQuery_UTF8` and not `DnsQueryEx`.** Both are applicable and both link
statically; the choice is about charset, and it is the one place where the
bindings could not settle the question. `DnsQueryEx` takes `PCWSTR` and
therefore returns Unicode records — but `windows-sys` 0.61.2 declares exactly
one `DNS_SVCB_DATA`, with no `DNS_SVCB_DATAW`, and its `pszTargetName` is
`PSTR` in both the `DNS_RECORDA` and `DNS_RECORDW` unions. Either Microsoft's
header genuinely has no A/W split for that struct, or the Win32 metadata models
it imprecisely; the bindings cannot distinguish the two, and no machine was
available to settle it by running the code. Reading a UTF-16 target name
through a `PSTR` yields a one-character host name — a silently *wrong host to
connect to*, which is worse than a crash because nothing announces it.
`DnsQuery_UTF8` removes the question instead of answering it: its results are
`DNS_RECORDA`, narrow throughout, so `PSTR` is unambiguously right.

**What the Windows path's safety rests on, separated by how well it is
known.** Stated this way deliberately, because the three have very different
weight:

1. *Read out of the bindings* (`windows-sys` 0.61.2): the declarations of
   `DnsQuery_UTF8`, `DNS_RECORDA`, `DNS_SVCB_DATA`, `DNS_SVCB_PARAM` and its
   union, and that `wSvcParamKey` discriminates that union. Checkable by
   anyone, in the vendored source.
2. *Taken on the project owner's word and not verified here:* that Windows
   parses HTTPS records into `DNS_SVCB_DATA` from **Windows 10 onward**. This
   is what makes a static link and a compile-time `true` correct, and the
   presence of `DNS_SVCB_DATA` in the Win32 metadata does **not** establish it
   — that metadata is not versioned by OS release. If the claim is wrong for
   some older Windows, that OS would return the payload in `DNS_UNKNOWN_DATA`
   and reading it as `DNS_SVCB_DATA` would dereference a `pszTargetName` built
   from raw response bytes. There is no in-process check that distinguishes
   them: the outer union's tag is `wType`, which is 65 either way, and
   comparing `wDataLength` against `size_of::<DNS_SVCB_DATA>()` is a
   coincidence of sizes, not a tag.
3. *Never executed.* Every line type-checks under `cargo check` and `cargo
   clippy --target x86_64-pc-windows-msvc`, and that the file is genuinely
   compiled for that target — rather than silently `#[cfg]`-ed out — was
   confirmed by planting a `compile_error!` in it and watching the Windows
   build fail while the Linux build stayed green. Nothing in it has ever run:
   there is no Windows machine in the environment that produced it.

**What "tested" therefore means for Windows, precisely.** The FFI walk is
verified by types only. What is genuinely tested is everything downstream of
it, and that is not an accident of layout: the RFC 9460 client rules were
deliberately factored onto a backend-neutral `RawBinding`/`RawParam` pair that
holds no borrowed memory and no platform detail. `windows.rs` fills it from an
OS-parsed struct, the Unix path fills it from a decoded message, and both then
go through one `endpoint_from_binding` — so mode handling (§2.4), root-target
substitution (§2.5) and `mandatory` semantics (§8) are written once and tested
once. Five tests drive that seam through `RawBinding` directly, which is the
door Windows uses; one of them covers a rule the Unix path structurally cannot
reach (AliasMode carrying SvcParams, which the Unix decoder refuses outright
but Windows will hand over), so it is not dead code there and that test is the
only thing that can prove it holds.

One asymmetry between the backends is worth stating because it looks like a
bug from either side alone: the ECHConfigList arrives on Windows through
`pUnknown` as the verbatim SvcParamValue, prefix included, which is already the
form RFC 9460 §7.3 defines and rustls parses — so `windows.rs` adds nothing,
while the Unix bridge has to put back a prefix its decoder stripped. Both
converge on the same bytes, and a shared test asserts exactly that.

**Exactly two marker positions are accepted, and the one people reach for
first is not among them.** The fold attaches a lone marker line to its
*predecessor*, so:

```text
unsafe { b() } // unsafe-code-exception: amendment-CN     OK

unsafe extern "C" {                                       OK
    // unsafe-code-exception: amendment-CN                  (rustfmt's placement)

// unsafe-code-exception: amendment-CN                    NOT OK
unsafe { b() }                                              (excuses the line above it)
```

A marker written *above* a construct excuses whatever precedes it — usually a
line with no `unsafe` in it — and the construct stays flagged. This is worth
stating because Rust attributes are written above the item they apply to, so
writing the marker there is the natural first guess. The fold deliberately does
not also look upward: the failure direction as it stands is a false red rather
than a false green, and looking both ways would let one marker excuse two
different `unsafe` lines. Both the job's comment and its `::error::` message
spell the two positions out, so the person who hits this in CI does not have to
go and read the job.

**Verified in thirteen directions.** The `no-unsafe-code` job carries one
per-line, path-scoped filter per amendment. Checked by planting probes in this
tree and reverting each: a marker trailing on the line passes; a marker on the
following line, as rustfmt writes it, passes; a marker *above* the line does
not excuse it; a marker two lines away excuses nothing; the real marked lines
in `res_query.rs` and `windows.rs` pass; an unmarked `unsafe` in `sys/mod.rs`
(same crate, one directory up), in `svcb.rs`, in `windows.rs`, and in another
crate entirely all fail; a marker citing a nonexistent amendment fails (the probe used
`amendment-C9`, which has since been allocated — see C9 below; the probe now
uses `amendment-C99`);
the correct C8 marker planted in a different file fails; and swapping the two
amendments' tokens — C7's marker in `res_query.rs`, C8's in `promise.rs` —
fails both. The whole set was re-run after `cargo fmt --all`, which is the
scenario that started this. The compiler layer was checked the same way and independently: an
`unsafe` block in `svcb.rs` is rejected by that module's `forbid`, an `unsafe`
block in `sys/mod.rs` is rejected by the crate root's `deny`, and a local
`#[allow(unsafe_code)]` in `svcb.rs` fails with `E0453` — the same
`forbid`-cannot-be-relaxed property C7 measured, now holding *inside* a crate
that has an exception rather than only across crates.

**And the mapping was mutation-tested, because a check nobody breaks is a check
nobody has tested.** Twenty-one mutants were applied one at a time and all
twenty-one were killed by a named test: emitting the right ALPN *key* with the
wrong *values* (the mutant that proves the `ServiceParameter::PartialEq` trap
above is actually avoided), handing back the decoder's stripped ECHConfigList,
dropping the port or either address hint, not substituting the owner name for a
root TargetName, emitting an AliasMode record that points at the root,
honouring a `mandatory` key this client does not understand, skipping the
check that a mandatory key is present at all, reporting every unregistered key
as ALPN, leaving the trailing root dot on a target, swallowing a decoder
refusal into an empty result, reading the additional section instead of the
answer section, each of the four header-classification bounds, and both
`classify_written` bounds.

### C9. `unsafe` for the platform's UTS 46, in the one file this amendment names

The third exception to "no crate writes `unsafe`", and the second at a
foreign-function boundary. It reuses C8's mechanism verbatim —
`#![deny(unsafe_code)]` in place of `forbid` for one crate, a per-line
`unsafe-code-exception` marker, and a CI check that path-scopes that marker to
the one file this amendment names — under its own token
`unsafe-code-exception: amendment-C9`, for **`crates/hclient-idn/src/icu/windows.rs`
and nothing else**. `icu/mod.rs` above it is deliberately not among them,
and neither is `lib.rs`, and neither is a directory.

**It named two files until the ELF backend was removed**, and the removal
is the more interesting half of this amendment: `icu/elf.rs` reached
`libicuuc.so.NN` through `dlopen` because both the soname and every symbol
carry the ICU major version, and it worked — the corpus was validated
against a real ICU 78.2 through it, which is how the option word and the
error mask were established in the first place. It was deleted anyway. On
Linux the ICU version is a property of the user's machine that nobody
chooses and nothing reports, and for IDN a Unicode version difference is a
different host; that is a correctness risk accepted in exchange for a size
saving. The rule the project settled on is narrower than "the platform has
an ICU": **static linkage against an ABI the OS versions for us**, which
today is Windows alone. Deleting an `unsafe` file is the best outcome
available to this amendment family, and it is recorded here so the next
person does not re-add it for the size number.

**Why the platform's UTS 46 cannot be reached in safe Rust.** Not because
nobody has wrapped ICU — because nobody has wrapped *this part* of it.
Established by reading the published sources, not by searching for a crate
name:

- There is **no `rust_icu_uidna` crate**. The `rust_icu` family (Google,
  5.7.0, 2026-07-06, 25 crates) covers `ustring`, `uloc`, `ucol`, `unorm2`,
  `ubrk` and twenty others; `rust_icu_sys`' own
  `BINDGEN_SOURCE_MODULES` in `build.rs` does not list `uidna`, and its
  `BINDGEN_ALLOWLIST_FUNCTIONS` has no `uidna_.*`. So the family does not
  expose the entry point even at the `-sys` level.
- It would be the wrong shape even if it did. `rust_icu_sys` defaults to
  `use-bindgen` + `icu_config`, i.e. **`bindgen` (libclang) and
  `icu-config`/`pkg-config` at build time**, and declares `links = "icuuc"`.
  That resolves the ICU of the machine doing the *building*. A client library
  has to run on machines it was not built on, against ICU 74 or 78 or none.
- **ICU4X is not an alternative**, and this is worth recording because it
  looks like one: it is a reimplementation in Rust with its own bundled data,
  and it is already what `idna` uses. `idna_adapter`'s own README settles the
  "smaller subset" question — 1.2.x is ICU4X, 1.1.x is unicode-rs with
  *larger* binary size, 1.0.x is a stub with no Unicode data — and the choice
  is a pin in the top-level `Cargo.lock`, which a library cannot express.
- `IdnToAscii`, which `windows-sys` does expose, is **IDNA2003** and answers
  `strasse.de` where this project answers `xn--strae-oqa.de`. Reaching it in
  safe Rust would be worse than not reaching ICU at all.

So the alternative to declaring three foreign functions is not "the same thing
in safe Rust" — it is carrying 1.9 MB of Unicode tables on every target that
already ships them.

**Why the two backends are not the same shape, and why only one of them
needs a loader.** On ELF unixes ICU's exported symbols are version-suffixed
— `uidna_openUTS46_78` on this development machine (`nm -D --defined-only
/usr/lib/x86_64-linux-gnu/libicuuc.so.78`), `_74` on the next — and so is
the soname, so there is nothing stable to link against and `elf.rs` resolves
at run time through `libloading`. On Windows there is: its ICU is built with
`U_DISABLE_RENAMING` (the SDK's `icu.h` opens with `#define
U_DISABLE_RENAMING 1`) and the exports are unsuffixed, which is why
`windows-sys` can and does bind the plain names. `windows.rs` therefore
declares nothing at all — the signatures, the `UIDNAInfo` layout and every
`UIDNA_*` constant come from Microsoft's own Win32 metadata, and the only
`unsafe` left is the three calls.

That asymmetry has a price, and it is recorded rather than hedged:
`windows-link` emits a `raw-dylib` **load-time** import, so a Windows with no
`icuuc.dll` (10 before 1703, and Server 2016) does not fall back — the
process fails to start. The Windows backend states a floor of 10 1703 /
Server 2019 instead of degrading; the ELF backend, which has to resolve at
run time anyway, degrades for free. Both are covered by the same load-time
acceptance probe in `icu/mod.rs`, which refuses an ICU that does not answer
`straße.de` correctly — so a wrong `OPTIONS`, an ABI drift, or (on the split
Windows libraries) an `open` that turns out to need `CoInitializeEx` becomes
`Backend::None` rather than a wrong host, one name at a time.

**The scope, and the two things kept out of it.** `icu.rs` declares three
signatures, resolves them through `libloading`, calls them into a buffer it
sized itself, and returns an owned `String` or `None`. It parses nothing and
hands nothing borrowed upward. Two decisions were deliberately moved *out* of
it, for the same reason C8 moved `classify_written` and the RCODE
classification out of its FFI files:

- **the option word** (`OPTIONS`) and **which of ICU's error bits are fatal**
  (`IGNORED_ERRORS`, `is_fatal`) live in `lib.rs` as plain constants with unit
  tests around them. `OPTIONS` in particular is the whole point of the crate —
  `UIDNA_DEFAULT` is 0 and 0 is *transitional*, so a handle opened with it
  agrees with IDNA2003 rather than with this project — and a constant nobody
  can see is a constant nobody reviews;
- **the WHATWG deny list** (`is_forbidden_domain_byte`), because ICU has no
  option for it: `UIDNA_USE_STD3_RULES` is a different set, not a stricter
  one.

**`libloading`, not our own `dlopen`.** The loading half is a solved problem
with a 467M-download solution, one transitive dependency per platform
(`cfg-if` on unix, `windows-link` on Windows) and an MSRV under this
workspace's floor. Writing `dlopen`/`LoadLibraryW` here instead would have
doubled the file and added the one bug class — a wrong loader flag on one
platform — that a widely used crate has already had found for it. What
`libloading` does not remove is the `unsafe`: `Library::new` and
`Library::get` are both unsafe, because loading runs initialisers and because
the caller asserts each symbol's type. Those assertions are the three
signatures, transcribed from `unicode/uidna.h` and restated above each type
alias, and a layout disagreement is caught rather than silent —
`uidna_nameToASCII_UTF8` validates `UIDNAInfo.size` against its own
`sizeof` and refuses the call.

**The check that holds this in place, and a gap it had.** The
`unsafe-code-policy.sh` job now carries an explicit `file:token` map rather
than a crate list plus an `amendment-C[78]` regex. That regex was the gap: the
spec said each marker was "pinned to the file paths its amendment names", and
the script did not do it — a correct C7 marker anywhere under
`hclient-dns-system/src` was accepted. Both halves are now enforced and both
were probed: fifteen directions, all as expected. A marker trailing on the
line passes; alone on the line below (what `cargo fmt` writes) passes; above
the line does not excuse; two lines away does not excuse; C7's token in a C9
file fails, and C8's does; `amendment-C99` fails; a correct C9 marker in
`lib.rs` fails; renaming `icu.rs` out from under the amendment fails; an
exempt crate that downgrades its own `deny` to `allow` and then writes an
unmarked `unsafe` still fails; a non-exempt crate that drops
`[lints] workspace = true` fails; and a non-exempt crate that writes `unsafe`
is left to rustc, whose `forbid` refuses to compile it (checked separately —
`error: usage of an unsafe block`, citing the workspace table).

### C10. `hclient-h3` declares `Send` because `quinn::Runtime` does

The third class, and the first where the bound comes from **neither**
erasure (C1/C2) **nor** a capability trait this project designed (C5).

`hclient-h3` implements `quinn::Runtime`, `quinn::AsyncTimer`,
`quinn::AsyncUdpSocket` and `quinn::UdpPoller` from outside quinn — which
is the whole reason HTTP/3 was reachable here at all, where hyper's sealed
`Http2ClientConnExec` made the equivalent impossible for HTTP/2. quinn
declares `Runtime: Send + Sync + Debug + 'static` (`quinn-0.11.11/src/
runtime.rs:16`) and hands its driver over as
`Pin<Box<dyn Future<Output = ()> + Send>>`. Those are quinn's conditions,
not ours; a crate that implements the trait either satisfies them or does
not implement it.

**The bound is deliberately confined to this crate and does not reach a
seam.** The design proposed putting `Send + Sync + 'static` on
`UdpBind::Socket` itself; that was rejected, because one consumer's
requirement would then tax every implementer — an embassy UDP backend can
implement `UdpBind` honestly and report `UdpCaps::NONE` without a `Send` it
cannot give. So every bound lives in `hclient_h3::H3`'s where-clause, where
the compile error lands on whoever asked for QUIC.

The consequence is stated rather than hidden: **HTTP/3 requires
`R: Spawn`**, and therefore a runtime that can carry a `Send` future. That
excludes exactly the runtimes UDP had already excluded — quinn refuses a
`!Send` runtime itself (measured: `E0277` on an `Rc<()>` inside the runtime
type) — so it takes nothing from embassy that was still there to take.

The marker is `send-bound-exception: amendment-C10`, and the sites are the
where-clauses and type aliases in `crates/hclient-h3/src/{lib,runtime}.rs`.
It is cited nowhere else: a bound demanded by a *different* third-party
trait would be a different amendment, because the argument is about which
external contract is being satisfied and cannot be reused by gesture.

### C11. `unsafe` at Apple's Objective-C boundary, in the files this amendment names

C8's shape at a different foreign boundary: `hclient-urlsession` puts
Apple's `URLSession` behind `Transport`, and every call into it goes
through `objc2`'s message send, which is `unsafe` by construction — the
selector, the argument types and the ownership convention are checked by
nothing the compiler can see. `unsafe` is the medium there rather than an
optimisation, so a crate-wide `forbid` would mean no backend.

The exemption follows the policy rather than relaxing it. The crate is in
`scripts/unsafe-code-policy.sh`'s `EXEMPT` list, **every** `unsafe` line in
it carries an `amendment-C11` marker, and the files are named in the same
script — `crates/hclient-urlsession/src/{delegate,session}.rs` and no
others. `body.rs` has none at all, which is the check the naming exists to
make possible: a file drifting into `unsafe` fails the script rather than
passing unnoticed.

objc2 0.6 needed far less than the design expected — 23 sites became 11 —
because `define_class!` and the typed framework bindings absorb the message
sends that would otherwise each be one.

### C12. `hclient` declares `Send` on two setters, to erase a store and a list

The fourth class. Not erasure forced by a seam (C1/C2), not a capability
trait of ours (C5), not a third-party contract (C10): a bound this crate
chooses, on two opt-in calls, so that a caller's own cache store and public
suffix list can reach `Client` without a type parameter.

`hclient_cache::HttpCache<S>` and `hclient_cookie::CookieJar<P>` are both
generic; `Client` accepted only their defaulted forms, so the seams were
unreachable through the facade. Both routes to closing that were weighed:

- **A type parameter on `Client`** puts `S` on the public `ClientBody`
  alias, because a recording body holds a cache handle — so the *arity of
  a public alias* would change with a feature, and Cargo unifies features.
  Worse, a defaulted parameter needs a default type, and both crates are
  **optional dependencies**: `Client<T, Tm, P = hclient_cookie::
  BuiltinList, ..>` names a type that does not exist without the feature,
  so the declaration forks four ways.
- **Erasure** (`AnyList`, `AnyStore` in `crates/hclient/src/erased.rs`)
  leaves every arity fixed and every method of `HttpCache` and `CookieJar`
  reachable, at the cost of this bound.

The bound states a property the concrete types already had. `Inner`'s doc
has said since the jar landed that its `Mutex` is there because *"a
`Client` is meant to cross a `tokio::spawn`"*, and `BuiltinList` and
`MemoryStore` are both `Send`. Erasing without it would make every
`Client` in a build with either feature compiled in `!Send` — configured
or not — which is the feature-unification hazard the first bullet is
about. `pluggable_stores.rs` asserts the negation.

It is `Native::multiplexed()`'s shape: the bound sits on the opt-in call,
no signature anyone else meets acquires one, and a caller handing in a
`!Send` list gets `E0277` on the line where they asked. The marker is
`send-bound-exception: amendment-C12`, and the sites are `erased.rs`,
`predicate.rs`, and the three `where` clauses in
`crates/hclient/src/client.rs`.

**A third site joined after this was written, and it belongs to this
amendment rather than to one of its own**: `ClientBuilder::
redirect_predicate` erases a caller's closure into `RedirectPredicate` for
exactly the reason above, and the alternative — `Client<T, Tm, R>` — is
the same arity change rejected here. C10's rule about not reusing an
amendment by gesture is about a bound demanded by *someone else's* trait,
where the argument turns on which external contract is being satisfied.
This bound is chosen here, by us, for one purpose, and a second amendment
restating that purpose would be the copy C10 is warning against. What a
future site must share to cite C12 is the whole argument: a value the
caller owns, reaching `Client` by erasure rather than by a type
parameter, with the bound on the opt-in call and nowhere else.
