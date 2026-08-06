//! `SseStream`: декодированный поток SSE-событий поверх `Response::chunk`.
//!
//! `#![cfg(feature = "test-util")]` — этот файл тянет `http_ng::mock`, а он
//! гейтед за `test-util` (см. `mock.rs`); без гейта здесь голый
//! `cargo test -p http-ng` не собрался бы (тот же паттерн, что в `shape.rs`
//! и `response.rs`).
#![cfg(feature = "test-util")]

use http_ng::mock::MockTransport;
use http_ng::{Client, DEFAULT_MAX_EVENT_SIZE, SseEvent, SseStream};

fn sse_response(body: &'static str) -> http::Response<&'static str> {
    http::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}

#[test]
fn parses_events_from_a_response() {
    let m = MockTransport::new();
    m.push_response(sse_response("data: one\n\ndata: two\n\n"));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();

    let mut got = Vec::new();
    while let Some(e) = futures_executor::block_on(s.next()) {
        got.push(e.unwrap())
    }

    assert_eq!(
        got,
        vec![
            SseEvent::Message {
                event: None,
                data: "one".into(),
                id: None
            },
            SseEvent::Message {
                event: None,
                data: "two".into(),
                id: None
            },
        ]
    );
}

#[test]
fn rejects_wrong_content_type() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body("{}")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_err());
}

#[test]
fn rejects_non_200_status() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(204)
            .header("content-type", "text/event-stream")
            .body("")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(
        SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_err(),
        "204 означает «прекрати навсегда», а не «пустой поток»"
    );
}

#[test]
fn tracks_last_event_id_for_future_reconnects() {
    let m = MockTransport::new();
    m.push_response(sse_response("id: 99\ndata: x\n\n"));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();
    while futures_executor::block_on(s.next()).is_some() {}
    assert_eq!(s.last_event_id(), Some("99"));
}

// ── Разрыв на границе чанка ─────────────────────────────────────────────
//
// Ни один тест выше не пересекает границу чанка транспорта:
// `MockTransport::push_response` отдаёт всё тело одним кадром, так что
// склейка на уровне `SseStream` (а не только внутри `SseDecoder`/
// `LineSplitter`, уже покрытых в http-ng-proto) остаётся непроверенной.
// `push_response_frames` существует ровно для этого.

/// Событие разорвано посреди поля (`"on" | "e\n\n..."`) — самый частый
/// реальный случай: TCP-чанк почти никогда не совпадает с границей строки.
#[test]
fn event_split_mid_field_across_frames_still_yields_two_events() {
    let m = MockTransport::new();
    m.push_response_frames(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: on", "e\n\ndata: two\n", "\n"])
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();

    let mut got = Vec::new();
    while let Some(e) = futures_executor::block_on(s.next()) {
        got.push(e.unwrap())
    }

    assert_eq!(
        got,
        vec![
            SseEvent::Message {
                event: None,
                data: "one".into(),
                id: None
            },
            SseEvent::Message {
                event: None,
                data: "two".into(),
                id: None
            },
        ]
    );
}

/// CRLF-терминатор разорван ровно между CR и LF границей кадра транспорта.
/// В `LineSplitter` это путь `carried_terminator` (`sse/lines.rs`) — здесь он
/// впервые проверяется сквозь весь стек: транспорт -> `Response::chunk` ->
/// `SseDecoder` -> `SseStream`, а не только напрямую против сплиттера.
#[test]
fn crlf_terminator_split_across_frame_boundary_is_still_one_event() {
    let m = MockTransport::new();
    m.push_response_frames(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: hi\r", "\n\r\n"])
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();

    let mut got = Vec::new();
    while let Some(e) = futures_executor::block_on(s.next()) {
        got.push(e.unwrap())
    }

    assert_eq!(
        got,
        vec![SseEvent::Message {
            event: None,
            data: "hi".into(),
            id: None
        }]
    );
}
