//! Транспорт http-ng поверх `wasi:http` 0.3 (пакет `wasip3`).
//!
//! Собирается под `wasm32-wasip2`. Ни один тип `wasip3` не появляется в
//! публичном API этого крейта: `Body::Error` — это `http_ng_core::Error`,
//! которая стирает источник в `Arc<dyn std::error::Error + Send + Sync>`.
#![deny(unsafe_code)]

mod body;
mod convert;

pub use body::Body;

use convert::{Payload, TrailerWatch};
use http_ng_core::unversioned::Transport;
use http_ng_core::{
    Capabilities, Error, RedirectSupport, RequestBody, TimeoutSupport, Timeouts, TlsSupport,
    UpgradeSupport,
};
use wasip3::http::types::{ErrorCode, Fields, Request, RequestOptions};
use wasip3::http_compat::{BodyWriter, http_from_wasi_response};

/// Заголовки, которые хост `wasi:http` отказывается принять от гостя
/// (резолюция review, находка B-6): `connection`/`keep-alive` — управление
/// соединением здесь целиком на стороне хоста; `transfer-encoding` — хост
/// сам решает кодировку передачи по фактическому телу; `upgrade` — нет
/// поддержки апгрейда протокола (`Capabilities::upgrade` и так
/// `UpgradeSupport::None`); `host` — хост сам вычисляет его из `authority`.
/// Измерено попыткой отправить каждый из них — `wasi_request.set_method`
/// проходит, а сам запрос с любым из этих заголовков в `Fields`
/// хост отвергает. Раньше список был пуст, так что вызывающая сторона,
/// фильтрующая заголовки именно по этому полю — весь смысл его
/// существования, — ловила рантайм-ошибку вместо того, чтобы просто не
/// выставить запрещённый заголовок.
// `LazyLock`, не `static X: &[..] = &[..]`: `HeaderName` хранит имя в
// `Bytes`, у которой интерьерная мутабельность (атомарный рефкаунт) —
// компилятор запрещает "продвигать" литерал с таким значением в
// статический слайс напрямую (`E0492`). Инициализируется один раз при
// первом обращении и живёт до конца процесса — `'static`-ссылка на неё
// корректна.
static FORBIDDEN_REQUEST_HEADERS: std::sync::LazyLock<[http::HeaderName; 5]> =
    std::sync::LazyLock::new(|| {
        [
            http::header::CONNECTION,
            http::header::HeaderName::from_static("keep-alive"),
            http::header::TRANSFER_ENCODING,
            http::header::UPGRADE,
            http::header::HOST,
        ]
    });

/// Транспорт над амбиентным `wasi:http/client.send` — гость не держит
/// собственного сокета, всё сетевое взаимодействие делегировано хосту.
#[derive(Debug)]
pub struct WasiHttp {
    caps: Capabilities,
}

