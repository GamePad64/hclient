# http-ng v0.1, вертикаль 3: fetch и приёмка — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Тот же прикладной код работает в браузере; рантайм-модель
`Capabilities` проверена единственным бэкендом, где возможности различаются **в
рантайме**; SSE умеет переподключаться; и всё это подтверждено живым
потребителем — компонентом `act`.

**Architecture:** `http-ng-fetch` — третий `Transport`, зависящий только от
Tier A. Ключевое: свой промис-адаптер на `Arc<Mutex<..>>` вместо
`wasm_bindgen_futures::JsFuture`, потому что у последнего внутри `Rc<RefCell<..>>`
и он `!Send` **без нужды**. `Capabilities` заполняется по результатам
рантайм-проб, а не `cfg!`. Реконнект SSE — стадия поверх уже существующего
декодера.

**Tech Stack:** `wasm-bindgen` 0.2.126, `web-sys` 0.3.103, `js-sys`,
`wasm-streams` 0.6, `wasm-bindgen-test` 0.3, `wasm-bindgen-futures` (только для
`spawn_local` в тестах).

## Global Constraints

Наследуются из вертикалей 1 и 2 и дополняются:

- **`http-ng-fetch` зависит только от Tier A** (`http-ng-core`, `http-ng-proto`).
  Ни hyper, ни tokio, ни `http-ng-rt` в его графе быть не должно — проверяется
  в CI.
- **Единственный `unsafe impl Send` во всём проекте** живёт в `http-ng-fetch`,
  на одном типе, под `#[cfg(not(target_feature = "atomics"))]`, и зеркалит то,
  что делает сам wasm-bindgen для `JsValue`. Крейт получает
  `#![deny(unsafe_code)]` с одним точечным `#[allow]` и комментарием-обоснованием.
- **`Capabilities` заполняются рантайм-пробами, а не `cfg!`.** Один и тот же
  wasm-бинарь работает и в Chrome, и в Safari.
- **Ни одного тихого no-op.** Всё, чего fetch не может, объявлено в
  `Capabilities` и отвергается в `build()`.
- MSRV 1.85.

## Файловая структура

```
crates/http-ng-proto/
  src/backoff.rs             чистый backoff с джиттером (задача 6)
crates/http-ng-fetch/
  src/lib.rs                 Fetch: Transport
  src/promise.rs             SendJsFuture — Send-совместимый адаптер промисов
  src/caps.rs                рантайм-пробы возможностей
  src/convert.rs             http <-> web_sys, запрещённые заголовки
  src/body.rs                тело ответа поверх ReadableStream
crates/http-ng/
  src/sse.rs                 реконнект (модификация)
```

---

### Task 1: `http-ng-fetch` — Send-совместимый адаптер промисов

Проверено спайком заранее: приём собирается без atomics и **корректно
отвергается компилятором** с atomics.

**Files:**
- Create: `crates/http-ng-fetch/Cargo.toml`, `src/lib.rs`, `src/promise.rs`
- Test: `crates/http-ng-fetch/tests/promise.rs` (wasm-bindgen-test)

**Interfaces:**
- Produces:
  - `pub(crate) struct SendJsFuture`; `SendJsFuture::new(p: js_sys::Promise) -> Self`;
    `impl Future for SendJsFuture { type Output = Result<JsValue, JsValue>; }`
  - `pub(crate) struct SingleThreaded<T>(T)` — `Send` только без `+atomics`

- [ ] **Step 1: Написать падающий тест**

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
    // Главное утверждение: `!Send` — свойство сборки с wasm-потоками,
    // а не браузера. Без `+atomics` всё Send.
    fn assert_send<T: Send>() {}
    assert_send::<http_ng_fetch::testing::SendJsFutureAlias>();
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests`
Expected: FAIL — крейта нет.

- [ ] **Step 3: Создать крейт**

```toml
# crates/http-ng-fetch/Cargo.toml
[package]
name = "http-ng-fetch"
version = "0.1.0"
description = "Транспорт http-ng поверх браузерного fetch"
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

- [ ] **Step 4: Реализовать адаптер**

