//! Конверсия http-ng <-> `wasi:http` 0.3 и honoring сеттеров хоста.
//!
//! `wasi:http` 0.3 возвращает из своих сеттеров `result<_, ...>` именно для
//! того, чтобы хост мог сказать «не умею» или «неизменяемо». Предшественник
//! этого крейта, `wasi-fetch`, отбрасывал семь таких `Result` через
//! `let _ =` (`docs/superpowers/specs/2026-08-05-http-ng-design.md`, §4.6:
//! `set_connect_timeout`, `set_first_byte_timeout`, `set_between_bytes_timeout`,
//! `set_method`, `set_scheme`, `set_authority`, `set_path_with_query`). Здесь
//! каждый такой отказ становится типизированной `Error`, а не тихо
//! исчезает — CI (job `no-discarded-wasi-setters`) проверяет это
//! структурно, не полагаясь на дисциплину ревью.

use bytes::Bytes;
use http_ng_core::{Error, ErrorKind, RequestBody};
use wasip3::http::types::{
    ErrorCode, HeaderError, Method as WM, RequestOptions, RequestOptionsError, Scheme,
};

pub(crate) fn to_wasi_method(m: &http::Method) -> WM {
    match *m {
        http::Method::GET => WM::Get,
        http::Method::POST => WM::Post,
        http::Method::PUT => WM::Put,
        http::Method::DELETE => WM::Delete,
        http::Method::PATCH => WM::Patch,
        http::Method::HEAD => WM::Head,
        http::Method::OPTIONS => WM::Options,
        _ => WM::Other(m.to_string()),
    }
}

#[derive(Debug)]
pub(crate) struct BadScheme;
impl std::fmt::Display for BadScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "URI scheme must be http or https")
    }
}
impl std::error::Error for BadScheme {}

pub(crate) fn scheme_of(uri: &http::Uri) -> Result<Scheme, Error> {
    match uri.scheme_str() {
        Some("https") => Ok(Scheme::Https),
        Some("http") => Ok(Scheme::Http),
        _ => Err(Error::new(ErrorKind::Other, BadScheme)),
    }
}

/// Применяет таймауты, **не проглатывая отказы хоста**.
///
/// `wasi:http` 0.3 возвращает из сеттеров
/// `result<_, request-options-error{not-supported, immutable, other}>` именно
/// для того, чтобы хост мог сказать «не умею». `wasi-fetch` отбрасывал семь
/// таких `Result` через `let _ =`; здесь каждый отказ становится ошибкой —
/// см. `unsupported_timeout` про то, что конкретно уносится в неё из
/// `RequestOptionsError`.
pub(crate) fn apply_timeouts(
    opts: &RequestOptions,
    connect: Option<u64>,
    first_byte: Option<u64>,
    between_bytes: Option<u64>,
) -> Result<(), Error> {
    if let Some(ns) = connect {
        opts.set_connect_timeout(Some(ns))
            .map_err(|e| unsupported_timeout("connect_timeout", e))?;
    }
    if let Some(ns) = first_byte {
        opts.set_first_byte_timeout(Some(ns))
            .map_err(|e| unsupported_timeout("first_byte_timeout", e))?;
    }
    if let Some(ns) = between_bytes {
        opts.set_between_bytes_timeout(Some(ns))
            .map_err(|e| unsupported_timeout("between_bytes_timeout", e))?;
    }
    Ok(())
}

fn unsupported_timeout(what: &'static str, source: RequestOptionsError) -> Error {
    Error::new(ErrorKind::Unsupported, TimeoutRejected { what, source })
}

