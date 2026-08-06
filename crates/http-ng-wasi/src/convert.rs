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
use http_body::{Body as HttpBody, Frame};
use http_ng_core::{Error, ErrorKind, RequestBody};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
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

/// Резолюция review, находка B-12: `BadScheme` — тот же класс отказа, что
/// `Rejected` (ниже) — «это конкретное значение бэкенд не берёт» — и
/// раньше единственный из них расплющивался в `ErrorKind::Other`, хотя
/// вызывающей стороне здесь так же полезно уметь отличить «бэкенд это не
/// умеет» от прочих ошибок через `is_unsupported()`.
pub(crate) fn scheme_of(uri: &http::Uri) -> Result<Scheme, Error> {
    match uri.scheme_str() {
        Some("https") => Ok(Scheme::Https),
        Some("http") => Ok(Scheme::Http),
        _ => Err(Error::new(ErrorKind::Unsupported, BadScheme)),
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
/// Резолюция review, находка B-6: `HeaderError::Forbidden`/`::Immutable` —
/// хост отказывается принять конкретный заголовок, структурно тот же класс
/// отказа, что `Rejected`/`TimeoutRejected` (`ErrorKind::Unsupported`), а не
/// общая ошибка. `InvalidSyntax`/`SizeExceeded`/`Other` — не отказ
/// возможности, а дефект/лимит на стороне вызывающего, остаются `Other`.
pub(crate) fn fields_error(e: HeaderError) -> Error {
    let kind = match &e {
        HeaderError::Forbidden | HeaderError::Immutable => ErrorKind::Unsupported,
        _ => ErrorKind::Other,
    };
    Error::new(kind, FieldsError(e))
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
        | EC::HttpResponseContentCoding(_)
        // Резолюция review, находка B-8: те же семьи запроса/трейлеров,
        // что уже даёт `ErrorKind::Body` для ответа выше — 14 из 39
        // вариантов `ErrorCode` падали в `_ => Other` без разбора; здесь
        // явно присоединены только те, что очевидно принадлежат уже
        // существующей категории `Body` (размер/наличие тела или
        // трейлеров), а не изобретена новая категория под остальные —
        // `HttpRequestMethodInvalid`/`HttpRequestUriInvalid`/
        // `HttpProtocolError` и т.п. по-прежнему честно `Other`: для них
        // здесь нет очевидной категории.
        | EC::HttpRequestLengthRequired
        | EC::HttpRequestBodySize(_)
        | EC::HttpRequestTrailerSectionSize(_)
        | EC::HttpRequestTrailerSize(_)
        | EC::HttpResponseTrailerSectionSize(_)
        | EC::HttpResponseTrailerSize(_) => ErrorKind::Body,
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
/// `BodyWriter::send_http_body`) в один `Result`. Ни один из двух входных
/// `Result` не отбрасывается — именно в этой точке черновик задачи
/// предлагал `let (resp, _written) = join!(..)`.
///
/// **Про третий отброшенный `Result` — резолюция review, находка B-3,
/// пересмотрена с доказательством, формулировка уточнена в fix round 2
/// (находка 5: предыдущая версия этого комментария переоценивала то, что
/// было измерено — см. ниже).** Второй возврат `Request::new` —
/// `FutureReader<Result<(), ErrorCode>>`, задокументированный upstream как
/// "resolves to result of transmission of this request" — тоже
/// отбрасывается (`Transport::execute` дропает его явно, с комментарием на
/// месте дропа). План ревью — включить его сюда третьим входом — был
/// реализован и **откачен**: измерено на живом хосте (wasmtime 47,
/// `wasip3` 0.7.0, дважды — один раз через полный путь этого крейта, один
/// раз через голые вызовы `wasip3` в обход `http-ng-wasi` целиком, чтобы
/// исключить баг в собственной логике гонки), что эта футура **не
/// гарантированно** резолвится до того, как тело ответа вычитано целиком —
/// НЕ "никогда не резолвится раньше": заново проверено отдельно (12-байтный
/// `Content-Length`-ответ, `resp` дропнут немедленно, тело не тронуто ни
/// разу) — `transmitted` резолвится в `Ok(())` без единого вызова
/// `poll_frame`. Но для `chunked`, ответов с трейлерами и вообще
/// сколько-нибудь заметных тел измеримо не резолвится, пока `Body` не
/// вычитан вручную — `send()` возвращал `Ok` немедленно, футура передачи
/// оставалась `Pending`, пока весь `Body` не был вычитан, и резолвилась
/// только тогда. `execute()` возвращает `Body` вызывающей стороне ДО того,
/// как та решит его читать — штатно позже, частично, или никогда. Ждать
/// эту футуру здесь БЕЗУСЛОВНО означало бы: либо `execute()` не
/// возвращается для типичного ответа с телом, пока сама не вычитает его
/// целиком за вызывающую сторону (разрушая стриминг, ради которого
/// существует `Body`), либо виснет для штатного сценария "тело читают
/// после получения `Response`" — то есть после того, как `execute()` уже
/// обязана была вернуться.
///
/// Ни один вариант не совместим с ЭТОЙ формой seam — но третий вариант
/// совместим и вне объёма этого фикс-раунда: пронести эту футуру в
/// возвращаемый `Body` и дождаться её на конце потока, выдав отказ
/// передачи как терминальную ошибку тела вместо чистого `None`. Ровно
/// измеренное здесь его и оправдывает — футура резолвится в момент, когда
/// тело закончило вычитываться, а это именно та точка, где живёт `Body`.
/// Кандидат на v0.2, а не отвергнутый тупик. Подробности и точка дропа —
/// `Transport::execute` в `lib.rs`.
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

/// Гонит `client::send` наперегонки с записью тела, а не ждёт оба через
/// `join!`, — резолюция review, находка B-5. Раньше `join!` держал
/// `execute` до конца ОБОИХ плеч; отказ хоста, известный за t≈0 (например
/// `ConnectionRefused`), придерживался до конца записи тела — для
/// неограниченного потокового тела навсегда, поскольку оно никогда не
/// допишется само. `resolve_send` и так уже отбрасывает исход записи, когда
/// `send` проваливается (см. её doc-комментарий) — короткое замыкание
/// здесь меняет только задержку, не политику: если `send` первым
/// резолвится в `Err`, эта функция возвращает его немедленно, роняя ещё не
/// завершённый `write_fut` (безопасно — компонентная модель поддерживает
/// отмену незавершённой подзадачи через `drop`, см. `TaskCancelOnDrop` в
/// сгенерированных байндингах). Если `send` резолвится успехом первым (или
/// вторым), запись всё равно дожидается — `resolve_send` не может
/// довериться ответу, не зная её исхода.
///
/// **Именно это ожидание, и только оно, делает `full_duplex = false`** (M1
/// финального ревью ветки). Плечо `Either::Left((Ok(r), write_fut))` держит
/// на руках готовый ответ и блокируется на записи; для бесконечного
/// `Streaming`-тела — навсегда. Форма `Transport::execute` тут ни при чём:
/// `write_fut` можно пронести в возвращаемый `Body` (тип этого крейта) и
/// доопрашивать из `poll_frame` — ровно тот приём, который doc-комментарий
/// `resolve_send` уже предлагает для `transmitted`. Ревью реализовало это и
/// померило: 0.094s до головы ответа против зависания. Полное обоснование и
/// три цены, из-за которых отложено, — в `WasiHttp::new` (`lib.rs`).
pub(crate) async fn race_send_with_body<T>(
    send_fut: impl Future<Output = Result<T, ErrorCode>>,
    write_fut: impl Future<Output = Result<u64, wasip3::http_compat::Error>>,
) -> Result<T, Error> {
    use futures::future::Either;
    let send_fut = std::pin::pin!(send_fut);
    let write_fut = std::pin::pin!(write_fut);
    match futures::future::select(send_fut, write_fut).await {
        Either::Left((Err(e), _write_fut)) => Err(wasi_err(e)),
        Either::Left((Ok(r), write_fut)) => resolve_send(Ok(r), write_fut.await),
        Either::Right((written, send_fut)) => resolve_send(send_fut.await, written),
    }
}

/// Разбирает заголовок(и) `Trailer:` запроса в множество объявленных имён
/// трейлер-полей. `Trailer:` — список через запятую (RFC 9110 §6.5.1),
/// возможно повторённый несколькими заголовками; оба варианта складываются
/// в одно множество. `HeaderName` уже сравнивается регистронезависимо
/// (`http` хранит имя в канонической форме), так что `X-Checksum` и
/// `x-checksum` — одно и то же имя.
pub(crate) fn declared_trailer_names(
    headers: &http::HeaderMap,
) -> std::collections::HashSet<http::HeaderName> {
    headers
        .get_all(http::header::TRAILER)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| http::HeaderName::from_bytes(s.trim().as_bytes()).ok())
        .collect()
}

/// Тело запроса эмитировало трейлер-поле(я), которые `Trailer:` не назвал.
///
/// `wasi:http` честно принимает трейлеры от `BodyWriter` (они уходят в
/// `result_writer` независимо от заголовков — см.
/// `wasip3::http_compat::body_writer::send_http_body`), но измерено на
/// живом хосте (резолюция review, находка B-1), что HTTP/1.1-кодировщик
/// хоста молча роняет их на проводе, если конкретное имя поля не было
/// заранее объявлено через `Trailer:` (RFC 9110 §6.5.1: получатель,
/// решивший не буферизовать тело, обязан игнорировать необъявленные
/// трейлеры) — резолюция review, находка 2 фикс-раунда 2: сверять надо
/// именно ИМЕНА полей, а не сам факт присутствия заголовка `Trailer:`.
/// Заголовок `Trailer: X-Other`, объявляющий не то поле, что реально
/// эмитировано (`x-checksum`), теряет данные точно так же, как полное
/// отсутствие заголовка — измерено: `execute` возвращал `Ok(200)`, а
/// провод показывал `0\r\n\r\n` без трейлера.
///
/// **Ошибка приходит постфактум** (резолюция review, находка 4 фикс-раунда
/// 2): к моменту, когда вызывающая сторона видит эту ошибку, запрос уже
/// дошёл до сервера и получил ответ — гвард срабатывает уже ПОСЛЕ успешного
/// `race_send_with_body`, потому что имена трейлер-полей, вообще говоря,
/// известны только когда тело закончилось, а раньше заголовков это не
/// предсказать. Не считать это признаком, что запрос можно бездумно
/// повторить: для неидемпотентного запроса это двойная отправка.
#[derive(Debug)]
pub(crate) struct UndeclaredTrailers(Vec<http::HeaderName>);
impl std::fmt::Display for UndeclaredTrailers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = self
            .0
            .iter()
            .map(http::HeaderName::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            f,
            "streaming request body emitted trailer field(s) [{names}] that the request's \
             `Trailer:` header did not declare — wasi:http's HTTP/1.1 encoder drops undeclared \
             trailer fields silently. This error arrives after the request already reached the \
             server and was answered (race_send_with_body already succeeded) — do not retry \
             blindly, a non-idempotent request may already have taken effect."
        )
    }
}
impl std::error::Error for UndeclaredTrailers {}
pub(crate) fn undeclared_trailers(names: Vec<http::HeaderName>) -> Error {
    Error::new(ErrorKind::Body, UndeclaredTrailers(names))
}