```rust
// crates/http-ng-fetch/src/promise.rs
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use wasm_bindgen::prelude::*;

/// Тот же приём, который wasm-bindgen применяет к самому `JsValue`.
///
/// `JsValue` — индекс в таблице, которой владеет сгенерированный JS-glue.
/// Пока модуль собран **без** `target_feature = "atomics"`, инстанс один,
/// таблица одна, потоков нет — и upstream сам объявляет
/// `unsafe impl Send for JsValue` под тем же `cfg`. С `+atomics` каждый worker
/// получает свою таблицу, и компилятор корректно нас отвергает.
#[repr(transparent)]
pub(crate) struct SingleThreaded<T>(pub(crate) T);

#[allow(unsafe_code, reason = "зеркалит wasm_bindgen::JsValue: без wasm-потоков процесс однопоточен по построению")]
#[cfg(not(target_feature = "atomics"))]
unsafe impl<T> Send for SingleThreaded<T> {}

#[derive(Default)]
struct State {
    result: Option<Result<JsValue, JsValue>>,
    waker: Option<Waker>,
}

/// `Send`-совместимая замена `wasm_bindgen_futures::JsFuture`.
///
/// `JsFuture` держит внутри `Rc<RefCell<futures::Inner>>`
/// (js-sys-0.3.103/src/futures/mod.rs:118) и потому `!Send` — но это выбор
/// реализации, а не свойство платформы: сам `JsValue`, `js_sys::Promise` и
/// `web_sys::{Request, Response, ReadableStream}` **являются** `Send` на
/// дефолтном таргете.
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
//! Транспорт http-ng поверх браузерного `fetch`.
//!
//! Зависит **только** от Tier A: ни hyper, ни tokio, ни `http-ng-rt` в графе.
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

- [ ] **Step 5: Запустить wasm-тесты**

Run: `cargo install wasm-bindgen-cli --locked` (если ещё нет), затем
`wasm-pack test --headless --chrome crates/http-ng-fetch` либо
`cargo test -p http-ng-fetch --target wasm32-unknown-unknown`
Expected: PASS, три теста.

Если headless-браузера в окружении нет — оставить
`cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests` как шлюз
и завести issue; прогон в браузере включается в CI (Task 9).

- [ ] **Step 6: Проверить, что с atomics сборка честно ломается**

Run: `RUSTFLAGS="-Ctarget-feature=+atomics,+bulk-memory" cargo +nightly check -p http-ng-fetch --target wasm32-unknown-unknown -Zbuild-std=std,panic_abort`
Expected: FAIL с `*mut u8 cannot be sent between threads safely` — это
**желаемое** поведение, а не дефект: с wasm-потоками приём небезопасен, и
компилятор обязан это сказать.

- [ ] **Step 7: Commit**

```bash
git add crates/http-ng-fetch
git commit -m "feat(fetch): Send-capable promise adapter mirroring wasm-bindgen's own reasoning"
```

---

### Task 2: `http-ng-fetch` — рантайм-пробы возможностей

Единственное место в проекте, где `Capabilities` **действительно** меняются в
рантайме. Ради этого мы и выбрали реестр вместо `cfg`.

**Files:**
- Create: `crates/http-ng-fetch/src/caps.rs`
- Test: `crates/http-ng-fetch/tests/caps.rs`

**Interfaces:**
- Produces:
  - `pub(crate) fn probe() -> Capabilities`
  - `pub(crate) fn supports_duplex() -> bool` — проверяет, читается ли
    `Request.prototype.duplex`, как предписывает whatwg/fetch
  - `pub const FORBIDDEN_HEADERS: [http::HeaderName; N]`

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/http-ng-fetch/tests/caps.rs
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn declares_what_fetch_genuinely_cannot_do() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    // Ни трейлеров, ни 1xx, ни выбора версии — whatwg/fetch#772 предлагает
    // API трейлеров вообще удалить.
    assert!(!c.request_trailers);
    assert!(!c.response_trailers);
    assert!(!c.informational_1xx);
    assert!(!c.version_select);
    assert!(!c.version_reported);
    // Ни TLS, ни клиентских сертификатов, ни прокси.
    assert_eq!(c.tls_config, http_ng_core::TlsSupport::None);
    assert!(!c.client_certs);
    assert!(!c.proxy);
    // Cookie и кэш — ambient, ими владеет браузер.
    assert!(c.owns_cookie_jar);
    assert!(c.owns_cache);
    // Upgrade недостижим: WebSocket в браузере — отдельный global.
    assert_eq!(c.upgrade, http_ng_core::UpgradeSupport::None);
}

#[wasm_bindgen_test]
fn only_the_connect_deadline_exists_and_it_is_one_for_everything() {
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    // AbortSignal — один дедлайн на весь обмен. Заявлять три раздельных
    // таймаута было бы ложью.
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
        assert!(names.contains(&must), "{must} должен быть в списке");
    }
}

#[wasm_bindgen_test]
fn duplex_support_is_probed_not_assumed() {
    // В Chrome 131+ — true, в Firefox и Safari — false. Один бинарь.
    let c = http_ng_fetch::Fetch::new().capabilities_for_test();
    assert_eq!(c.streaming_request_body, c.full_duplex,
               "в fetch дуплекс и стриминг тела запроса — одно и то же");
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests`
Expected: FAIL — `Fetch` не найден.

