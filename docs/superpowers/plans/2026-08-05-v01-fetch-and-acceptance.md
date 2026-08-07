# http-ng v0.1, vertical 3: fetch and acceptance — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The same application code works in the browser; the `Capabilities`
runtime model is verified by the one backend whose capabilities differ **at
runtime**; SSE can reconnect; and all of this is confirmed by a live
consumer — the `act` component.

**Architecture:** `http-ng-fetch` is the third `Transport`, depending only on
Tier A. The key point: our own promise adapter on `Arc<Mutex<..>>` instead of
`wasm_bindgen_futures::JsFuture`, because the latter has an `Rc<RefCell<..>>`
inside and is `!Send` **needlessly**. `Capabilities` gets filled from runtime
probe results, not `cfg!`. SSE reconnect is a stage on top of the decoder
that already exists.

**Tech Stack:** `wasm-bindgen` 0.2.126, `web-sys` 0.3.103, `js-sys`,
`wasm-streams` 0.6, `wasm-bindgen-test` 0.3, `wasm-bindgen-futures` (only for
`spawn_local` in tests).

## Global Constraints

Inherited from verticals 1 and 2, and extended:

- **`http-ng-fetch` depends only on Tier A** (`http-ng-core`, `http-ng-proto`).
  Neither hyper, nor tokio, nor `http-ng-rt` may appear in its graph —
  checked in CI.
- **The only `unsafe impl Send` in the entire project** lives in
  `http-ng-fetch`, on a single type, under `#[cfg(not(target_feature =
  "atomics"))]`, and mirrors exactly what wasm-bindgen itself does for
  `JsValue`. The crate carries `#![deny(unsafe_code)]` with one
  narrowly-scoped `#[allow]` and a justification comment.
- **`Capabilities` are filled by runtime probes, not `cfg!`.** The same
  wasm binary works in both Chrome and Safari.
- **Not a single silent no-op.** Everything fetch can't do is declared in
  `Capabilities` and rejected at `build()`.
- MSRV 1.85.

## File layout

```
crates/http-ng-proto/
  src/backoff.rs             pure backoff with jitter (task 6)
crates/http-ng-fetch/
  src/lib.rs                 Fetch: Transport
  src/promise.rs             SendJsFuture — a Send-compatible promise adapter
  src/caps.rs                runtime capability probes
  src/convert.rs             http <-> web_sys, forbidden headers
  src/body.rs                response body over ReadableStream
crates/http-ng/
  src/sse.rs                 reconnect (modification)
```

---

### Task 1: `http-ng-fetch` — Send-compatible promise adapter

Verified by a spike beforehand: the technique compiles without atomics and
is **correctly rejected by the compiler** with atomics.

**Files:**
- Create: `crates/http-ng-fetch/Cargo.toml`, `src/lib.rs`, `src/promise.rs`
- Test: `crates/http-ng-fetch/tests/promise.rs` (wasm-bindgen-test)

**Interfaces:**
- Produces:
  - `pub(crate) struct SendJsFuture`; `SendJsFuture::new(p: js_sys::Promise) -> Self`;
    `impl Future for SendJsFuture { type Output = Result<JsValue, JsValue>; }`
  - `pub(crate) struct SingleThreaded<T>(T)` — `Send` only without `+atomics`

- [ ] **Step 1: Write a failing test**

```rust
// crates/http-ng-fetch/tests/promise.rs
#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn resolves_a_promise() {
    let p = js_sys::Promise::resolve(&wasm_bindgen::JsValue::from_str("ok"));
    let v = http_ng_fetch::testing::send_js_future(p).await.unwrap();
    assert_eq!(v.as_string().as_deref(), Some("ok"));
}

#[wasm_bindgen_test]
async fn propagates_rejection() {
    let p = js_sys::Promise::reject(&wasm_bindgen::JsValue::from_str("nope"));
    let e = http_ng_fetch::testing::send_js_future(p).await.unwrap_err();
    assert_eq!(e.as_string().as_deref(), Some("nope"));
}

#[wasm_bindgen_test]
fn future_is_send_on_the_default_target() {
    // The main claim: `!Send` is a property of building with wasm threads,
    // not of the browser. Without `+atomics`, everything is Send.
    fn assert_send<T: Send>() {}
    assert_send::<http_ng_fetch::testing::SendJsFutureAlias>();
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests`
Expected: FAIL — the crate doesn't exist.

- [ ] **Step 3: Create the crate**

```toml
# crates/http-ng-fetch/Cargo.toml
[package]
name = "http-ng-fetch"
version = "0.1.0"
description = "http-ng transport over the browser fetch API"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
bytes         = { workspace = true }
futures-core  = { workspace = true }
http          = { workspace = true }
http-body     = { workspace = true }
http-ng-core  = { workspace = true }
js-sys        = "0.3"
wasm-bindgen  = "0.2.126"
wasm-streams  = "0.6"

[dependencies.web-sys]
version = "0.3.103"
features = [
  "AbortController", "AbortSignal", "Headers", "ReadableStream", "Request",
  "RequestInit", "RequestMode", "RequestRedirect", "Response", "Window",
  "WorkerGlobalScope",
]

[dev-dependencies]
wasm-bindgen-test = "0.3"

[lints]
workspace = true
```

- [ ] **Step 4: Implement the adapter**

```rust
// crates/http-ng-fetch/src/promise.rs
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use wasm_bindgen::prelude::*;

/// The same technique wasm-bindgen applies to `JsValue` itself.
///
/// `JsValue` is an index into a table owned by the generated JS glue.
/// As long as the module is built **without** `target_feature = "atomics"`,
/// there's one instance, one table, no threads — and upstream itself declares
/// `unsafe impl Send for JsValue` under the same `cfg`. With `+atomics` each
/// worker gets its own table, and the compiler correctly rejects us.
#[repr(transparent)]
pub(crate) struct SingleThreaded<T>(pub(crate) T);

#[allow(unsafe_code, reason = "mirrors wasm_bindgen::JsValue: without wasm threads the process is single-threaded by construction")]
#[cfg(not(target_feature = "atomics"))]
unsafe impl<T> Send for SingleThreaded<T> {}

#[derive(Default)]
struct State {
    result: Option<Result<JsValue, JsValue>>,
    waker: Option<Waker>,
}

/// A `Send`-compatible replacement for `wasm_bindgen_futures::JsFuture`.
///
/// `JsFuture` holds an `Rc<RefCell<futures::Inner>>` inside
/// (js-sys-0.3.103/src/futures/mod.rs:118) and is therefore `!Send` — but
/// that's an implementation choice, not a platform property: `JsValue`
/// itself, `js_sys::Promise`, and `web_sys::{Request, Response,
/// ReadableStream}` **are** `Send` on the default target.
pub(crate) struct SendJsFuture {
    state: Arc<Mutex<State>>,
    _keepalive: SingleThreaded<(Closure<dyn FnMut(JsValue)>, Closure<dyn FnMut(JsValue)>)>,
}

impl SendJsFuture {
    pub(crate) fn new(promise: js_sys::Promise) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let make = |ok: bool| {
            let state = state.clone();
            Closure::wrap(Box::new(move |v: JsValue| {
                let mut s = state.lock().expect("promise state poisoned");
                s.result = Some(if ok { Ok(v) } else { Err(v) });
                if let Some(w) = s.waker.take() {
                    w.wake();
                }
            }) as Box<dyn FnMut(JsValue)>)
        };
        let (on_ok, on_err) = (make(true), make(false));
        let _ = promise.then2(&on_ok, &on_err);
        Self { state, _keepalive: SingleThreaded((on_ok, on_err)) }
    }
}