/// Обёртка `http_body::Body`, которая ничего не меняет в передаваемых
/// кадрах, а только собирает имена полей из кадров трейлеров, которые
/// реально прошли через обёрнутое тело — нужна `Transport::execute`, чтобы
/// после успешной отправки сверить их с тем, что объявил `Trailer:`
/// (находка B-1, уточнена находкой 2 фикс-раунда 2 — именами, не только
/// фактом присутствия заголовка), в точке, где кадр реально пришёл, не
/// предсказывая это заранее.
pub(crate) struct TrailerWatch<B> {
    inner: B,
    seen: Arc<Mutex<Vec<http::HeaderName>>>,
}

impl<B> TrailerWatch<B> {
    /// Оборачивает `inner` и возвращает список имён полей, увиденных в
    /// кадрах трейлеров, — растёт по мере опроса. `Arc<Mutex<_>>`, а не
    /// `Rc<RefCell<_>>`: `Payload::Streaming` уже несёт `+ Send` (amendment
    /// C2), и обёртка не должна сузить этот бонд для будущего
    /// `Transport::execute` (`tests/shape.rs` проверяет это свойство
    /// снаружи крейта). `Mutex`, а не что-то более экзотичное — гость
    /// однопоточный, конкуренции за блокировку никогда не бывает, `Mutex`
    /// здесь ровно формальность ради `Sync`.
    pub(crate) fn new(inner: B) -> (Self, Arc<Mutex<Vec<http::HeaderName>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                inner,
                seen: seen.clone(),
            },
            seen,
        )
    }
}

