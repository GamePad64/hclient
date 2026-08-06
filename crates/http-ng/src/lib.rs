//! Кроссплатформенный асинхронный HTTP-клиент.
//!
//! Инвариант крейта: ни одного объявленного бонда `Send`/`Sync`, ни одного
//! `#[cfg]`-переключаемого трейт-алиаса. Send-ность выводится auto-traits.
#![deny(unsafe_code)]

mod client;
mod config;
#[cfg(feature = "test-util")]
pub mod mock;
mod request;
mod response;
mod sse;
mod stages;

pub use client::{Client, ClientBuilder};
pub use config::{Config, Timeouts, check_supported, effective_timeouts};
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