impl WasiHttp {
    pub fn new() -> Self {
        let mut caps = Capabilities::none();
        // `streaming_request_body`: `RequestBody::Streaming` уходит в
        // `BodyWriter::send_http_body` как есть, кадр за кадром, без
        // буферизации в память — честный стриминг, см.
        // `convert::resolve_payload`.
        caps.streaming_request_body = true;
        // `full_duplex = false` — резолюция review, находка B-2; атрибуция
        // переписана по M1 финального ревью ветки. Сам протокол `wasi:http`
        // 0.3 поддерживает body-дуплекс (данные тела могут идти, пока ответ
        // ещё не получен), и хост его действительно даёт — ревью померило
        // прямыми вызовами `wasip3` в обход этого крейта: `send` резолвился
        // полным 200 после того, как в писателя ушло 1.6% 16-мегабайтного
        // тела. Не даёт его ЭТА реализация `execute`:
        // `race_send_with_body` дожидается и `send`, и записи тела (кроме
        // одного случая — раннего отказа `send`, см. B-5). Измерено на
        // живом хосте: ответ существовал на стороне сервера в момент
        // t≈0.10s, но вызывающая сторона видела его только в t≈2.00s, к
        // концу записи тела; для тела без конца — не увидела бы вообще.
        //
        // **Форма seam здесь ни при чём** — прежняя версия этого
        // комментария утверждала обратное («исправление меняет форму
        // сигнатуры seam»), и это было неверно. `Transport::execute`
        // возвращает `http::Response<Self::Body>`, а `Self::Body` — это
        // `Body` из этого же крейта: недописанную футуру записи можно
        // пронести в неё и доопрашивать из `poll_frame`, отдав ответ
        // вызывающей стороне немедленно, а отказ передачи — терминальной
        // ошибкой тела. Ревью это реализовало как proof-of-concept: около
        // сорока строк, один новый вариант `Inner`, сигнатура
        // `Transport::execute` не тронута — и померило на том же госте и
        // сервере: ветка как есть висит до убийства на 25s, вариант с
        // футурой в `Body` отдаёт голову ответа за 0.094s. Приём не новый:
        // ровно его предлагает doc-комментарий `convert::resolve_send` для
        // ДРУГОЙ отброшенной футуры (`transmitted`) — рассуждение было
        // проделано и просто не применено к `write_fut`.
        //
        // Отложено (вертикаль 2, целиком внутри этого крейта) из-за трёх
        // реальных цен, ни одна из которых не про seam:
        //  1. Гвард необъявленных трейлеров не может отработать до возврата
        //     `execute`: имена трейлеров известны только когда тело
        //     кончилось. Он переезжает в `Body` и становится терминальной
        //     ошибкой тела — это настоящая переделка функции, над которой
        //     ветка много работала (см. `caps.request_trailers` ниже).
        //  2. Политика `resolve_send` («ответ, пришедший поверх
        //     провалившейся записи тела, — не успех») из ошибки уровня
        //     `execute` становится ошибкой уровня тела. Слабее — хотя,
        //     возможно, и правильнее: ответ у вызывающей стороны уже на
        //     руках.
        //  3. Вызывающая сторона, которая никогда не читает тело ответа,
        //     никогда не дописывает и тело запроса. Присуще дуплексу без
        //     `spawn`, требует документирования в контракте.
        caps.full_duplex = false;
        // `request_trailers`/`response_trailers`: трейлеры РАБОТАЮТ на
        // `wasi:http`, но только для полей, чьи ИМЕНА запрос заранее
        // объявил через заголовок `Trailer:` — измерено (резолюция review,
        // находка B-1, уточнена находкой 2 фикс-раунда 2), что
        // HTTP/1.1-кодировщик хоста молча роняет на проводе любое
        // трейлер-поле, чьё имя не было объявлено, даже если `Trailer:`
        // присутствует, но называет ДРУГОЕ поле. Имена трейлеров известны
        // только после того, как тело закончилось — раньше заголовков их
        // предсказать нельзя, инъецировать `Trailer:` за вызывающую сторону
        // здесь некому. Вызывающая сторона обязана сама выставить
        // `Trailer:` с именами ВСЕХ полей, которые её `RequestBody::Streaming`
        // будет эмитить как трейлеры; `Transport::execute` сверяет реально
        // пришедшие имена с объявленными (см. `convert::TrailerWatch`,
        // `convert::declared_trailer_names`, `convert::undeclared_trailers`)
        // и ловит нарушение типизированной ошибкой, а не тихо теряет
        // данные. **Эта ошибка приходит постфактум**: к моменту, когда она
        // видна вызывающей стороне, запрос уже дошёл до сервера и получил
        // ответ (гвард срабатывает уже после успешного `race_send_with_body`)
        // — не повод бездумно повторять неидемпотентный запрос.
        caps.request_trailers = true;
        caps.response_trailers = true;
        caps.timeouts = TimeoutSupport {
            connect: true,
            first_byte: true,
            between_bytes: true,
        };
        // И беднее по всему остальному: в спеке нет ни TLS, ни прокси, ни
        // выбора версии, ни upgrade.
        //
        // Редиректы — `Transparent`, а не `None` (M2 финального ревью
        // ветки). Измерено на живом хосте (резолюция review Task 16,
        // находка B-9): 3xx доходит до гостя как обычный ответ, вести
        // цепочку обязан он сам — то есть стадия редиректа `Client`'а здесь
        // работает полностью. `None` этого сказать не мог: то же значение
        // отдаёт `Capabilities::none()`, так что вызывающая сторона не
        // отличала «бэкенд прозрачен» от «поле не заполняли» и, решив по
        // `redirects == None`, что редиректы тут невозможны, была бы
        // неправа насчёт единственного существующего бэкенда.
        caps.redirects = RedirectSupport::Transparent;
        caps.tls_config = TlsSupport::None;
        caps.upgrade = UpgradeSupport::None;
        caps.forbidden_request_headers = FORBIDDEN_REQUEST_HEADERS.as_slice();
        Self { caps }
    }
}

