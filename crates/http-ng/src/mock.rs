//! Мок-транспорт: позволяет тестировать клиент и стадии на хосте, без сети и
//! без wasm-рантайма. Доступен за фичей `test-util`.
//!
//! Очередь ответов и лог запросов лежат за `std::sync::Mutex`, а не
//! `RefCell`. Это не стиль: `RefCell` сделал бы `MockTransport` `!Sync`,
//! тогда `&MockTransport` был бы `!Send`, а значит и футура, которую
//! возвращает `Client::execute` (она заимствует транспорт), тоже была бы
//! `!Send` — `tokio::spawn(client.get(u).send())` не собрался бы ни в одном
//! тесте на этом моке. Свойство "футура клиента Send, когда Send транспорт"
//! — центральное для дизайна крейта, и тест-двойник не должен быть тем, что
//! мешает его проверить.

use bytes::Bytes;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, RetryKind};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

/// Один зафиксированный запрос: всё, что мок увидел, прежде чем тело
/// отбросили.
///
/// `extensions` хранится целиком, а не распакован до конкретных известных
/// сегодня типов: `Timeouts` (Task 10) уже путешествует через
/// `http::Extensions`, и это не последний тип, который начнёт так делать —
/// узкое поле пришлось бы расширять заново на каждый новый случай.
/// `retry_kind` и `body_size_hint` — это то немногое, что о теле можно
/// сказать, не читая его: сами байты мок не собирает, иначе `Streaming`-тела
/// пришлось бы дочитывать до конца, а тестам это не нужно.
///
/// Без `PartialEq`/`Eq`: `http::Extensions` их не реализует (это карта
/// `TypeId -> Box<dyn Any>`, сравнивать которую в общем случае нечем), так
/// что сравнивать `RecordedRequest` целиком нельзя — только по полям.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: http::Method,
    pub uri: http::Uri,
    pub headers: http::HeaderMap,
    pub extensions: http::Extensions,
    pub retry_kind: RetryKind,
    pub body_size_hint: Option<u64>,
}

#[derive(Debug)]
pub struct MockTransport {
    queue: Mutex<VecDeque<http::Response<VecDeque<Bytes>>>>,
    seen: Mutex<Vec<RecordedRequest>>,
    caps: Capabilities,
}