impl<B> HttpBody for TrailerWatch<B>
where
    B: HttpBody<Data = Bytes, Error = Error> + Unpin,
{
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let poll = Pin::new(&mut self.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(f))) = &poll {
            // Резолюция review, находка 3 фикс-раунда 2: `trailers_ref`
            // возвращает `Some(&HeaderMap)` и для ПУСТОГО кадра трейлеров
            // (`Frame::trailers(HeaderMap::new())`) — такой кадр ничего не
            // теряет на проводе (нечему теряться), так что регистрируем
            // только непустые карты, а не сам факт "это кадр трейлеров".
            if let Some(map) = f.trailers_ref()
                && !map.is_empty()
            {
                let mut seen = self
                    .seen
                    .lock()
                    .expect("single-threaded guest, never poisoned");
                seen.extend(map.keys().cloned());
            }
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
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

// Ручной `Debug`, не `#[derive]`: `Streaming` несёт `Box<dyn http_body::Body>`
// — объект-трейт без бонда `Debug`, derive не собрался бы. Тот же приём,
// что у `body::Inner` в `body.rs` — печатает только имя варианта.
impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Payload::Bytes(b) => f.debug_tuple("Bytes").field(b).finish(),
            Payload::Streaming(_) => f.write_str("Streaming(..)"),
        }
    }
}