impl Future for SendJsFuture {
    type Output = Result<JsValue, JsValue>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut s = self.state.lock().expect("promise state poisoned");
        match s.result.take() {
            Some(r) => Poll::Ready(r),
            None => {
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
```

```rust
// crates/http-ng-fetch/src/lib.rs
//! http-ng transport over the browser's `fetch`.
//!
//! Depends **only** on Tier A: neither hyper, tokio, nor `http-ng-rt` in the graph.
#![deny(unsafe_code)]

mod body;
mod caps;
mod convert;
mod promise;

pub use body::Body;

#[doc(hidden)]
pub mod testing {
    pub use crate::promise::SendJsFuture as SendJsFutureAlias;
    pub fn send_js_future(p: js_sys::Promise)
        -> impl std::future::Future<Output = Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>>
    { crate::promise::SendJsFuture::new(p) }
}
```

- [ ] **Step 5: Run the wasm tests**

Run: `cargo install wasm-bindgen-cli --locked` (if not already installed),
then `wasm-pack test --headless --chrome crates/http-ng-fetch` or
`cargo test -p http-ng-fetch --target wasm32-unknown-unknown`
Expected: PASS, three tests.

If there's no headless browser in the environment, leave
`cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests` as the
gate and file an issue; the browser run gets wired into CI (Task 9).

- [ ] **Step 6: Check that the build honestly breaks with atomics**

Run: `RUSTFLAGS="-Ctarget-feature=+atomics,+bulk-memory" cargo +nightly check -p http-ng-fetch --target wasm32-unknown-unknown -Zbuild-std=std,panic_abort`
Expected: FAIL with `*mut u8 cannot be sent between threads safely` — this is
the **desired** behavior, not a defect: with wasm threads the technique is
unsafe, and the compiler is obligated to say so.

- [ ] **Step 7: Commit**

```bash
git add crates/http-ng-fetch
git commit -m "feat(fetch): Send-capable promise adapter mirroring wasm-bindgen's own reasoning"
```

---

### Task 2: `http-ng-fetch` — runtime capability probes

The only place in the project where `Capabilities` **actually** change at
runtime. This is exactly why we chose a registry over `cfg`.

**Files:**
- Create: `crates/http-ng-fetch/src/caps.rs`
- Test: `crates/http-ng-fetch/tests/caps.rs`

**Interfaces:**
- Produces:
  - `pub(crate) fn probe() -> Capabilities`
  - `pub(crate) fn supports_duplex() -> bool` — checks whether
    `Request.prototype.duplex` is readable, as whatwg/fetch prescribes
  - `pub const FORBIDDEN_HEADERS: [http::HeaderName; N]`

- [ ] **Step 1: Write a failing test**

```rust
// crates/http-ng-fetch/tests/caps.rs
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn declares_what_fetch_genuinely_cannot_do() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    // No trailers, no 1xx, no version selection — whatwg/fetch#772 proposes
    // removing the trailers API altogether.
    assert!(!c.request_trailers);
    assert!(!c.response_trailers);
    assert!(!c.informational_1xx);
    assert!(!c.version_select);
    assert!(!c.version_reported);
    // No TLS, no client certificates, no proxy.
    assert_eq!(c.tls_config, http_ng_core::TlsSupport::None);
    assert!(!c.client_certs);
    assert!(!c.proxy);
    // Cookies and cache are ambient, owned by the browser.
    assert!(c.owns_cookie_jar);
    assert!(c.owns_cache);
    // Upgrade is unreachable: WebSocket in the browser is a separate global.
    assert_eq!(c.upgrade, http_ng_core::UpgradeSupport::None);
}

#[wasm_bindgen_test]
fn only_the_connect_deadline_exists_and_it_is_one_for_everything() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    // AbortSignal is one deadline for the whole exchange. Declaring three
    // separate timeouts would be a lie.
    assert!(!c.timeouts.connect);
    assert!(!c.timeouts.first_byte);
    assert!(!c.timeouts.between_bytes);
}

#[wasm_bindgen_test]
fn forbidden_headers_are_listed_not_silently_dropped() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    let names: Vec<_> = c.forbidden_request_headers.iter()
        .map(|h| h.as_str()).collect();
    for must in ["host", "connection", "content-length", "cookie", "origin",
                 "transfer-encoding", "te", "upgrade"] {
        assert!(names.contains(&must), "{must} must be in the list");
    }
}