- [ ] **Step 3: Реализовать пробы**

```rust
// crates/http-ng-fetch/src/caps.rs
use http_ng_core::{Capabilities, RedirectSupport, TimeoutSupport, TlsSupport,
                   UpgradeSupport};

/// Заголовки, которые fetch запрещает выставлять. Список — из спецификации
/// WHATWG Fetch (forbidden request-headers). Мы их **объявляем**, а не молча
/// выбрасываем: молчаливое выбрасывание `Proxy-*` или `Cookie` — источник
/// ошибок безопасности.
// ВНИМАНИЕ (amendment-C4): `static FORBIDDEN: &[HeaderName] = &[..]` НЕ
// компилируется на stable — E0492 для любого заголовка, потому что тип
// HeaderName содержит вариант Custom поверх Bytes с AtomicPtr. Работает
// const-массив (как здесь) либо Box::leak/OnceLock для среза. Проверить
// форму компиляцией до написания всего списка.
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

/// Читается ли `Request.prototype.duplex`.
///
/// BCD `api.Request.duplex`: chrome/edge/webview **131** (2024-11-12),
/// Firefox `false` (bugzil.la/1792434), Safari/iOS `false`
/// (webkit.org/b/245671). Один и тот же wasm-бинарь крутится во всех трёх —
/// поэтому проба рантаймовая, а не `cfg!`.
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
    // Стриминг тела запроса в fetch возможен только вместе с duplex:"half".
    c.streaming_request_body = duplex;
    c.full_duplex = duplex;
    // Браузер владеет редиректами; политику задать можно, наблюдать хопы нельзя
    // (`redirect: "manual"` даёт opaqueredirect со status 0 и без заголовков).
    c.redirects = RedirectSupport::Configurable;
    c.owns_cookie_jar = true;
    c.owns_cache = true;
    // AbortSignal — один дедлайн на весь обмен, ни один из трёх фазовых
    // таймаутов им не выражается.
    c.timeouts = TimeoutSupport { connect: false, first_byte: false, between_bytes: false };
    c.tls_config = TlsSupport::None;
    c.upgrade = UpgradeSupport::None;
    c.forbidden_request_headers = &FORBIDDEN_HEADERS;
    c
}
```

- [ ] **Step 4: Добавить `Fetch` со скелетом и тестовым аксессором**

```rust
// в crates/http-ng-fetch/src/lib.rs
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

- [ ] **Step 5: Запустить тесты**

Run: `wasm-pack test --headless --chrome crates/http-ng-fetch`
Expected: PASS. В Chrome `streaming_request_body == true`; тот же бинарь в
Firefox даст `false` — это и проверяется в CI (Task 9).

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-fetch
git commit -m "feat(fetch): runtime capability probing, the reason the registry exists"
```

---

### Task 3: `http-ng-fetch` — конверсия запроса

**Files:**
- Create: `crates/http-ng-fetch/src/convert.rs`
- Test: `crates/http-ng-fetch/tests/convert.rs`

**Interfaces:**
- Consumes: `FORBIDDEN_HEADERS`, `supports_duplex` (Task 2).
- Produces:
  - `pub(crate) fn to_web_request(req: http::Request<RequestBody>, caps: &Capabilities) -> Result<(web_sys::Request, Option<web_sys::AbortController>), Error>`
  - `pub(crate) fn check_headers(h: &http::HeaderMap, caps: &Capabilities) -> Result<(), Error>`

