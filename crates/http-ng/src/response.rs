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
}

impl<B> Response<B> {
    pub(crate) fn new(resp: http::Response<B>, url: http::Uri) -> Self {
        let (parts, body) = resp.into_parts();
        Self { parts, body, url }
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
    pub async fn chunk(&mut self) -> Option<Result<Bytes, Error>> {
        loop {
            let frame = std::future::poll_fn(|cx| Pin::new(&mut self.body).poll_frame(cx)).await;
            match frame {
                Some(Ok(f)) => match f.into_data() {
                    Ok(d) => return Some(Ok(d)),
                    Err(_) => continue, // трейлеры
                },
                Some(Err(e)) => return Some(Err(Error::new(ErrorKind::Body, e))),
                None => return None,
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