#[wasm_bindgen_test]
fn duplex_support_is_probed_not_assumed() {
    // In Chrome 131+ — true, in Firefox and Safari — false. One binary.
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    assert_eq!(c.streaming_request_body, c.full_duplex,
               "in fetch, duplex and streaming request bodies are the same thing");
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests`
Expected: FAIL — `Fetch` not found.

- [ ] **Step 3: Implement the probes**

```rust
// crates/http-ng-fetch/src/caps.rs
use http_ng_core::{Capabilities, RedirectSupport, TimeoutSupport, TlsSupport,
                   UpgradeSupport};

/// Headers that fetch forbids setting. The list comes from the WHATWG Fetch
/// spec (forbidden request-headers). We **declare** them, rather than silently
/// dropping them: silently dropping `Proxy-*` or `Cookie` is a source of
/// security bugs.
// NOTE (amendment-C4): `static FORBIDDEN: &[HeaderName] = &[..]` does NOT
// compile on stable — E0492 for any header, because the HeaderName type
// contains a Custom variant over Bytes with an AtomicPtr. A const array
// (as here) works, or Box::leak/OnceLock for the slice. Verify the shape
// by compiling before writing out the whole list.
pub const FORBIDDEN_HEADERS: [http::HeaderName; 14] = [
    http::header::HOST,
    http::header::CONNECTION,
    http::header::CONTENT_LENGTH,
    http::header::COOKIE,
    http::header::ORIGIN,
    http::header::TRANSFER_ENCODING,
    http::header::TE,
    http::header::UPGRADE,
    http::header::REFERER,
    http::header::DATE,
    http::header::VIA,
    http::header::PROXY_AUTHENTICATE,
    http::header::PROXY_AUTHORIZATION,
    http::header::ACCEPT_ENCODING,
];

/// Whether `Request.prototype.duplex` is readable.
///
/// BCD `api.Request.duplex`: chrome/edge/webview **131** (2024-11-12),
/// Firefox `false` (bugzil.la/1792434), Safari/iOS `false`
/// (webkit.org/b/245671). The same wasm binary runs on all three — that's
/// why the probe is runtime-based, not `cfg!`.
pub(crate) fn supports_duplex() -> bool {
    let Ok(init) = js_sys::Reflect::get(
        &js_sys::global(), &wasm_bindgen::JsValue::from_str("Request")
    ) else { return false };
    let Ok(proto) = js_sys::Reflect::get(
        &init, &wasm_bindgen::JsValue::from_str("prototype")
    ) else { return false };
    js_sys::Reflect::has(&proto, &wasm_bindgen::JsValue::from_str("duplex"))
        .unwrap_or(false)
}

pub(crate) fn probe() -> Capabilities {
    let duplex = supports_duplex();
    let mut c = Capabilities::none();
    // Streaming request body in fetch is only possible together with duplex:"half".
    c.streaming_request_body = duplex;
    c.full_duplex = duplex;
    // The browser owns redirects; you can set the policy but can't observe hops
    // (`redirect: "manual"` gives opaqueredirect with status 0 and no headers).
    c.redirects = RedirectSupport::Configurable;
    c.owns_cookie_jar = true;
    c.owns_cache = true;
    // AbortSignal is one deadline for the whole exchange; none of the three
    // phase timeouts can be expressed through it.
    c.timeouts = TimeoutSupport { connect: false, first_byte: false, between_bytes: false };
    c.tls_config = TlsSupport::None;
    c.upgrade = UpgradeSupport::None;
    c.forbidden_request_headers = &FORBIDDEN_HEADERS;
    c
}
```

- [ ] **Step 4: Add `Fetch` with a skeleton and a test accessor**

```rust
// in crates/http-ng-fetch/src/lib.rs
use http_ng_core::Capabilities;

#[derive(Debug)]
pub struct Fetch {
    caps: Capabilities,
}

impl Fetch {
    pub fn new() -> Self {
        Self { caps: caps::probe() }
    }
    #[doc(hidden)]
    pub fn capabilities_for_test(&self) -> &Capabilities { &self.caps }
}

impl Default for Fetch {
    fn default() -> Self { Self::new() }
}
```

- [ ] **Step 5: Run the tests**

Run: `wasm-pack test --headless --chrome crates/http-ng-fetch`
Expected: PASS. In Chrome `streaming_request_body == true`; the same binary
in Firefox will give `false` — that's exactly what CI checks (Task 9).

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-fetch
git commit -m "feat(fetch): runtime capability probing, the reason the registry exists"
```

---

### Task 3: `http-ng-fetch` — request conversion

**Files:**
- Create: `crates/http-ng-fetch/src/convert.rs`
- Test: `crates/http-ng-fetch/tests/convert.rs`

**Interfaces:**
- Consumes: `FORBIDDEN_HEADERS`, `supports_duplex` (Task 2).
- Produces:
  - `pub(crate) fn to_web_request(req: http::Request<RequestBody>, caps: &Capabilities) -> Result<(web_sys::Request, Option<web_sys::AbortController>), Error>`
  - `pub(crate) fn check_headers(h: &http::HeaderMap, caps: &Capabilities) -> Result<(), Error>`

- [ ] **Step 1: Write a failing test**

```rust
// crates/http-ng-fetch/tests/convert.rs
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use http_ng_core::RequestBody;

#[wasm_bindgen_test]
fn rejects_a_forbidden_header_instead_of_dropping_it() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("host", "evil.example")
        .body(RequestBody::Empty).unwrap();
    let err = http_ng_fetch::testing::to_web_request(&f, req).unwrap_err();
    assert!(matches!(err.kind(), http_ng_core::ErrorKind::Unsupported), "{err}");
    assert!(err.to_string().contains("host"), "{err}");
}

#[wasm_bindgen_test]
fn ordinary_headers_pass_through() {
    let f = http_ng_fetch::Fetch::new();
    let req = http::Request::builder()
        .uri("https://example.com/")
        .header("x-custom", "v")
        .body(RequestBody::Empty).unwrap();
    assert!(http_ng_fetch::testing::to_web_request(&f, req).is_ok());
}

#[wasm_bindgen_test]
fn streaming_body_is_rejected_where_duplex_is_absent() {
    let f = http_ng_fetch::Fetch::new();
    if f.capabilities_for_test().streaming_request_body {
        return; // supported in Chrome — nothing to check
    }
    let req = http::Request::builder()
        .uri("https://example.com/")
        .body(RequestBody::rewindable(|| RequestBody::Empty)).unwrap();
    // Rewindable is bufferable, so it passes; Streaming does not.
    assert!(http_ng_fetch::testing::to_web_request(&f, req).is_ok());
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
// crates/http-ng-fetch/src/convert.rs
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, UnsupportedCapability};
use wasm_bindgen::{JsCast, JsValue};

#[derive(Debug)]
pub(crate) struct ForbiddenHeader(pub(crate) String);
impl std::fmt::Display for ForbiddenHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "fetch forbids setting the `{}` header", self.0)
    }
}
impl std::error::Error for ForbiddenHeader {}

#[derive(Debug)]
pub(crate) struct JsError(pub(crate) String);
impl std::fmt::Display for JsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "javascript error: {}", self.0)
    }
}
impl std::error::Error for JsError {}

pub(crate) fn js_err(v: JsValue) -> Error {
    let msg = v.as_string()
        .or_else(|| js_sys::Reflect::get(&v, &JsValue::from_str("message"))
            .ok().and_then(|m| m.as_string()))
        .unwrap_or_else(|| format!("{v:?}"));
    Error::new(ErrorKind::Other, JsError(msg))
}

/// Checks headers **before** sending. Silently dropping forbidden
/// headers is a source of security bugs: the user thinks they sent
/// `Cookie`, but it isn't there.
pub(crate) fn check_headers(h: &http::HeaderMap, caps: &Capabilities) -> Result<(), Error> {
    for name in h.keys() {
        if caps.forbidden_request_headers.contains(name) {
            return Err(Error::new(ErrorKind::Unsupported,
                                  ForbiddenHeader(name.as_str().to_owned())));
        }
    }
    Ok(())
}

pub(crate) fn to_web_request(
    req: http::Request<RequestBody>,
    caps: &Capabilities,
) -> Result<(web_sys::Request, Option<web_sys::AbortController>), Error> {
    let (parts, body) = req.into_parts();
    check_headers(&parts.headers, caps)?;

    let init = web_sys::RequestInit::new();
    init.set_method(parts.method.as_str());

    let headers = web_sys::Headers::new().map_err(js_err)?;
    for (k, v) in parts.headers.iter() {
        headers.append(k.as_str(), v.to_str().map_err(|e|
            Error::new(ErrorKind::Other, e))?).map_err(js_err)?;
    }
    init.set_headers(&headers);

    match body {
        RequestBody::Empty => {}
        RequestBody::Full(b) => {
            let arr = js_sys::Uint8Array::from(&b[..]);
            init.set_body_opt_u8_array(Some(&arr));
        }
        RequestBody::Rewindable(f) => match f() {
            RequestBody::Full(b) => {
                let arr = js_sys::Uint8Array::from(&b[..]);
                init.set_body_opt_u8_array(Some(&arr));
            }
            _ => {}
        },
        RequestBody::Streaming(_) => {
            if !caps.streaming_request_body {
                return Err(Error::new(ErrorKind::Unsupported, UnsupportedCapability {
                    what: "streaming_request_body", backend: "fetch",
                }));
            }
            // Chrome requires `duplex: "half"`, and web-sys 0.3.103 has no
            // `set_duplex` — set it via Reflect.
            js_sys::Reflect::set(&init, &JsValue::from_str("duplex"),
                                 &JsValue::from_str("half")).map_err(js_err)?;
            // A full ReadableStream from RequestBody::Streaming is v0.2;
            // until then this can't be reached, because Capabilities
            // only declares streaming_request_body together with duplex.
            return Err(Error::new(ErrorKind::Unsupported, UnsupportedCapability {
                what: "streaming_request_body", backend: "fetch",
            }));
        }
    }

    let controller = web_sys::AbortController::new().ok();
    if let Some(c) = &controller {
        init.set_signal(Some(&c.signal()));
    }

    let request = web_sys::Request::new_with_str_and_init(&parts.uri.to_string(), &init)
        .map_err(js_err)?;
    Ok((request, controller))
}
```

- [ ] **Step 4: Export the test helper and run the tests**

```rust
// in the testing module of lib.rs
    pub fn to_web_request(f: &crate::Fetch, req: http::Request<http_ng_core::RequestBody>)
        -> Result<(web_sys::Request, Option<web_sys::AbortController>), http_ng_core::Error>
    { crate::convert::to_web_request(req, f.capabilities_for_test()) }
```

Run: `wasm-pack test --headless --chrome crates/http-ng-fetch`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-fetch
git commit -m "feat(fetch): request conversion rejecting forbidden headers explicitly"
```

---

### Task 4: `http-ng-fetch` — response body over ReadableStream

**Files:**
- Create: `crates/http-ng-fetch/src/body.rs`
- Test: `crates/http-ng-fetch/tests/body.rs`

**Interfaces:**
- Produces: `pub struct Body`; `impl http_body::Body for Body { type Data = Bytes; type Error = Error }`;
  `Body::empty()`; `Body::from_response(&web_sys::Response) -> Result<Self, Error>`

- [ ] **Step 1: Write a failing test**

```rust
// crates/http-ng-fetch/tests/body.rs
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn streams_a_response_body_in_chunks() {
    // A data: URL gives a deterministic response with no network.
    let f = http_ng_fetch::Fetch::new();
    let body = http_ng_fetch::testing::fetch_body(
        &f, "data:text/plain,hello%20world").await.unwrap();
    assert_eq!(&body[..], b"hello world");
}

#[wasm_bindgen_test]
async fn empty_body_is_end_stream() {
    let b = http_ng_fetch::Body::empty();
    assert!(http_body::Body::is_end_stream(&b));
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
// crates/http-ng-fetch/src/body.rs
use crate::convert::js_err;
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};
use http_ng_core::Error;
use std::pin::Pin;
use std::task::{Context, Poll};
use wasm_bindgen::JsCast;

/// A response body over `ReadableStream`.
///
/// Trailers aren't supported: fetch has none in either direction
/// (whatwg/fetch#772 proposes removing the API altogether). `Capabilities`
/// declares this, so they simply don't exist here.
pub struct Body {
    inner: Inner,
}

enum Inner {
    Stream(Pin<Box<dyn futures_core::Stream<Item = Result<Bytes, Error>>>>),
    Done,
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner {
            Inner::Stream(_) => f.write_str("Body(stream)"),
            Inner::Done => f.write_str("Body(done)"),
        }
    }
}