- [ ] **Step 1: Написать падающий тест**

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
        return; // в Chrome это поддержано — проверять нечего
    }
    let req = http::Request::builder()
        .uri("https://example.com/")
        .body(RequestBody::rewindable(|| RequestBody::Empty)).unwrap();
    // Rewindable буферизуемо, поэтому проходит; Streaming — нет.
    assert!(http_ng_fetch::testing::to_web_request(&f, req).is_ok());
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

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

/// Проверяет заголовки **до** отправки. Молчаливое выбрасывание запрещённых
/// заголовков — источник ошибок безопасности: пользователь думает, что послал
/// `Cookie`, а его нет.
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
            // Chrome требует `duplex: "half"`, а web-sys 0.3.103 не имеет
            // `set_duplex` — ставим через Reflect.
            js_sys::Reflect::set(&init, &JsValue::from_str("duplex"),
                                 &JsValue::from_str("half")).map_err(js_err)?;
            // Полноценный ReadableStream из RequestBody::Streaming — v0.2;
            // до тех пор сюда попасть нельзя, потому что Capabilities
            // объявляют streaming_request_body только вместе с duplex.
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

- [ ] **Step 4: Экспортировать тестовый хелпер и запустить тесты**

```rust
// в testing-модуле lib.rs
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

### Task 4: `http-ng-fetch` — тело ответа поверх ReadableStream

**Files:**
- Create: `crates/http-ng-fetch/src/body.rs`
- Test: `crates/http-ng-fetch/tests/body.rs`

**Interfaces:**
- Produces: `pub struct Body`; `impl http_body::Body for Body { type Data = Bytes; type Error = Error }`;
  `Body::empty()`; `Body::from_response(&web_sys::Response) -> Result<Self, Error>`

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/http-ng-fetch/tests/body.rs
#![cfg(target_arch = "wasm32")]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn streams_a_response_body_in_chunks() {
    // data:-URL даёт детерминированный ответ без сети.
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

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

```rust
// crates/http-ng-fetch/src/body.rs
use crate::convert::js_err;
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};
use http_ng_core::Error;
use std::pin::Pin;
use std::task::{Context, Poll};
use wasm_bindgen::JsCast;

/// Тело ответа поверх `ReadableStream`.
///
/// Трейлеры не поддерживаются: их нет в fetch ни в одну сторону
/// (whatwg/fetch#772 предлагает удалить API вовсе). `Capabilities` это
/// объявляют, поэтому здесь их просто нет.
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

Добавить `futures-util` с фичей `std` ради `StreamExt::map`.

- [ ] **Step 4: Запустить тесты**

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

- [ ] **Step 1: Написать падающий тест**

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

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo check -p http-ng-fetch --target wasm32-unknown-unknown --tests`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

```rust
// в crates/http-ng-fetch/src/lib.rs
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

        // `fetch` живёт и в Window, и в WorkerGlobalScope — берём через global,
        // чтобы работать в обоих контекстах.
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
                // Сетевую ошибку fetch отличает только по TypeError —
                // большего браузер не даёт, и это записано в Capabilities.
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

- [ ] **Step 4: Запустить тесты**

Run: `wasm-pack test --headless --chrome crates/http-ng-fetch`
Expected: PASS.

- [ ] **Step 5: Проверить, что в графе нет hyper и tokio**