/// Возвращается вместо ответа, когда очередь мока пуста.
///
/// `pub`, а не приватный тип: тест обязан уметь отличить именно это от любой
/// другой ошибки с `ErrorKind::Other` через
/// `Error::source().downcast_ref::<QueueEmpty>()`, а не полагаться на то,
/// что сегодня это единственный путь мока к ошибке.
#[derive(Debug)]
pub struct QueueEmpty;
impl std::fmt::Display for QueueEmpty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MockTransport: response queue is empty")
    }
}
impl std::error::Error for QueueEmpty {}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
            caps: Capabilities::none(),
        }
    }

    pub fn with_capabilities(mut self, caps: Capabilities) -> Self {
        self.caps = caps;
        self
    }

    /// Ставит в очередь ответ из одного кадра — частый случай, когда границы
    /// чанков тесту не важны.
    pub fn push_response(&self, resp: http::Response<&'static str>) {
        let (parts, body) = resp.into_parts();
        let mut frames = VecDeque::new();
        frames.push_back(Bytes::from_static(body.as_bytes()));
        self.queue
            .lock()
            .expect("mock lock poisoned")
            .push_back(http::Response::from_parts(parts, frames));
    }

    /// Ставит в очередь ответ из нескольких кадров — например, чтобы
    /// воспроизвести разрыв SSE-потока на границе чанка (Task 14). Кадры
    /// отдаются `poll_frame`'ом по одному, в переданном порядке.
    pub fn push_response_frames(&self, resp: http::Response<Vec<&'static str>>) {
        let (parts, body) = resp.into_parts();
        let frames: VecDeque<Bytes> = body
            .into_iter()
            .map(|s| Bytes::from_static(s.as_bytes()))
            .collect();
        self.queue
            .lock()
            .expect("mock lock poisoned")
            .push_back(http::Response::from_parts(parts, frames));
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.seen.lock().expect("mock lock poisoned").clone()
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for MockTransport {
    type Body = MockBody;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        let (parts, body) = req.into_parts();
        // Читаем то немногое, что о теле известно без его чтения, прежде
        // чем оно упадёт вместе с остальными `parts`.
        let retry_kind = body.retry_kind();
        let body_size_hint = body.size_hint();
        self.seen
            .lock()
            .expect("mock lock poisoned")
            .push(RecordedRequest {
                method: parts.method,
                uri: parts.uri,
                headers: parts.headers,
                extensions: parts.extensions,
                retry_kind,
                body_size_hint,
            });
        match self.queue.lock().expect("mock lock poisoned").pop_front() {
            Some(r) => {
                let (p, frames) = r.into_parts();
                Ok(http::Response::from_parts(p, MockBody { frames }))
            }
            None => Err(Error::new(ErrorKind::Other, QueueEmpty)),
        }
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// Тело ответа мока: последовательность кадров, чтобы через мок можно было
/// воспроизвести разрыв на границе чанка. С однокадровым телом SSE-стрим в
/// Task 14 получал бы весь поток одним куском, и путь склейки на уровне
/// стрима остался бы непроверенным.
#[derive(Debug)]
pub struct MockBody {
    frames: VecDeque<Bytes>,
}

impl http_body::Body for MockBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        Poll::Ready(
            self.frames
                .pop_front()
                .map(|b| Ok(http_body::Frame::data(b))),
        )
    }

    fn is_end_stream(&self) -> bool {
        self.frames.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_core::unversioned::Transport;

    #[test]
    fn records_requests_and_replays_queued_responses() {
        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(204).body("").unwrap());

        let fut = m.execute(
            http::Request::builder()
                .method("POST")
                .uri("https://a/x")
                .body(RequestBody::Empty)
                .unwrap(),
        );
        let resp = futures_executor::block_on(fut).unwrap();

        assert_eq!(resp.status(), 204);
        let rec = m.requests();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].method, http::Method::POST);
        assert_eq!(rec[0].uri, "https://a/x".parse::<http::Uri>().unwrap());
    }

    /// Продолжение `records_requests_and_replays_queued_responses`: тот тест
    /// покрывает метод и URI, но не заголовки. Здесь запрос отличается от
    /// значений по умолчанию по всем трём полям сразу — метод не GET, URI не
    /// пустой, и добавлен нестандартный заголовок — чтобы `requests()`
    /// нельзя было случайно "починить" копированием только части `parts`.
    #[test]
    fn records_headers_that_differ_from_defaults_too() {
        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(200).body("").unwrap());

        let fut = m.execute(
            http::Request::builder()
                .method("PATCH")
                .uri("https://a/y")
                .header("x-mock-test", "custom-value")
                .body(RequestBody::Empty)
                .unwrap(),
        );
        futures_executor::block_on(fut).unwrap();

        let rec = m.requests();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].method, http::Method::PATCH);
        assert_eq!(rec[0].uri, "https://a/y".parse::<http::Uri>().unwrap());
        assert_eq!(
            rec[0]
                .headers
                .get("x-mock-test")
                .map(|v| v.to_str().unwrap()),
            Some("custom-value"),
        );
    }

    #[test]
    fn errors_when_the_queue_is_empty() {
        let m = MockTransport::new();
        let fut = m.execute(http::Request::new(RequestBody::Empty));
        let err = futures_executor::block_on(fut).unwrap_err();
        assert_eq!(err.kind(), &ErrorKind::Other);
        // Не любая ErrorKind::Other сойдёт: тест обязан отличить именно
        // исчерпание очереди мока от гипотетической другой Other-ошибки.
        let src = std::error::Error::source(&err).expect("Error::new всегда кладёт source");
        assert!(
            src.downcast_ref::<QueueEmpty>().is_some(),
            "источник ошибки должен даункаститься именно в QueueEmpty"
        );
    }

    #[test]
    fn with_capabilities_overrides_the_default_none() {
        let mut caps = Capabilities::none();
        caps.streaming_request_body = true;
        let m = MockTransport::new().with_capabilities(caps);
        assert!(m.capabilities().streaming_request_body);
    }

    /// `Empty` и `Full` — единственные варианты `RequestBody`, чей размер
    /// известен заранее (см. `body::size_hint_is_known_for_empty_and_full_bodies`
    /// в `http-ng-core`). `Some(0)` для `Empty` — это не "размер неизвестен",
    /// а "тела не было"; отличать одно от другого и есть смысл записи этого
    /// поля для Task 12 (редирект не должен посылать тело там, где его не
    /// было).
    #[test]
    fn body_size_hint_distinguishes_empty_from_populated() {
        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(200).body("").unwrap());
        m.push_response(http::Response::builder().status(200).body("").unwrap());

        futures_executor::block_on(m.execute(http::Request::new(RequestBody::Empty))).unwrap();
        futures_executor::block_on(m.execute(http::Request::new(RequestBody::Full(
            Bytes::from_static(b"payload"),
        ))))
        .unwrap();

        let rec = m.requests();
        assert_eq!(rec.len(), 2);
        assert_eq!(
            rec[0].body_size_hint,
            Some(0),
            "Empty body carries no bytes"
        );
        assert_eq!(rec[0].retry_kind, RetryKind::Free);
        assert_eq!(
            rec[1].body_size_hint,
            Some(7),
            "Full body's real size must survive into the recording"
        );
        assert_eq!(rec[1].retry_kind, RetryKind::Free);
    }

    /// `Timeouts` (Task 10) едет к транспорту через `http::Extensions` — это
    /// весь механизм, ради которого он там лежит. Без записи `extensions`
    /// целиком ни один тест не смог бы убедиться, что клиент действительно
    /// прикладывает per-request таймауты и что они доживают до транспорта.
    #[test]
    fn extensions_round_trip_through_the_recording_so_timeouts_survive() {
        use http_ng_core::Timeouts;
        use std::time::Duration;

        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(200).body("").unwrap());

        let mut req = http::Request::new(RequestBody::Empty);
        req.extensions_mut().insert(Timeouts {
            connect: Some(Duration::from_secs(3)),
            ..Default::default()
        });

        futures_executor::block_on(m.execute(req)).unwrap();

        let rec = m.requests();
        let recorded = rec[0]
            .extensions
            .get::<Timeouts>()
            .expect("Timeouts вставленные в запрос обязаны дожить до записи");
        assert_eq!(recorded.connect, Some(Duration::from_secs(3)));
    }

    /// Проверяет, что `poll_frame` отдаёт кадры по одному, а не всё тело
    /// целиком за первый вызов. Без этого свойства `SseStream` (Task 14),
    /// построенный поверх `Response::chunk()`, никогда не увидел бы через
    /// мок разрыв события на границе чанка — только конкатенированный поток.
    #[test]
    fn multi_frame_response_yields_frames_separately_not_concatenated() {
        use http_body::Body as _;

        let m = MockTransport::new();
        m.push_response_frames(
            http::Response::builder()
                .status(200)
                .body(vec!["first-chunk", "second-chunk"])
                .unwrap(),
        );

        let resp =
            futures_executor::block_on(m.execute(http::Request::new(RequestBody::Empty))).unwrap();

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut pinned = std::pin::pin!(resp.into_body());

        let first = match pinned.as_mut().poll_frame(&mut cx) {
            Poll::Ready(Some(Ok(f))) => f,
            other => panic!("expected the first frame ready, got {other:?}"),
        };
        assert_eq!(
            first.into_data().unwrap(),
            Bytes::from_static(b"first-chunk"),
            "first poll must yield only the first chunk, not the whole payload"
        );

        let second = match pinned.as_mut().poll_frame(&mut cx) {
            Poll::Ready(Some(Ok(f))) => f,
            other => panic!("expected the second frame ready, got {other:?}"),
        };
        assert_eq!(
            second.into_data().unwrap(),
            Bytes::from_static(b"second-chunk")
        );

        match pinned.as_mut().poll_frame(&mut cx) {
            Poll::Ready(None) => {}
            other => panic!("expected end of stream after both frames, got {other:?}"),
        }
    }

    /// Ни один существующий тест до этого раунда не ставил в очередь больше
    /// одного ответа — порядок мог совпадать с ожидаемым случайно (если бы
    /// `push_back`/`pop_front` где-то перепутали со стеком). Здесь три
    /// различимых по статусу ответа должны вернуться строго в порядке
    /// постановки.
    #[test]
    fn responses_replay_in_fifo_order() {
        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(200).body("").unwrap());
        m.push_response(http::Response::builder().status(201).body("").unwrap());
        m.push_response(http::Response::builder().status(202).body("").unwrap());

        for expected in [200u16, 201, 202] {
            let resp =
                futures_executor::block_on(m.execute(http::Request::new(RequestBody::Empty)))
                    .unwrap();
            assert_eq!(resp.status().as_u16(), expected);
        }
    }
}