impl Body {
    pub fn empty() -> Self { Self { inner: Inner::Done } }

    pub(crate) fn from_response(resp: &web_sys::Response) -> Result<Self, Error> {
        let Some(raw) = resp.body() else { return Ok(Self::empty()) };
        let stream = wasm_streams::ReadableStream::from_raw(raw.unchecked_into())
            .into_stream()
            .map(|chunk| {
                chunk
                    .map_err(js_err)
                    .and_then(|v| {
                        v.dyn_into::<js_sys::Uint8Array>()
                            .map(|a| Bytes::from(a.to_vec()))
                            .map_err(js_err)
                    })
            });
        Ok(Self { inner: Inner::Stream(Box::pin(stream)) })
    }
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Result<Frame<Bytes>, Error>>>
    {
        match &mut self.inner {
            Inner::Stream(s) => match s.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(b))) => Poll::Ready(Some(Ok(Frame::data(b)))),
                Poll::Ready(Some(Err(e))) => {
                    self.inner = Inner::Done;
                    Poll::Ready(Some(Err(e)))
                }
                Poll::Ready(None) => { self.inner = Inner::Done; Poll::Ready(None) }
                Poll::Pending => Poll::Pending,
            },
            Inner::Done => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool { matches!(self.inner, Inner::Done) }
}
```

Add `futures-util` with the `std` feature, for `StreamExt::map`.

- [ ] **Step 4: Run the tests**

Run: `wasm-pack test --headless --chrome crates/http-ng-fetch`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-fetch
git commit -m "feat(fetch): streaming response body over ReadableStream"
```

---

### Task 5: `http-ng-fetch` — `impl Transport for Fetch`

**Files:**
- Modify: `crates/http-ng-fetch/src/lib.rs`
- Test: `crates/http-ng-fetch/tests/transport.rs`

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces: `impl Transport for Fetch { type Body = Body; type Error = Error; }`

- [ ] **Step 1: Write a failing test**

```rust
// crates/http-ng-fetch/tests/transport.rs
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

use http_ng::Client;
use http_ng_fetch::Fetch;

#[wasm_bindgen_test]
async fn end_to_end_through_the_client() {
    let c = Client::builder(Fetch::new()).build().unwrap();
    let resp = c.get("data:text/plain,ok").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");
}

#[wasm_bindgen_test]
async fn build_rejects_timeouts_fetch_cannot_express() {
    let err = Client::builder(Fetch::new())
        .timeouts(http_ng::Timeouts {
            connect: Some(std::time::Duration::from_secs(1)), ..Default::default() })
        .build().unwrap_err();
    assert_eq!(err.what, "connect_timeout");
    assert!(err.backend.contains("Fetch"), "{}", err.backend);
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
// in crates/http-ng-fetch/src/lib.rs
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, ErrorKind, RequestBody};
use wasm_bindgen::JsCast;

impl Transport for Fetch {
    type Body = Body;
    type Error = Error;

    async fn execute(&self, req: http::Request<RequestBody>)
        -> Result<http::Response<Body>, Error>
    {
        let (request, _abort) = convert::to_web_request(req, &self.caps)?;

        // `fetch` lives in both Window and WorkerGlobalScope — go through global,
        // to work in both contexts.
        let global = js_sys::global();
        let fetch_fn = js_sys::Reflect::get(&global, &wasm_bindgen::JsValue::from_str("fetch"))
            .map_err(convert::js_err)?
            .dyn_into::<js_sys::Function>()
            .map_err(convert::js_err)?;
        let promise = fetch_fn
            .call1(&global, &request)
            .map_err(convert::js_err)?
            .dyn_into::<js_sys::Promise>()
            .map_err(convert::js_err)?;

        let value = promise::SendJsFuture::new(promise).await
            .map_err(|e| {
                // fetch distinguishes a network error only by TypeError —
                // that's all the browser gives you, and it's recorded in Capabilities.
                let base = convert::js_err(e);
                Error::new(ErrorKind::Connect, base)
            })?;
        let resp: web_sys::Response = value.dyn_into().map_err(convert::js_err)?;

        let mut builder = http::Response::builder()
            .status(resp.status());
        let headers = resp.headers();
        let iter = js_sys::try_iter(&headers).map_err(convert::js_err)?;
        if let Some(iter) = iter {
            for entry in iter {
                let entry = entry.map_err(convert::js_err)?;
                let pair: js_sys::Array = entry.into();
                let (Some(k), Some(v)) = (pair.get(0).as_string(), pair.get(1).as_string())
                    else { continue };
                builder = builder.header(k, v);
            }
        }
        let body = Body::from_response(&resp)?;
        builder.body(body).map_err(|e| Error::new(ErrorKind::Other, e))
    }

    fn capabilities(&self) -> &http_ng_core::Capabilities { &self.caps }
}
```

