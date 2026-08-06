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
use std::sync::atomic::Ordering;
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
        // `full_duplex = false` — резолюция review, находка B-2. Сам
        // протокол `wasi:http` 0.3 поддерживает body-дуплекс (данные тела
        // могут идти, пока ответ ещё не получен), но ЭТА форма
        // `Transport::execute` — нет: она возвращается только после того,
        // как `race_send_with_body` дождётся и `send`, и записи тела (кроме
        // одного случая — раннего отказа `send`, см. B-5). Измерено на
        // живом хосте: ответ существовал на стороне сервера в момент
        // t≈0.10s, но вызывающая сторона видела его только в t≈2.00s, к
        // концу записи тела; для тела без конца вызывающая сторона не
        // увидела бы ответ вообще. Это ограничение ЭТОЙ реализации
        // `execute`, а не хоста — исправление меняет форму сигнатуры seam
        // (`Transport::execute` должен был бы возвращать поток кадров
        // ответа отдельно от завершения записи тела), а это затрагивает
        // каждый бэкенд и принадлежит вертикали 2, не фикс-раунду здесь.
        caps.full_duplex = false;
        // `request_trailers`/`response_trailers`: трейлеры РАБОТАЮТ на
        // `wasi:http`, но только если запрос заранее объявил их имена
        // через заголовок `Trailer:` — измерено (резолюция review, находка
        // B-1), что HTTP/1.1-кодировщик хоста молча роняет необъявленные
        // трейлеры на проводе (RFC 9110 §6.5.1). Имена трейлеров известны
        // только после того, как тело закончилось — раньше заголовков их
        // предсказать нельзя, инъецировать `Trailer:` за вызывающую сторону
        // здесь некому. Вызывающая сторона обязана сама выставить
        // `Trailer:` с именами полей, которые её `RequestBody::Streaming`
        // будет эмитить как трейлеры; `Transport::execute` ловит нарушение
        // этого контракта типизированной ошибкой в момент, когда кадр
        // трейлеров реально пришёл (см. `convert::TrailerWatch`,
        // `convert::undeclared_trailers`), а не тихо теряет данные.
        caps.request_trailers = true;
        caps.response_trailers = true;
        caps.timeouts = TimeoutSupport {
            connect: true,
            first_byte: true,
            between_bytes: true,
        };
        // И беднее по всему остальному: в спеке нет ни редиректов, ни TLS,
        // ни прокси, ни выбора версии, ни upgrade.
        caps.redirects = RedirectSupport::None;
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

        // Резолюция review, находка B-1: захватываем ДО того, как заголовки
        // уйдут в `Fields` — нужно и после, чтобы решить, доверять ли
        // трейлерам, которые может испустить `RequestBody::Streaming`.
        let declares_trailer_names = parts.headers.contains_key(http::header::TRAILER);

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

        // Резолюция review, находка B-3, ПЕРЕСМОТРЕНО экспериментально при
        // подготовке этого фикс-раунда. Второй возврат `Request::new` —
        // `FutureReader<Result<(), ErrorCode>>`, задокументированный
        // upstream как "resolves to result of transmission of this
        // request". План ревью — включить его третьим плечом в
        // `race_send_with_body` — был реализован и ОТКАЧЕН: измерено на
        // живом хосте дважды (полным путём этого крейта и голыми вызовами
        // `wasip3` в обход крейта целиком, чтобы исключить баг в
        // собственной логике гонки), что эта футура не резолвится для
        // ответа с телом, пока тело ответа не будет вычитано ЦЕЛИКОМ —
        // `send()` возвращал `Ok` немедленно, а `transmitted` оставался
        // `Pending`, пока весь `Body` из ответа не был вычитан вручную.
        // `execute()` возвращает `Body` вызывающей стороне ДО того, как та
        // решит его читать — штатно позже, частично, или никогда. Ждать
        // здесь означало бы: либо `execute()` не возвращается ни для
        // одного ответа с непустым телом, пока сама не вычитает его целиком
        // (разрушая стриминг, ради которого существует `Body`), либо виснет
        // навсегда для штатного сценария "тело читают после получения
        // `Response`". Ни один вариант не совместим с формой seam —
        // подробности и доказательство см. doc-комментарий
        // `convert::resolve_send`. Отбрасывается здесь явно, с этим
        // комментарием, а не через `let (.., _) = ..`.
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
                let (mut watched, trailer_seen) = TrailerWatch::new(s);
                let resp = convert::race_send_with_body(
                    wasip3::http::client::send(wasi_request),
                    w.send_http_body(&mut watched),
                )
                .await?;
                // Резолюция review, находка B-1: тело реально испустило
                // трейлеры, но запрос не объявил их именами через
                // `Trailer:` — на проводе они уже потеряны (см.
                // `convert::undeclared_trailers`), не выдаём успех, который
                // это скрыл бы.
                if trailer_seen.load(Ordering::Relaxed) && !declares_trailer_names {
                    return Err(convert::undeclared_trailers());
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

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}