/// Верхняя граница вложенности `RequestBody::Rewindable`, чья фабрика сама
/// возвращает `Rewindable`. Легитимного сценария для этого нет — фабрика,
/// зовущая другую фабрику, ссылающуюся на третью, ничего не покупает
/// вызывающей стороне — так что после этой глубины `resolve_payload`
/// останавливается типизированной ошибкой, а не тихим `None` или
/// неограниченной рекурсией (резолюция review, находка B-11).
const MAX_REWIND_DEPTH: u8 = 16;

#[derive(Debug)]
pub(crate) struct RewindTooDeep;
impl std::fmt::Display for RewindTooDeep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RequestBody::Rewindable factory nested more than {MAX_REWIND_DEPTH} levels deep \
             (each factory call returned another Rewindable instead of a terminal body)"
        )
    }
}
impl std::error::Error for RewindTooDeep {}

/// Разворачивает `RequestBody` в то, что реально нужно отправить: `None`
/// для пустого тела, иначе — байты или поток.
///
/// `Rewindable` разворачивается вызовом фабрики — так же, как
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
///
/// Итеративно, не рекурсивно, и с границей `MAX_REWIND_DEPTH` — фабрика,
/// сама возвращающая `Rewindable`, разворачивалась бы бесконечно (или до
/// переполнения стека) без неё.
pub(crate) fn resolve_payload(body: RequestBody) -> Result<Option<Payload>, Error> {
    let mut body = body;
    for _ in 0..MAX_REWIND_DEPTH {
        match body {
            RequestBody::Empty => return Ok(None),
            RequestBody::Full(b) if b.is_empty() => return Ok(None),
            RequestBody::Full(b) => return Ok(Some(Payload::Bytes(b))),
            RequestBody::Rewindable(f) => body = f(),
            RequestBody::Streaming(s) => return Ok(Some(Payload::Streaming(s))),
        }
    }
    Err(Error::new(ErrorKind::Other, RewindTooDeep))
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

    /// Резолюция review, находка B-12: `BadScheme` — тот же класс отказа,
    /// что `Rejected`/`TimeoutRejected` — должен классифицироваться как
    /// `Unsupported`, а не расплющиваться в `Other`.
    #[test]
    fn bad_scheme_is_classified_as_unsupported_not_other() {
        let ftp: http::Uri = "ftp://a/x".parse().unwrap();
        let err = scheme_of(&ftp).unwrap_err();
        assert!(err.is_unsupported(), "{err:?}");
    }

    #[test]
    fn capabilities_declare_what_wasi_http_actually_does() {
        let c = super::super::WasiHttp::new();
        let caps = http_ng_core::unversioned::Transport::capabilities(&c);
        // wasi:http 0.3 богаче нативного по стримингу тела запроса…
        assert!(caps.streaming_request_body);
        assert!(caps.request_trailers && caps.response_trailers);
        // …но НЕ по body-дуплексу: резолюция review, находка B-2.
        // `WasiHttp::execute` не возвращает ответ, пока не завершится (или
        // не откажет) запись тела — `race_send_with_body` дожидается обоих
        // плеч, кроме случая раннего отказа `send` (B-5). Это ограничение
        // ЭТОЙ реализации: и хост дуплекс даёт, и снять ограничение можно
        // не трогая `Transport` — футуру записи проносят в `Body` и
        // доопрашивают из `poll_frame`. Полное обоснование и три цены,
        // из-за которых отложено, — в `WasiHttp::new` (M1 финального
        // ревью ветки).
        assert!(!caps.full_duplex);
        // И беднее по всему остальному.
        // `Transparent`, а не `None` (M2 финального ревью ветки): 3xx
        // доходит до гостя как обычный ответ и стадия редиректа `Client`'а
        // отрабатывает его полностью. `None` — то же, что отдаёт
        // `Capabilities::none()`, и означало бы «редиректов тут нет».
        assert_eq!(caps.redirects, http_ng_core::RedirectSupport::Transparent);
        assert_ne!(
            caps.redirects,
            http_ng_core::Capabilities::none().redirects,
            "заявленная способность обязана отличаться от «поле не заполняли»"
        );
        assert_eq!(caps.upgrade, http_ng_core::UpgradeSupport::None);
        assert_eq!(caps.tls_config, http_ng_core::TlsSupport::None);
        assert!(!caps.proxy);
        // Резолюция review, находка B-6: пять заголовков, которые хост
        // реально отказывается принять от гостя.
        for name in [
            http::header::CONNECTION,
            http::header::HeaderName::from_static("keep-alive"),
            http::header::TRANSFER_ENCODING,
            http::header::UPGRADE,
            http::header::HOST,
        ] {
            assert!(
                caps.forbidden_request_headers.contains(&name),
                "{name} should be in forbidden_request_headers"
            );
        }
    }

    /// B2 финального ревью ветки: вся классификация `wasi_err` (39
    /// вариантов `ErrorCode` на восемь `ErrorKind`) существует только
    /// потому, что `Transport::to_error` этого бэкенда — тождество. С
    /// дефолтной реализацией `Client::execute` завернул бы её ещё раз, в
    /// `ErrorKind::Other`, и всё, что проверяют тесты выше про
    /// `is_connect()`/`is_unsupported()`, было бы верно на уровне
    /// `wasi_err` и ложно у вызывающей стороны.
    ///
    /// Проверяются оба наблюдаемых следствия сразу: категория и `Display`
    /// (тот при обёртывании печатал бы `Other: Tls: …` — отложенный минор
    /// Task 6 про удвоение).
    #[test]
    fn to_error_is_the_identity_so_the_classification_survives_the_client() {
        use http_ng_core::unversioned::Transport as _;

        let t = super::super::WasiHttp::new();
        let classified = wasi_err(ErrorCode::TlsProtocolError);
        let seen = t.to_error(classified);

        assert_eq!(seen.kind(), &ErrorKind::Tls);
        assert!(
            !seen.to_string().starts_with("Other:"),
            "категория не должна вкладываться вторично: {seen}"
        );
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

    /// Крутит будущее вручную ограниченное число раз вместо неограниченного
    /// цикла — если бы `race_send_with_body` в самом деле ждала
    /// "никогда не завершающуюся" футуру (баг, который этот тест и должен
    /// поймать), неограниченный цикл повис бы навсегда вместо честного
    /// провала теста.
    fn poll_bounded<F: Future>(mut fut: Pin<&mut F>, max_polls: usize) -> Option<F::Output> {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        for _ in 0..max_polls {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return Some(v);
            }
        }
        None
    }

    /// Резолюция review, находка B-5, доказана детерминированно, а не по
    /// времени на часах: `write_fut` — `std::future::pending()`, то есть
    /// буквально никогда не завершается сама. Если бы `race_send_with_body`
    /// ждала `join!`-ом оба плеча целиком (старое поведение), результат не
    /// пришёл бы никогда — `poll_bounded` вернул бы `None` за отведённые
    /// попытки, и `.expect(..)` ниже провалил бы тест. То, что она
    /// возвращается — прямое доказательство короткого замыкания на ранней
    /// ошибке `send`, а не измерение таймингов на живом хосте, которое
    /// могло бы разойтись между прогонами.
    #[test]
    fn race_send_with_body_short_circuits_on_early_send_failure() {
        let send_fut = std::future::ready(Err::<u32, ErrorCode>(ErrorCode::ConnectionRefused));
        let write_fut = std::future::pending::<Result<u64, wasip3::http_compat::Error>>();
        let mut fut = std::pin::pin!(race_send_with_body(send_fut, write_fut));
        let got = poll_bounded(fut.as_mut(), 64).expect(
            "race_send_with_body must resolve promptly on an early send failure, \
             not hang waiting for a body write that never finishes",
        );
        assert!(got.unwrap_err().is_connect());
    }

    /// Симметрия: когда `send` успевает раньше и завершается успехом,
    /// `race_send_with_body` обязана дождаться записи, а не вернуть успех
    /// преждевременно — короткое замыкание работает только для отказа
    /// `send`, см. doc-комментарий.
    #[test]
    fn race_send_with_body_still_waits_for_the_body_when_send_succeeds() {
        let send_fut = std::future::ready(Ok::<u32, ErrorCode>(7));
        let write_fut = std::future::ready(Ok::<u64, wasip3::http_compat::Error>(3));
        let mut fut = std::pin::pin!(race_send_with_body(send_fut, write_fut));
        let got = poll_bounded(fut.as_mut(), 64).expect("must resolve when both are ready");
        assert_eq!(got.unwrap(), 7);
    }

    /// Управляемая футура для тестов: `Pending` первые `delay` опросов,
    /// затем `Ready(value)` — нужна, чтобы детерминированно свести гонку в
    /// ветку `Either::Right` (запись тела резолвится раньше `send`), а не
    /// полагаться на то, что `futures::future::select` опрашивает первый
    /// аргумент первым (с двумя `std::future::ready(..)` он бы всегда
    /// уходил в `Either::Left`, не проверяя вторую ветку вовсе).
    struct DelayedReady<T> {
        delay: usize,
        value: Option<T>,
    }
    impl<T: Unpin> Future for DelayedReady<T> {
        type Output = T;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
            if self.delay > 0 {
                self.delay -= 1;
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(self.value.take().expect("polled again after ready"))
            }
        }
    }

    /// Симметрия: когда запись тела завершается первой (успешно), гонка всё
    /// равно обязана дождаться `send` — не факт, что он тоже успешен.
    /// Проверяет ветку `Either::Right`, которую предыдущие два теста не
    /// достигают (там `send` всегда готов на первом опросе).
    #[test]
    fn race_send_with_body_waits_for_send_when_the_write_finishes_first() {
        let send_fut = DelayedReady {
            delay: 5,
            value: Some(Err::<u32, ErrorCode>(ErrorCode::ConnectionRefused)),
        };
        let write_fut = std::future::ready(Ok::<u64, wasip3::http_compat::Error>(3));
        let mut fut = std::pin::pin!(race_send_with_body(send_fut, write_fut));
        let got = poll_bounded(fut.as_mut(), 64).expect("must resolve");
        assert!(got.unwrap_err().is_connect());
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

    /// Резолюция review, находка B-6: `HeaderError::Forbidden`/`::Immutable`
    /// — хост отказал, тот же класс, что `Rejected` — обязаны быть
    /// `Unsupported`, а не `Other`.
    #[test]
    fn fields_error_classifies_host_refusals_as_unsupported() {
        let forbidden = fields_error(HeaderError::Forbidden);
        assert!(forbidden.is_unsupported(), "{forbidden:?}");
        let immutable = fields_error(HeaderError::Immutable);
        assert!(immutable.is_unsupported(), "{immutable:?}");
    }

    #[test]
    fn fields_error_leaves_genuine_input_defects_as_other() {
        let bad = fields_error(HeaderError::InvalidSyntax);
        assert!(!bad.is_unsupported(), "{bad:?}");
        assert_eq!(bad.kind(), &ErrorKind::Other);
    }

    /// Резолюция review, находка B-8: варианты `ErrorCode`, явно из той же
    /// семьи "размер/наличие тела или трейлеров", что уже даёт `Body` для
    /// ответа — присоединены к той же категории на стороне запроса.
    #[test]
    fn wasi_err_categorizes_request_and_trailer_size_errors_as_body() {
        for code in [
            ErrorCode::HttpRequestLengthRequired,
            ErrorCode::HttpRequestBodySize(Some(1)),
            ErrorCode::HttpRequestTrailerSectionSize(Some(1)),
            ErrorCode::HttpResponseTrailerSize(wasip3::http::types::FieldSizePayload {
                field_name: None,
                field_size: None,
            }),
        ] {
            let e = wasi_err(code);
            assert_eq!(e.kind(), &ErrorKind::Body, "{e:?}");
        }
    }

    #[test]
    fn wasi_err_leaves_genuinely_uncategorized_codes_as_other() {
        // `HttpProtocolError` не принадлежит ни одной существующей
        // категории очевидным образом — остаётся честным `Other`, а не
        // насильно распределённым по неподходящей корзине.
        let e = wasi_err(ErrorCode::HttpProtocolError);
        assert_eq!(e.kind(), &ErrorKind::Other);
    }

    #[test]
    fn resolve_payload_treats_empty_and_absent_bodies_alike() {
        assert!(resolve_payload(RequestBody::Empty).unwrap().is_none());
        assert!(
            resolve_payload(RequestBody::Full(Bytes::new()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn resolve_payload_keeps_a_non_empty_full_body() {
        match resolve_payload(RequestBody::Full(Bytes::from_static(b"abc"))).unwrap() {
            Some(Payload::Bytes(b)) => assert_eq!(&b[..], b"abc"),
            _ => panic!("expected Payload::Bytes"),
        }
    }

    /// Резолюция team lead, п.2: `Rewindable` не должен схлопываться в
    /// `None` — фабрика обязана быть вызвана, а не проигнорирована.
    #[test]
    fn resolve_payload_calls_the_rewindable_factory_instead_of_dropping_it() {
        let body = RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"replayed")));
        match resolve_payload(body).unwrap() {
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
        assert!(matches!(
            resolve_payload(body).unwrap(),
            Some(Payload::Streaming(_))
        ));
    }

    /// Резолюция review, находка B-11: раньше рекурсивная реализация
    /// разворачивала бы такую фабрику до переполнения стека. `infinite` —
    /// функция-элемент (не замыкание), поэтому она тривиально `Fn + Send +
    /// Sync + 'static` без ручных бондов — и каждый её вызов возвращает
    /// ЕЩЁ один `Rewindable`, ссылающийся на неё же, то есть легитимного
    /// завершения у этой цепочки нет вообще.
    #[test]
    fn resolve_payload_stops_at_a_bounded_depth_instead_of_recursing_forever() {
        fn infinite() -> RequestBody {
            RequestBody::rewindable(infinite)
        }
        let err = resolve_payload(RequestBody::rewindable(infinite)).unwrap_err();
        assert_eq!(err.kind(), &ErrorKind::Other);
    }

    struct DataThenTrailers {
        data: Option<Bytes>,
        trailers: Option<http::HeaderMap>,
    }
    impl HttpBody for DataThenTrailers {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
            if let Some(d) = self.data.take() {
                return Poll::Ready(Some(Ok(Frame::data(d))));
            }
            if let Some(t) = self.trailers.take() {
                return Poll::Ready(Some(Ok(Frame::trailers(t))));
            }
            Poll::Ready(None)
        }
    }

    fn poll_once<B: HttpBody<Data = Bytes, Error = Error> + Unpin>(
        b: &mut B,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        Pin::new(b).poll_frame(&mut cx)
    }

    /// Резолюция review, находка B-1: тело эмитит кадр трейлеров —
    /// `TrailerWatch` обязана это заметить (по имени поля), не трогая сами
    /// кадры.
    #[test]
    fn trailer_watch_records_the_field_name_without_altering_the_frame() {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("x-checksum", "deadbeef".parse().unwrap());
        let body = DataThenTrailers {
            data: Some(Bytes::from_static(b"x")),
            trailers: Some(trailers),
        };
        let (mut watched, seen) = TrailerWatch::new(body);
        assert!(seen.lock().unwrap().is_empty());

        // Кадр данных: список ещё должен быть пуст.
        match poll_once(&mut watched) {
            Poll::Ready(Some(Ok(f))) => assert!(f.is_data()),
            other => panic!("expected a data frame, got {other:?}"),
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "data frame is not trailers"
        );

        // Кадр трейлеров: имя поля должно появиться, сам кадр — дойти до
        // вызывающей стороны нетронутым.
        match poll_once(&mut watched) {
            Poll::Ready(Some(Ok(f))) => {
                assert!(f.is_trailers());
                assert_eq!(
                    f.trailers_ref().unwrap().get("x-checksum").unwrap(),
                    "deadbeef"
                );
            }
            other => panic!("expected a trailers frame, got {other:?}"),
        }
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[http::header::HeaderName::from_static("x-checksum")]
        );
    }

    /// Резолюция review, находка 3 фикс-раунда 2: пустой кадр трейлеров
    /// (`Frame::trailers(HeaderMap::new())`) ничего не теряет на проводе —
    /// `TrailerWatch` не должна регистрировать его как "трейлеры были".
    /// До фикса именно это ловило запрос, который на деле ничего не терял:
    /// регресс, который этот тест и предотвращает.
    #[test]
    fn trailer_watch_ignores_an_empty_trailers_frame() {
        let body = DataThenTrailers {
            data: Some(Bytes::from_static(b"x")),
            trailers: Some(http::HeaderMap::new()),
        };
        let (mut watched, seen) = TrailerWatch::new(body);
        let _ = poll_once(&mut watched); // data frame
        match poll_once(&mut watched) {
            Poll::Ready(Some(Ok(f))) => assert!(f.is_trailers()),
            other => panic!("expected a (empty) trailers frame, got {other:?}"),
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "an empty trailers frame loses nothing on the wire and must not be flagged"
        );
    }

    #[test]
    fn declared_trailer_names_parses_a_comma_separated_list() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::TRAILER,
            "X-Checksum, X-Other".parse().unwrap(),
        );
        let declared = declared_trailer_names(&headers);
        assert!(declared.contains(&http::HeaderName::from_static("x-checksum")));
        assert!(declared.contains(&http::HeaderName::from_static("x-other")));
        assert_eq!(declared.len(), 2);
    }

    #[test]
    fn declared_trailer_names_merges_repeated_headers() {
        let mut headers = http::HeaderMap::new();
        headers.append(http::header::TRAILER, "X-Checksum".parse().unwrap());
        headers.append(http::header::TRAILER, "X-Other".parse().unwrap());
        let declared = declared_trailer_names(&headers);
        assert_eq!(declared.len(), 2);
    }

    #[test]
    fn declared_trailer_names_is_empty_without_a_trailer_header() {
        let headers = http::HeaderMap::new();
        assert!(declared_trailer_names(&headers).is_empty());
    }

    /// Резолюция review, находка 2 фикс-раунда 2: сообщение обязано назвать
    /// конкретное поле — "generic refusal" недостаточно — и явно
    /// предупредить о постфактум-природе ошибки (находка 4).
    #[test]
    fn undeclared_trailers_error_names_the_field_and_warns_about_retrying() {
        let e = undeclared_trailers(vec![http::HeaderName::from_static("x-checksum")]);
        assert_eq!(e.kind(), &ErrorKind::Body);
        let msg = e.to_string();
        assert!(msg.contains("x-checksum"), "{msg}");
        assert!(msg.contains("Trailer"), "{msg}");
        assert!(
            msg.to_lowercase().contains("retry"),
            "must warn against blind retries: {msg}"
        );
    }
}