- [ ] **Step 4: Run the tests**

Run: `wasm-pack test --headless --chrome crates/http-ng-fetch`
Expected: PASS.

- [ ] **Step 5: Check that hyper and tokio aren't in the graph**

Run: `cargo tree -p http-ng-fetch -e normal --prefix none | grep -E '^(hyper|tokio)' && exit 1 || echo OK`
Expected: `OK` — **this is exactly the "ambient build with no tokio" promise**.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-fetch
git commit -m "feat(fetch): Transport implementation, ambient build with zero tokio"
```

---

### Task 6: `http-ng-proto` — jittered backoff

A pure state machine: it takes the attempt number and "randomness" as
parameters, so it's tested without a clock and without a generator. **Not
one** of the four existing SSE crates jitters.

**Files:**
- Create: `crates/http-ng-proto/src/backoff.rs`
- Modify: `crates/http-ng-proto/src/lib.rs`
- Test: inside `backoff.rs`

**Interfaces:**
- Produces:
  - `pub struct Backoff { pub base: Duration, pub max: Duration, pub max_attempts: Option<u32> }` (`Default` = 1s / 30s / `None`)
  - `Backoff::delay(&self, attempt: u32, jitter: f64) -> Option<Duration>` —
    `jitter` in `[0.0, 1.0)`; `None` means "stop"

- [ ] **Step 1: Write failing tests**

```rust
// crates/http-ng-proto/src/backoff.rs
#[cfg(test)]
mod tests {
    use super::*;

    fn b() -> Backoff { Backoff::default() }

    #[test]
    fn grows_exponentially_from_the_base() {
        assert_eq!(b().delay(0, 0.0), Some(Duration::from_secs(1)));
        assert_eq!(b().delay(1, 0.0), Some(Duration::from_secs(2)));
        assert_eq!(b().delay(2, 0.0), Some(Duration::from_secs(4)));
    }

    #[test]
    fn saturates_at_max_and_never_overflows() {
        // 2^40 seconds would overflow u32::pow — check there's no panic.
        assert_eq!(b().delay(40, 0.0), Some(Duration::from_secs(30)));
        assert_eq!(b().delay(u32::MAX, 0.0), Some(Duration::from_secs(30)));
    }

    #[test]
    fn jitter_only_ever_reduces_the_delay() {
        // Full jitter (the AWS model): a random point in [0, delay].
        let full = b().delay(3, 0.0).unwrap();
        let jittered = b().delay(3, 0.999).unwrap();
        assert!(jittered <= full);
        assert!(b().delay(3, 0.5).unwrap() <= full);
    }

    #[test]
    fn stops_after_max_attempts() {
        let b = Backoff { max_attempts: Some(3), ..Backoff::default() };
        assert!(b.delay(2, 0.0).is_some());
        assert!(b.delay(3, 0.0).is_none(), "a fourth attempt is forbidden");
    }

