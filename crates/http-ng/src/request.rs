use crate::client::Client;
use crate::response::Response;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, ErrorKind, RequestBody};

#[derive(Debug)]
pub struct RequestBuilder<'a, T> {
    client: &'a Client<T>,
    method: http::Method,
    /// Уже разрешённый относительно `base_url` клиента (или просто
    /// разобранный, если базы нет). Разрешение живёт в `new`, а не в `send`,
    /// потому что делать его нужно на исходной СТРОКЕ: `http::Uri` не умеет
    /// представлять path-relative ссылку, а это ровно та форма, ради которой
    /// база существует (см. `config::effective_uri`).
    uri: Result<http::Uri, Error>,
    headers: http::HeaderMap,
    body: RequestBody,
    extensions: http::Extensions,
    /// Первая ошибка построения. Всплывает в `send()`: молча проглоченный
    /// невалидный заголовок — это ровно тот тихий no-op, против которого
    /// построен `ClientBuilder::build` (см. `check_supported` в config.rs).
    /// Брифовый код `header()` отбрасывал невалидную пару молча (`if let
    /// (Ok(n), Ok(v)) = .. { .. }`, без `else`) — дефект самого брифа, а не
    /// намеренное поведение: см. отчёт о задаче, Task 13 fix round 1.
    error: Option<Error>,
}

impl<'a, T: Transport> RequestBuilder<'a, T> {
    pub(crate) fn new(client: &'a Client<T>, method: http::Method, url: &str) -> Self {
        Self {
            client,
            method,
            uri: crate::config::effective_uri(client.config().base_url.as_ref(), url),
            headers: http::HeaderMap::new(),
            body: RequestBody::Empty,
            extensions: http::Extensions::new(),
            error: None,
        }
    }

    /// Первая ошибка построения побеждает и переживает дальнейшие вызовы —
    /// не перезаписывается второй невалидной парой и не теряется, если после
    /// неё вызвали ещё валидный `header()`.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if self.error.is_some() {
            return self;
        }
        match (
            name.parse::<http::HeaderName>(),
            value.parse::<http::HeaderValue>(),
        ) {
            (Ok(n), Ok(v)) => {
                self.headers.insert(n, v);
            }
            (Err(e), _) => self.error = Some(Error::new(ErrorKind::Other, e)),
            (_, Err(e)) => self.error = Some(Error::new(ErrorKind::Other, e)),
        }
        self
    }

    /// Дополняет уже выставленные заголовки, а не заменяет их.
    ///
    /// `self.headers = headers` (как было до m4 финального ревью ветки)
    /// выбрасывал всё, что успел выставить `header()`, без всякой
    /// диагностики — тот же класс дефекта, что брифовый `header()`, который
    /// Task 13 чинил и обкладывал тестом. `HeaderMap::extend` при этом
    /// перекрывает одноимённое значение, а не накапливает дубликат
    /// (проверено по контракту `http`: первое значение ключа из
    /// расширяющей карты идёт через `insert`, последующие — через
    /// `append`), так что «дополняет» не превращается в два `accept` на
    /// проводе.
    ///
    /// **Слот ошибки здесь НЕ консультируется, в отличие от `header()`, и
    /// это не забытая симметрия.** У `header()` гвард
    /// `if self.error.is_some() { return self; }` несёт нагрузку: этот метод
    /// сам умеет ставить ошибку, и без гварда вторая невалидная пара
    /// перезаписала бы первую — проверено мутацией, роняет
    /// `header_first_error_wins_name_over_later_value_error`. У `headers()`
    /// ставить ошибку нечем: `HeaderMap` уже валиден по построению. Такой же
    /// гвард здесь ничего не менял бы наблюдаемо — `send()` возвращает
    /// сохранённую ошибку раньше, чем вообще смотрит на заголовки, — и он
    /// действительно тут был один раунд, вместе с тестом, который не мог
    /// упасть: снятие гварда оставляло ВЕСЬ набор `http-ng` зелёным
    /// (перепроверено самостоятельно). Мёртвый код и тест, не способный
    /// покраснеть, — хуже, чем ничего, поэтому оба удалены.
    ///
    /// Триггер вернуть гвард: как только `headers()` научится отказывать —
    /// например, фильтру `Capabilities::forbidden_request_headers`,
    /// намеченному на v0.2, — он снова станет наблюдаемым, и возвращать его
    /// нужно ВМЕСТЕ с тестом, который без него краснеет.
    pub fn headers(mut self, headers: http::HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn body(mut self, body: RequestBody) -> Self {
        self.body = body;
        self
    }

    /// Таймауты только для этого запроса. Кладутся в `Extensions`, откуда их
    /// читает транспорт; незаданные поля падают обратно на конфигурацию
    /// клиента — слияние делает `Client::execute` через
    /// `config::effective_timeouts`, он же и проверяет **слитый** результат
    /// против `Capabilities` транспорта, так что фаза, которой бэкенд не
    /// умеет, становится `ErrorKind::Unsupported` из `send()`, а не тихо
    /// отброшенным значением.
    ///
    /// reqwest этого не умеет вовсе (issue #2641), из-за чего `act-cli`
    /// вынужден строить отдельный `reqwest::Client` на каждый вызов
    /// компонента — со своим пулом соединений.
    pub fn timeouts(mut self, t: http_ng_core::Timeouts) -> Self {
        self.extensions.insert(t);
        self
    }

    pub async fn send(self) -> Result<Response<T::Body>, Error>
    where
        // Sestra бонда `Client::execute` в `client.rs` (spec amendment C1):
        // `send` зовёт `Client::execute`, которое требует `T::Error: Send +
        // Sync + 'static` — обобщённая функция обязана повторить бонд своего
        // callee, трейт сам его не несёт.
        T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        if let Some(e) = self.error {
            return Err(e);
        }
        let uri = self.uri?;
        let mut req = http::Request::new(self.body);
        *req.method_mut() = self.method;
        *req.uri_mut() = uri.clone();
        *req.headers_mut() = self.headers;
        *req.extensions_mut() = self.extensions;
        let resp = self.client.execute(req).await?;
        Ok(Response::new(resp, uri))
    }
}
