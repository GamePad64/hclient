//! Тесты стадии redirect на уровне `Client`: `http-ng-proto::redirect::decide`
//! уже протестирован как чистая функция (Task 5) — здесь проверяется, что
//! плагин `client.rs`/`stages/redirect.rs` не искажает её решение при
//! перекладывании данных между хопами.

// `http_ng::mock` живёт за фичей `test-util` (см. `mock.rs`); без этой строки
// `cargo test -p http-ng` без флагов падал с E0432 вместо того, чтобы
// собраться в пустоту — как уже было сделано для `shape.rs` в Task 12. Task
// 13 fix round 2, Residual 3.
#![cfg(feature = "test-util")]

use http_ng::mock::MockTransport;
use http_ng::{Client, RedirectPolicy, RequestBody};

fn redirect_to(loc: &'static str) -> http::Response<&'static str> {
    http::Response::builder()
        .status(302)
        .header("location", loc)
        .body("")
        .unwrap()
}

#[test]
fn follows_a_redirect_and_records_both_hops() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://a/second"));
    m.push_response(http::Response::builder().status(200).body("done").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .uri("https://a/first")
        .body(RequestBody::Empty)
        .unwrap();
    let resp = futures_executor::block_on(c.execute(req)).unwrap();

    assert_eq!(resp.status(), 200);
    let seen = c.transport().requests();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[1].uri,
        "https://a/second".parse::<http::Uri>().unwrap()
    );
}

#[test]
fn strips_authorization_when_the_host_changes() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://evil/steal"));
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .uri("https://a/first")
        .header("authorization", "Bearer secret")
        .header("x-safe", "keep")
        .body(RequestBody::Empty)
        .unwrap();
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c.transport().requests();
    assert!(
        seen[0].headers.contains_key("authorization"),
        "первый хоп сохраняет"
    );
    assert!(
        !seen[1].headers.contains_key("authorization"),
        "второй хоп снимает"
    );
    assert!(
        seen[1].headers.contains_key("x-safe"),
        "несекретные заголовки остаются"
    );
}

#[test]
fn does_not_follow_304() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(304)
            .header("location", "https://a/nope")
            .body("")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    let resp = futures_executor::block_on(c.execute(req)).unwrap();

    assert_eq!(resp.status(), 304);
    assert_eq!(c.transport().requests().len(), 1);
}

#[test]
fn enforces_the_hop_limit() {
    let m = MockTransport::new();
    for _ in 0..5 {
        m.push_response(redirect_to("https://a/loop"));
    }

    let c = Client::builder(m)
        .redirect(RedirectPolicy { limit: 2 })
        .build()
        .unwrap();
    let req = http::Request::builder()
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    let err = futures_executor::block_on(c.execute(req)).unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert_eq!(
        c.transport().requests().len(),
        3,
        "исходный запрос плюс два хопа"
    );
}

/// `hops >= policy.limit` — off-by-one bait: `limit: 0` must reject the very
/// first redirect without ever incrementing `hops`, sending only the
/// original request.
#[test]
fn redirect_limit_of_zero_sends_only_the_original_request() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://a/loop"));

    let c = Client::builder(m)
        .redirect(RedirectPolicy { limit: 0 })
        .build()
        .unwrap();
    let req = http::Request::builder()
        .uri("https://a/x")
        .body(RequestBody::Empty)
        .unwrap();
    let err = futures_executor::block_on(c.execute(req)).unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert_eq!(
        c.transport().requests().len(),
        1,
        "только исходный запрос, ни одного хопа"
    );
}

#[test]
fn post_becomes_get_and_drops_body_on_302() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://a/second"));
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://a/first")
        .body(RequestBody::Full(bytes::Bytes::from_static(b"payload")))
        .unwrap();
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c.transport().requests();
    assert_eq!(seen[1].method, http::Method::GET);
    // Найдено ревью: имя теста обещает, что тело отброшено, но исходная
    // версия проверяла только метод. Проверяем форму тела на обоих хопах —
    // на первом оно было (7 байт полезной нагрузки "payload"), на втором
    // обязано стать пустым, а не проехать до кросс-оригинного назначения.
    assert_eq!(
        seen[1].body_size_hint,
        Some(0),
        "тело обязано быть отброшено"
    );
    assert_eq!(seen[0].body_size_hint, Some(7), "на первом хопе оно было");
}