Run: `cargo tree -p http-ng-fetch -e normal --prefix none | grep -E '^(hyper|tokio)' && exit 1 || echo OK`
Expected: `OK` — **это и есть обещание «ambient-сборка без tokio»**.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-fetch
git commit -m "feat(fetch): Transport implementation, ambient build with zero tokio"
```

---

### Task 6: `http-ng-proto` — backoff с джиттером

Чистый автомат: принимает номер попытки и «случайность» параметром, поэтому
проверяется без часов и без генератора. Джиттер не делает **ни один** из четырёх
существующих SSE-крейтов.

**Files:**
- Create: `crates/http-ng-proto/src/backoff.rs`
- Modify: `crates/http-ng-proto/src/lib.rs`
- Test: внутри `backoff.rs`

**Interfaces:**
- Produces:
  - `pub struct Backoff { pub base: Duration, pub max: Duration, pub max_attempts: Option<u32> }` (`Default` = 1 с / 30 с / `None`)
  - `Backoff::delay(&self, attempt: u32, jitter: f64) -> Option<Duration>` —
    `jitter` в `[0.0, 1.0)`; `None` означает «прекратить»

- [ ] **Step 1: Написать падающие тесты**

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
        // 2^40 секунд переполнило бы u32::pow — проверяем, что нет паники.
        assert_eq!(b().delay(40, 0.0), Some(Duration::from_secs(30)));
        assert_eq!(b().delay(u32::MAX, 0.0), Some(Duration::from_secs(30)));
    }

    #[test]
    fn jitter_only_ever_reduces_the_delay() {
        // Full jitter (AWS-модель): случайная точка в [0, delay].
        let full = b().delay(3, 0.0).unwrap();
        let jittered = b().delay(3, 0.999).unwrap();
        assert!(jittered <= full);
        assert!(b().delay(3, 0.5).unwrap() <= full);
    }

    #[test]
    fn stops_after_max_attempts() {
        let b = Backoff { max_attempts: Some(3), ..Backoff::default() };
        assert!(b.delay(2, 0.0).is_some());
        assert!(b.delay(3, 0.0).is_none(), "четвёртая попытка запрещена");
    }

    #[test]
    fn unlimited_by_default_which_must_be_a_conscious_choice() {
        // ExponentialBackoff у rmcp имеет max_times: None и `2u32.pow(n)`,
        // который паникует при переполнении примерно после 32 попыток.
        assert!(b().max_attempts.is_none());
        assert!(b().delay(1000, 0.0).is_some(), "и при этом не паникует");
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-proto backoff`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

```rust
// crates/http-ng-proto/src/backoff.rs
//! Экспоненциальный backoff с full jitter. Чистый: случайность приходит
//! параметром, поэтому поведение проверяется без генератора и без часов.

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
    /// `attempt` считается с нуля. `jitter` — в `[0.0, 1.0)`.
    /// `None` означает «больше не пробовать».
    pub fn delay(&self, attempt: u32, jitter: f64) -> Option<Duration> {
        if let Some(limit) = self.max_attempts {
            if attempt >= limit {
                return None;
            }
        }
        // Насыщающее возведение в степень: `2u32.pow(n)` паникует примерно
        // после 32 попыток — ровно этот дефект живёт в rmcp.
        let factor = 1u64.checked_shl(attempt.min(63)).unwrap_or(u64::MAX);
        let raw = self.base.checked_mul(factor.min(u32::MAX as u64) as u32)
            .unwrap_or(self.max)
            .min(self.max);
        // Full jitter: равномерная точка в [0, raw].
        let scaled = raw.as_secs_f64() * (1.0 - jitter.clamp(0.0, 1.0));
        Some(Duration::from_secs_f64(scaled.max(0.0)))
    }
}
```

- [ ] **Step 4: Подключить и запустить**

Добавить `pub mod backoff;` в `crates/http-ng-proto/src/lib.rs`.

Run: `cargo test -p http-ng-proto`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-proto
git commit -m "feat(proto): jittered exponential backoff that cannot overflow"
```

---

### Task 7: `http-ng` — реконнект SSE

**Files:**
- Modify: `crates/http-ng/src/sse.rs`
- Test: `crates/http-ng/tests/sse_reconnect.rs`

**Interfaces:**
- Consumes: `Backoff` (Task 6), `SseDecoder`, `Client` (вертикаль 1).
- Produces:
  - `pub struct SseOptions { pub max_event_size: usize, pub backoff: Backoff, pub reconnect: bool }`
  - `Client::sse(&self, url: &str) -> SseBuilder<'_, T>`; `SseBuilder::{header, options, connect}`
  - `SseStream::next` теперь переподключается, подставляя `Last-Event-ID`
  - Терминальные правила: **204 — прекратить навсегда**; статус ≠ 200 —
    прекратить; `Content-Type` ≠ `text/event-stream` — прекратить;
    `EventTooLarge` — прекратить (фатально, не ретраится)