    #[test]
    fn unlimited_by_default_which_must_be_a_conscious_choice() {
        // rmcp's ExponentialBackoff has max_times: None and `2u32.pow(n)`,
        // which panics on overflow after about 32 attempts.
        assert!(b().max_attempts.is_none());
        assert!(b().delay(1000, 0.0).is_some(), "and doesn't panic doing it");
    }
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p http-ng-proto backoff`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
// crates/http-ng-proto/src/backoff.rs
//! Exponential backoff with full jitter. Pure: randomness comes in as a
//! parameter, so behavior is tested without a generator and without a clock.

use core::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub base: Duration,
    pub max: Duration,
    pub max_attempts: Option<u32>,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
            max_attempts: None,
        }
    }
}

impl Backoff {
    /// `attempt` is zero-based. `jitter` is in `[0.0, 1.0)`.
    /// `None` means "stop trying".
    pub fn delay(&self, attempt: u32, jitter: f64) -> Option<Duration> {
        if let Some(limit) = self.max_attempts {
            if attempt >= limit {
                return None;
            }
        }
        // Saturating exponentiation: `2u32.pow(n)` panics after about
        // 32 attempts — this exact defect lives in rmcp.
        let factor = 1u64.checked_shl(attempt.min(63)).unwrap_or(u64::MAX);
        let raw = self.base.checked_mul(factor.min(u32::MAX as u64) as u32)
            .unwrap_or(self.max)
            .min(self.max);
        // Full jitter: a uniform point in [0, raw].
        let scaled = raw.as_secs_f64() * (1.0 - jitter.clamp(0.0, 1.0));
        Some(Duration::from_secs_f64(scaled.max(0.0)))
    }
}
```

- [ ] **Step 4: Wire it up and run**

Add `pub mod backoff;` to `crates/http-ng-proto/src/lib.rs`.

Run: `cargo test -p http-ng-proto`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-proto
git commit -m "feat(proto): jittered exponential backoff that cannot overflow"
```

---

### Task 7: `http-ng` — SSE reconnect

**Files:**
- Modify: `crates/http-ng/src/sse.rs`
- Test: `crates/http-ng/tests/sse_reconnect.rs`

**Interfaces:**
- Consumes: `Backoff` (Task 6), `SseDecoder`, `Client` (vertical 1).
- Produces:
  - `pub struct SseOptions { pub max_event_size: usize, pub backoff: Backoff, pub reconnect: bool }`
  - `Client::sse(&self, url: &str) -> SseBuilder<'_, T>`; `SseBuilder::{header, options, connect}`
  - `SseStream::next` now reconnects, filling in `Last-Event-ID`
  - Terminal rules: **204 — stop forever**; status ≠ 200 — stop;
    `Content-Type` ≠ `text/event-stream` — stop; `EventTooLarge` — stop
    (fatal, not retried)

- [ ] **Step 1: Write failing tests**

```rust
// crates/http-ng/tests/sse_reconnect.rs
use http_ng::mock::MockTransport;
use http_ng::{Client, SseEvent};

fn sse(body: &'static str) -> http::Response<&'static str> {
    http::Response::builder().status(200)
        .header("content-type", "text/event-stream").body(body).unwrap()
}

#[test]
fn reconnects_and_sends_last_event_id() {
    let m = MockTransport::new();
    m.push_response(sse("id: 7\ndata: first\n\n"));   // the stream will break on EOF
    m.push_response(sse("data: second\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream").connect()).unwrap();

    let mut got = Vec::new();
    for _ in 0..2 {
        if let Some(Ok(e)) = futures_executor::block_on(s.next()) { got.push(e) }
    }
    assert_eq!(got.len(), 2);

    let seen = c.transport().requests();
    assert_eq!(seen.len(), 2, "the second request is the reconnect");
    assert_eq!(seen[1].headers.get("last-event-id").unwrap(), "7",
               "the reconnect must fill in the last id");
    assert!(seen[0].headers.get("last-event-id").is_none(),
            "there's no id yet on the first request — can't send an empty one");
}

#[test]
fn stops_forever_on_204() {
    let m = MockTransport::new();
    m.push_response(sse("data: x\n\n"));
    m.push_response(http::Response::builder().status(204)
        .header("content-type", "text/event-stream").body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream").connect()).unwrap();
    while futures_executor::block_on(s.next()).is_some() {}

    assert_eq!(c.transport().requests().len(), 2,
               "204 means \"stop,\" not \"try again\"");
}

#[test]
fn honours_server_sent_retry_over_the_policy() {
    let m = MockTransport::new();
    m.push_response(sse("retry: 100\ndata: x\n\n"));
    m.push_response(sse("data: y\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream").connect()).unwrap();
    let mut n = 0;
    while futures_executor::block_on(s.next()).is_some() { n += 1; if n > 4 { break } }
    assert!(n >= 2);
}

#[test]
fn oversized_event_is_fatal_and_not_retried() {
    let m = MockTransport::new();
    let big = "data: 0123456789abcdefghijklmnop\n\n";
    m.push_response(sse(big));
    m.push_response(sse("data: never\n\n"));

    let c = Client::builder(m).build().unwrap();
    let mut s = futures_executor::block_on(
        c.sse("https://a/stream").options(http_ng::SseOptions {
            max_event_size: 8, ..Default::default() }).connect()).unwrap();

    assert!(futures_executor::block_on(s.next()).unwrap().is_err());
    assert!(futures_executor::block_on(s.next()).is_none());
    assert_eq!(c.transport().requests().len(), 1, "reconnecting is forbidden");
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p http-ng --features test-util --test sse_reconnect`
Expected: FAIL — `no method named sse`.

- [ ] **Step 3: Implement**

```rust
// crates/http-ng/src/sse.rs
use crate::client::Client;
use crate::response::Response;
use bytes::Bytes;
use http_body::Body as HttpBody;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, ErrorKind, RequestBody};
use http_ng_proto::backoff::Backoff;
use http_ng_proto::sse::{SseDecoder, SseEvent};
use std::time::Duration;

const MIME: &str = "text/event-stream";

#[derive(Debug, Clone, Copy)]
pub struct SseOptions {
    pub max_event_size: usize,
    pub backoff: Backoff,
    pub reconnect: bool,
}

impl Default for SseOptions {
    fn default() -> Self {
        Self {
            max_event_size: http_ng_proto::sse::DEFAULT_MAX_EVENT_SIZE,
            backoff: Backoff::default(),
            reconnect: true,
        }
    }
}

pub struct SseBuilder<'a, T> {
    client: &'a Client<T>,
    url: String,
    headers: http::HeaderMap,
    options: SseOptions,
}

impl<'a, T: Transport> SseBuilder<'a, T> {
    pub(crate) fn new(client: &'a Client<T>, url: &str) -> Self {
        Self { client, url: url.to_owned(), headers: http::HeaderMap::new(),
               options: SseOptions::default() }
    }
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if let (Ok(n), Ok(v)) = (name.parse::<http::HeaderName>(),
                                 value.parse::<http::HeaderValue>()) {
            self.headers.insert(n, v);
        }
        self
    }
    pub fn options(mut self, o: SseOptions) -> Self { self.options = o; self }

    pub async fn connect(self) -> Result<SseStream<'a, T>, Error> {
        let mut s = SseStream {
            client: self.client,
            url: self.url,
            headers: self.headers,
            options: self.options,
            decoder: SseDecoder::new(self.options.max_event_size),
            state: SseState::Disconnected,
            attempt: 0,
            server_retry: None,
        };
        s.open().await?;
        Ok(s)
    }
}

enum SseState<B> { Live(Response<B>), Disconnected, Terminated }

pub struct SseStream<'a, T: Transport> {
    client: &'a Client<T>,
    url: String,
    headers: http::HeaderMap,
    options: SseOptions,
    decoder: SseDecoder,
    state: SseState<T::Body>,
    attempt: u32,
    /// The server sent `retry:` — it overrides our policy.
    server_retry: Option<Duration>,
}

#[derive(Debug)] struct SseRejected(&'static str);
impl std::fmt::Display for SseRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not an SSE stream: {}", self.0)
    }
}
impl std::error::Error for SseRejected {}

impl<'a, T> SseStream<'a, T>
where
    T: Transport,
    T::Body: HttpBody<Data = Bytes> + Unpin,
    <T::Body as HttpBody>::Error: std::error::Error + 'static,
{
    pub fn last_event_id(&self) -> Option<&str> { self.decoder.last_event_id() }

    /// Open (or reopen) the stream.
    ///
    /// We send `Last-Event-ID` **only if it's non-empty**: `reqwest-eventsource`
    /// sends an empty header on the first reconnect, which the spec forbids.
    async fn open(&mut self) -> Result<(), Error> {
        let mut req = http::Request::builder()
            .uri(self.url.as_str())
            .header(http::header::ACCEPT, MIME);
        for (k, v) in self.headers.iter() {
            req = req.header(k, v);
        }
        if let Some(id) = self.decoder.last_event_id() {
            if !id.is_empty() {
                req = req.header("last-event-id", id);
            }
        }
        let resp = self.client
            .execute(req.body(RequestBody::Empty).map_err(|e|
                Error::new(ErrorKind::Other, e))?)
            .await?;

        // WHATWG: 204 — stop forever; any other non-200 is also a stop.
        if resp.status() == http::StatusCode::NO_CONTENT {
            self.state = SseState::Terminated;
            return Ok(());
        }
        if resp.status() != http::StatusCode::OK {
            self.state = SseState::Terminated;
            return Err(Error::new(ErrorKind::Status, SseRejected("status is not 200")));
        }
        let ok_ct = resp.headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim_start().starts_with(MIME));
        if !ok_ct {
            self.state = SseState::Terminated;
            return Err(Error::new(ErrorKind::Decode,
                                  SseRejected("content-type is not text/event-stream")));
        }

        let url = self.url.parse::<http::Uri>()
            .map_err(|e| Error::new(ErrorKind::Other, e))?;
        self.state = SseState::Live(Response::new(resp, url));
        self.attempt = 0;
        Ok(())
    }

    pub async fn next(&mut self) -> Option<Result<SseEvent, Error>> {
        loop {
            if let Some(e) = self.decoder.next() {
                if let SseEvent::Retry(d) = e {
                    self.server_retry = Some(d);
                }
                return Some(Ok(e));
            }
            match &mut self.state {
                SseState::Terminated => return None,
                SseState::Disconnected => {
                    if !self.options.reconnect { self.state = SseState::Terminated; return None }
                    let jitter = jitter_source();
                    let Some(delay) = self.options.backoff.delay(self.attempt, jitter)
                        else { self.state = SseState::Terminated; return None };
                    // The server's `retry:` is a lower bound, not a suggestion.
                    let delay = self.server_retry.map_or(delay, |s| s.max(delay));
                    self.attempt = self.attempt.saturating_add(1);
                    self.client.timer().sleep(delay).await;
                    if let Err(e) = self.open().await { return Some(Err(e)) }
                }
                SseState::Live(resp) => match resp.chunk().await {
                    Some(Ok(chunk)) => {
                        if let Err(e) = self.decoder.push(&chunk) {
                            // Exceeding the limit is fatal and not retried.
                            self.state = SseState::Terminated;
                            return Some(Err(Error::new(ErrorKind::Decode, e)));
                        }
                    }
                    Some(Err(e)) => { self.state = SseState::Disconnected; return Some(Err(e)) }
                    // A normal end of stream also triggers a reconnect: the
                    // server may have simply closed the connection.
                    None => self.state = SseState::Disconnected,
                },
            }
        }
    }
}

/// Full jitter. In tests, substituted with a deterministic value via
/// `SseOptions::backoff` with zero spread.
fn jitter_source() -> f64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).unwrap_or(());
    (u64::from_le_bytes(b) as f64) / (u64::MAX as f64)
}
```

`Client` gets `pub fn sse(&self, url: &str) -> SseBuilder<'_, T>` and
`pub(crate) fn timer(&self) -> &impl Timer` — the timer comes from the
client's configuration; it wasn't there in vertical 1, because there was no
reconnect.

Add `getrandom = "0.4"` to `http-ng`'s dependencies.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p http-ng --features test-util`
Expected: PASS, four reconnect tests plus everything from before.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng
git commit -m "feat(http-ng): SSE reconnect with Last-Event-ID, jitter and WHATWG terminal rules"
```

---

### Task 8: `http-ng` — `Client::new()` in the browser

**Files:**
- Modify: `crates/http-ng/Cargo.toml`, `src/lib.rs`
- Test: `crates/http-ng/tests/wasm_default.rs`

**Interfaces:**
- Produces: `DefaultTransport = http_ng_fetch::Fetch` for
  `wasm32-unknown-unknown`; `Client::new()` with no `Result`, because
  fetch's constructor can never fail.

- [ ] **Step 1: Write a failing test**

```rust
// crates/http-ng/tests/wasm_default.rs
#![cfg(all(target_family = "wasm", target_os = "unknown"))]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn the_two_line_example_from_the_readme_works_in_a_browser() {
    // Exactly the same code that works on native in vertical 2.
    let client = http_ng::Client::new();
    let text = client.get("data:text/plain,portable")
        .send().await.unwrap()
        .collect().await.unwrap()
        .text().unwrap();
    assert_eq!(text, "portable");
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo check -p http-ng --target wasm32-unknown-unknown --tests --features default-transport`
Expected: FAIL — `DefaultTransport` isn't defined for this target.

- [ ] **Step 3: Add the target dependency and the type**

```toml
# crates/http-ng/Cargo.toml
[target.'cfg(all(target_family = "wasm", target_os = "unknown"))'.dependencies]
http-ng-fetch = { path = "../http-ng-fetch", version = "0.1.0", optional = true }

[target.'cfg(all(target_family = "wasm", target_os = "unknown"))'.dev-dependencies]
wasm-bindgen-test = "0.3"
```

```rust
// in crates/http-ng/src/lib.rs
#[cfg(all(feature = "default-transport", target_family = "wasm", target_os = "unknown"))]
pub type DefaultTransport = http_ng_fetch::Fetch;

#[cfg(all(feature = "default-transport", target_family = "wasm", target_os = "unknown"))]
impl Client<DefaultTransport> {
    /// A client with the browser transport.
    ///
    /// No `Result`: fetch's constructor can't fail, and incompatible
    /// settings are rejected in `build()`.
    pub fn new() -> Self {
        Self::builder(http_ng_fetch::Fetch::new())
            .build()
            .expect("fetch transport with default config is always supported")
    }
}
```

- [ ] **Step 4: Run the wasm test**

Run: `wasm-pack test --headless --chrome crates/http-ng -- --features default-transport,test-util`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng
git commit -m "feat(http-ng): Client::new() on the browser target"
```

---

### Task 9: CI — three targets, two browsers, proof of no tokio

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:** nothing for code.

- [ ] **Step 1: Add the jobs**

```yaml
  browser:
    strategy:
      matrix:
        browser: [chrome, firefox]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: wasm32-unknown-unknown }
      - uses: jetli/wasm-pack-action@v0.4
      - name: browser tests
        run: |
          wasm-pack test --headless --${{ matrix.browser }} crates/http-ng-fetch
          wasm-pack test --headless --${{ matrix.browser }} crates/http-ng \
            -- --features default-transport,test-util

  ambient-has-no-tokio:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: wasm32-unknown-unknown }
      - name: fetch build must contain neither tokio nor hyper
        run: |
          cargo tree -p http-ng-fetch -e normal --prefix none \
            | grep -E '^(tokio|hyper|h2)' && exit 1 || true
      - name: proto must stay sans-io
        run: |
          cargo tree -p http-ng-proto -e normal --prefix none \
            | grep -Ei '^(tokio|futures-|async-)' && exit 1 || true

  fetch-must-fail-under-wasm-threads:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
        with: { targets: wasm32-unknown-unknown, components: rust-src }
      - name: the unsafe Send must be rejected when atomics are on
        run: |
          RUSTFLAGS="-Ctarget-feature=+atomics,+bulk-memory" \
            cargo +nightly check -p http-ng-fetch \
            --target wasm32-unknown-unknown -Zbuild-std=std,panic_abort \
            && { echo "expected a compile error"; exit 1; } || echo OK
```

**The `browser` job is the one that tests the design's central decision:**
in Chrome `streaming_request_body == true`, in Firefox `false`, and **the
same binary** must behave correctly in both. If `caps.rs`'s tests pass in
only one browser, the runtime registry doesn't work.

- [ ] **Step 2: Run locally what can be run**

Run: `cargo tree -p http-ng-fetch -e normal --prefix none | grep -E '^(tokio|hyper|h2)' && echo FAIL || echo OK`
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: browser matrix and machine-checked absence of tokio in ambient builds"
```

---

### Task 10: Acceptance — the `act` component builds with no logic changes

The central test of the `Transport` shape: a live consumer, written
**before** our library, must fit onto it with no rework.

**Files:**
- Create: `crates/http-ng/examples/portable.rs`
- Create: `docs/porting-wasi-fetch.md`

**Interfaces:** nothing for code.

- [ ] **Step 1: Write an example that mirrors `components/http-client`**

```rust
// crates/http-ng/examples/portable.rs
//! Mirrors the logic of `act/components/http-client/src/lib.rs` on http-ng.
//!
//! Builds for three targets with not a single `#[cfg]` in this file:
//!   cargo build --example portable
//!   cargo build --example portable --target wasm32-wasip2
//!   cargo build --example portable --target wasm32-unknown-unknown

use http_ng::{Client, RequestBody, Timeouts};

pub async fn fetch<T>(
    client: &Client<T>,
    url: &str,
    method: http::Method,
    headers: &[(String, String)],
    body: Option<Vec<u8>>,
    timeout_ms: Option<u64>,
) -> Result<(u16, http::HeaderMap, Vec<u8>), http_ng::Error>
where
    T: http_ng_core::unversioned::Transport,
    T::Body: http_body::Body<Data = bytes::Bytes> + Unpin,
    <T::Body as http_body::Body>::Error: std::error::Error + 'static,
{
    let mut req = client.request(method, url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    if let Some(b) = body {
        req = req.body(RequestBody::Full(bytes::Bytes::from(b)));
    }
    // Per-request timeout — something reqwest can't do at all (issue #2641).
    if let Some(ms) = timeout_ms {
        req = req.timeouts(Timeouts {
            first_byte: Some(std::time::Duration::from_millis(ms)),
            ..Default::default()
        });
    }

    let resp = req.send().await?;
    // Non-destructive read: status and headers stay alive after reading the body.
    let collected = resp.collect().await?;
    Ok((
        collected.status().as_u16(),
        collected.headers().clone(),
        collected.bytes().to_vec(),
    ))
}

fn main() {
    println!("builds for native, wasip2 and wasm32-unknown-unknown");
}
```

If `Response::new` turns out to be `pub(crate)`, write the example via
`client.get(url).send()` instead of `client.execute`, and drop the helper.

- [ ] **Step 2: Build for three targets**

Run:
```
cargo build -p http-ng --example portable
cargo build -p http-ng --example portable --target wasm32-wasip2
cargo build -p http-ng --example portable --target wasm32-unknown-unknown
```
Expected: three successful builds. **If even one needs a `#[cfg]` in the
example itself, the `Transport` shape is wrong, and that's the stop
criterion from the spec.**

- [ ] **Step 3: Write the `wasi-fetch` migration guide**

```markdown
<!-- docs/porting-wasi-fetch.md -->
# Migrating `wasi-fetch` → `http-ng`

`wasi-fetch` 0.2.0 (571 lines) breaks down like this:

| was | becomes |
|---|---|
| `Client` + `get/post/...` | `http_ng::Client<T>` |
| `RequestBuilder::{header, headers, body, json}` | `http_ng::RequestBuilder` |
| `timeout` (set connect **and** first_byte) | `Timeouts { connect, first_byte, between_bytes }` |
| `between_bytes_timeout` | same struct, as the third field |
| `redirect_limit` + a ~60-line loop | the `Redirect` stage in `http-ng` |
| `send_raw`, `BodyWriter`, `join!`, `to_wasi_method` | `http-ng-wasi` |
| `Body::{chunk, bytes, text, json}` | `http_ng::Response`/`Collected` methods |
| `Error::Transport(String)` | `http_ng::Error` with `ErrorKind` |
| seven `let _ =` on setters | `Capabilities` + `UnsupportedCapability` |

What the migration fixes:
1. **304 and 305 are no longer followed.** The old loop used
   `status.is_redirection()`.
2. **`Authorization` and `Cookie` get stripped** on a host **or** scheme
   change.
3. **301/302 with POST get downgraded to GET**, same as 303.
4. **Host rejections in option setters stop being silent.**

`wasi-fetch` 0.3 stays a thin facade (~40 lines) over
`http_ng::Client<WasiHttp>` with the old names: the crate stays findable,
users migrate with one line.
```

- [ ] **Step 4: Commit**

```bash
git add crates/http-ng/examples docs/porting-wasi-fetch.md
git commit -m "docs: portable example building for all three targets, wasi-fetch porting guide"
```

---

### Task 11: README and final check against the spec

**Files:**
- Modify: `README.md`
- Create: `docs/v01-acceptance.md`

- [ ] **Step 1: Update the README**

```markdown
## v0.1 status

| target | transport | tokio in the graph |
|---|---|---|
| native | `http-ng-native` (TCP + rustls + h1) | yes, `sync` on the h1 path |
| WASI | `http-ng-wasi` (`wasi:http` 0.3) | **no** |
| browser | `http-ng-fetch` | **no** |

Runtimes in CI: tokio, smol. HTTP/2, HTTP/3, connection pooling, WebSocket — v0.2+.
```

- [ ] **Step 2: Write the acceptance report**

```markdown
<!-- docs/v01-acceptance.md -->
# v0.1 acceptance

The four claims from spec §10, and what proves each one.

| claim | proof |
|---|---|
| The runtime seam is real | `crates/http-ng/tests/two_runtimes.rs` — one generic piece of code on tokio and smol, zero `#[cfg]`. Plus `crates/http-ng-native/tests/h1.rs` — an exchange on a bare `futures` executor with no spawn and no timer |
| The delegation seam is real | `http-ng-wasi` on top of `wasi:http` 0.3, where there's no socket at all |
| The capability model degrades honestly | `crates/http-ng-fetch/tests/caps.rs` in the Chrome + Firefox CI matrix: `streaming_request_body` differs **in one binary** |
| The `Transport` shape was guessed correctly | `crates/http-ng/examples/portable.rs` builds for three targets with no `#[cfg]` in the example itself |

## Deliberately not done in v0.1

Connection pooling; HTTP/2 and HTTP/3; streaming request bodies;
`first_byte` and `between_bytes` timeouts on native (declared unsupported,
not silently unimplemented); two `getaddrinfo` slots instead of one; h1
upgrade and WebSocket; hickory and DoH; Alt-Svc; middleware and
`http-ng-tower`; `http-ng-rmcp`.

## What remains unverified

`RequestBody::Streaming` doesn't pass through any transport: native buffers
it, fetch rejects it via `Capabilities`, wasi only takes `Full`. The replay
contract is covered by unit tests, but not by an end-to-end scenario. The
first real consumer is `http-ng-rmcp` in v0.2.
```

- [ ] **Step 3: Run everything**

Run:
```
cargo test --workspace --all-features
cargo check -p http-ng-wasi --target wasm32-wasip2
cargo check -p http-ng-fetch --target wasm32-unknown-unknown
```
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add README.md docs/v01-acceptance.md
git commit -m "docs: v0.1 acceptance report mapping spec claims to their proofs"
```

---

## What's left outside v0.1

Everything from spec §10 for v0.2 and beyond: h2 via ALPN with the executor
as typestate; a pool with body draining and idle eviction; `AltSvcCache`;
decompression; async `CookieStore`; retry with a typed replayable body;
middleware and `http-ng-tower`; `http-ng-dns-hickory` with SVCB;
`http-ng-tls-native`; multipart, proxy, base URL; and `http-ng-rmcp` as the
second verification loop.
