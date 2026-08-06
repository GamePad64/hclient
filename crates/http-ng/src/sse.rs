use crate::response::Response;
use bytes::Bytes;
use http_body::Body as HttpBody;
use http_ng_core::{Error, ErrorKind};
use http_ng_proto::sse::{SseDecoder, SseEvent};

const MIME: &str = "text/event-stream";

/// `Content-Type` совпадает с MIME SSE ровно как токен, а не как префикс:
/// `text/event-stream` без учёта регистра (HTTP media types регистронезависимы
/// — RFC 9110 §5.5), и следующий байт — конец строки, `;` (граница параметра,
/// например `; charset=utf-8`) или пробельный символ. Голый `starts_with`
/// (review round 1, Finding 2) принимал `"text/event-streamfoo"` как валидный
/// тип и отвергал `"Text/Event-Stream"` из-за регистра — оба дефекта здесь
/// закрыты одной проверкой границы токена.
fn is_event_stream_content_type(v: &str) -> bool {
    let v = v.trim_start();
    let Some(head) = v.get(..MIME.len()) else {
        return false;
    };
    if !head.eq_ignore_ascii_case(MIME) {
        return false;
    }
    match v.as_bytes().get(MIME.len()) {
        None => true,
        Some(b';') => true,
        Some(b) => b.is_ascii_whitespace(),
    }
}

/// Поток событий SSE поверх любого тела ответа.
///
/// Реконнект здесь **не** реализован: он требует повторной отправки запроса и
/// приедет со стадией retry в v0.2. `last_event_id()` уже доступен, поэтому
/// добавление реконнекта не изменит публичный API.
#[derive(Debug)]
pub struct SseStream<B> {
    resp: Response<B>,
    decoder: SseDecoder,
    /// Фатальная ошибка, придержанная до опустошения очереди декодера: уже
    /// разобранные события были получены целиком и корректно, терять их ради
    /// более раннего сообщения об ошибке — потеря корректных данных (review
    /// round 1, Finding 1). Кладётся сюда и телом-ошибкой транспорта
    /// (`Response::chunk` вернул `Some(Err(_))`), и превышением лимита
    /// декодера — оба пути обязаны дать уже готовым событиям выйти первыми.
    /// `next()` отдаёт её ровно один раз (`Option::take`), после чего `done`
    /// гарантирует бесконечный `None` — а не воскрешение или повтор ошибки.
    fatal: Option<Error>,
    /// Больше не читать у тела новые чанки. Не совпадает по смыслу с
    /// `fatal.is_some()`: чистый EOF (`chunk()` вернул `None`, без ошибки)
    /// тоже взводит `done`, но `fatal` при этом остаётся `None`.
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
            .is_some_and(is_event_stream_content_type);
        if !ok_ct {
            return Err(Error::new(
                ErrorKind::Decode,
                SseRejected("content-type is not text/event-stream"),
            ));
        }
        Ok(Self {
            resp,
            decoder: SseDecoder::new(max_event_size),
            fatal: None,
            done: false,
        })
    }

    pub fn last_event_id(&self) -> Option<&str> {
        self.decoder.last_event_id()
    }

    /// Следующее декодированное событие. Читает у тела ровно столько чанков,
    /// сколько нужно декодеру, чтобы набрать хотя бы одно готовое событие —
    /// границы чанков транспорта не обязаны совпадать с границами событий SSE.
    ///
    /// Порядок при фатальной ошибке (превышение лимита или ошибка тела)
    /// важен: события, уже полностью и корректно разобранные ДО того, как
    /// ошибка случилась, отдаются первыми — очередь декодера опустошается
    /// раньше, чем `next()` вернёт `Err`. После этого поток кончен навсегда:
    /// `Err` отдаётся ровно один раз, дальнейшие вызовы — всегда `None`.
    pub async fn next(&mut self) -> Option<Result<SseEvent, Error>> {
        loop {
            if let Some(e) = self.decoder.next() {
                return Some(Ok(e));
            }
            if let Some(e) = self.fatal.take() {
                return Some(Err(e));
            }
            if self.done {
                return None;
            }
            match self.resp.chunk().await {
                Some(Ok(chunk)) => {
                    if let Err(e) = self.decoder.push(&chunk) {
                        // Превышение лимита фатально и не ретраится — но не
                        // раньше, чем события, уже осевшие в очереди декодера
                        // за этот же `push`, дойдут до вызывающего: цикл выше
                        // их сначала сольёт через `self.decoder.next()`.
                        self.done = true;
                        self.fatal = Some(Error::new(ErrorKind::Decode, e));
                    }
                }
                Some(Err(e)) => {
                    // Тот же порядок, что и при превышении лимита: уже
                    // готовые события не приносятся в жертву более раннему
                    // сообщению об ошибке тела.
                    self.done = true;
                    self.fatal = Some(e);
                }
                None => {
                    // Конец тела без финальной пустой строки: недодиспетченный
                    // "хвост" в буфере данных декодера теряется молча — это
                    // осознанное поведение WHATWG (событие диспетчится только
                    // по пустой строке; браузерный EventSource делает то же
                    // самое при закрытии соединения без финального
                    // разделителя), а не дефект `SseStream` (review round 1,
                    // Finding 5).
                    self.done = true;
                }
            }
        }
    }
}