- [ ] **Step 1: Написать падающие тесты**

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
    m.push_response(sse("id: 7\ndata: first\n\n"));   // поток оборвётся на EOF
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
    assert_eq!(seen.len(), 2, "второй запрос — это реконнект");
    assert_eq!(seen[1].headers.get("last-event-id").unwrap(), "7",
               "реконнект обязан подставить последний id");
    assert!(seen[0].headers.get("last-event-id").is_none(),
            "на первом запросе id ещё нет — пустой слать нельзя");
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
               "204 означает «прекрати», а не «попробуй ещё раз»");
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
    assert_eq!(c.transport().requests().len(), 1, "переподключаться нельзя");
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng --features test-util --test sse_reconnect`
Expected: FAIL — `no method named sse`.

- [ ] **Step 3: Реализовать**

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
    /// Сервер прислал `retry:` — он перекрывает нашу политику.
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

    /// Открыть (или переоткрыть) поток.
    ///
    /// `Last-Event-ID` шлём **только если он непуст**: `reqwest-eventsource`
    /// шлёт пустой заголовок на первом реконнекте, что спека запрещает.
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

        // WHATWG: 204 — прекратить навсегда; любой иной не-200 — тоже стоп.
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
                    // Серверный `retry:` — нижняя граница, а не подсказка.
                    let delay = self.server_retry.map_or(delay, |s| s.max(delay));
                    self.attempt = self.attempt.saturating_add(1);
                    self.client.timer().sleep(delay).await;
                    if let Err(e) = self.open().await { return Some(Err(e)) }
                }
                SseState::Live(resp) => match resp.chunk().await {
                    Some(Ok(chunk)) => {
                        if let Err(e) = self.decoder.push(&chunk) {
                            // Превышение лимита фатально и не ретраится.
                            self.state = SseState::Terminated;
                            return Some(Err(Error::new(ErrorKind::Decode, e)));
                        }
                    }
                    Some(Err(e)) => { self.state = SseState::Disconnected; return Some(Err(e)) }
                    // Штатный конец потока тоже реконнектится: сервер мог
                    // просто закрыть соединение.
                    None => self.state = SseState::Disconnected,
                },
            }
        }
    }
}

/// Full jitter. В тестах подменяется детерминированным значением через
/// `SseOptions::backoff` с нулевым разбросом.
fn jitter_source() -> f64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).unwrap_or(());
    (u64::from_le_bytes(b) as f64) / (u64::MAX as f64)
}
```

`Client` получает `pub fn sse(&self, url: &str) -> SseBuilder<'_, T>` и
`pub(crate) fn timer(&self) -> &impl Timer` — таймер приходит из конфигурации
клиента; в вертикали 1 его не было, потому что реконнекта не было.

Добавить `getrandom = "0.4"` в зависимости `http-ng`.

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng --features test-util`
Expected: PASS, четыре теста реконнекта плюс всё предыдущее.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng
git commit -m "feat(http-ng): SSE reconnect with Last-Event-ID, jitter and WHATWG terminal rules"
```

---

### Task 8: `http-ng` — `Client::new()` в браузере

**Files:**
- Modify: `crates/http-ng/Cargo.toml`, `src/lib.rs`
- Test: `crates/http-ng/tests/wasm_default.rs`

