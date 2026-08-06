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

// ── Content-Type: границы токена, не префикс (review round 1, Finding 2) ──
//
// `starts_with(MIME)` принимал `"text/event-streamfoo"` и отвергал
// `"Text/Event-Stream"`. Четыре формы ниже покрывают обе стороны дефекта:
// точное совпадение уже проверено `parses_events_from_a_response` выше.

fn sse_response_with_content_type(
    body: &'static str,
    content_type: &'static str,
) -> http::Response<&'static str> {
    http::Response::builder()
        .status(200)
        .header("content-type", content_type)
        .body(body)
        .unwrap()
}

#[test]
fn accepts_content_type_with_charset_parameter() {
    let m = MockTransport::new();
    m.push_response(sse_response_with_content_type(
        "data: x\n\n",
        "text/event-stream; charset=utf-8",
    ));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_ok());
}

#[test]
fn accepts_content_type_regardless_of_case() {
    let m = MockTransport::new();
    m.push_response(sse_response_with_content_type(
        "data: x\n\n",
        "Text/Event-Stream",
    ));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(
        SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_ok(),
        "HTTP media types are case-insensitive (RFC 9110 §5.5)"
    );
}

#[test]
fn rejects_content_type_that_merely_starts_with_the_mime_type() {
    let m = MockTransport::new();
    m.push_response(sse_response_with_content_type(
        "data: x\n\n",
        "text/event-streamfoo",
    ));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(
        SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_err(),
        "a prefix match is not a media-type token match"
    );
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

// ── Порядок фатальной ошибки (review round 1, Finding 1) ──────────────────
//
// Раньше `next()` возвращал `Err` на превышении лимита СРАЗУ, до того как
// уже разобранное валидное событие того же `push` дошло бы до вызывающего
// через отдельный вызов `next()` — и вдобавок стрим не был по-настоящему
// кончен: следующий вызов молча отдавал это событие как `Ok`, а уже ПОСЛЕ
// него — `None`. Наблюдаемая последовательность была `Err, Ok("a"), None` —
// теряло и «`Err` раньше `Ok`», и собственно «фатальность».
#[test]
fn oversized_event_is_fatal_but_does_not_lose_events_decoded_before_it() {
    let m = MockTransport::new();
    // Валидное событие под лимитом, затем — то же переразмеренное событие,
    // что использует юнит-тест декодера `oversized_event_is_a_fatal_error`
    // (sse/decode.rs), в одном кадре: оба разбираются за один `push`, так что
    // декодер успевает продиспетчить первое ДО того, как второе провалит
    // проверку лимита.
    m.push_response(sse_response("data: a\n\ndata: 0123456789abcdefghij\n\n"));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, 16).unwrap();

    let first = futures_executor::block_on(s.next())
        .expect("the event decoded before the limit tripped must not be lost")
        .expect("the first event is valid, not the error");
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "a".into(),
            id: None
        },
        "the valid event must survive, and survive intact"
    );

    let second = futures_executor::block_on(s.next())
        .expect("the limit violation must surface as an item, not be swallowed");
    assert!(
        second.is_err(),
        "the oversized event must be reported as an error, not accepted"
    );

    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "the stream must be over immediately after the fatal error"
    );
    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "\"fatal\" must mean forever, not a one-shot glitch the stream recovers from"
    );
}

/// The other fatal path: a body-level error from `Response::chunk()` (e.g. a
/// dropped connection mid-stream), not a decoder size-limit violation. Same
/// ordering contract as the test above — "structurally identical" is not
/// "actually exercised", so this is tested independently rather than assumed
/// to hold by resemblance (review round 2, Finding 2).
/// `MockTransport::push_response_frames_then_error` (`mock.rs`) makes
/// `MockBody::poll_frame` return `Err` after its data frames, which
/// `Response::chunk()` wraps as `Error::new(ErrorKind::Body, ..)`.
#[test]
fn body_error_is_fatal_but_does_not_lose_events_decoded_before_it() {
    let m = MockTransport::new();
    m.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: a\n\n"])
            .unwrap(),
        http_ng_core::Error::new(
            http_ng_core::ErrorKind::Other,
            std::io::Error::other("boom"),
        ),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();

    let first = futures_executor::block_on(s.next())
        .expect("the event decoded before the body error must not be lost")
        .expect("the first event is valid, not the error");
    assert_eq!(
        first,
        SseEvent::Message {
            event: None,
            data: "a".into(),
            id: None
        },
        "the valid event must survive, and survive intact"
    );

    let second = futures_executor::block_on(s.next())
        .expect("the body error must surface as an item, not be swallowed");
    assert!(
        second.is_err(),
        "the body error must be reported as an error, not accepted as a valid end of stream"
    );

    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "the stream must be over immediately after the fatal error"
    );
    assert!(
        futures_executor::block_on(s.next()).is_none(),
        "\"fatal\" must mean forever, not a one-shot glitch the stream recovers from"
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

/// CRLF-терминатор разорван ровно между CR и LF границей кадра транспорта, на
/// строке `data:` — не на границе пустой строки. (review round 1, Finding 3:
/// разрыв на границе *пустой* строки не был диагностическим — мутация,
/// отключающая `carried_terminator`, всё равно давала верный результат,
/// потому что фантомная пустая строка приходилась на уже опустевший буфер
/// данных и `dispatch()` на ней no-op'ился. Здесь же непроглоченный LF
/// стартует ПРЕЖДЕВРЕМЕННЫЙ dispatch между двумя `data:`-строками одного
/// события: сломанный `carried_terminator` даёт два события `"ab"`/`"cd"`
/// вместо одного `"ab\ncd"`.) `carried_terminator` сам по себе уже покрыт
/// юнит-тестами `sse/lines.rs`; здесь он впервые проверяется сквозь весь
/// стек — транспорт -> `Response::chunk` -> `SseDecoder` -> `SseStream`.
#[test]
fn crlf_terminator_split_mid_event_across_frame_boundary_joins_the_data_lines() {
    let m = MockTransport::new();
    m.push_response_frames(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(vec!["data: ab\r", "\ndata: cd\n\n"])
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
            data: "ab\ncd".into(),
            id: None
        }]
    );
}