impl Default for WasiHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for WasiHttp {
    type Body = Body;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Body>, Error> {
        let (parts, body) = req.into_parts();
        let scheme = convert::scheme_of(&parts.uri)?;

        // Резолюция review, находка B-1 (уточнена находкой 2 фикс-раунда
        // 2): захватываем ДО того, как заголовки уйдут в `Fields` — нужно и
        // после, чтобы сверить с именами полей, которые реально испустит
        // `RequestBody::Streaming`, а не просто с фактом присутствия
        // заголовка.
        let declared_trailer_names = convert::declared_trailer_names(&parts.headers);

        let header_list: Vec<(String, Vec<u8>)> = parts
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect();
        let fields = Fields::from_list(&header_list).map_err(convert::fields_error)?;

        let timeouts = parts
            .extensions
            .get::<Timeouts>()
            .copied()
            .unwrap_or_default();
        let opts = RequestOptions::new();
        // Резолюция review, находка B-10: `as u64` на `u128` из
        // `Duration::as_nanos()` усекает молча для длительностей за
        // ~584 лет. `u64::MAX` наносекунд — уже ~584 года, так что
        // усечение здесь физически недостижимо для любого разумного
        // таймаута, но `try_from` + `unwrap_or(u64::MAX)` называет это
        // явно вместо тихого оборачивания через `as`.
        let nanos = |d: core::time::Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        convert::apply_timeouts(
            &opts,
            timeouts.connect.map(nanos),
            timeouts.first_byte.map(nanos),
            timeouts.between_bytes.map(nanos),
        )?;

        // `writer` и `payload` заводятся вместе, в одном `Option`: держать
        // их как два независимых `Option`, которые "должны" совпадать по
        // построению, было бы ровно тем классом инварианта, который потом
        // приходится закрывать недостижимой веткой `match`. Здесь эта пара
        // не может разойтись — писать нечего ровно тогда, когда писать
        // некому.
        let payload = convert::resolve_payload(body)?;
        let (writer_and_payload, contents, trailers) = match payload {
            None => {
                let (_, trailers) =
                    wasip3::wit_future::new::<Result<Option<Fields>, ErrorCode>>(|| Ok(None));
                (None, None, trailers)
            }
            Some(p) => {
                let (w, reader, trailers) = BodyWriter::new();
                (Some((w, p)), Some(reader), trailers)
            }
        };

        // Резолюция review, находка B-3, пересмотрена экспериментально при
        // подготовке фикс-раунда 1, формулировка уточнена в фикс-раунде 2
        // (находка 5). Второй возврат `Request::new` —
        // `FutureReader<Result<(), ErrorCode>>`, задокументированный
        // upstream как "resolves to result of transmission of this
        // request". План ревью — включить его третьим плечом в
        // `race_send_with_body` — был реализован и ОТКАЧЕН: измерено на
        // живом хосте, что эта футура НЕ ГАРАНТИРОВАННО резолвится до того,
        // как тело ответа вычитано целиком (маленький `Content-Length`-ответ
        // резолвит её без единого вызова `poll_frame` — отдельно
        // перепроверено), но для `chunked`, ответов с трейлерами и вообще
        // сколько-нибудь заметных тел измеримо не резолвится, пока `Body` не
        // вычитан вручную. `execute()` возвращает `Body` вызывающей стороне
        // ДО того, как та решит его читать — штатно позже, частично, или
        // никогда. Ждать эту футуру здесь безусловно означало бы: либо
        // `execute()` не возвращается для типичного ответа с телом, пока
        // сама не вычитает его целиком (разрушая стриминг, ради которого
        // существует `Body`), либо виснет для штатного сценария "тело
        // читают после получения `Response`". Ни один вариант не совместим
        // с ЭТОЙ формой seam — но пронести футуру в `Body` и дождаться её
        // на конце потока, выдав отказ передачи терминальной ошибкой тела,
        // совместим и остаётся кандидатом на v0.2 (подробности и полное
        // доказательство — doc-комментарий `convert::resolve_send`).
        // Отбрасывается здесь явно, с этим комментарием, а не через
        // `let (.., _) = ..`.
        let (wasi_request, transmitted) = Request::new(fields, contents, trailers, Some(opts));
        drop(transmitted);
        wasi_request
            .set_method(&convert::to_wasi_method(&parts.method))
            .map_err(|_| convert::rejected("method"))?;
        wasi_request
            .set_scheme(Some(&scheme))
            .map_err(|_| convert::rejected("scheme"))?;
        if let Some(a) = parts.uri.authority() {
            wasi_request
                .set_authority(Some(a.as_str()))
                .map_err(|_| convert::rejected("authority"))?;
        }
        wasi_request
            .set_path_with_query(parts.uri.path_and_query().map(|p| p.as_str()))
            .map_err(|_| convert::rejected("path_with_query"))?;

        // `send` и запись тела не отбрасываются (см. doc-комментарии
        // `convert::resolve_send`/`race_send_with_body`): короткое
        // замыкание на раннем отказе `send` (находка B-5) — единственное
        // исключение из "дождаться обоих", и оно не меняет то, что политика
        // УЖЕ отбрасывала бы в этом случае.
        let wasi_response = match writer_and_payload {
            None => wasip3::http::client::send(wasi_request)
                .await
                .map_err(convert::wasi_err)?,
            Some((w, Payload::Bytes(bytes))) => {
                let mut b = Body::from_bytes(bytes);
                convert::race_send_with_body(
                    wasip3::http::client::send(wasi_request),
                    w.send_http_body(&mut b),
                )
                .await?
            }
            Some((w, Payload::Streaming(s))) => {
                let (mut watched, trailer_names_seen) = TrailerWatch::new(s);
                let resp = convert::race_send_with_body(
                    wasip3::http::client::send(wasi_request),
                    w.send_http_body(&mut watched),
                )
                .await?;
                // Резолюция review, находка B-1 (уточнена находкой 2
                // фикс-раунда 2): сравниваем ИМЕНА реально пришедших
                // трейлер-полей с объявленными, а не факт присутствия
                // `Trailer:` — заголовок, называющий не то поле, теряет
                // данные точно так же, как его отсутствие (измерено). Не
                // выдаём успех, который это скрыл бы (см.
                // `convert::undeclared_trailers`).
                let undeclared: Vec<http::HeaderName> = trailer_names_seen
                    .lock()
                    .expect("single-threaded guest, never poisoned")
                    .iter()
                    .filter(|name| !declared_trailer_names.contains(*name))
                    .cloned()
                    .collect();
                if !undeclared.is_empty() {
                    return Err(convert::undeclared_trailers(undeclared));
                }
                resp
            }
        };

        let (resp_parts, incoming) = http_from_wasi_response(wasi_response)
            .map_err(convert::wasi_err)?
            .into_parts();
        Ok(http::Response::from_parts(
            resp_parts,
            Body::from_incoming(incoming),
        ))
    }

    /// Тождество, а не дефолтное обёртывание: `Self::Error` — уже
    /// `http_ng_core::Error`, и `convert::wasi_err` только что разложила по
    /// ней 39 вариантов `ErrorCode`.
    ///
    /// Без переопределения `Client::execute` завернул бы её ещё раз, с
    /// `ErrorKind::Other`, и вся эта классификация пропадала бы у
    /// вызывающей стороны: `is_timeout()`/`is_connect()`/`is_unsupported()`
    /// возвращали бы `false` для DNS-сбоя, TLS-сбоя, connect-таймаута и
    /// отказа хоста одинаково, а `Display` печатал бы категорию дважды —
    /// `Other: Unsupported: wasi:http host rejected setting 'scheme'`. Это
    /// и была находка B2 финального ревью ветки, ровно здесь и лечится.
    fn into_error(&self, e: Self::Error) -> Error {
        e
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}
