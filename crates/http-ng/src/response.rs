use bytes::{Bytes, BytesMut};
use http_body::Body as HttpBody;
use http_ng_core::{Error, ErrorKind};
use std::pin::Pin;

/// Ответ с сохранённым URL. `into_parts` отдаёт полную верность;
/// `chunk`/`collect` — удобство поверх неё.
#[derive(Debug)]
pub struct Response<B> {
    parts: http::response::Parts,
    body: B,
    url: http::Uri,
    /// Взводится, когда `chunk()` отдал `Some(Err(_))`, и после этого
    /// `chunk()` возвращает `None`, не трогая `body` вовсе.
    ///
    /// m6 финального ревью ветки: без него `chunk()` после ошибки заново
    /// опрашивал нижележащее тело, и вызывающая сторона, работающая с
    /// `Response::chunk` напрямую, могла крутиться в цикле по телу,
    /// отдающему ошибку на каждый опрос. `SseStream` компенсировал это
    /// своим флагом `done` и тестировал его подробно — но только для себя.
    /// Терминальность здесь ровно та же: ошибка отдаётся один раз, дальше
    /// конец потока.
    sealed: bool,
}

impl<B> Response<B> {
    pub(crate) fn new(resp: http::Response<B>, url: http::Uri) -> Self {
        let (parts, body) = resp.into_parts();
        Self {
            parts,
            body,
            url,
            sealed: false,
        }
    }
    pub fn status(&self) -> http::StatusCode {
        self.parts.status
    }
    pub fn headers(&self) -> &http::HeaderMap {
        &self.parts.headers
    }
    pub fn version(&self) -> http::Version {
        self.parts.version
    }
    pub fn url(&self) -> &http::Uri {
        &self.url
    }
    pub fn into_parts(self) -> (http::response::Parts, B) {
        (self.parts, self.body)
    }
}

impl<B> Response<B>
where
    B: HttpBody<Data = Bytes> + Unpin,
    // `B::Error: Send + Sync` — вторая точка исключения из инварианта «ядро не
    // объявляет Send/Sync», sestra бонда `Client::execute` в `client.rs`
    // (spec amendment C1). Без него `Error::new(ErrorKind::Body, e)` ниже не
    // собрался бы: `Error` хранит источник как `Arc<dyn Error + Send + Sync>`,
    // и стирание типа не пропускает auto-traits неограниченного
    // объекта-трейта. Тот же бонд, что и `Client::execute`, только на этот раз
    // требуется у `T::Body::Error`, а не `T::Error` — тело читается уже после
    // того, как транспорт его вернул.
    B::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    /// Следующий чанк данных. Трейлер-фреймы пропускаются — за ними идти в
    /// `into_parts` и поллить тело напрямую.
    ///
    /// Ошибка терминальна: после `Some(Err(_))` тело запечатано и все
    /// последующие вызовы отдают `None`, не опрашивая его заново (m6
    /// финального ревью ветки — см. поле `sealed`).
    pub async fn chunk(&mut self) -> Option<Result<Bytes, Error>> {
        if self.sealed {
            return None;
        }
        loop {
            let frame = std::future::poll_fn(|cx| Pin::new(&mut self.body).poll_frame(cx)).await;
            match frame {
                Some(Ok(f)) => match f.into_data() {
                    Ok(d) => return Some(Ok(d)),
                    Err(_) => continue, // трейлеры
                },
                Some(Err(e)) => {
                    self.sealed = true;
                    return Some(Err(classify_body_error(e)));
                }
                None => {
                    self.sealed = true;
                    return None;
                }
            }
        }
    }

    pub async fn collect(mut self) -> Result<Collected, Error> {
        let mut acc = BytesMut::new();
        while let Some(c) = self.chunk().await {
            acc.extend_from_slice(&c?);
        }
        Ok(Collected {
            parts: self.parts,
            url: self.url,
            body: acc.freeze(),
        })
    }
}

/// Classifies a response body's read error — the response-half twin of
/// `Transport::to_error`'s default (`http-ng-core/src/unversioned/
/// transport.rs`), and for the same reason: if `e` is already our own
/// `Error`, its `kind()` was set at the point the backend actually
/// classified the failure (`ErrorKind::Cancelled` from a shutting-down
/// runtime, `ErrorKind::Tls` from a mid-stream handshake failure — whatever
/// it genuinely was), and re-wrapping it here would be exactly finding B2 of
/// vertical 1's final review, reproduced one seam later: `kind()` becomes
/// `Body` for everything, every `is_*` predicate lies, and `Display` prints
/// the category twice. Only a body whose error type carries no category of
/// its own — the common case for backends whose bodies are plain
/// `std::io::Error` or similar — falls back to `ErrorKind::Body`, which
/// remains the right default for a genuinely opaque body failure.
///
/// Found by vertical 2's final review (finding F2): `chunk()` used to wrap
/// unconditionally, and nothing in the test suite noticed, because
/// `NativeBody::poll_frame`'s own fallback already defaults to
/// `ErrorKind::Body` — the double-wrap was invisible by coincidence, not
/// because it was harmless. `Body`'s own `chunk_is_terminal_after_an_error_
/// and_does_not_poll_the_body_again` (in `tests/response.rs`) now pins the
/// non-coincidental case directly.
fn classify_body_error<E>(e: E) -> Error
where
    E: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    let boxed: Box<dyn std::any::Any> = Box::new(e);
    match boxed.downcast::<Error>() {
        Ok(already_ours) => *already_ours,
        Err(foreign) => Error::new(
            ErrorKind::Body,
            *foreign.downcast::<E>().unwrap_or_else(|_| {
                // Unreachable, and not an invariant spanning two distant
                // places: established three lines above in the same
                // expression — boxed exactly `E`, the first downcast
                // missed, so the second is guaranteed to hit.
                unreachable!("boxed exactly E three lines above")
            }),
        ),
    }
}

/// Прочитанное тело **вместе** со статусом, заголовками и URL.
///
/// У reqwest `Response::{text,json,bytes}` берут `self` по значению, из-за чего
/// после чтения тела статус недоступен (issue #1542).
#[derive(Debug, Clone)]
pub struct Collected {
    parts: http::response::Parts,
    url: http::Uri,
    body: Bytes,
}

impl Collected {
    pub fn status(&self) -> http::StatusCode {
        self.parts.status
    }
    pub fn headers(&self) -> &http::HeaderMap {
        &self.parts.headers
    }
    pub fn url(&self) -> &http::Uri {
        &self.url
    }
    pub fn bytes(&self) -> &Bytes {
        &self.body
    }
    pub fn text(&self) -> Result<String, Error> {
        String::from_utf8(self.body.to_vec()).map_err(|e| Error::new(ErrorKind::Decode, e))
    }
    /// Десериализует тело как JSON. Часть интерфейса, объявленного для этой
    /// задачи (`Collected::json<T>()`), но отсутствовавшая в шаге 3 брифа —
    /// см. отчёт о задаче.
    ///
    /// За фичей `json`, выключенной по умолчанию: `serde`/`serde_json` не
    /// нужны потребителю, который тело только стримит или читает как байты —
    /// см. комментарий у фичи в Cargo.toml про цену на wasm.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, Error> {
        serde_json::from_slice(&self.body).map_err(|e| Error::new(ErrorKind::Decode, e))
    }
}
