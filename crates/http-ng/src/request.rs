use crate::client::Client;
use crate::response::Response;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, ErrorKind, RequestBody};

#[derive(Debug)]
pub struct RequestBuilder<'a, T> {
    client: &'a Client<T>,
    method: http::Method,
    uri: Result<http::Uri, http::uri::InvalidUri>,
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
            uri: url.parse(),
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

    pub fn headers(mut self, headers: http::HeaderMap) -> Self {
        self.headers = headers;
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
        let uri = self.uri.map_err(|e| Error::new(ErrorKind::Other, e))?;
        let mut req = http::Request::new(self.body);
        *req.method_mut() = self.method;
        *req.uri_mut() = uri.clone();
        *req.headers_mut() = self.headers;
        *req.extensions_mut() = self.extensions;
        let resp = self.client.execute(req).await?;
        Ok(Response::new(resp, uri))
    }
}
