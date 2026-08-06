//! Тесты `Response`/`Collected`/`RequestBuilder` на уровне `Client`.

use http_ng::mock::MockTransport;
use http_ng::{Client, RequestBody};

#[test]
fn collected_keeps_status_and_headers_after_reading_the_body() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(201)
            .header("x-trace", "abc")
            .body("hello")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    let collected = futures_executor::block_on(resp.collect()).unwrap();
    assert_eq!(collected.text().unwrap(), "hello");
    // Ключевое отличие от reqwest, где `.text()` берёт self по значению:
    // status/headers/url обязаны остаться читаемыми ПОСЛЕ чтения тела.
    assert_eq!(collected.status(), 201);
    assert_eq!(collected.headers().get("x-trace").unwrap(), "abc");
    assert_eq!(
        collected.url(),
        &"https://a/x".parse::<http::Uri>().unwrap()
    );
}

/// Кладёт в очередь двухкадровый ответ и проверяет, что `chunk()` отдаёт
/// кадры по отдельности, в исходном порядке — а не один склеенный блок.
/// Однокадровая версия этого теста прошла бы и с реализацией, которая читает
/// всё тело целиком за первый вызов `poll_frame`, что не доказывало бы
/// собственно стриминг: см. `MockTransport::push_response_frames` в mock.rs.
#[test]
fn chunk_streams_the_body_frame_by_frame_not_concatenated() {
    let m = MockTransport::new();
    m.push_response_frames(
        http::Response::builder()
            .status(200)
            .body(vec!["stream ", "me"])
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let mut resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    let first = futures_executor::block_on(resp.chunk())
        .expect("first frame must be present")
        .unwrap();
    assert_eq!(
        &first[..],
        b"stream ",
        "first chunk() call must yield only the first frame, not the whole body"
    );

    let second = futures_executor::block_on(resp.chunk())
        .expect("second frame must be present")
        .unwrap();
    assert_eq!(&second[..], b"me");

    assert!(
        futures_executor::block_on(resp.chunk()).is_none(),
        "no third frame was queued"
    );
}

#[test]
fn request_builder_sets_method_and_headers() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let _ = futures_executor::block_on(
        c.post("https://a/x")
            .header("x-k", "v")
            .body(RequestBody::Full(bytes::Bytes::from_static(b"p")))
            .send(),
    )
    .unwrap();

    let seen = c.transport().requests();
    assert_eq!(seen[0].method, http::Method::POST);
    assert_eq!(seen[0].headers.get("x-k").unwrap(), "v");
}

/// `request_builder_sets_method_and_headers` только доказывает POST; каждый
/// глагол клиента обязан ставить именно свой метод, а не переиспользовать
/// один и тот же путь построения запроса случайно правильно только для POST.
#[test]
fn get_sends_the_get_method() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let _ = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    assert_eq!(c.transport().requests()[0].method, http::Method::GET);
}

#[test]
fn delete_sends_the_delete_method() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let _ = futures_executor::block_on(c.delete("https://a/x").send()).unwrap();

    assert_eq!(c.transport().requests()[0].method, http::Method::DELETE);
}

/// `Collected::json` не входил в код шага 3 брифа, но объявлен в разделе
/// Interfaces этой задачи («`Collected::json<T>()`, и сохраняет status/
/// headers/url»). Реализован по контракту Interfaces; см. отчёт о задаче.
#[test]
fn collected_json_decodes_the_body_and_still_keeps_status() {
    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Payload {
        ok: bool,
        n: u32,
    }

    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(200)
            .body(r#"{"ok":true,"n":7}"#)
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();
    let collected = futures_executor::block_on(resp.collect()).unwrap();

    let payload: Payload = collected.json().unwrap();
    assert_eq!(payload, Payload { ok: true, n: 7 });
    assert_eq!(collected.status(), 200, "json() must not consume status");
}

/// `RequestBuilder::timeouts` обязан класть `Timeouts` в `Extensions` запроса,
/// откуда их читает транспорт (lookup «request-first, client-fallback»,
/// §4.5 спеки, Task 10). Без записи в `extensions` этот сеттер был бы тихим
/// no-op — ровно тот класс дефекта, которого дизайн крейта старается избежать.
#[test]
fn timeouts_are_placed_in_extensions_where_the_transport_reads_them() {
    use http_ng_core::Timeouts;
    use std::time::Duration;

    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let _ = futures_executor::block_on(
        c.get("https://a/x")
            .timeouts(Timeouts {
                connect: Some(Duration::from_secs(3)),
                ..Default::default()
            })
            .send(),
    )
    .unwrap();

    let seen = c.transport().requests();
    let t = seen[0]
        .extensions
        .get::<Timeouts>()
        .expect("Timeouts set via RequestBuilder::timeouts must reach the transport");
    assert_eq!(t.connect, Some(Duration::from_secs(3)));
}