**Interfaces:**
- Produces: `DefaultTransport = http_ng_fetch::Fetch` для
  `wasm32-unknown-unknown`; `Client::new()` без `Result`, потому что у fetch
  конструктор не может отказать.

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/http-ng/tests/wasm_default.rs
#![cfg(all(target_family = "wasm", target_os = "unknown"))]
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn the_two_line_example_from_the_readme_works_in_a_browser() {
    // Ровно тот же код, что в вертикали 2 работает на native.
    let client = http_ng::Client::new();
    let text = client.get("data:text/plain,portable")
        .send().await.unwrap()
        .collect().await.unwrap()
        .text().unwrap();
    assert_eq!(text, "portable");
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo check -p http-ng --target wasm32-unknown-unknown --tests --features default-transport`
Expected: FAIL — `DefaultTransport` для этого таргета не определён.

- [ ] **Step 3: Добавить таргет-зависимость и тип**

```toml
# crates/http-ng/Cargo.toml
[target.'cfg(all(target_family = "wasm", target_os = "unknown"))'.dependencies]
http-ng-fetch = { path = "../http-ng-fetch", version = "0.1.0", optional = true }

[target.'cfg(all(target_family = "wasm", target_os = "unknown"))'.dev-dependencies]
wasm-bindgen-test = "0.3"
```

```rust
// в crates/http-ng/src/lib.rs
#[cfg(all(feature = "default-transport", target_family = "wasm", target_os = "unknown"))]
pub type DefaultTransport = http_ng_fetch::Fetch;

#[cfg(all(feature = "default-transport", target_family = "wasm", target_os = "unknown"))]
impl Client<DefaultTransport> {
    /// Клиент с браузерным транспортом.
    ///
    /// Без `Result`: у fetch конструктор отказать не может, а несовместимые
    /// настройки отвергаются в `build()`.
    pub fn new() -> Self {
        Self::builder(http_ng_fetch::Fetch::new())
            .build()
            .expect("fetch transport with default config is always supported")
    }
}
```

- [ ] **Step 4: Запустить wasm-тест**

Run: `wasm-pack test --headless --chrome crates/http-ng -- --features default-transport,test-util`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng
git commit -m "feat(http-ng): Client::new() on the browser target"
```

---

### Task 9: CI — три таргета, два браузера, доказательство отсутствия tokio

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:** ничего для кода.

- [ ] **Step 1: Добавить job'ы**

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
            && { echo "ожидалась ошибка компиляции"; exit 1; } || echo OK
```

**Job `browser` — единственный, который проверяет главное решение дизайна:**
в Chrome `streaming_request_body == true`, в Firefox `false`, и **один и тот же
бинарь** должен вести себя корректно в обоих. Если тесты `caps.rs` пройдут
только в одном браузере — рантайм-реестр не работает.

- [ ] **Step 2: Прогнать локально то, что можно**

Run: `cargo tree -p http-ng-fetch -e normal --prefix none | grep -E '^(tokio|hyper|h2)' && echo FAIL || echo OK`
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: browser matrix and machine-checked absence of tokio in ambient builds"
```

---

### Task 10: Приёмка — компонент `act` собирается без изменений логики

Главная проверка формы `Transport`: живой потребитель, написанный **до** нашей
библиотеки, должен лечь на неё без переделки.

**Files:**
- Create: `crates/http-ng/examples/portable.rs`
- Create: `docs/porting-wasi-fetch.md`

**Interfaces:** ничего для кода.

- [ ] **Step 1: Написать пример, повторяющий `components/http-client`**

```rust
// crates/http-ng/examples/portable.rs
//! Повторяет логику `act/components/http-client/src/lib.rs` на http-ng.
//!
//! Собирается под три таргета без единого `#[cfg]` в этом файле:
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
    // Per-request таймаут — то, чего reqwest не умеет вовсе (issue #2641).
    if let Some(ms) = timeout_ms {
        req = req.timeouts(Timeouts {
            first_byte: Some(std::time::Duration::from_millis(ms)),
            ..Default::default()
        });
    }

    let resp = req.send().await?;
    // Неразрушающее чтение: статус и заголовки живы после чтения тела.
    let collected = resp.collect().await?;
    Ok((
        collected.status().as_u16(),
        collected.headers().clone(),
        collected.bytes().to_vec(),
    ))
}

fn main() {
    println!("собирается под native, wasip2 и wasm32-unknown-unknown");
}
```

Если `Response::new` окажется `pub(crate)` — сделать пример через
`client.get(url).send()`, а не `client.execute`, и убрать хелпер.

- [ ] **Step 2: Собрать под три таргета**

Run:
```
cargo build -p http-ng --example portable
cargo build -p http-ng --example portable --target wasm32-wasip2
cargo build -p http-ng --example portable --target wasm32-unknown-unknown
```
Expected: три успешные сборки. **Если хоть одна требует `#[cfg]` в самом
примере — форма `Transport` неверна, и это критерий остановки из спеки.**

- [ ] **Step 3: Написать руководство по переезду `wasi-fetch`**