/// Отказ хоста применить таймаут-опцию запроса.
///
/// В отличие от `Rejected` (ниже), у этого отказа есть содержательная
/// причина — `RequestOptionsError::{NotSupported,Immutable,Other}` — и она
/// не расплющивается в строку: `Display` сообщает и что́ отказано, и чем
/// именно, а сам `RequestOptionsError` остаётся доступен целиком через
/// `Error::source()`. Тот же принцип, что у `wasi_err` для `ErrorCode`:
/// категория (`ErrorKind::Unsupported`) — отдельно от причины, причина —
/// не строка.
#[derive(Debug)]
pub(crate) struct TimeoutRejected {
    what: &'static str,
    source: RequestOptionsError,
}
impl std::fmt::Display for TimeoutRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "backend `wasi:http` does not support `{}`: {}",
            self.what, self.source
        )
    }
}
impl std::error::Error for TimeoutRejected {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Отказ хоста применить `method`/`scheme`/`authority`/`path_with_query`.
///
/// В отличие от `TimeoutRejected`, эти сеттеры `wasi:http` возвращают голый
/// `Result<(), ()>` — от хоста в принципе не приезжает никакой причины,
/// заворачивать в `source` нечего. `what` — это всё, что можно сообщить.
#[derive(Debug)]
pub(crate) struct Rejected(&'static str);
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "wasi:http host rejected setting `{}`", self.0)
    }
}
impl std::error::Error for Rejected {}
pub(crate) fn rejected(what: &'static str) -> Error {
    Error::new(ErrorKind::Unsupported, Rejected(what))
}

/// `Fields::from_list` отказал: заголовок синтаксически невалиден, запрещён
/// или превышает лимит хоста.
#[derive(Debug)]
pub(crate) struct FieldsError(HeaderError);
impl std::fmt::Display for FieldsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid headers: {}", self.0)
    }
}
impl std::error::Error for FieldsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
pub(crate) fn fields_error(e: HeaderError) -> Error {
    Error::new(ErrorKind::Other, FieldsError(e))
}

/// `ErrorCode` идёт в `Error::new` как есть, без обёртки — тот же выбор, что
/// в `body.rs` для ошибок чтения тела ответа: `ErrorCode` уже реализует
/// `Debug`/`Display`/`core::error::Error` вручную, обёртка не добавила бы
/// содержательности, а `Error::new` всё равно стирает конкретный тип в
/// `Arc<dyn Error + Send + Sync>`.
///
/// Категория сохраняется, а не расплющивается в одну строку: `wasi-fetch`
/// схлопывал всё в `Error::Transport(format!("{e:?}"))`, а хостовая сторона
/// `act` потом восстанавливала категорию подстрочным матчингом по цепочке
/// `source()`. Имена вариантов сверены с
/// `wasip3-0.7.0+wasi-0.3.0/src/service.rs:161-206`.
pub(crate) fn wasi_err(e: ErrorCode) -> Error {
    use http_ng_core::Phase;
    use wasip3::http::types::ErrorCode as EC;
    let kind = match &e {
        EC::DnsTimeout | EC::DnsError(_) => ErrorKind::Resolve,
        EC::DestinationNotFound
        | EC::DestinationUnavailable
        | EC::DestinationIpProhibited
        | EC::DestinationIpUnroutable
        | EC::ConnectionRefused
        | EC::ConnectionTerminated
        | EC::ConnectionLimitReached => ErrorKind::Connect,
        EC::ConnectionTimeout => ErrorKind::Timeout(Phase::Connect),
        EC::ConnectionReadTimeout | EC::HttpResponseTimeout => ErrorKind::Timeout(Phase::FirstByte),
        EC::ConnectionWriteTimeout => ErrorKind::Timeout(Phase::BetweenBytes),
        EC::TlsProtocolError | EC::TlsCertificateError | EC::TlsAlertReceived(_) => ErrorKind::Tls,
        EC::HttpRequestDenied => ErrorKind::Status,
        EC::LoopDetected => ErrorKind::Redirect,
        EC::HttpUpgradeFailed | EC::ConfigurationError => ErrorKind::Unsupported,
        EC::HttpResponseIncomplete
        | EC::HttpResponseBodySize(_)
        | EC::HttpResponseTransferCoding(_)
        | EC::HttpResponseContentCoding(_) => ErrorKind::Body,
        _ => ErrorKind::Other,
    };
    Error::new(kind, e)
}