#[test]
fn build_rejects_a_timeout_the_backend_cannot_honour() {
    use http_ng::Timeouts;
    let m = MockTransport::new(); // Capabilities::none() — таймауты не поддержаны
    let err = Client::builder(m)
        .timeouts(Timeouts {
            connect: Some(std::time::Duration::from_secs(1)),
            ..Default::default()
        })
        .build()
        .unwrap_err();
    assert_eq!(err.what, "connect_timeout");
}

/// Только `Location` читается из ответа при построении следующего хопа.
/// Ничто в `next_hop`/`decide()` сегодня не трогает остальные заголовки
/// ответа, но до fix round 1 ни один тест не проверял это поведенчески —
/// мутация, сливающая `resp.headers()` в заголовки следующего запроса
/// (`Set-Cookie` от сервера или что угодно ещё утёкшее в цепочку), проходила
/// бы все шесть тестов из брифа незамеченной.
#[test]
fn response_headers_do_not_leak_into_the_next_hop() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a/second")
            .header("set-cookie", "sid=abc123")
            .header("x-injected", "should-not-cross")
            .body("")
            .unwrap(),
    );
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .uri("https://a/first")
        .body(RequestBody::Empty)
        .unwrap();
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c.transport().requests();
    assert!(
        !seen[1].headers.contains_key("set-cookie"),
        "response header set-cookie must not leak into the next request"
    );
    assert!(
        !seen[1].headers.contains_key("x-injected"),
        "response header x-injected must not leak into the next request"
    );
}

/// `Timeouts` (Task 10) едет к транспорту через `http::Extensions` запроса —
/// весь механизм, ради которого оно там лежит, полагается на то, что
/// `extensions` доживают до каждого хопа, а не только до первого.
#[test]
fn per_request_extensions_survive_a_hop_unchanged() {
    use http_ng_core::Timeouts;
    use std::time::Duration;

    let m = MockTransport::new();
    m.push_response(redirect_to("https://a/second"));
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let mut req = http::Request::builder()
        .uri("https://a/first")
        .body(RequestBody::Empty)
        .unwrap();
    req.extensions_mut().insert(Timeouts {
        connect: Some(Duration::from_secs(3)),
        ..Default::default()
    });
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c.transport().requests();
    let t0 = seen[0]
        .extensions
        .get::<Timeouts>()
        .expect("hop 0 carries the Timeouts inserted on the original request");
    let t1 = seen[1]
        .extensions
        .get::<Timeouts>()
        .expect("hop 1 must carry the same Timeouts, not drop it");
    assert_eq!(t0.connect, Some(Duration::from_secs(3)));
    assert_eq!(
        t1.connect,
        Some(Duration::from_secs(3)),
        "unchanged across the hop"
    );
}

/// Тело `Streaming` невоспроизводимо: `RequestBody::rewind()` вернёт `None`.
/// Честное поведение — вернуть 3xx как есть и не отправлять второй запрос с
/// пустым телом туда, где сервер ждёт оригинальный payload.
///
/// Статус — 307, не 302: `decide()` понижает метод (и с ним отбрасывает тело
/// осознанно, `drop_body`) только для POST на 301/302/303. На 307/308 метод и
/// тело обязаны выжить как есть — это и есть путь, где непереигрываемость
/// тела действительно встаёт в полный рост, а не маскируется даунгрейдом.
#[test]
fn unreplayable_streaming_body_stops_at_the_3xx_instead_of_a_second_empty_request() {
    struct OneShot(Option<bytes::Bytes>);
    impl http_body::Body for OneShot {
        type Data = bytes::Bytes;
        type Error = http_ng_core::Error;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, Self::Error>>> {
            std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
        }
    }

    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(307)
            .header("location", "https://a/second")
            .body("")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder()
        .method("POST")
        .uri("https://a/first")
        .body(RequestBody::Streaming(Box::new(OneShot(Some(
            bytes::Bytes::from_static(b"payload"),
        )))))
        .unwrap();
    let resp = futures_executor::block_on(c.execute(req)).unwrap();

    // Ни один второй запрос не был отправлен: мок видел только исходный хоп.
    assert_eq!(resp.status(), 307, "3xx возвращён как есть");
    let seen = c.transport().requests();
    assert_eq!(
        seen.len(),
        1,
        "второй запрос с пустым телом не отправляется"
    );
}