```markdown
<!-- docs/porting-wasi-fetch.md -->
# Переезд `wasi-fetch` → `http-ng`

`wasi-fetch` 0.2.0 (571 строка) раскладывается так:

| было | стало |
|---|---|
| `Client` + `get/post/...` | `http_ng::Client<T>` |
| `RequestBuilder::{header, headers, body, json}` | `http_ng::RequestBuilder` |
| `timeout` (ставил connect **и** first_byte) | `Timeouts { connect, first_byte, between_bytes }` |
| `between_bytes_timeout` | там же, третьим полем |
| `redirect_limit` + цикл ~60 строк | стадия `Redirect` в `http-ng` |
| `send_raw`, `BodyWriter`, `join!`, `to_wasi_method` | `http-ng-wasi` |
| `Body::{chunk, bytes, text, json}` | методы `http_ng::Response`/`Collected` |
| `Error::Transport(String)` | `http_ng::Error` с `ErrorKind` |
| семь `let _ =` на сеттерах | `Capabilities` + `UnsupportedCapability` |

Что чинится переездом:
1. **304 и 305 больше не следуются.** Прежний цикл использовал
   `status.is_redirection()`.
2. **`Authorization` и `Cookie` снимаются** при смене host **и scheme**.
3. **301/302 с POST понижаются до GET** наравне с 303.
4. **Отказы хоста в сеттерах опций перестают быть тихими.**

`wasi-fetch` 0.3 остаётся тонким фасадом (~40 строк) над
`http_ng::Client<WasiHttp>` со старыми именами: крейт остаётся findable,
пользователи мигрируют одной строкой.
```

- [ ] **Step 4: Commit**

```bash
git add crates/http-ng/examples docs/porting-wasi-fetch.md
git commit -m "docs: portable example building for all three targets, wasi-fetch porting guide"
```

---

### Task 11: README и финальная сверка со спекой

**Files:**
- Modify: `README.md`
- Create: `docs/v01-acceptance.md`

- [ ] **Step 1: Обновить README**

```markdown
## Статус v0.1

| таргет | транспорт | tokio в графе |
|---|---|---|
| native | `http-ng-native` (TCP + rustls + h1) | да, `sync` на h1-пути |
| WASI | `http-ng-wasi` (`wasi:http` 0.3) | **нет** |
| браузер | `http-ng-fetch` | **нет** |

Рантаймы в CI: tokio, smol. HTTP/2, HTTP/3, пул соединений, WebSocket — v0.2+.
```

- [ ] **Step 2: Написать отчёт о приёмке**

```markdown
<!-- docs/v01-acceptance.md -->
# Приёмка v0.1

Четыре утверждения из спеки §10 и чем каждое доказано.

| утверждение | доказательство |
|---|---|
| Runtime-шов настоящий | `crates/http-ng/tests/two_runtimes.rs` — один обобщённый код на tokio и smol, ноль `#[cfg]`. Плюс `crates/http-ng-native/tests/h1.rs` — обмен на голом `futures`-executor'е без spawn и таймера |
| Delegation-шов настоящий | `http-ng-wasi` поверх `wasi:http` 0.3, где сокета нет вовсе |
| Capability-модель деградирует честно | `crates/http-ng-fetch/tests/caps.rs` в CI-матрице Chrome + Firefox: `streaming_request_body` различается **в одном бинаре** |
| Форма `Transport` угадана верно | `crates/http-ng/examples/portable.rs` собирается под три таргета без `#[cfg]` в самом примере |

## Осознанно не сделано в v0.1

Пул соединений; HTTP/2 и HTTP/3; стриминговые тела запроса; `first_byte` и
`between_bytes` таймауты на native (объявлены неподдерживаемыми, а не сделаны
молча); два слота `getaddrinfo` вместо одного; h1-upgrade и WebSocket;
hickory и DoH; Alt-Svc; middleware и `http-ng-tower`; `http-ng-rmcp`.

## Что осталось непроверенным

`RequestBody::Streaming` не проходит ни через один транспорт: native буферизует,
fetch отвергает по `Capabilities`, wasi берёт только `Full`. Контракт replay
проверен юнит-тестами, но не сквозным сценарием. Первый настоящий потребитель —
`http-ng-rmcp` в v0.2.
```

- [ ] **Step 3: Прогнать всё**

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

## Что осталось за пределами v0.1

Всё из §10 спеки для v0.2 и дальше: h2 через ALPN с executor'ом как typestate;
пул с дренажом тела и idle-эвикцией; `AltSvcCache`; decompression; async
`CookieStore`; retry с типизированным replayable-телом; middleware и
`http-ng-tower`; `http-ng-dns-hickory` с SVCB; `http-ng-tls-native`; multipart,
proxy, base URL; и `http-ng-rmcp` вторым проверочным контуром.
