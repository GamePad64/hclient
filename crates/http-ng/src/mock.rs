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
use http_body_util::Full;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody};
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    pub method: http::Method,
    pub uri: http::Uri,
    pub headers: http::HeaderMap,
}

#[derive(Debug)]
pub struct MockTransport {
    queue: Mutex<VecDeque<http::Response<Bytes>>>,
    seen: Mutex<Vec<RecordedRequest>>,
    caps: Capabilities,
}

#[derive(Debug)]
struct QueueEmpty;
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

    pub fn push_response(&self, resp: http::Response<&'static str>) {
        let (parts, body) = resp.into_parts();
        self.queue
            .lock()
            .expect("mock lock poisoned")
            .push_back(http::Response::from_parts(
                parts,
                Bytes::from_static(body.as_bytes()),
            ));
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
    type Body = Full<Bytes>;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        let (parts, _body) = req.into_parts();
        self.seen
            .lock()
            .expect("mock lock poisoned")
            .push(RecordedRequest {
                method: parts.method,
                uri: parts.uri,
                headers: parts.headers,
            });
        match self.queue.lock().expect("mock lock poisoned").pop_front() {
            Some(r) => {
                let (p, b) = r.into_parts();
                Ok(http::Response::from_parts(p, Full::new(b)))
            }
            None => Err(Error::new(ErrorKind::Other, QueueEmpty)),
        }
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
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
    }

    #[test]
    fn with_capabilities_overrides_the_default_none() {
        let mut caps = Capabilities::none();
        caps.streaming_request_body = true;
        let m = MockTransport::new().with_capabilities(caps);
        assert!(m.capabilities().streaming_request_body);
    }
}
