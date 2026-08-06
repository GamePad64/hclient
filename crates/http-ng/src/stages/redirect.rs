//! Применение решения, принятого в `http-ng-proto`. Здесь только перекладывание
//! данных: вся логика — чистая функция `proto::redirect::decide`.

use http_ng_core::RequestBody;
use http_ng_proto::redirect::{Follow, SENSITIVE_HEADERS};

/// Всё, что переносится между хопами, кроме тела.
///
/// Отдельный тип, потому что `http::request::Parts` **не реализует `Clone`**,
/// а между хопами метод, URI и заголовки нужны и до, и после отправки.
/// `HeaderMap`, `Uri`, `Method` и `Extensions` клонируемы — проверено.
#[derive(Debug, Clone)]
pub(crate) struct HopParts {
    pub(crate) method: http::Method,
    pub(crate) uri: http::Uri,
    pub(crate) headers: http::HeaderMap,
    pub(crate) version: http::Version,
    pub(crate) extensions: http::Extensions,
}

impl HopParts {
    pub(crate) fn to_request(&self, body: RequestBody) -> http::Request<RequestBody> {
        let mut req = http::Request::new(body);
        *req.method_mut() = self.method.clone();
        *req.uri_mut() = self.uri.clone();
        *req.headers_mut() = self.headers.clone();
        *req.version_mut() = self.version;
        *req.extensions_mut() = self.extensions.clone();
        req
    }
}

/// Построить следующий хоп. `replay` — снимок тела, снятый **до** отправки
/// предыдущей попытки; `None` означает, что тело невоспроизводимо.
///
/// Возвращает `None`, когда тело переиграть нельзя, а метод не понижен: тогда
/// честнее вернуть 3xx как есть, чем отправить пустое тело туда, где его ждут.
///
/// **`extensions` переносятся на следующий хоп безусловно, включая
/// кросс-ориджин** — в отличие от `headers`, откуда `strip_sensitive`
/// вычищает учётные данные. Асимметрия сегодня без последствий: единственный
/// тип в `extensions` — `Timeouts`, который через границу источника нести
/// безопасно и нужно (иначе после редиректа таймауты бы исчезли, а с B1 они
/// туда именно и кладутся). Записано как известный долг, а не незамеченный:
/// §4.9 дизайна кладёт в per-request config авторизацию и политику, и в тот
/// момент `extensions` понадобится тот же фильтр по чувствительности, что
/// уже есть у заголовков (m7 финального ревью ветки).
pub(crate) fn next_hop(
    prev: &HopParts,
    replay: Option<RequestBody>,
    follow: &Follow,
) -> Option<(HopParts, RequestBody)> {
    let mut headers = prev.headers.clone();
    if follow.strip_sensitive {
        for h in SENSITIVE_HEADERS {
            headers.remove(&h);
        }
    }
    let body = if follow.drop_body {
        headers.remove(http::header::CONTENT_LENGTH);
        headers.remove(http::header::CONTENT_TYPE);
        RequestBody::Empty
    } else {
        replay?
    };
    Some((
        HopParts {
            method: follow.method.clone(),
            uri: follow.uri.clone(),
            headers,
            version: prev.version,
            extensions: prev.extensions.clone(),
        },
        body,
    ))
}
