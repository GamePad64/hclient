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
///
/// За фичей `json`: сам метод `#[cfg(feature = "json")]`-гейтед, так что этот
/// тест обязан быть гейтед так же — иначе `cargo test -p http-ng --features
/// test-util` (без `json`) не соберётся.
#[cfg(feature = "json")]
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

/// Брифовый `header()` отбрасывал невалидную пару молча (`if let (Ok(n),
/// Ok(v)) = .. { .. }`, без `else`) — тот самый тихий no-op, против которого
/// построен `ClientBuilder::build` (Task 13 fix round 1, Finding 4). Кладём
/// валидный ответ в очередь: если баг вернётся, `send()` тихо дойдёт до
/// транспорта и вернёт `Ok`, а не `Err`.
#[test]
fn invalid_header_name_fails_send_instead_of_silently_dropping_it() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m).build().unwrap();

    let result = futures_executor::block_on(c.get("https://a/x").header("bad header", "v").send());
    assert!(
        result.is_err(),
        "an invalid header name must fail send(), not silently proceed: {result:?}"
    );
    assert!(
        c.transport().requests().is_empty(),
        "the request must never reach the transport once header() recorded an error"
    );
}

/// `chunk()` skips trailer frames — documented in `response.rs` but, before
/// this fix round, untested: `push_response`/`push_response_frames` only ever
/// produce data frames. `push_response_with_trailers` closes that gap
/// (Task 13 fix round 1, Finding 5).
#[test]
fn chunk_skips_trailer_frames() {
    let mut trailers = http::HeaderMap::new();
    trailers.insert("x-trailer", "v".parse().unwrap());

    let m = MockTransport::new();
    m.push_response_with_trailers(
        http::Response::builder()
            .status(200)
            .body(vec!["data"])
            .unwrap(),
        trailers,
    );

    let c = Client::builder(m).build().unwrap();
    let mut resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    let first = futures_executor::block_on(resp.chunk())
        .expect("the data frame must be present")
        .unwrap();
    assert_eq!(&first[..], b"data");
    assert!(
        futures_executor::block_on(resp.chunk()).is_none(),
        "chunk() must skip the trailer frame and report end of stream, not surface it as data"
    );
}

/// The other half of the asymmetry `chunk_skips_trailer_frames` proves one
/// side of: `into_parts()` hands back the raw body, and polling it directly
/// (as `SseStream`/any caller with `Task 14`-style needs would) DOES see the
/// trailer frame that `chunk()` swallows.
#[test]
fn into_parts_lets_you_poll_the_trailer_frame_directly() {
    use http_body::Body as _;
    use std::task::{Context, Poll};

    let mut trailers = http::HeaderMap::new();
    trailers.insert("x-trailer", "v".parse().unwrap());

    let m = MockTransport::new();
    m.push_response_with_trailers(
        http::Response::builder()
            .status(200)
            .body(vec!["data"])
            .unwrap(),
        trailers,
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();
    let (_, body) = resp.into_parts();

    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut pinned = std::pin::pin!(body);

    match pinned.as_mut().poll_frame(&mut cx) {
        Poll::Ready(Some(Ok(f))) => {
            assert_eq!(f.into_data().unwrap(), bytes::Bytes::from_static(b"data"))
        }
        other => panic!("expected the data frame, got {other:?}"),
    }
    match pinned.as_mut().poll_frame(&mut cx) {
        Poll::Ready(Some(Ok(f))) => {
            let t = f
                .into_trailers()
                .expect("second frame must be trailers, not data");
            assert_eq!(t.get("x-trailer").unwrap(), "v");
        }
        other => panic!("expected the trailers frame, got {other:?}"),
    }
    match pinned.as_mut().poll_frame(&mut cx) {
        Poll::Ready(None) => {}
        other => panic!("expected end of stream after the trailer frame, got {other:?}"),
    }
}

/// `Response::version()` and `into_parts()` had no direct test (Task 13 fix
/// round 1, Finding 6): `into_parts()` was only exercised indirectly through
/// the trailer tests above, and `version()` not at all.
#[test]
fn version_and_into_parts_expose_the_full_response_head() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(201)
            .version(http::Version::HTTP_11)
            .header("x-k", "v")
            .body("body")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();
    assert_eq!(resp.version(), http::Version::HTTP_11);

    let (parts, mut body) = resp.into_parts();
    assert_eq!(parts.status, 201);
    assert_eq!(parts.headers.get("x-k").unwrap(), "v");

    // The body handed back by into_parts() is the real, unread one.
    use http_body::Body as _;
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    match std::pin::Pin::new(&mut body).poll_frame(&mut cx) {
        std::task::Poll::Ready(Some(Ok(f))) => {
            assert_eq!(f.into_data().unwrap(), bytes::Bytes::from_static(b"body"))
        }
        other => panic!("expected the data frame via the raw body, got {other:?}"),
    }
}