/// Запись тела запроса (`BodyWriter::send_http_body`) не удалась.
///
/// Оборачивает `wasip3::http_compat::Error` целиком, а не его `Display`:
/// причина (`HttpBody` — упал наш собственный источник кадров;
/// `StreamReaderClosed` — хост закрыл читающий конец раньше времени;
/// `ResultReaderClosed` / `InvalidTrailers` — сбой на хвосте передачи)
/// остаётся доступна через `Error::source()`, а не расплющена в строку. См.
/// `resolve_send` про то, когда именно эта ошибка побеждает, а когда —
/// уступает ошибке `send`.
#[derive(Debug)]
pub(crate) struct BodyWriteFailed(wasip3::http_compat::Error);
impl std::fmt::Display for BodyWriteFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to write request body: {}", self.0)
    }
}
impl std::error::Error for BodyWriteFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
fn body_write_failed(e: wasip3::http_compat::Error) -> Error {
    Error::new(ErrorKind::Body, BodyWriteFailed(e))
}

/// Сводит исход двух параллельных действий (`client::send` и
/// `BodyWriter::send_http_body`, отправленных через `join!` в
/// `Transport::execute` — структурная конкуррентность, без `spawn`, это и
/// есть честный `Capabilities::full_duplex`) в один `Result`. Ни один из
/// двух входных `Result` не отбрасывается — именно в этой точке черновик
/// задачи предлагал `let (resp, _written) = join!(..)`, разворачивая ровно
/// тот класс бага, ради устранения которого существует Task 16, только на
/// другом конце запроса, а не на сеттерах.
///
/// **Политика при расхождении исходов.**
/// - `send` вернул `Err` → он в приоритете независимо от исхода записи:
///   `ErrorCode` несёт содержательную таксономию (DNS/connect/TLS/…), а
///   отказ записи тела в этой ситуации почти всегда её симптом (хост
///   закрыл соединение — значит не дочитает и тело).
/// - `send` вернул `Ok`, а запись тела — `Err` → результат **не** считается
///   успехом, даже с ответом на руках. `wasi:http` в этом случае мог
///   вернуть что угодно, в том числе основанное на не полученных им до
///   конца данных — ответу нельзя доверять как ответу именно на тот запрос,
///   который просил вызывающий. Ранний осознанный отказ хоста (сервер
///   прочитал заголовки, вернул 4xx и закрыл поток запроса, не дочитав
///   тело) в v0.1 намеренно не выделяется в успешный путь: это осознанное
///   сужение, а не забытый случай — различать «хост отказался читать
///   дальше» от «наш источник кадров сломался» по одному только
///   `wasip3::http_compat::Error` ненадёжно (оба всплывают похожими
///   вариантами), а расширять `Capabilities` под этот случай — вне объёма
///   этой задачи.
pub(crate) fn resolve_send<T>(
    resp: Result<T, ErrorCode>,
    written: Result<u64, wasip3::http_compat::Error>,
) -> Result<T, Error> {
    match (resp, written) {
        (Ok(r), Ok(_)) => Ok(r),
        (Err(e), _) => Err(wasi_err(e)),
        (Ok(_), Err(e)) => Err(body_write_failed(e)),
    }
}

/// То, что реально нужно записать в тело запроса, после разворачивания
/// `RequestBody`. `Bytes` — цельный буфер (`RequestBody::Full`, уже
/// непустой); `Streaming` — поток кадров как есть, без буферизации.
///
/// `+ Send` на `Streaming` — то же исключение C2, что у
/// `RequestBody::Streaming` (`http-ng-core/src/body.rs`), которое сюда и
/// разворачивается (см. `resolve_payload`): `Box<T>: Send` требует только
/// `T: Send`, `Sync` не нужен. Не новый бонд, а перенос уже существующего —
/// `RequestBody::Streaming` был `Send` до того, как попасть сюда.
pub(crate) enum Payload {
    Bytes(Bytes),
    Streaming(Box<dyn http_body::Body<Data = Bytes, Error = Error> + Unpin + Send>), // send-bound-exception: amendment-C2
}

