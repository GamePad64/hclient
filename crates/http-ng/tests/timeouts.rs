//! Композиция таймаутов клиента и запроса — та самая, которую обещают
//! doc-комментарии `ClientBuilder::timeouts` и `RequestBuilder::timeouts`, и
//! которой до финального ревью всей ветки не существовало ни в одну сторону
//! (B1/M3): клиентские проверялись на `build()` и **не доезжали** до
//! транспорта, запросные доезжали и **не проверялись** против `Capabilities`.
//!
//! Все три свойства проверяются через `MockTransport`, а не юнит-тестом на
//! `effective_timeouts`: у той функции три собственных юнит-теста в
//! `config.rs`, и они были зелёными всё время, пока никто её не вызывал.
//! Красный здесь может дать только реально пройденный путь
//! `Client::execute` → `Transport::execute`.

// `http_ng::mock` живёт за фичей `test-util` (см. `mock.rs`).
#![cfg(feature = "test-util")]

use http_ng::mock::MockTransport;
use http_ng::{Capabilities, Client, ErrorKind, TimeoutSupport, Timeouts, UnsupportedCapability};
use std::time::Duration;

fn secs(n: u64) -> Option<Duration> {
    Some(Duration::from_secs(n))
}

/// Транспорт, который умеет все три фазы — иначе `check_supported` отвергнет
/// конфигурацию раньше, чем тест доберётся до проверяемого свойства.
fn all_timeouts_supported() -> Capabilities {
    let mut caps = Capabilities::none();
    caps.timeouts = TimeoutSupport {
        connect: true,
        first_byte: true,
        between_bytes: true,
    };
    caps
}

/// B1. `ClientBuilder::timeouts()` был тихим no-op: `effective_timeouts`
/// существовала, была публична и покрыта тремя юнит-тестами — и не
/// вызывалась ниоткуда в продакшн-коде. Единственный канал к транспорту —
/// `http::Extensions`, и клиентская конфигурация в них не попадала.
#[test]
fn client_level_timeouts_reach_the_transport() {
    let m = MockTransport::new().with_capabilities(all_timeouts_supported());
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m)
        .timeouts(Timeouts {
            connect: secs(7),
            ..Default::default()
        })
        .build()
        .unwrap();
    futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    let seen = c.transport().requests();
    let t = seen[0]
        .extensions
        .get::<Timeouts>()
        .expect("client-level timeouts must reach the transport");
    assert_eq!(t.connect, secs(7));
}

/// B1, вторая половина: «request-first, client-fallback» **поле за полем**, а
/// не «всё или ничего». Запрос задаёт только `first_byte`; две другие фазы
/// обязаны прийти от клиента. Наивная реализация («в extensions уже лежит
/// `Timeouts` — значит клиента не смотрим») оставила бы здесь `None`.
#[test]
fn request_timeouts_override_the_client_field_by_field() {
    let m = MockTransport::new().with_capabilities(all_timeouts_supported());
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m)
        .timeouts(Timeouts {
            connect: secs(1),
            first_byte: secs(2),
            between_bytes: secs(3),
        })
        .build()
        .unwrap();
    futures_executor::block_on(
        c.get("https://a/x")
            .timeouts(Timeouts {
                first_byte: secs(9),
                ..Default::default()
            })
            .send(),
    )
    .unwrap();

    let seen = c.transport().requests();
    let t = seen[0]
        .extensions
        .get::<Timeouts>()
        .expect("merged timeouts");
    assert_eq!(t.connect, secs(1), "не перекрыт запросом — берём клиента");
    assert_eq!(t.first_byte, secs(9), "запрос перекрывает");
    assert_eq!(
        t.between_bytes,
        secs(3),
        "не перекрыт запросом — берём клиента"
    );
}

/// M3. `check_supported` гоняется один раз, на `build()`, по
/// `config.timeouts`. `RequestBuilder::timeouts()` писал прямо в
/// `Extensions`, минуя `Capabilities` вовсе — то есть неподдерживаемый
/// таймаут на уровне запроса принимался молча, ровно там, где тот же таймаут
/// на уровне клиента давал типизированную ошибку.
#[test]
fn unsupported_per_request_timeout_is_a_typed_error_not_a_silent_noop() {
    // `MockTransport::new()` — `Capabilities::none()`, все три фазы `false`.
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let err = futures_executor::block_on(
        c.get("https://a/x")
            .timeouts(Timeouts {
                connect: secs(3),
                ..Default::default()
            })
            .send(),
    )
    .expect_err("транспорт не умеет connect-таймаут — это ошибка, а не тихо отброшенное значение");

    assert_eq!(*err.kind(), ErrorKind::Unsupported, "{err}");
    let src = std::error::Error::source(&err).expect("Error::new всегда кладёт source");
    let unsupported = src
        .downcast_ref::<UnsupportedCapability>()
        .expect("источник обязан называть саму неподдерживаемую настройку, а не быть строкой");
    assert_eq!(unsupported.what, "connect_timeout");

    // И запрос не должен был уйти: отвергнутая настройка не превращается в
    // «отправили как есть, просто без таймаута».
    assert!(
        c.transport().requests().is_empty(),
        "запрос с неподдерживаемой настройкой не должен доходить до транспорта"
    );
}
