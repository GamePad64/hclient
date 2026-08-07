//! Кроссплатформенный асинхронный HTTP-клиент.
//!
//! Инвариант крейта: ни одного объявленного бонда `Send`/`Sync`, ни одного
//! `#[cfg]`-переключаемого трейт-алиаса. Send-ность выводится auto-traits.
#![forbid(unsafe_code)]

mod client;
mod config;
#[cfg(feature = "test-util")]
pub mod mock;
mod request;
mod response;
mod sse;
mod stages;

pub use client::{Client, ClientBuilder};
pub use config::{Config, InvalidBaseUrl, Timeouts, check_supported, effective_timeouts};
// Task 17 fix round 1: этот список обязан покрывать не только `Capabilities`/
// `RequestBody`/`UnsupportedCapability`, но и КАЖДЫЙ тип `http-ng-core`,
// достижимый из сигнатуры, поля или варианта чего-то уже реэкспортированного
// отсюда — иначе потребитель, зависящий только от `http-ng`, не может назвать
// тип, который у него уже есть на руках. `Error` — самый частый случай:
// возвращают `Client::execute`, `RequestBuilder::send`, `Response::chunk`/
// `collect`, `Collected::text`/`json`, `SseStream::new`/`next`, а
// `mock::MockTransport::push_response_frames_then_error` даже принимает его
// параметром. `ErrorKind` реэкспортирован ради `Error::kind()`, `Phase` —
// ради варианта `ErrorKind::Timeout(Phase)`, оба нужны, чтобы результат
// `.kind()` можно было хоть с чем-то сравнить. `RetryKind` — инвариант
// `RequestBody::retry_kind()` и поле `mock::RecordedRequest::retry_kind`.
// `RedirectSupport`/`TlsSupport`/`TimeoutSupport`/`UpgradeSupport` — поля
// `Capabilities`, которые нужно называть, чтобы вручную собрать свой
// `Capabilities` для `MockTransport::with_capabilities` (доступно и без
// `unversioned::Transport`, `with_capabilities` — обычный инструментальный
// метод). `RewindFactory` — тип варианта `RequestBody::Rewindable`; строго
// не блокирующий (это алиас, `Arc<dyn Fn() -> RequestBody + Send + Sync>`
// выражается и без имени алиаса), реэкспортирован ради называемости и
// симметрии с остальными вариантами.
//
// Намеренно НЕ реэкспортированы `unversioned::{Transport, Timer}`: это
// карантинный контракт для авторов бэкендов/рантаймов (см. doc-комментарий
// `http-ng-core/src/unversioned/mod.rs`), не часть фасада для потребителя,
// который просто собирает запросы и читает ответы. У этого решения была
// цена — `client.transport().capabilities()` требовал бы `Transport` в зоне
// видимости, поскольку `capabilities()` там трейтовый метод, а `.transport()`
// отдаёт голый `&T` — фикс-раунд 2 снял её форвардером `Client::capabilities()`
// (`client.rs`), а не реэкспортом трейта: самый частый вопрос к `Capabilities`
// закрыт, карантин остаётся карантином.
pub use http_ng_core::{
    Capabilities, Error, ErrorKind, Phase, RedirectSupport, RequestBody, RetryKind, RewindFactory,
    TimeoutSupport, TlsSupport, UnsupportedCapability, UpgradeSupport,
};
pub use http_ng_proto::redirect::RedirectPolicy;
pub use http_ng_proto::sse::{DEFAULT_MAX_EVENT_SIZE, SseEvent};
pub use request::RequestBuilder;
pub use response::{Collected, Response};
pub use sse::SseStream;

/// Транспорт по умолчанию, выбираемый **таргетом, а не пользователем**.
///
/// Дефолт — мнение, а не ограничение: `Client` без параметра означает
/// `Client<DefaultTransport>`, а `Client<ЧтоУгодно>` работает так же.
/// Взаимоисключающих cargo-фич не возникает, потому что выбирает таргет, а
/// не набор включённых фич — только `default-transport` целиком
/// включает/выключает саму возможность написать `Client` без параметра,
/// но не выбирает МЕЖДУ вариантами: на каждом конкретном таргете
/// компилируется ровно одна ветка ниже (или ни одной, см. дальше).
///
/// # Что резолвится, при какой фиче — измерено, не предположено
///
/// - Без фичи `default-transport` (дефолт крейта, `default = []`): тип не
///   существует вовсе. `Client` без параметра или `http_ng::DefaultTransport`
///   — обычная ошибка компиляции («cannot find type», «missing generics»),
///   не тихий фолбэк на что-то более слабое. То же решение, что Task 9
///   вертикали 2 уже проверила эмпирически для доверенных TLS-якорей: сборка
///   без верификатора не компилируется, а не молча доверяет всему.
/// - С фичей `default-transport` на любом НЕ-wasm таргете (linux/macos/
///   windows — единственная ветка ниже, `not(target_family = "wasm")`):
///   `Native<Tokio, Rustls, SystemDns<Tokio>>` — `http-ng-rt-tokio` как
///   рантайм, `http-ng-tls-rustls` с `rustls-platform-verifier` (системное
///   хранилище доверия ОС, не `webpki-roots`: `Client::new()` — клиент
///   «просто заработавший», как у пользователя браузера или `curl`, а не
///   клиент с явно выбранным набором корневых сертификатов), `http-ng-
///   dns-system` — `getaddrinfo` через способность `Blocking`. Тянет `tokio`
///   безусловно (см. README, «Что в графе зависимостей»: `hyper` зависит от
///   `tokio` даже на HTTP/1-пути, не только эта ветка).
/// - С фичей `default-transport` на `wasm32-unknown-unknown` (браузер) ИЛИ
///   `wasm32-wasip2`/`wasm32-wasip1` (`target_os = "wasi"`): ветки ниже нет
///   ни для той, ни для другой цели — тип не существует, та же честная
///   ошибка компиляции, что и без фичи вовсе. Браузерный транспорт —
///   вертикаль 3, ещё не начата. WASI-транспорт (`http_ng_wasi::WasiHttp`)
///   уже существует (вертикаль 1) и им можно пользоваться напрямую через
///   `Client::builder(http_ng_wasi::WasiHttp::new())` — но НЕ через этот
///   механизм: `http-ng` намеренно не зависит от `http-ng-wasi` (сам
///   `http-ng-wasi/Cargo.toml` записывает это как инвариант — у него
///   `http-ng` в `dev-dependencies` ради собственного примера, обратной
///   зависимости нет), и заводить её здесь означало бы add путь, который
///   ни один CI job в этом репозитории не собирает: `wasip2`-job гоняет
///   `http-ng-wasi` напрямую, не `http-ng` под `default-transport` на wasm.
///   Опциональная, никогда не проверенная сборкой ветка — то самое
///   «неявная замена сообщения ошибки на воображаемую доступность»,
///   которую этот тип как раз обязан не делать. Решение оставлено как
///   находка вместо молчаливого следования брифу: black-box-приёмка
///   вертикали (`crates/http-ng/tests/two_runtimes.rs`) эту ветку не
///   требует — оба её теста собирают `Native` явно, тем же путём, что и
///   `Client::builder`.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
pub type DefaultTransport = http_ng_native::Native<
    http_ng_rt_tokio::Tokio,
    http_ng_tls_rustls::Rustls,
    http_ng_dns_system::SystemDns<http_ng_rt_tokio::Tokio>,
>;
