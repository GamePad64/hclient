use crate::response::Response;
use bytes::Bytes;
use http_body::Body as HttpBody;
use http_ng_core::{Error, ErrorKind};
use http_ng_proto::sse::{SseDecoder, SseEvent};

const MIME: &str = "text/event-stream";

/// Поток событий SSE поверх любого тела ответа.
///
/// Реконнект здесь **не** реализован: он требует повторной отправки запроса и
/// приедет со стадией retry в v0.2. `last_event_id()` уже доступен, поэтому
/// добавление реконнекта не изменит публичный API.
#[derive(Debug)]
pub struct SseStream<B> {
    resp: Response<B>,
    decoder: SseDecoder,
    done: bool,
}

#[derive(Debug)]
struct SseRejected(&'static str);
impl std::fmt::Display for SseRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not an SSE stream: {}", self.0)
    }
}
impl std::error::Error for SseRejected {}

impl<B> SseStream<B>
where
    B: HttpBody<Data = Bytes> + Unpin,
    // `next()` зовёт `self.resp.chunk()`, а тот определён (`response.rs`)
    // только при `B::Error: Send + Sync + 'static` (spec amendment C1).
    // Rust не пробрасывает бонды через вызов: обобщённая функция обязана
    // повторить бонд своего callee в собственной where-клаузе — тот же приём,
    // что у `RequestBuilder::send` относительно `Client::execute`
    // (`request.rs`). Четвёртая независимая цепочка такого рода в крейте,
    // после `Client::execute` (client.rs), `Response::chunk`+`collect`,
    // разделяющих один бонд (response.rs), и `RequestBuilder::send`
    // (request.rs) — см. счётчик и правило в `.github/workflows/ci.yml`.
    B::Error: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    /// Строит поток из ответа. Терминальные правила WHATWG проверяются здесь,
    /// а не откладываются до первого `next()`: статус ≠ 200 — ошибка (204 в
    /// частности значит «прекрати навсегда», а не «пустой поток»);
    /// `Content-Type` ≠ `text/event-stream` — тоже ошибка, а не молчаливое
    /// приведение типа содержимого.
    pub fn new(resp: Response<B>, max_event_size: usize) -> Result<Self, Error> {
        if resp.status() != http::StatusCode::OK {
            return Err(Error::new(
                ErrorKind::Status,
                SseRejected("status is not 200"),
            ));
        }
        let ok_ct = resp
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim_start().starts_with(MIME));
        if !ok_ct {
            return Err(Error::new(
                ErrorKind::Decode,
                SseRejected("content-type is not text/event-stream"),
            ));
        }
        Ok(Self {
            resp,
            decoder: SseDecoder::new(max_event_size),
            done: false,
        })
    }

    pub fn last_event_id(&self) -> Option<&str> {
        self.decoder.last_event_id()
    }

    /// Следующее декодированное событие. Читает у тела ровно столько чанков,
    /// сколько нужно декодеру, чтобы набрать хотя бы одно готовое событие —
    /// границы чанков транспорта не обязаны совпадать с границами событий SSE.
    pub async fn next(&mut self) -> Option<Result<SseEvent, Error>> {
        loop {
            if let Some(e) = self.decoder.next() {
                return Some(Ok(e));
            }
            if self.done {
                return None;
            }
            match self.resp.chunk().await {
                Some(Ok(chunk)) => {
                    if let Err(e) = self.decoder.push(&chunk) {
                        // Превышение лимита фатально и не ретраится.
                        self.done = true;
                        return Some(Err(Error::new(ErrorKind::Decode, e)));
                    }
                }
                Some(Err(e)) => {
                    self.done = true;
                    return Some(Err(e));
                }
                None => self.done = true,
            }
        }
    }
}
