# http-ng — дизайн

**Дата:** 2026-08-05
**Статус:** утверждён к реализации
**Проверено на:** rustc 1.97.1 (2026-07-14), crates.io на 2026-08-05

Асинхронный HTTP-клиент на Rust: один и тот же прикладной код собирается под
native, браузер и WASI, потому что транспорт подменяется, а не обкладывается
`#[cfg]`.

**Цель реализации по этой спеке — v0.1 (§10).** Остальные версии описаны здесь,
чтобы решения v0.1 не закрывали дорогу позже; отдельный план реализации пишется
только под v0.1.

---

## 1. Мотивация и позиционирование

**Движущая сила — кроссплатформенность.** Написать приложение один раз и собрать
его под server, browser (WASM) и WASI. Отсюда приоритеты: ambient-бэкенды и
единый API важнее, чем полный паритет с reqwest.

**Ниша.** reqwest модуляризуется, но по оси *middleware*, не по осям *runtime* и
*backend*:

- [seanmonstar/reqwest#2585](https://github.com/seanmonstar/reqwest/issues/2585) —
  «Meta: more modular pieces», разложение пула, прокси, редиректов и декомпрессии
  на tower-Service'ы.
- [#557](https://github.com/seanmonstar/reqwest/issues/557) «Allow setting Hyper
  executor» — открыт с 2019-07-04, до сих пор не решён.
- [discussion #2486](https://github.com/seanmonstar/reqwest/discussions/2486) —
  пользователь просит подставить свой backend-Service (у него wasip1/Extism);
  ответ мейнтейнера — «используйте `tower::Service`», но только в обратную
  сторону (Client *как* Service, не Service *под* Client).

То есть подставляемый транспорт и рантайм-нейтральность в reqwest не приедут.
Это и есть наша ниша.

**Историческое предупреждение.** Кладбище в этой нише состоит целиком из
проектов, обещавших N рантаймов и тестировавших один: `hreq` («set out with the
ambition to be runtime agnostic… in practice that was not a viable route»),
`surf`/`http-client` (7.4M загрузок, мёртв с 2022-06-20 вместе с async-std,
официально прекращённым 2025-03-01). Митигация в §11.

---

## 2. Журнал решений

Каждая строка — развилка, на которой мы могли пойти иначе.

| # | Решение | Почему |
|---|---|---|
| D1 | Ядро **не зависит от hyper**; hyper — один из транспортов | В браузере hyper физически не участвует: `fetch` не даёт доступа к байтам соединения. Иначе fetch/wasi тянут hyper+h2+tokio ради ничего |
| D2 | `Transport` — **публичный трейт**, бэкенды — отдельные крейты | Сторонние бэкенды (esp, URLSession, curl, BoringSSL) пишутся без PR в нас. Побочно: взаимоисключающих cargo-фич не возникает по построению |
| D3 | Свой минимальный `Transport`, tower — **адаптер** в отдельном крейте | `poll_ready` бэкендам не нужен (у reqwest он захардкожен в `Poll::Ready(Ok(()))`); tower 0.x в публичном API привязал бы нас к его мажору |
| D4 | Узкое портативное ядро + **extension-трейты** на бэкенд | Различия *между* бэкендами закрываются на этапе компиляции, честно |
| D5 | Плюс **рантайм-реестр `Capabilities`** и типизированная ошибка на `build()` | Один wasm-бинарь работает и в Chrome (streaming request body с 131), и в Safari (нет). `cfg` этого не выражает |
| D6 | **Ни одного `Send`/`Box`/`cfg`-алиаса в ядре** | Вся машинерия была следствием стирания типа middleware. Убрали стирание из встроенных стадий — машинерия ушла целиком (§4.2) |
| D7 | Встроенные возможности — **стадии, настраиваемые данными**, а не layer'ы | Иначе тип клиента = `Decompression<FollowRedirect<Cookie<Retry<…>>>>` (буквально внутренность reqwest 0.13) и запечатанные `Unnameable`/`Conn`, из-за которых connection-метрики невозможны ([reqwest#2955](https://github.com/seanmonstar/reqwest/issues/2955)) |
| D8 | В ядре только `Timer`; сеть и spawn — у транспорта | У ядра нет I/O вообще. Долг ядра — только sleep для таймаутов и backoff |
| D9 | **embassy/no_std вычеркнут**; «embedded» = ESP-IDF как ambient-бэкенд | Блокирует не hyper, а `http` 1.5.0: `#[cfg(not(feature = "std"))] compile_error!`. ESP-IDF при этом не требует hyper вовсе |
| D10 | CI-рантаймы: **tokio + smol + compio** | Заявлять нейтральность можно только про то, что в CI |
| D11 | tokio в графе hyper-сборок **принимается и документируется** | [hyper#3428](https://github.com/hyperium/hyper/pull/3428) (ровно этот фикс через `futures-channel`) отклонён, [#3767](https://github.com/hyperium/hyper/issues/3767) закрыт как *not planned* (§10) |
| D12 | Раскол `http-ng-core` (контракт плагина) / `http-ng` (пользовательская поверхность) | Только так `http-ng` может зависеть от `http-ng-hyper`, который зависит от контракта, без цикла. Даёт `Client<T = DefaultTransport>` |
| D13 | **h1/h2/h3 внутри одного native-транспорта**, негоциация прозрачна | Пользователь не должен знать, что ходит по h3. Композиция `AltSvc<H2,H3>` сделала бы выбор версии видимым в типе |
| D14 | Native-транспорт **не объявляет `Send`**; вместо бонда — ассерты в CI | `Send` в трейте заразен: `TcpConnect::Stream: Send` заставил бы compio повторять `SendWrapper`-хак из cyper, который паникует при кросс-тредовом дропе |
| D15 | QUIC-движок — **деталь реализации**, заменяемая в патч-релизе | `h3::quic::*` и типы quinn не в публичном API (§9.2) |
| D16 | **sans-io** как обязывающее правило, обеспеченное графом зависимостей | §8 |
| D17 | **`wasi-fetch` поглощается** и становится `http-ng-wasi`; **p3-only** в v0.1 | Крейт наш, 571 строка, уже работает; §7.1 |

---

## 3. Архитектура

Три тира, а не одна лестница. Тир определяется тем, что физически доступно.

```
Tier A — портативный. Нет hyper, нет tokio, нет сокетов.
┌───────────────────────────────────────────────────────────────┐
│ http-ng-proto   чистые автоматы: SSE-декодер, Alt-Svc,         │
│                 redirect/retry/cookie-логика, Happy Eyeballs-  │
│                 планировщик, multipart, URL. Ноль async.       │
│ http-ng-core    Transport, Capabilities, RequestBody, Error,   │
│                 **Timer**. ~500 строк. Карантин `unversioned`. │
│ http-ng         Client<T = DefaultTransport>, builder, стадии, │
│                 SSE-стрим, сахар.                              │
└───────────────────────────────────────────────────────────────┘
        ▲                     ▲                        ▲
Tier B — socket tier          │        Tier C — ambient (зависят
(hyper, Send де-факто)        │        только от Tier A)
┌──────────────────────────┐  │        ┌────────────────────────┐
│ http-ng-rt   Spawn,      │  │        │ http-ng-wasi  (p3)     │
│   Timer, TcpConnect,     │  │        │ http-ng-fetch          │
│   TcpAdoptStd, Blocking, │  │        │ http-ng-espidf   (v0.4)│
│   + FuturesIo-шим        │  │        │ http-ng-nyquest  (v0.4)│
│ http-ng-rt-{tokio,smol,  │  │        └────────────────────────┘
│              compio}     │  │
│ http-ng-tls  +-rustls    │  │        Сбоку:
│              +-native    │  │        http-ng-tower   адаптер
│ http-ng-dns  +-system    │  │        http-ng-ws      message-API
│              +-hickory   │  │        http-ng-wt      hook
│              +-doh       │  │        http-ng-rmcp    адаптер
│ http-ng-native  h1/h2/h3 │──┘        wasi-fetch      фасад-совместимость
│ http-ng-h3   (движок)    │
└──────────────────────────┘
```

**Инвариант:** `http-ng` не зависит от hyper. `http-ng-h3` — движок внутри
`http-ng-native`, а не пользовательский `Transport`.

---

## 4. Ядро (Tier A)

### 4.1 `Transport`

Форма взята не от hyper, а от `wasi:http/client.send` — самого бедного из
ambient-API. Всё, что богаче, деградирует к ней чисто; обратное неверно.

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

Ни `poll_ready`, ни `&mut self`, ни `Send`. Per-request конфигурация — в
`req.extensions()`, **не** отдельным `Context`-параметром: rama держала `Context`
16 месяцев и выпилила его в 0.3.0 в пользу extensions (PR #711/#714).

**Форма выведена независимо трижды:** спека `wasi:http` 0.3.0;
`rmcp::transport::OAuthHttpClient::execute` (`http::Request<Vec<u8>>` →
`http::Response<Vec<u8>>`); `wasi_fetch::send_raw` (`http::Request<Bytes>` →
`http::Response<Body>`). Причём `act-cli` реализует ту же сигнатуру с *хостовой*
стороны границы (`wasmtime-wasi-http` outgoing-handler), а `wasi-fetch` — с
гостевой.

### 4.2 Почему в ядре нет `Send`, `Box` и cfg

Цепочка, которую мы разорвали: встроенные слои как layer'ы → взрыв типа →
нужно стирание → нужен `dyn` → `dyn` требует объявить `Send` → на wasm его нет →
нужен cfg-переключаемый `MaybeSend`. Пять уровней машинерии из-за первого шага.

Ломаем первое звено (D7): встроенные возможности — стадии, настраиваемые
данными. Тогда `Send` нигде не объявляется, auto-traits просачиваются сами через
`impl Future`.

**Проверено на rustc 1.97.1:**

| приём | статус |
|---|---|
| `where B::execute(..): Send` (RTN) | ❌ unstable, [rust#109417](https://github.com/rust-lang/rust/issues/109417) |
| `type Fut = impl Future` (ATPIT) | ❌ unstable, [rust#63063](https://github.com/rust-lang/rust/issues/63063) |
| `async fn` в трейте без Send | ✅ |
| `-> impl Future + Send` в трейте | ✅ (но принудительно Send) |

RTN нужен только тому, кто пишет **обобщённый и не стёртый** код и спавнит.
Такого случая не остаётся: **Send-ность и стирание — одна ось**. Кто стирает —
берёт `BoxTransport`, который `Send` по построению; кто мономорфен — получает
auto-traits бесплатно.

Стирание — явное и опциональное, два именованных типа, ~40 строк ручной обвязки
каждый, **без** `dynosaur` и `trait_variant`:

```rust
let c: Client<BoxTransport>   = builder.build().boxed();        // Send + Sync
let c: Client<LocalTransport> = builder.build().boxed_local();  // !Send
```

Из зависимостей ушли `cfg_aliases`, `dynosaur`, `trait-variant`, `async-trait`.

### 4.3 `!Send` — свойство конфигурации сборки, а не платформы

Проверено сборкой под `wasm32-unknown-unknown`:

| тип | Send? |
|---|---|
| `wasm_bindgen::JsValue` | ✅ |
| `js_sys::Promise` | ✅ |
| `web_sys::{Request, Response, ReadableStream}` | ✅ |
| `wasm_bindgen_futures::JsFuture` | ❌ — `Rc<RefCell<futures::Inner>>` |

wasm-bindgen 0.2.126 сам объявляет:

```rust
pub struct JsValue { idx: u32, _marker: PhantomData<*mut u8> /* not at all threadsafe */ }
#[cfg(not(target_feature = "atomics"))] unsafe impl Send for JsValue {}
#[cfg(not(target_feature = "atomics"))] unsafe impl Sync for JsValue {}
```

Причина: `JsValue` — индекс в таблице, которой владеет JS-glue («*A `JsValue`
doesn't actually live in Rust right now but actually in a table owned by the
wasm-bindgen generated JS glue code*»). С `+atomics` каждый worker получает свою
таблицу — индекс становится небезопасным, и компилятор это ловит.

Единственный блокер на дефолтном таргете — `Rc` внутри `JsFuture`. Свой
промис-адаптер на `Arc<Mutex<..>>` (~50 строк) **проверен: собирается без
atomics и корректно отвергается с atomics**. Значит:

- `http-ng-fetch` будет `Send` на дефолтном браузерном таргете;
- `!Send` остаётся только для сборки с wasm-потоками — сознательный опт-ин;
- **в нашем коде cfg не появляется**; единственный
  `#[cfg(not(target_feature = "atomics"))] unsafe impl Send` — на одном типе в
  `http-ng-fetch`, зеркалящем upstream.

Ретроспективно это оправдывает D6 сильнее: изначально предложенный `MaybeSend`
ключился по `target_family = "wasm"` — **по неверной оси**.

### 4.4 `RequestBody` с явным контрактом replay

```rust
pub enum RequestBody {
    Empty,
    Full(Bytes),                              // replay бесплатен
    Rewindable(Arc<dyn Fn() -> BodyStream>),  // replay через фабрику
    Streaming(BodyStream),                    // replay невозможен
}

impl RequestBody {
    pub fn retry_kind(&self) -> RetryKind;              // до отправки, не после
    pub fn buffer_for_retry(self, max: usize) -> Self;  // Streaming → Rewindable
}
```

Закрывает корень двух дыр: `reqwest::Request::try_clone() -> None` молча
выключает retry; `reqwest-retry` на стриминговом теле падает с
`Error::Middleware("Request object is not cloneable")` **до первой попытки**. И
снимает выбор «или стриминг, или редиректы», из-за которого `wasi-fetch` держит
тело как `Bytes`.

### 4.5 Таймауты — тройка

```rust
pub struct Timeouts {
    pub connect:       Option<Duration>,
    pub first_byte:    Option<Duration>,
    pub between_bytes: Option<Duration>,
}
```

Форма `wasi:http` — богатейшая из ambient-моделей. В fetch схлопывается в один
`AbortController`, в hyper раскладывается на коннектор / ожидание ответа / idle
тела.

Два живых доказательства, что один `Duration` недостаточен:
`act-cli/src/runtime/http_client.rs` делает
`tokio::time::timeout(config.connect_timeout + config.first_byte_timeout, ..)` —
**складывает два таймаута**, потому что reqwest принимает один;
`wasi-fetch` ставит `set_connect_timeout(ns)` и `set_first_byte_timeout(ns)` из
одного значения и отдельно отломил `between_bytes_timeout` ради SSE.

### 4.6 `Capabilities` — рантайм, реестр, типизированная ошибка

```rust
#[non_exhaustive]
pub struct Capabilities {
    pub streaming_request_body: bool,   // Chrome 131+ да, Safari нет — в одном бинаре
    pub full_duplex: bool,
    pub request_trailers: bool, pub response_trailers: bool,
    pub redirects: RedirectSupport,     // Internal | Configurable | Inspectable | None
    pub tls_config: TlsSupport,         // None | ServerTrustCallbackOnly | Full
    pub client_certs: bool, pub proxy: bool,
    pub owns_cookie_jar: bool, pub owns_cache: bool,
    pub version_select: bool, pub version_reported: bool,
    pub timeouts: TimeoutSupport,       // по каждому из трёх отдельно
    pub informational_1xx: bool,
    pub upgrade: UpgradeSupport,        // None | H1 | ExtendedConnect | Both
    pub forbidden_request_headers: &'static [HeaderName],  // ~25 у fetch
}
```

Неподдерживаемая настройка — `Err(UnsupportedCapability)` на `build()`, **никогда
тихий no-op**. Образец — сам `wasi:http`: сеттеры возвращают
`result<_, request-options-error::not-supported>`.

Живой антипример: `wasi-fetch/src/request.rs` содержит **семь** `let _ =` на
таких `Result` (`set_connect_timeout`, `set_first_byte_timeout`,
`set_between_bytes_timeout`, `set_method`, `set_scheme`, `set_authority`,
`set_path_with_query`). Если хост не поддерживает таймаут, гость молча остаётся
без него.

**Инвариант:** `Capabilities` не должен превысить ~25 полей (§11, критерий
остановки).

### 4.7 `Error`

```rust
#[non_exhaustive]
pub enum ErrorKind { Resolve, Connect, Tls, Redirect, Timeout(Phase), Body, Decode, Status, .. }
```

`Clone` через `Arc<dyn Error>` (без `Send`-бонда — auto-trait прозрачность
доходит и до ошибок). Предикаты `is_*` оставить как удобство.

Мотивация — не эстетика. В `act`:

- гость расплющивает `wasip3 ErrorCode` в строку:
  `Error::Transport(format!("{e:?}"))`;
- хост восстанавливает её обратно подстрочным матчингом по всей цепочке
  `source()`: `error_chain_contains(&err, &["deny cidr", "failed to lookup", "dns"])`,
  с комментарием «*reqwest wraps DNS resolver errors through multiple layers …
  so a single `.source()` hop isn't enough*».

Полный круг структура → строка → структура → строка. Обе потери — от
непрозрачного `Error` ([reqwest#1053](https://github.com/seanmonstar/reqwest/issues/1053)).

### 4.8 `Client` и стадии

```rust
pub struct Client<T = DefaultTransport> { transport: T, config: Config }

#[cfg(not(target_family = "wasm"))]
pub type DefaultTransport = http_ng_native::Native<Tokio, Rustls, SystemDns>;
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub type DefaultTransport = http_ng_fetch::Fetch;
#[cfg(target_os = "wasi")]
pub type DefaultTransport = http_ng_wasi::WasiHttp;
```

Три уровня пользователя:

```rust
// 1. Просто клиент — ноль генериков, один код на трёх таргетах
let text = http_ng::Client::new().get(url).send().await?.text().await?;

// 2. Настройка — по-прежнему ноль генериков
let client = http_ng::Client::builder()
    .redirect(Redirect::limited(10))
    .timeouts(Timeouts { connect: Some(secs(3)), ..default() })
    .retry(Retry::idempotent())
    .build()?;

// 3. Другой бэкенд / свой middleware — здесь появляются генерики
let client = Client::builder_with(Native::new(Smol, Rustls, hickory))
    .layer(Signing::new(key))
    .build()?;                       // Client<Signing<Native<…>>>
```

Дефолт — мнение, а не ограничение: `Client` без параметра означает
`Client<DefaultTransport>`, `Client<Что-Угодно>` работает так же.
Взаимоисключающих фич не возникает — выбирает таргет.

**Стадии** (порядок фиксирован в коде и корректен по построению; к нему reqwest
пришёл эмпирически):

```
decompression → redirect → retry → transport
```

**Middleware** — единственная генерик-ось, растёт только на то, что добавил
пользователь; типы наши и публичные, не `Unnameable`:

```rust
pub trait Middleware<T> { type Output: Transport; fn wrap(self, inner: T) -> Self::Output; }
```

Две точки вставки, чего reqwest+reqwest-middleware дать не могут (у них
`FollowRedirect` снаружи всего):

- `.layer_outer(..)` — одна логическая пара req/resp: auth, трейсинг, кэш;
- `.layer_inner(..)` — **каждый хоп**, включая редиректы и ретраи: подпись
  запроса, per-hop политика, per-hop метрики.

Connection-level middleware существует **только** на native-пути — записать в
доках, иначе пользователи напишут layer, который на iOS молча ничего не делает.

### 4.9 Что чиним из reqwest, потому что проектируем с нуля

| Дыра | Реакций | Решение |
|---|---|---|
| Base URL / относительные URL (#988, #213, с 2017 и 2020) | 104 | `ClientBuilder::base_url()` |
| Event hooks (#155, с 2017) | 48 | публичные `ConnectRequest`/`Connected` (URI, resolved addr, ALPN, peer certs) + наблюдаемый Drop соединения |
| Per-request config (#2641) | 16 | `http::Extensions`, lookup «request-first, client-fallback» — **заложить сразу**, иначе breaking change |
| Неразрушающее чтение тела (#1542) | 18 | `into_parts() -> (Parts, Body)` + `Collected`, сохраняющий status/headers/url после `.text()` |
| `Error` без kind-enum, не `Clone` (#1053) | — | §4.7 |
| Синхронный `CookieStore` | — | async-трейт с `&self` |

### 4.10 SSE

Пишем свой, потому что все существующие сломаны конкретно:

- `eventsource-stream` 0.2.3 (17.2M загрузок, под `reqwest-eventsource`): в
  `dispatch()` **теряет `retry:`** в блоках без `data`; **выбрасывает
  comment-строки** → keep-alive-детектор не построить; парсит `retry` через
  `u64::from_str`, принимая `+5000` вопреки «ASCII digits only».
- `reqwest-eventsource` 0.6.0: **не реализует правило HTTP 204** → против
  сервера, сказавшего «прекрати», переподключается вечно; шлёт пустой
  `Last-Event-ID`; `try_clone().unwrap()` → паника на стриминговом теле.
- LaunchDarkly `eventsource-client` 0.17.5: лучший парсер, но **не проверяет
  `Content-Type`**, содержит `TODO … (e.g. 204)`, с 0.17.x тянет проприетарный
  `launchdarkly-sdk-transport`.
- `sse-stream` 0.2.5 (используется rmcp): нет варианта comment; `Error` содержит
  `DuplicatedEventLine`/`DuplicatedIdLine`/`DuplicatedRetry` — **ошибается** там,
  где WHATWG предписывает «последнее значение побеждает».
- Джиттер при реконнекте не делает **ни один**.

**Два слоя** (rmcp подтвердил необходимость раскола — ему нужен только декодер):

- `SseDecoder` — в `http-ng-proto`. Побайтовый, BOM-автомат, переживающий разрыв
  BOM между чанками; без знания об HTTP; **обязательный лимит размера сырого
  события**, превышение — фатально и не ретраится (требование rmcp:
  `DEFAULT_MAX_SSE_EVENT_SIZE = 16 MiB`, применяется «*at the raw byte layer,
  before SSE parsing*»).
- `SseStream` — в `http-ng`. Реконнект, `Last-Event-ID`, backoff с джиттером.

События — enum `{ Message, Comment, Retry, Open }`: comment первым классом даёт
keep-alive-детектор, retry первым классом чинит баг retry-only-блоков на уровне
типов.

Нормативные правила WHATWG к соблюдению: снятие **одного** BOM; три терминатора
(CRLF/LF/CR); `:` = комментарий; split по первому `:` со снятием одного пробела;
`id` игнорируется при NUL; `retry` только при чистых ASCII-цифрах; при пустом
data-буфере — сброс без диспатча, но last-event-ID **не сбрасывается**; обрезка
хвостового LF; `Last-Event-ID` только если непусто; fail при статусе ≠ 200 или
Content-Type ≠ `text/event-stream`; **204 = прекратить навсегда**; 301/307 —
следовать.

Строим поверх обычного `Client`, а не поверх браузерного `EventSource` — тот не
умеет ни заголовков, ни POST, ни auth.

---

## 5. Транспортный тир (Tier B)

`Send` не объявляется нигде; он появляется в трёх местах, и все три — требования
чужого кода: hyper (`B::Data: Send`, `Upgraded`, `Sleep: Send + Sync`), quinn
(`Runtime: Send + Sync + 'static`), hickory (`RuntimeProvider: Send + Sync`).

### 5.1 `http-ng-rt` — раздельные способности, не один Runtime

```rust
// Форма Spawn скопирована у hyper::rt::Executor намеренно: генерик по future,
// ноль бондов в трейте — Send добавляет impl, а не объявление.
pub trait Spawn<F: Future<Output = ()>> { fn spawn(&self, f: F); }
impl<F: Future<Output=()> + Send + 'static> Spawn<F> for Tokio {}
impl<F: Future<Output=()> + 'static>        Spawn<F> for TokioLocal {}

// ВНИМАНИЕ: `Timer` определён ОДИН раз — в `http-ng-core` (D8), потому что он
// нужен ядру для таймаутов и backoff. `http-ng-rt` зависит от `http-ng-core` и
// только реэкспортирует его рядом со своими способностями. Двух Timer'ов нет.
pub use http_ng_core::Timer;
// pub trait Timer {
//     fn sleep(&self, d: Duration) -> impl Future<Output = ()>;  // не Pin<Box<dyn Sleep>>
//     fn now(&self) -> Self::Instant;                            // не std::time::Instant
// }

pub trait TcpConnect {
    type Stream: hyper::rt::Read + hyper::rt::Write + Unpin;
    async fn connect(&self, addr: SocketAddr, opts: &TcpOpts) -> io::Result<Self::Stream>;
}

/// Отдельный трейт, а не метод: на wasm его нет, и это должно быть
/// ошибкой компиляции, а не unimplemented!() в рантайме.
pub trait Blocking { async fn run<T>(&self, f: impl FnOnce() -> T) -> T; }

pub trait TcpAdoptStd: TcpConnect {          // только fd-платформы
    fn adopt(&self, std: std::net::TcpStream) -> io::Result<Self::Stream>;
}
```

Свой `Timer`, а не `hyper::rt::Timer`: у того `Sleep: Send + Sync`
**безусловно**, `sleep()` возвращает `Pin<Box<dyn Sleep>>` (аллокация на каждый
sleep), `now()` типизирован на `std::time::Instant`, который **паникует** на
`wasm32-unknown-unknown`. `impl hyper::rt::Timer for Tokio` живёт в
`http-ng-rt-tokio` и никуда не течёт.

`TcpAdoptStd` существует потому, что весь набор socket-опций (nodelay,
keepalive+interval+retries, send/recv buffer size, local_address,
local_addresses(v4,v6), connect_timeout, happy_eyeballs_timeout, reuse_address,
`SO_BINDTODEVICE`, `TCP_USER_TIMEOUT`) применяется на `socket2::Socket` — это
самый чистый шов на fd-платформах. Опции живут в http-ng один раз, рантайм
только усыновляет дескриптор.

**Написать самим:** мост `futures_io::{AsyncRead,AsyncWrite}` →
`hyper::rt::{Read,Write}`. В hyper-util только `TokioIo`; `smol-hyper` 0.1.1
мёртв с 2023-12-29 **и** реализует направление не в ту сторону. ~200 строк, без
них smol/compio-бэкендов не существует.

**Ловушка h2:** с hyper 1.8.0 (2025-11-11, breaking в CHANGELOG)
`Http2ClientConnExec` требует `Clone` и «executor должен уметь спавнить сам
себя», трейт запечатан, `H2ClientFuture` в приватном `mod proto`. Единственный
способ удовлетворить — blanket `impl<F: Future<Output=()> + 'static> Executor<F>`.

**Приятно для v0.1:** h1-handshake не требует ни executor'а, ни таймера —
`Connection` поллится инлайн через `select`. Первый клиент поедет на голом
`futures`-executor'е с нулевой способностью спавнить.

### 5.2 TLS

Адаптер пишется **напрямую под `hyper::rt::Read/Write`**, а не под futures-io или
tokio-io. Следствие: per-runtime TLS-склейки не существует вообще — один адаптер
на все рантаймы.

```rust
pub trait TlsConnect {
    type Stream<S>: hyper::rt::Read + hyper::rt::Write + Unpin;
    async fn connect<S>(&self, io: S, req: TlsRequest<'_>) -> Result<(Self::Stream<S>, TlsInfo)>;
}

pub struct TlsRequest<'a> {
    pub server_name: ServerName<'a>,
    pub alpn: &'a [&'a [u8]],                     // на коннект, не на конфиг
    pub ech: Option<EchConfigListBytes<'a>>,      // с первого дня
}
```

- **ALPN на коннект**: пин версии и h2-prior-knowledge требуют разного набора
  ALPN для разных соединений к одному origin. Внутри `http-ng-tls-rustls` — кэш
  `Arc<ClientConfig>` по набору ALPN.
- **`ech` сразу, даже не реализуя**: ECH — **RFC 9849** (Proposed Standard, март
  2026), `EchConfigList` берётся из HTTPS/SVCB-записи. Зафиксируй мы резолвер и
  TLS-запрос на `SocketAddr` — ECH закрыт навсегда без breaking change.
- **`TlsInfo` — все методы `Option`**: native-tls отдаёт только leaf-сертификат,
  ALPN и tls-server-end-point.

Строим на поверхности, стабильной с rustls 0.20: `process_new_packets` +
`wants_read`/`wants_write` + poll-обёртки над `read_tls`/`write_tls`. **Не** на
`unbuffered` — его в main rustls уже удалили (PR #2905, 2026-02-06). Пин
`rustls = "0.23"` (текущий 0.23.43, 2026-07-29), rustls **не в публичном API**.
В 0.24 ждут: удалённая фича `std`, провайдеры вынесены в
`rustls-ring`/`rustls-aws-lc-rs`, MSRV 1.85, edition 2024 — один переписанный
крейт заложен в бюджет.

Доверие: `rustls-platform-verifier` 0.7 (`new_with_extra_roots`) по умолчанию,
`webpki-roots` 1.0.9 для wasm/wasi, `rustls-native-certs` 0.8.4 опционально.
Плюс escape hatch «дай свой готовый `ClientConfig`» — бесплатно даёт ECH,
keylog, FIPS, Graviola/SymCrypt/OpenSSL.

### 5.3 DNS

```rust
pub trait Resolve {
    fn lookup_ipv4(&self, name: &Name) -> impl Stream<Item = Result<ResolvedAddr>>;
    fn lookup_ipv6(&self, name: &Name) -> impl Stream<Item = Result<ResolvedAddr>>;

    /// Дефолт возвращает пусто — чтобы getaddrinfo, wasi и embedded
    /// удовлетворяли трейт тривиально.
    fn lookup_svcb(&self, _: &Name) -> impl Stream<Item = Result<SvcbEndpoint>> { empty() }
}
pub struct ResolvedAddr { pub addr: IpAddr, pub ttl: Option<Duration> }
pub struct SvcbEndpoint {
    pub priority: u16, pub target: Name, pub alpn: Vec<Vec<u8>>, pub port: Option<u16>,
    pub ipv4hint: Vec<Ipv4Addr>, pub ipv6hint: Vec<Ipv6Addr>,
    pub ech_config_list: Option<Bytes>,
}
```

Раздельные **стримы**, а не `Vec<SocketAddr>`: по RFC 8305 надо начинать
коннектиться по AAAA, не дожидаясь A.

- **Системный резолвер требует `Blocking`** — `getaddrinfo` блокирующий везде.
  **Два слота, не один**: curl 8.20 запускает v4 и v6 в отдельных потоках, чтобы
  частичные результаты запускали Happy Eyeballs раньше.
- **`getaddrinfo` никогда не вернёт HTTPS/SVCB** → системный путь структурно не
  даёт ни ECH, ни h3-discovery. Лазейки только платформенные: Apple
  `DNSServiceQueryRecord`, Android `DnsResolver.rawQuery` (API 29+).
- **hickory — честно tokio-only.** `RuntimeProvider` переехал в новый крейт
  `hickory-net` 0.26.1 (2026-05-01, MSRV 1.88); шифрованный DNS замкнут на
  `__tls = ["dep:rustls", "dep:tokio-rustls", "tokio"]`; issue #3304 «Support
  smol runtime in the resolver» открыт с 2025-10-10 без assignee. Единственный
  не-tokio `RuntimeProvider` в природе — `cyper-hickory` 0.1.0, и он вынужден
  реализовывать TLS/HTTPS/H3 сам.
  **Обязательно** `LookupIpStrategy::Ipv6AndIpv4` — дефолт `Ipv4thenIpv6`
  последовательный и IPv4-first, что противоречит RFC 8305 §3.
- **Рантайм-нейтральный шифрованный DNS — только DoH поверх самого http-ng**, на
  `hickory-proto` как чистом кодеке (он `#![no_std]` + alloc, CI на
  `aarch64-unknown-none`). Нужен bootstrap-резолвер и cycle guard.

### 5.4 Happy Eyeballs — пишем сами, RFC 8305

Готового нет: `happy-eyeballs` 0.2.1 мёртв с 2023-05; `happyeyeballs` сам
объявляет себя не-RFC-совместимым; hyper-util реализует **RFC 6555** (две
family-группы + один `sleep(300ms)`, максимум две параллельные попытки, внутри
группы последовательно) за запечатанным трейтом.

Наше: интерливинг с First Address Family Count = 1, Resolution Delay 50 мс,
Connection Attempt Delay 250 мс (clamp 10 мс…2 с), на `FuturesUnordered` +
`select`, **без spawn** (spawn потребовал бы `Send + 'static`). Три константы —
публичный конфиг. Планировщик — чистый автомат в `http-ng-proto`, принимающий
`now` параметром: константы тестируются без единого `sleep`.

Своя RFC 6724 Destination Address Selection здесь предполагалась вероятно
нужной ("поддерживаемого крейта нет") — Task 11 (вертикаль 2,
`http-ng-native::connect`) проверила перед реализацией и закрыла вопрос:
не делаем, см. §9.

### 5.5 Пул

Сначала пробуем `hyper_util::client::pool` (фича `client-pool`, 0.1.19 от
2025-12-03, ~1860 строк): слои `cache`/`map`/`negotiate`/`singleton`, где
`negotiate` — ALPN h2-с-fallback-на-h1, обкатано reqwest'ом.

Блокер однострочный: фича `client` тянет `tokio/net` безусловно → сборка на wasm
падает. Сам `client-pool` = `["client", "dep:futures-util", "dep:tower-layer",
"tokio/sync"]`, без `net` и `rt`. **PR в апстрим подать немедленно.**

Доделать в любом случае:

- **Idle-эвикция.** В `cache.rs` буквально `// todo: on_idle`; единственный API —
  `Cache::retain(..)`. `Spawn` и `Timer` проектировать как **`Option`**: без них
  ленивая эвикция при checkout (режим однопоточного WASM).
- **Дренаж недочитанного тела с дедлайном.** rmcp отключает пул целиком
  (`pool_max_idle_per_host(0)`) из-за «*~40 ms stalls caused by TCP Delayed ACK
  on Linux when the previous response body was not fully consumed before the pool
  attempts to reuse the connection*», и отдельно делает
  `tokio::time::timeout(50ms, ..)` чтобы дочитать хвост SSE. Не оптимизация:
  реальный потребитель из-за этого выключает пул.
- **Учёт стримов H2** ([hyper#3623](https://github.com/hyperium/hyper/issues/3623),
  открыт с 2024-04-05): `SendRequest` unbounded, `poll_ready` всегда Ready →
  соединение с исчерпанным `MAX_CONCURRENT_STREAMS` считается здоровым.
- Типы пула намеренно неименуемы (`pub` только под `#[cfg(docsrs)]`) → хранить
  `Cache` в своём поле без боксинга нельзя.

Эвикция h1 после upgrade уже корректна: `Drop for Pooled` проверяет `is_open()`
**до** вставки.

Ключ пула — `(scheme, authority, protocol)`.

### 5.6 Негоциация h1/h2/h3 внутри одного транспорта

```
кандидаты(origin):
  svcb = resolver.lookup_svcb(host)              // RFC 9460
  если svcb.alpn ∋ "h3"                → H3(ipv4hint/ipv6hint, svcb.port)
  если altsvc_cache свежий и не broken → H3(...)                 // RFC 7838, с ma=
  всегда                               → TCP(A/AAAA через HE, ALPN=[h2, http/1.1])

выбор:
  есть H3-кандидат → QUIC-handshake и TCP с задержкой, берём первый успешный
  иначе            → только TCP

при провале h3:
  altsvc_cache.mark_broken(origin)     // 30с → 60с → 120с, экспоненциально
  продолжить по TCP; ошибку пользователю не показывать
```

Broken-backoff обязателен: UDP/443 режут на ~2–5% сетей. Без него клиент вечно
пытается в h3 и платит таймаутом на каждом запросе (модель Chrome).

**Следствие для DNS:** `getaddrinfo` не вернёт SVCB, поэтому с системным
резолвером первый запрос всегда TCP, а h3 подхватывается со второго через
`Alt-Svc`. h3-с-первого-пакета требует `-dns-hickory` или `-doh` — записать в
доках, и это повышает приоритет hickory до v0.2.

Управление и наблюдаемость («может не знать» ≠ «не может узнать»):

```rust
.http3(Http3::Auto)            // дефолт: SVCB + Alt-Svc + гонка + откат
.http3(Http3::Disabled)
.http3(Http3::PriorKnowledge)  // сразу QUIC, без отката
.version_pin(Version::HTTP_11) // часть ключа пула и ALPN-offer, НЕ поле запроса
```

`Response::version()` всегда говорит правду; `on_connect`-хук отдаёт
согласованный протокол, resolved addr и ALPN.

**Почему пин версии не может быть полем запроса:** ALPN переопределяет его —
`reqwest::RequestBuilder::version()` по этой причине **не работает**
([reqwest#2116](https://github.com/seanmonstar/reqwest/issues/2116), открыт), а
`reqwest-websocket` держит для этого отдельную ошибку с комментарием «*this could
be the case because reqwest silently upgraded the connection to http2*».

### 5.7 Upgrade и WebSocket-over-h2

hyper 1.11 **уже умеет** клиентский RFC 8441 extended CONNECT, и этим не
пользуется никто: `reqwest-websocket` 0.6.0 возвращает `UnsupportedHttpVersion`
на `Version::HTTP_2`, единственный h2-WS-клиент имеет 59 загрузок. Механика:
`proto/h2/client.rs:731` переносит `hyper::ext::Protocol` в h2-extensions; h2
0.4.15 сохраняет `:scheme`/`:path` при непустом protocol; `ResponseFutMap::poll`
при статусе **ровно 200** кладёт `OnUpgrade` в extensions.

Три ловушки, из-за которых нужна своя обёртка h2-соединения:

1. Ни hyper, ни h2 **не проверяют `SETTINGS_ENABLE_CONNECT_PROTOCOL`** перед
   отправкой `:protocol` → stream error (нарушение RFC 8441 §3). Флаг
   `is_extended_connect_protocol_enabled()` живёт только на `Connection`, а
   hyper-util спавнит её в задачу и флаг теряет. Нужен проброс в запись пула.
2. **Время жизни туннеля привязано к пулу**: hyper-util дропает `Pooled` сразу
   после заголовков, живой туннель держится клоном `SendRequest` в пуле;
   `pool_idle_timeout` (дефолт 90 с) или вытеснение рвут его.
3. ALPN молча ломает h1-upgrade (см. §5.6).

Публичная форма шва:

```rust
pub struct Upgraded<S> { pub io: S, pub read_buf: Bytes, pub version: Version }
```

Не `hyper::upgrade::Upgraded`: тот хранит `Rewind<Box<dyn Io + Send>>`, а
`with_upgrades()` требует `T: Read + Write + Unpin + Send + 'static` — `!Send` IO
через него непредставим, и он протёк бы hyper во все downstream-крейты.
`!Send`-путь: `Connection::without_shutdown() -> Parts<T> { io, read_buf }` с
бондом только `T: Read + Write + Unpin`. Подвох: при апгрейде обычный
`Connection` возвращает `Poll::Ready(Ok(()))`, **не ошибку** — 101 детектить по
статусу. `Parts` — `#[non_exhaustive]`.

**Публичный WS/WT API — message-oriented:**

```rust
pub trait WebSocket: Stream<Item = Result<Message>> + Sink<Message> {}
```

Не вкусовщина: в браузере `WebSocket` — отдельный global, недостижимый через
fetch; на Apple это `NSURLSessionWebSocketTask`, **message-framed**. Отдай мы
наружу `impl Read + Write` — wasm и iOS стали бы невозможны. Сырой дуплекс
остаётся native-only деталью внутри `http-ng-ws`. Фрейминг —
`async-tungstenite` 0.35.0 (2026-07-28, на `futures_io`, рантайм-нейтрален),
проверка хендшейка — `tungstenite::handshake::client::{generate_key,
derive_accept_key}`.

**WebTransport не пишем:** `h3-webtransport` 0.1.2 — **server-only** (в `src/`
только `lib.rs`/`server.rs`/`stream.rs`), рабочие клиенты (`wtransport` 0.7.1,
`web-transport-quinn` 0.11.12) везут собственный HTTP/3 и прибиты к
`quinn/runtime-tokio`. Делегируем в `web-transport` 0.10.9, который сам
cfg-переключается native/wasm.

---

## 6. HTTP/3

hyper даёт здесь **ничего**: нет `client::conn::http3`, нет `hyper::rt::quic`, нет
h3/quinn в Cargo.toml; roadmap (последнее заявление 2024-12-10) перечисляет три
нестартовавших пункта. h3 — отдельный стек `h3` + `h3-quinn` + `quinn`.

**Хорошее:** quinn 0.11.11 (2026-06-22) везёт три in-tree рантайма (Tokio, Smol,
AsyncStd); smol-impl ~25 строк + ~60 строк общей обёртки, потому что весь
платформенный ад (GSO/GRO/ECN/recvmmsg/DF) живёт в рантайм-независимом
`quinn-udp`. **Не наращиваем `http-ng-rt` до UDP** — `http-ng-h3` принимает
`quinn::Runtime` напрямую.

**Плохое (причина, по которой это v0.3–v0.4):**

- `h3` 0.0.8 выпущен **2025-05-06**; 13 коммитов в master за 15 месяцев, 3
  смёрженных PR за весь 2026. Нужное лежит в master невыпущенным: RFC 9220
  `:protocol` (#236), connect-ip (#273), фикс CONNECT (#322), 0-RTT (#323).
  Git-зависимость = невозможность публиковаться на crates.io.
- quinn **0.12.0** в main ломает ровно те трейты, под которые пишутся адаптеры:
  `UdpPoller` → `UdpSender`, `create_io_poller(Arc<Self>)` → `create_sender(&self)`,
  `try_send` убран, `poll_recv` берёт `&mut self`, `wrap_udp_socket` возвращает
  `Box`, `runtime-async-std` удалён. `h3-quinn` 0.0.10 требует quinn `^0.11.7`.

**Ограничения, которые пишем в доки сразу:**

1. HTTP/3 **никогда не будет `!Send`**: `quinn::Runtime: Send + Sync + Debug +
   'static`, `spawn(Pin<Box<dyn Future + Send>>)`, `quinn-proto` std-only.
2. **Pluggable TLS на h3 не распространяется**: единственная реализация
   `quinn_proto::crypto::Session` — rustls. Формулировка: «HTTP/1.1 и HTTP/2 —
   native-tls / rustls / другие; HTTP/3 — только rustls».
3. WebTransport поверх нашего h3 не поедет (§5.7).

**Пины:** `h3 = "=0.0.8"` (точный: caret для `0.0.x` совместимости не даёт);
`quinn-proto` — текущий 0.11.16 уже перекрывает пол RUSTSEC-2026-0037 (CVSS 8.7,
`>= 0.11.14`); `h3::quic::*` и типы quinn **не в публичном API**.

### 6.1 Замена QUIC-движка — задокументированные аварийные выходы

`Send` у quinn не в QUIC-коде, а целиком в асинхронной обёртке. Ниже неё всё уже
sans-io (замерено):

| крейт | зависимости |
|---|---|
| `quinn-proto` 0.11.16 | bytes, fastbloom, lru-slab, rand, ring, rustc-hash, rustls — **ни tokio, ни async** |
| `quinn-udp` 0.6.1 | libc, socket2, tracing — **без рантайма** |
| `quinn` 0.11.11 | здесь `Runtime: Send + Sync + 'static` |

Размер обёртки: `wc -l quinn-0.11.11/src` = 5347, из них `tests.rs` 1111 →
**~4200 строк**; клиент-онли ≈ 3000–3500. Скрытая цена: пришлось бы **вырастить
`http-ng-rt` до UDP**.

Альтернативный движок — `quiche` 0.29.3: в `src/` **ноль `async fn` и ноль
`tokio::`**, есть собственный `src/h3/` с qpack → проблема замороженного `h3`
исчезает целиком. Цена: зависимость `boring` 4.22 (BoringSSL) — C-тулчейн, cmake,
вторая TLS-реализация в графе.

Апстрим `!Send`-quinn не обсуждает: поиск по заголовкам issue в quinn-rs/quinn
даёт 98 совпадений на «Send», все про отправку данных.

**Триггеры пересмотра:**

| триггер | ход | цена |
|---|---|---|
| compio-с-h3 реально нужен | драйвим `quinn-proto` сами, оставив `quinn-udp` | ~3000 строк + UDP в публичном рантайм-контракте |
| `h3` заморожен, нужен WS-over-h3 / connect-ip | переход на `quiche` | BoringSSL в сборке, второй TLS-стек |

Замена возможна **в патч-релизе**, потому что движок — деталь реализации одного
крейта (D15).

---

## 7. Ambient-бэкенды (Tier C)

### 7.1 `http-ng-wasi` — поглощение `wasi-fetch`

`wasi-fetch` 0.2.0 (наш крейт, `github.com/actcore/wasi-fetch`) — 571 строка,
зависимости `http`, `wasip3 0.7.0`, `futures`, `serde`, `http-body`, `bytes`,
`url`; **ни tokio, ни reqwest**. Это уже `http-ng-wasi` плюс кусок фасада.

| `wasi-fetch` сейчас | в http-ng |
|---|---|
| `Client` + `get/post/put/delete/patch/head/query/request` | `http_ng::Client<T>` |
| `RequestBuilder::{header, headers, body, json}` | `http_ng::RequestBuilder` |
| `timeout` (ставит connect **и** first_byte) + `between_bytes_timeout` | `Timeouts` (§4.5), per-request через `Extensions` |
| `redirect_limit` + цикл ~60 строк | стадия `Redirect` |
| `send_raw`: конверсия http↔wasi, `Fields`, `BodyWriter`, `join!`, `to_wasi_method` | **остаётся** → `impl Transport for WasiHttp` |
| `Body::{Incoming, Buffered, Done}` | `Incoming` → `WasiHttp::Body`; `chunk/bytes/text/json` → методы `http_ng::Response` |
| `Error::{Url, Transport(String), Utf8, Json}` | `http_ng::Error` с `ErrorKind` (§4.7) |
| семь `let _ =` на сеттерах | `Capabilities` (§4.6) |

Остаток `http-ng-wasi` ≈ **250–300 строк из 571**.

Сохраняем как есть: `Body::chunk()` пропускает трейлер-фреймы, `poll_frame` их
отдаёт. Удобный слой теряет верность, полный — нет.

**p3-only в v0.1.** wasmtime 46+ поддерживает WASI 0.3 (ратифицирован
2026-06-11); `act-cli` уже ходит по p3; `wasip3::http_compat` даёт
`impl http_body::Body for IncomingBody` (`Data = Bytes`, `Error = ErrorCode`)
даром. Зонтичный крейт `wasi` всё ещё 0.14.7+wasi-0.2.4 — брать нельзя. Сборка
идёт под `wasm32-wasip2` (MSRV 1.90); `wasm32-wasip3` — Tier 3. p2 — отдельным
плечом позже, если найдётся потребитель.

**Судьба имени:** `wasi-fetch` 0.3 становится тонким фасадом (~40 строк) над
`http_ng::Client<WasiHttp>` со старыми именами — крейт остаётся findable, старые
пользователи мигрируют одной строкой.

**Ограничения `wasi:http` 0.3.0, которые надо отразить в `Capabilities`:** вся
per-request конфигурация = три таймаута, каждый может вернуть
`not-supported`; `request.get-options` отдаёт **immutable** handle (опции
фиксируются в `request.new`); **нет понятия redirect вообще** (хост может
следовать, может нет, и вы не узнаете); **нет upgrade/CONNECT** (только
error-code `HTTP-upgrade-failed`) → WebSocket поверх wasi:http невозможен; нет
TLS/proxy/version/cookie/pool. Зато **полный дуплекс и трейлеры в обе стороны** —
богаче нативного.

### 7.2 `http-ng-fetch`

Не откладывается, потому что fetch — **единственный** бэкенд, где возможности
различаются в рантайме, а значит единственная проверка решения D5.

Что fetch физически не может (это и есть содержимое `Capabilities`):

- **Streaming request body — только Chromium**: BCD `api.Request.duplex` —
  chrome/edge/webview 131 (2024-11-12), Firefox `false`
  ([bugzil.la/1792434](https://bugzil.la/1792434)), Safari/iOS `false`
  ([webkit.org/b/245671](https://webkit.org/b/245671)). Даже в Chrome:
  отклоняется на HTTP/1.x, обязателен `duplex:"half"`, любой редирект кроме 303
  рвёт запрос, всегда preflight, `no-cors` запрещён.
- **web-sys 0.3.103 не имеет `set_duplex`/`set_keepalive`/`set_priority`** →
  только через `js_sys::Reflect::set`; `wasm-streams` 0.6.0 для Rust Stream →
  ReadableStream.
- ~25 запрещённых заголовков (Host, Connection, Content-Length, Cookie, Origin,
  Transfer-Encoding, TE, Upgrade, `Proxy-*`, `Sec-*`…).
- Нет трейлеров ни в одну сторону
  ([whatwg/fetch#772](https://github.com/whatwg/fetch/issues/772) предлагает
  удалить API); нет 1xx; нет выбора и наблюдения версии HTTP; нет TLS-конфига,
  cert pinning, client certs, proxy; cookie ambient; нет пула.
- Таймаут только через `AbortSignal` — один дедлайн на всё.
- `redirect: manual` даёт `opaqueredirect` со status 0 без заголовков и тела.
- Нет upgrade → **WebSocket через fetch недостижим** (отдельный global).
- `keepalive: true` — потолок 64 KiB, несовместим с ReadableStream.

Fallback при отсутствии duplex — буферизация тела, **документированная и
отключаемая**, не молчаливая.

### 7.3 Позже

`http-ng-espidf` (v0.4): `esp-idf-svc` 0.52.1 даёт **только блокирующий**
`EspHttpConnection` (ноль `async` в `src/http/client.rs`), обёртка = блокирующий
C-API на отдельной FreeRTOS-задаче + канал. `esp_http_client` умеет HTTP/1.1+2
через ALPN, mbedTLS, редиректы, chunked, Basic+Digest; нет h3, нет WebSocket.
**hyper туда не нужен вовсе.**

`http-ng-nyquest` (v0.4): мобильные нативные стеки. Мотив URLSession на iOS — не
скорость, а App Transport Security, системный trust store, MDM-CA, per-app VPN,
системный прокси/PAC, background transfer. Прецеденты: Mozilla `viaduct`
(буферизованные `Vec<u8>`, конфигурация = `{timeout, redirect_limit,
ohttp_channel, user_agent}`; iOS у них при этом на hyper), `frakt` 0.1.0
(push-based `mpsc::Receiver<Bytes>` для тел ответов — NSURLSession/Cronet отдают
байты в делегатах, их нельзя поллить). Отдельного Rust-крейта для Cronet/OkHttp
не существует; `objc2-foundation` 0.3.2 покрывает NSURLSession целиком.

---

## 8. Сквозной принцип: sans-io

**Правило.** Всякая логика, которую мы пишем сами, оформляется как чистый
автомат: без I/O, без `async`, без рантайма, без часов. Всё, зависящее от
времени, принимает `now` параметром. Async-обёртка — отдельный тонкий тип, не
принимающий решений.

Правило обеспечивается **графом зависимостей**, а не дисциплиной:
`http-ng-proto` не имеет в графе ни `tokio`, ни `futures-*`, ни `async-*`.

Это уже наш фактический паттерн: rustls-адаптер против `hyper::rt` вместо
`tokio-rustls`; DoH на `hickory-proto` вместо `hickory-resolver`; `quinn-proto` +
`quinn-udp` как подложка (что и делает замену движка возможной); `SseDecoder`
отдельно от `SseStream` (что подтвердил rmcp).

**Чего sans-io НЕ будет:** hyper h1 типизирован на `hyper::rt::Read/Write`, `h2`
0.4.15 io-сцеплен с `tokio::io` + `tokio-util::codec`, и переписывания там не
обсуждается (0 issues). Поэтому декларация звучит:

> Весь протокольный код, который http-ng пишет сам, — sans-io. Там, где мы
> зависим от чужого, мы берём sans-io-ядро (rustls, quinn-proto, hickory-proto),
> а io-обёртку пишем сами и тонко. Единственные не-sans-io зависимости — hyper и
> h2, и это осознанная плата за зрелый h1/h2.

**Проверка в CI:** `cargo tree` для `http-ng-proto` падает при появлении async-зависимости;
`grep -rn "async fn" http-ng-proto/src` → пусто; `SseDecoder` и Alt-Svc-парсер
идут в `cargo-fuzz` с первого дня (оба разбирают недоверенный ввод, и оба — тот
класс кода, где чужие крейты и сломались).

**Побочная выгода:** `http-ng-embedded` поверх reqwless как отдельный продукт
(§9, embassy) сохраняет опцию бесплатно — «чистая логика» теперь реальный крейт.

---

## 9. Что явно не делаем

| Не делаем | Доказательство |
|---|---|
| **embassy / bare-metal no_std** | `http` 1.5.0: `#[cfg(not(feature = "std"))] compile_error!`. Issue #551 открыт с 2022-05-10, PR #740 — с 2025-01-02 без ответа мейнтейнера, и даже он даёт no_std **+ alloc**. Плюс `embedded-nal-async::TcpConnect` заимствует стек через GAT (несовместимо с пулом), `embedded-tls` 0.19 не даёт ALPN → h2 невозможен |
| **TLS/H2-фингерпринтинг (JA3/JA4/Akamai)** | rustls #1932 → дубликат #2498 → закрыт как *not planned*. `wreq` 6.0.0-rc.29 ради этого **выкинул hyper целиком** (перешёл на форк h2 + BoringSSL). Плюс `http::HeaderMap` нормализует имена в lowercase, так что регистр браузера не воспроизвести. Компенсация — публичный `Transport` |
| **RustCrypto как TLS-бэкенд** | `rustls-rustcrypto` стоит на `0.0.2-alpha` с 2024-04-24, в README «DO NOT USE THIS IN PRODUCTION», требует std |
| **Абстракция над QUIC-бэкендами** | Жизнеспособный ровно один. s2n-quic жёстко тянет `s2n-quic-platform/tokio-runtime` и не имеет опубликованного h3-моста; quiche идёт через `tokio-quiche`; neqo нет на crates.io и требует NSS |
| **async-std** | Прекращён 2025-03-01; quinn 0.12 убирает `runtime-async-std` |
| **Сжатие тела запроса** | Поддержка серверов непоследовательна; дать чистый ручной путь |
| **`hyper::upgrade::Upgraded` в публичном API** | Течёт hyper во все downstream-крейты |
| **RFC 6724 Destination Address Selection (v0.2)** | §5.4 называла её вероятно нужной, без крейта. Task 11 (вертикаль 2, `http-ng-native::connect`) проверила перед реализацией: полное правило требует Source Address Selection (Rule 1 и далее) — знания, каким локальным адресом ОС реально соединилась бы с конкретным адресатом, то есть таблицы маршрутизации, которой не даёт ни один трейт этой вертикали (`Resolve`, `TcpConnect`, `Timer`). Частичная реализация (только правила без Source Address Selection) выглядела бы соответствием RFC 6724, не будучи им — тот же принцип, что развёл `RedirectSupport::None`/`Transparent`. Адреса каждого семейства сегодня идут в `Scheduler::offer_v4`/`offer_v6` в порядке, отданном резолвером (`http-ng-dns::Resolve`, `http-ng-native::connect` — оба задокументированы). Пересмотр возможен, если появится отдельная способность Source Address Selection |
| **Connection-level middleware на ambient-бэкендах** | Физически невозможно |
| **Blocking API** | Вне области по условию задачи |
| **HTTP/3 как блокер 1.0** | reqwest два года держит его за `--cfg reqwest_unstable` |

### 9.1 tokio в графе

Замерено `cargo tree -e normal` для внешнего потребителя:

| сборка | tokio |
|---|---|
| ambient-only (`http-ng` + `-fetch`/`-wasi`) | **нет вообще** |
| hyper, только HTTP/1 | есть, но фичи `sync` + `default`; весь его dep-tree — `pin-project-lite`. Ни mio, ни libc, ни socket2, ни tokio-macros |
| hyper + HTTP/2 | настоящий: `h2` тянет `tokio` с `io-util`+`bytes` и `tokio-util` с `codec` → и **`libc`**, плюс `tracing`, `indexmap`, `slab`, `fnv`, `once_cell` |

Использований в исходниках hyper на h1-пути ровно три: `tokio::sync::oneshot`
(`upgrade.rs`), `tokio::sync::{mpsc, oneshot}` (`client/dispatch.rs`),
`tokio::pin!` (`common/task.rs`). Мост `Compat` к `tokio::io` уже загейчен
`#[cfg(feature = "http2")]`.

Апстрим это не починит: [hyper#3428](https://github.com/hyperium/hyper/pull/3428)
(ровно замена на `futures-channel`) отклонён — «*As of 1.0, we are going to be
very careful about adding new dependencies to the public API… it "exposes" a
crate feature that we could never remove*»; [hyper#3767](https://github.com/hyperium/hyper/issues/3767)
закрыт как *not planned*.

**Решение:** принять и точно задокументировать таблицей выше в README, со
ссылками как обоснованием.

### 9.2 Про `Send` у native-транспорта

Не объявляем (D14), но фактически:

| конфигурация | Send? | кто объявил |
|---|---|---|
| дефолт (tokio + h3) | да | quinn |
| tokio, `http3` off | да, по auto-traits | никто, выводится |
| smol, `http3` off | да, по auto-traits | никто |
| compio, h1/h2 | честно нет | никто |

Гарантия для нас — ассерт в тестах, а не бонд:

```rust
#[test] fn default_stack_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Native<Tokio, Rustls, SystemDns>>();
    assert_send::<Client<Native<Tokio, Rustls, SystemDns>>>();
}
```

Обратимо: рантайм-трейты в карантине `unversioned`, ломающие изменения там едут
в minor.

---

## 10. План версий

### v0.1 — архитектура доказана (не продукт)

| Утверждение | Чем доказывается |
|---|---|
| Runtime-шов настоящий | h1 на **tokio и smol**, оба в CI, ноль `#[cfg]` в общем коде |
| Delegation-шов настоящий | `wasi:http` p3 — там нет сокета вообще |
| Capability-модель деградирует честно | **fetch** — единственный бэкенд с рантайм-различиями (Chrome/Safari duplex) |
| Форма `Transport` угадана верно | `components/http-client` из `act` собирается против http-ng **без изменений логики** |

Состав: `http-ng-proto`, `http-ng-core`, `http-ng`, `http-ng-rt`, `-rt-tokio`,
`-rt-smol`, `http-ng-native` (**только h1**, пул тривиальный), `http-ng-tls` +
`-tls-rustls`, `http-ng-dns` + `-dns-system`, `http-ng-wasi` (p3), `http-ng-fetch`,
`wasi-fetch` 0.3 как фасад.

Плюс: свой SSE-декодер, шим `futures_io → hyper::rt`, модуль `Negotiate` с одним
плечом, ключ пула уже включает протокол, два фазз-таргета.

**Стадия `Redirect` входит в v0.1** (перенесена из v0.2): иначе миграция
`components/http-client` — регресс. Сразу с тремя исправлениями против нынешнего
цикла `wasi-fetch`: не следовать **304/305**; снимать `Authorization`/`Cookie`
при смене host **и scheme**; понижать 301/302 POST→GET наравне с 303.

Почему h1-only: h1-handshake не требует ни executor'а, ни таймера — клиент
поедет на голом `futures`-executor'е с нулевой способностью спавнить.

**Проверочные задачи (сейчас непроверены):**

1. `Send` ли ресурс-хендлы в биндингах `wasip3` 0.7.0.
2. Реально ли `cargo tree` для `http-ng` + `http-ng-fetch` не содержит tokio.
3. Приземлится ли однострочный PR в hyper-util (`tokio/net` → в `client-legacy`).
   **Подать немедленно** — от ответа зависит ~2000 строк в v0.2.

### v0.2 — продукт становится продуктом

h2 через ALPN, с executor'ом и Timer'ом как **typestate билдера**
(`Client::builder()` даёт h1-only, `.executor(e)` разблокирует `.http2()`) — тогда
`keep_alive_interval` без таймера невозможен на уровне типов, а не паникует из
hyper с текстом «You must supply a timer.».

Пул (дренаж, idle-эвикция, `Spawn`/`Timer` как `Option`). `AltSvcCache` пишется
и тестируется здесь, хотя h3 ещё нет. Decompression, cookies (async
`CookieStore` с `&self`), retry с типизированным replayable-телом. Middleware +
`http-ng-tower`. `http-ng-dns-hickory` → SVCB. `http-ng-tls-native`. Multipart,
proxy, base URL.

`http-ng-rmcp` — второй проверочный контур.

### v0.3 — то, чего нет ни у кого

WebSocket с единым upgrade-швом H1 + h2 extended CONNECT (владение обёрткой
h2-соединения — **решить до заморозки архитектуры пула**). h3-плечо за
`feature = "http3"`, по умолчанию **выключенной**: гонка, откат, broken-backoff,
SVCB-первый-полёт. `http-ng-dns-doh`. ECH через SVCB. compio-бэкенд. Event hooks
и connection observability.

### v0.4 — `http3` в дефолт

Когда устаканятся `h3` и quinn 0.12. Плюс `http-ng-espidf`, `http-ng-nyquest`,
WebTransport-хук в `web-transport` 0.10.9.

### Условия 1.0

Плагин-трейты провалидированы против **≥3 бэкендов** (native/wasi/fetch) и **≥3
рантаймов**; карантин `unversioned` задокументирован; `http-ng-rmcp` и `act` в
проде; в публичном API нет ни одного чужого типа.

---

## 11. Риски

| # | Риск | Смягчение |
|---|---|---|
| 1 | **Скоуп.** Список требований больше, чем осилил любой из умерших предшественников | v0.1 доказывает четыре утверждения и **больше ничего не делает** |
| 2 | **`h3` заморожен / разрыв quinn 0.12** | `http3` default-off до v0.4; два аварийных выхода задокументированы (§6.1) |
| 3 | **rustls 0.24 — гарантированная переделка** | Строим на поверхности, стабильной с 0.20; rustls не в публичном API; один переписанный крейт в бюджете |
| 4 | **Пул hyper-util**: типы неименуемы, `// todo: on_idle`, PR может не приземлиться | Подать PR немедленно; дизайн пула гейтить на ответ; запасной путь ~2000 строк |
| 5 | **Кладбище runtime-нейтральности** | Каждый рантайм — leaf-крейт со **своим CI-job'ом**; **никогда** не подпирать smol через `async-compat` (он тихо поднимает второй рантайм); формулировка — «tokio first-class, smol/compio в CI, поверхность — 4 трейта» |
| 6 | **Ловушки extended CONNECT** | Решить в v0.2, до заморозки пула |
| 7 | **Протечка чужих типов в API** (`Upgraded`, `h3::quic::*`, rustls, quinn) | CI-проверка `cargo public-api`, падающая при появлении чужого типа |

**Критерии остановиться и пересмотреть:**

- v0.1 не собрался под smol **без** `#[cfg]` в общем коде → рантайм-шов
  декоративен; чинить `http-ng-rt`, а не идти дальше.
- `Capabilities` за v0.2 разросся выше ~25 полей → модель неверна, вернуться к
  обсуждению typestate.
- `http-ng-rmcp` или `act` потребовали изменений в `http-ng-core` → форма
  `Transport` угадана неправильно; лучше узнать это на v0.2, чем после 1.0.
- В `http-ng-proto` понадобился `async fn` → граница слоёв проведена неверно.

---

## 12. Проверочный контур: `act`

`act` становится первым потребителем **с обеих сторон границы**, что делает его
лучшим тестом формы `Transport`:

- **Хост:** `act-cli/src/runtime/http_client.rs` (872 строки) — реализует
  `wasi:http` outgoing-handler поверх reqwest. Переходит на `impl Transport` +
  политику как **декоратор `Resolve`** и **`Middleware`**. Что это чинит:
  - `tokio::time::timeout(connect_timeout + first_byte_timeout, ..)` → тройка
    таймаутов;
  - `error_chain_contains(&err, &["deny cidr", "failed to lookup", "dns"])` →
    типизированный `ErrorKind::Resolve`;
  - потеря трейлеров запроса (`reqwest::Body::wrap` требует `Send + Sync`, а
    `UnsyncBoxBody` `!Sync` → идут через `wrap_stream`, где трейлеры дропаются) →
    нет конверсии вовсе;
  - баг `StreamBody::is_end_stream()` всегда `false`, из-за которого «*wasi-fetch
    guests trap mid-read on HTTP/2 responses*» → общая реализация с гостевой
    стороной;
  - **один `reqwest::Client` на вызов компонента** (потому что политика печётся в
    конструктор) → один общий пул + политика на запрос через `Extensions`;
  - «*the redirect callback is sync and can't prompt… Per-hop ask-prompting is a
    later phase*» → `layer_inner` async и вызывается на каждом хопе;
  - резолвер возвращает `Box<dyn Iterator<Item=SocketAddr>>` без TTL и SVCB →
    стримы + `ResolvedAddr` + `lookup_svcb`.
- **Гость:** `wasi-fetch` → `http-ng-wasi` (§7.1); `components/http-client`
  собирается без изменений логики и получает нативную и браузерную сборку даром.

`act_policy::net::decide` уже чистая функция без I/O — ложится в раскладку §8 без
правок.

---

## 13. Фиксированные технические решения

- `default = []` во всех крейтах. Бэкенды — **отдельные крейты, не фичи**.
- Приватные join-фичи с префиксом `__` (паттерн reqwest), `?/`-пропагация,
  `[lints.rust] unexpected_cfgs.check-cfg`.
- `#![cfg_attr(docsrs, feature(doc_cfg))]` + `[package.metadata.docs.rs]
  all-features = true, rustdoc-args = ["--cfg","docsrs"]` — `#[doc(auto_cfg)]`
  всё ещё unstable на 1.97.1.
- **MSRV: 1.85 для ядра, 1.88 для `-dns-hickory` и `-h3`**, 1.90 для `-wasi`
  (per-crate MSRV в workspace). Полы: quinn 1.85, hickory 1.88, wasip3 1.90.
- Плагин-трейты (`Transport`, `Resolve`, `TlsConnect`, рантайм-трейты) — в модуль
  `unversioned` с явной политикой (паттерн ureq): ломающие изменения там едут в
  **minor**, а не major. Без этого 1.0 неотправляем.
- CI-матрица: `{tokio, smol} × {linux, macos, windows}` + `wasm32-unknown-unknown`
  + `wasm32-wasip2`, плюс тесты-ассерты на `Send` и `cargo public-api`.

---

## 14. Ближайшие действия

1. PR в hyper-util: `tokio/net` из фичи `client` в `client-legacy`.
2. Спайк: `Send` ли `wasip3` 0.7.0; собирается ли скелет под `wasm32-wasip2`.
3. Заморозить `http-ng-core` (`Transport`, `Capabilities`, `RequestBody`,
   `Error`) — на бумаге, против трёх бэкендов, до написания кода.
4. Скелет workspace + CI-матрица из §13.

---

## Поправки к дизайну

Каждое исключение из инварианта «нигде не объявляем `Send`/`Sync`» в коде
обязано цитировать одну из поправок этого раздела ASCII-токеном —
`amendment-C1`, `amendment-C2`, `amendment-C3`, `amendment-C4` или
`amendment-C5` — в комментарии `send-bound-exception: amendment-CN`. CI
(`no-declared-send`) проверяет исключения именно по этим токенам, так что
цитата и обоснование находятся одним и тем же поиском.

### C1. `Error` требует `Send + Sync` от источника

**Что оказалось неверным.** §4.7 и решение D6 утверждали, что «auto-trait
прозрачность доходит и до ошибок»: `Error` хранит `Arc<dyn Error + 'static>` без
`Send`, поэтому-де ошибка от `!Send`-транспорта работает, а от `Send`-транспорта
остаётся `Send`. Вторая половина неверна. Стирание в `dyn Trait` **никогда** не
пропускает auto-traits, если сам объект-трейт не ограничен. Проверено
компиляцией: даже с источником-ZST, тривиально `Send + Sync`, обёртка не `Send`:

```
error[E0277]: `(dyn std::error::Error + 'static)` cannot be sent between threads safely
   = note: required for `Arc<(dyn std::error::Error + 'static)>` to implement `Send`
```

Следствие было несущим: `Client::execute` заворачивает ошибку транспорта в
`Error`, поэтому футура `client.get(u).send()` оказывалась `!Send` **всегда**, и
`tokio::spawn` не скомпилировался бы ни при каком транспорте. Мой спайк этого не
поймал, потому что использовал конкретный `MockError` как `Transport::Error` и
ни разу не проходил через стёртую ошибку ядра.

**Решение.** Источник ограничивается: `Arc<dyn Error + Send + Sync + 'static>`,
и `Error::new<E: Error + Send + Sync + 'static>`.

**Почему это не размывает D6.** Эмпирика этой же сессии: у всех трёх бэкендов
v0.1 ошибки `Send`. `JsValue` и типы `web_sys` — `Send` без
`target_feature = "atomics"` (§4.3, замерено), ресурс-хендлы `wasip3` — `Send`
(замерено), hyper/quinn/hickory — `Send` по объявлению. То есть требование не
ограничение, а констатация.

**Уточнённая формулировка инварианта** взамен «нигде не объявляем Send»:

> Трейты шва — `Transport`, `Timer`, middleware — не объявляют `Send`/`Sync`.
> Единственное исключение: `http_ng_core::Error` требует `Send + Sync` от
> источника, а `Client::execute` несёт `T::Error: Send + Sync + 'static` в
> своей where-клаузе — не в трейте. Транспорт с `!Send`-ошибкой остаётся
> представимым; он просто не может пользоваться заворачиванием в `Error`.

**Цена, названная вслух.** Сборка wasm с `+atomics` (wasm-потоки), где `JsValue`
честно `!Send`, теряет путь через `Client`. Это та же конфигурация, для которой
§4.2 уже предписывает `.boxed_local()`, и она остаётся вне v0.1.

### C2. `RequestBody` тоже обязан ограничить свои объекты-трейты

Тот же класс, что C1, и найден до реализации — проверкой компиляции, а не
ревью. §4.4 задавал:

```rust
Rewindable(Arc<dyn Fn() -> BodyStream>),
Streaming(BodyStream),                    // Box<dyn Body + Unpin>
```

Оба объекта-трейта без `Send`, значит `RequestBody` — `!Send`, значит
`http::Request<RequestBody>` — `!Send`, значит футура `Transport::execute`
— `!Send`. Исправление C1 в одиночку не спасало бы: спавн всё равно
невозможен, просто по другой причине.

**Решение.**

```rust
Rewindable(Arc<dyn Fn() -> RequestBody + Send + Sync>),
Streaming(Box<dyn http_body::Body<Data = Bytes, Error = Error> + Unpin + Send>),
```

`Sync` нужен только у `Arc`: `Arc<T>: Send` требует `T: Send + Sync`, потому что
`Arc` разделяем; `Box<T>: Send` требует лишь `T: Send`. Проверено компиляцией:
с этими границами `RequestBody: Send` и `http::Request<RequestBody>: Send`,
а `Sync` у `RequestBody` не достигается и не нужен — заявка уходит в `execute`
по значению.

**Общий вывод, который стоит держать в голове до конца v0.1.** Всякий раз,
когда в тип на пути `Client -> Transport` попадает объект-трейт, auto-traits на
нём обрываются. Перед добавлением любого `dyn` в этот путь — компиляционная
проверка `assert_send`, а не рассуждение.

### C3. Соглашение: утверждения о `Send`/`Sync` живут в `tests/`

Проверка `no-declared-send` сканирует только `crates/*/src`, поэтому обычный
`fn assert_send_sync<T: Send + Sync>() {}` внутри `src` ломает её собственным
текстом. Обходной приём через `impl Send` в позиции аргумента работает и
доказывает ровно то же (проверено мутацией: добавление `Rc<()>` в поле роняет
компиляцию в обоих вариантах), но требует шести строк комментария, объясняющих,
почему тест написан странно.

**Соглашение с Task 9 и далее:** такие утверждения пишутся в
`crates/<крейт>/tests/`, обычной генерик-формой. Там их греп не видит, они
стоят на границе публичного API — то есть ровно там, откуда на тип смотрит
настоящий потребитель, — и список исключений сохраняет смысл «обоснованное
исключение в продакшн-коде», а не «неудобный тест».

### C4. `&'static [HeaderName]` не заполняется статическим срезом

Ловушка, найденная до того, как в неё попали. Поле
`Capabilities::forbidden_request_headers` объявлено как `&'static [HeaderName]`,
и у `Capabilities::none()` оно `&[]` — пустой литерал, никаких продвигаемых
временных значений. Но заполнить его нетривиально: на stable с `http` 1.5

```rust
static FORBIDDEN: &[HeaderName] = &[http::header::HOST /* ... */];
```

даёт `E0492: interior mutable shared borrows of temporaries` — **для любого**
заголовка, включая `host` и `content-length`. Проверка продвижения в rustc
работает по типу, а тип `HeaderName` содержит вариант `Custom` поверх `Bytes`
с `AtomicPtr`, независимо от того, какой вариант реально живой.

Работают: одиночный `static X: HeaderName = ...`, `const`-массив, либо
`Box::leak(vec![..].into_boxed_slice())` / `OnceLock` для среза. Это касается
`http-ng-fetch`, где список запрещённых заголовков — около двадцати пяти.
Реализатору проверить форму до написания списка, а не после.

### C5. `http-ng-rt::Blocking` объявляет `Send` в самом трейте способности

Другой класс, чем C1/C2: там бонд возникал не в объявлении трейта, а в точке
стирания (`dyn Error`, `dyn Fn`, `dyn Body`) на пути `Client -> Transport`, и
именно поэтому был неожиданным — §4.7 обещал прозрачность auto-traits, а
стирание в объект-трейт её обрывает. Здесь неожиданности нет: `Blocking` —
трейт способности рантайма (`http-ng-rt`, vertical 2), не seam-трейт ядра, и
`Send` в его сигнатуре — осознанное проектное решение, а не находка после
факта.

**Почему бонд обязателен, а не опция.** `Blocking::run` — это мост к
блокирующему пулу потоков: `getaddrinfo`, файловый ввод-вывод, любая
операция, которую нельзя опрашивать (`poll`) без блокировки исполнителя.
Единственные два бэкенда, которые эту способность вообще реализуют,
устанавливают `Send + 'static` на входе не по выбору http-ng, а по контракту
своего API:

- `tokio::task::spawn_blocking<F, R>(f: F) -> JoinHandle<R> where F: FnOnce()
  -> R + Send + 'static, R: Send + 'static`;
- `blocking::unblock<T, F>(f: F) -> Task<T> where F: FnOnce() -> T + Send +
  'static, T: Send + 'static` (крейт `blocking`, на котором строится
  `smol::unblock`).

Оба рантайма перекладывают замыкание в чужой поток и ждут результат обратно
— ровно то определение, для которого `Send` существует. Слабее этот бонд
объявить нельзя: он не выбор http-ng, а условие, без которого сам примитив
рантайма не компилируется ни у одного из двух бэкендов, которые вертикаль 2
обязана поддерживать без `#[cfg]` в общем коде (Global Constraints
вертикали).

**Почему это не заражает портативное ядро.** Способности `Blocking` не
существует на wasm вовсе — там нет пула потоков, на который можно было бы
перекинуть блокирующий вызов, и это отсутствие проявляется как ошибка
компиляции (нет реализации трейта), а не как `unimplemented!()` в рантайме
(см. doc-комментарий трейта: «отдельный трейт, а не метод»). Значит бонду
здесь нечего заражать за пределами `http-ng-rt` и его native-бэкендов
(`http-ng-rt-tokio`, `http-ng-rt-smol`) — портативное ядро (`http-ng-core`,
`http-ng-proto`) и wasm-транспорт (`http-ng-wasi`) от `http-ng-rt` не
зависят и этот трейт не видят. `Spawn`, `TcpConnect`, `TcpAdoptStd` и
`Timer` в этом же крейте `Send` не объявляют — только `Blocking`, и только
потому, что источник бонда — чужой, не наш API.

**Отличие от C1/C2 в механике маркера.** И `no-declared-send`, и это правило
требуют маркер `send-bound-exception: amendment-CN` на **той же строке**, где
объявлен бонд — построчный, не файловый скоп (см. обоснование в самой
CI-джобе). У `Blocking::run` оба бонда (`T: Send + 'static` и `F: FnOnce() ->
T + Send + 'static`) вынесены в собственный `where`, а не в список дженериков
метода, ровно для того, чтобы у каждого была своя строка и свой маркер —
единый комментарий после `fn run<T: Send + …>(…)` покрыл бы только
последнюю строку декларации, а не обе.

### C6. Полнота `#[non_exhaustive]`-типа проверяема только внутри крейта, который его определяет

Не про исключения из инварианта Send/Sync (тот класс закрыт C1–C5) — про
отдельный, ранее не названный вслух факт о `#[non_exhaustive]`, найденный
Task 13 вертикали 2 (review fix round 1) при попытке доказать, что тест на
`Capabilities` снаружи `http-ng-core` умеет то же самое, что умеет
`Capabilities::none_is_the_conservative_base` внутри него.

**Найдено измерением, не рассуждением.** `Capabilities::none_is_the_
conservative_base` (`http-ng-core/src/caps.rs`) деструктурирует `Capabilities`
БЕЗ `..`: новое поле, добавленное в структуру и не упомянутое в тесте — ошибка
компиляции, называющая поле. Ревью Task 13 построило ровно этот сценарий:
добавило семнадцатое поле в `Capabilities` — тест `http-ng-core` не
скомпилировался, как и задумано. Тот же самый приём, написанный в
`http-ng-native/tests/transport.rs` (крейт-потребитель, не крейт-владелец
типа), обязан завести `..` — `#[non_exhaustive]` требует его для любой
деструктуризации снаружи определяющего крейта (`E0638` без него) — и это `..`
молча поглощает новое поле: тест из `http-ng-native` на том же семнадцатом
поле скомпилировался и остался зелёным, не заметив его.

**Правило.** Проверка «структура типа A не изменилась без явного подтверждения»
над `#[non_exhaustive]`-типом (деструктуризация без `..`, компилирующаяся
только при точном совпадении множества полей) может жить лишь внутри крейта,
который этот тип определяет — там, где `#[non_exhaustive]` не действует на
собственный код крейта. Тест на тот же тип снаружи способен проверить, что
ПЕРЕЧИСЛЕННЫЕ им поля имеют ожидаемые значения (ценная, но другая проверка),
и обязан называть себя и документировать себя соответственно — не как проверку
полноты множества полей, которой он структурно быть не может.

**Следствие для `Capabilities` конкретно.** Единственная проверка полноты —
`Capabilities::none_is_the_conservative_base` в `http-ng-core`. Любой будущий
бэкенд-крейт (`http-ng-native`, `http-ng-wasi`, `http-ng-fetch` и далее),
которому нужно подтвердить, что он не включил лишнюю способность по ошибке —
пишет тест вида «значения перечисленных полей — консервативные дефолты
сегодня» (см. `http-ng-native/tests/transport.rs::undeclared_capability_
fields_match_their_conservative_defaults_today`), а не тест вида «это все
поля, что есть» — того он написать не может.

**Отличие от C3.** C3 — о том, ГДЕ живёт assert (`tests/`, а не `src`,
потому что `no-declared-send` сканирует только `src`). C6 — о том, ЧТO вообще
способен проверить assert над `#[non_exhaustive]`-типом, независимо от того,
в каком файле он написан: даже в `tests/` снаружи определяющего крейта
деструктуризация без `..` не компилируется, значит полнота недоказуема
структурно, а не только "неудобно" её туда писать. Не переиспользовать один
токен для обоих — прошлая ошибка ревью Task 13 (цитата amendment-C3 для этого
самого правила) была найдена и исправлена именно потому, что имплементор
Task 13 проверил цитату по тексту спеки перед тем, как записать её в код, а не
унаследовал её как есть.