/// Разворачивает `RequestBody` в то, что реально нужно отправить: `None`
/// для пустого тела, иначе — байты или поток.
///
/// `Rewindable` разворачивается вызовом фабрики один раз — так же, как
/// `RequestBody::rewind()` в `http-ng-core` разворачивает его для повтора
/// (см. doc-комментарий у `RequestBody::Rewindable`: контракт фабрики —
/// чистая функция, каждый вызов производит эквивалентное тело). Без этого
/// шага тело просто не появлялось бы в исходящем запросе — именно так был
/// устроен дефект черновика задачи: `Rewindable`- и `Streaming`-тела молча
/// схлопывались в пустое тело через `_ => None`, при том что
/// `WasiHttp::new()` заявляет `Capabilities::streaming_request_body = true`.
/// Здесь оба случая — реальный путь, а не тихая потеря данных: `Streaming`
/// передаётся в `BodyWriter::send_http_body` как есть (та читает любой
/// `http_body::Body` кадр за кадром и пишет в поток по мере поступления,
/// ничего не буферизуя целиком — то есть действительно стримит, оправдывая
/// заявленную способность), а `Rewindable` — после разворачивания фабрикой.
pub(crate) fn resolve_payload(body: RequestBody) -> Option<Payload> {
    match body {
        RequestBody::Empty => None,
        RequestBody::Full(b) if b.is_empty() => None,
        RequestBody::Full(b) => Some(Payload::Bytes(b)),
        RequestBody::Rewindable(f) => resolve_payload(f()),
        RequestBody::Streaming(s) => Some(Payload::Streaming(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_methods_and_passes_through_unknown() {
        use wasip3::http::types::Method as WM;
        assert!(matches!(to_wasi_method(&http::Method::GET), WM::Get));
        assert!(matches!(to_wasi_method(&http::Method::DELETE), WM::Delete));
        let query = http::Method::from_bytes(b"QUERY").unwrap();
        assert!(matches!(to_wasi_method(&query), WM::Other(ref s) if s == "QUERY"));
    }

    #[test]
    fn rejects_non_http_schemes() {
        let ftp: http::Uri = "ftp://a/x".parse().unwrap();
        assert!(scheme_of(&ftp).is_err());
        let none: http::Uri = "/relative".parse().unwrap();
        assert!(scheme_of(&none).is_err());
    }

    #[test]
    fn capabilities_declare_what_wasi_http_actually_does() {
        let c = super::super::WasiHttp::new();
        let caps = http_ng_core::unversioned::Transport::capabilities(&c);
        // wasi:http 0.3 богаче нативного по стримингу…
        assert!(caps.streaming_request_body);
        assert!(caps.full_duplex);
        assert!(caps.request_trailers && caps.response_trailers);
        // …и беднее по всему остальному.
        assert_eq!(caps.redirects, http_ng_core::RedirectSupport::None);
        assert_eq!(caps.upgrade, http_ng_core::UpgradeSupport::None);
        assert_eq!(caps.tls_config, http_ng_core::TlsSupport::None);
        assert!(!caps.proxy);
    }

    /// Ревью (резолюция team lead, п.1): у черновика задачи `join!` в
    /// `Transport::execute` отбрасывал результат записи тела через
    /// `let (resp, _written) = ..`. `resolve_send` — точка, где это больше
    /// не происходит; тестируем её как чистую функцию (generic по `T`,
    /// поэтому `Response` живого хоста не нужен) на всех трёх исходах.
    #[test]
    fn resolve_send_prefers_the_response_when_both_succeed() {
        let got = resolve_send::<u32>(Ok(7), Ok(3));
        assert_eq!(got.unwrap(), 7);
    }

    #[test]
    fn resolve_send_surfaces_the_send_error_even_if_the_body_wrote_fine() {
        let err = resolve_send::<u32>(Err(ErrorCode::ConnectionRefused), Ok(3)).unwrap_err();
        assert!(err.is_connect(), "{err:?}");
    }

    /// Резолюция team lead, п.1: политика на случай, когда `send` вернул
    /// `Ok`, а запись тела — `Err`. Выбор — не считать это успехом: ответ,
    /// полученный при недописанном теле запроса, нельзя выдавать вызывающей
    /// стороне без сигнала, что тело ушло не целиком.
    #[test]
    fn resolve_send_does_not_trust_a_response_that_arrived_over_a_failed_body_write() {
        let write_err = wasip3::http_compat::Error::StreamReaderClosed {
            written: 2,
            unwritten: vec![9, 9],
        };
        let err = resolve_send::<u32>(Ok(7), Err(write_err)).unwrap_err();
        assert_eq!(err.kind(), &ErrorKind::Body);
    }

    #[test]
    fn resolve_send_send_error_wins_over_a_body_write_error_too() {
        // Оба провалились: `send`-ошибка всё равно в приоритете (см.
        // doc-комментарий `resolve_send` — отказ записи здесь почти всегда
        // симптом того же обрыва, что уронил `send`).
        let write_err = wasip3::http_compat::Error::StreamReaderClosed {
            written: 0,
            unwritten: vec![],
        };
        let err =
            resolve_send::<u32>(Err(ErrorCode::ConnectionTerminated), Err(write_err)).unwrap_err();
        assert!(err.is_connect(), "{err:?}");
    }

    #[test]
    fn timeout_rejection_names_the_capability_and_keeps_the_host_reason_as_source() {
        let e = unsupported_timeout("connect_timeout", RequestOptionsError::NotSupported);
        assert!(e.is_unsupported());
        let msg = e.to_string();
        assert!(msg.contains("connect_timeout"), "{msg}");
        assert!(msg.contains("wasi:http"), "{msg}");
        // Уровень 1: `Error::source()` — сама `TimeoutRejected` (то, что
        // передали в `Error::new`). Уровень 2: её собственный `source()` —
        // настоящий `RequestOptionsError` хоста, ради сохранения которого
        // этот тип и заведён (см. doc-комментарий `TimeoutRejected`).
        let level1 = std::error::Error::source(&e).expect("должен сохранить причину хоста");
        let rejected = level1
            .downcast_ref::<TimeoutRejected>()
            .expect("верхний source — TimeoutRejected");
        let level2 = std::error::Error::source(rejected)
            .expect("TimeoutRejected должен хранить причину хоста как свой source");
        assert!(level2.downcast_ref::<RequestOptionsError>().is_some());
    }

    #[test]
    fn setter_rejection_names_the_field_wasi_refused() {
        let e = rejected("scheme");
        assert!(e.is_unsupported());
        assert!(e.to_string().contains("scheme"));
    }

    #[test]
    fn resolve_payload_treats_empty_and_absent_bodies_alike() {
        assert!(resolve_payload(RequestBody::Empty).is_none());
        assert!(resolve_payload(RequestBody::Full(Bytes::new())).is_none());
    }

    #[test]
    fn resolve_payload_keeps_a_non_empty_full_body() {
        match resolve_payload(RequestBody::Full(Bytes::from_static(b"abc"))) {
            Some(Payload::Bytes(b)) => assert_eq!(&b[..], b"abc"),
            _ => panic!("expected Payload::Bytes"),
        }
    }

    /// Резолюция team lead, п.2: `Rewindable` не должен схлопываться в
    /// `None` — фабрика обязана быть вызвана, а не проигнорирована.
    #[test]
    fn resolve_payload_calls_the_rewindable_factory_instead_of_dropping_it() {
        let body = RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"replayed")));
        match resolve_payload(body) {
            Some(Payload::Bytes(b)) => assert_eq!(&b[..], b"replayed"),
            _ => panic!("expected Payload::Bytes"),
        }
    }

    /// Резолюция team lead, п.2: `Streaming` тоже не должен схлопываться в
    /// `None` — иначе `Capabilities::streaming_request_body = true` было бы
    /// ложью.
    #[test]
    fn resolve_payload_keeps_a_streaming_body_instead_of_dropping_it() {
        struct OneShot(Option<Bytes>);
        impl http_body::Body for OneShot {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
                std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
            }
        }
        let body = RequestBody::Streaming(Box::new(OneShot(Some(Bytes::from_static(b"s")))));
        assert!(matches!(resolve_payload(body), Some(Payload::Streaming(_))));
    }
}
