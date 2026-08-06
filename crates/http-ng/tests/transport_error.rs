//! Доживает ли категория ошибки транспорта до потребителя.
//!
//! B2 финального ревью ветки: `Client::execute` делал
//! `Error::new(ErrorKind::Other, e)` для ЛЮБОЙ ошибки транспорта. Сорок
//! строк `http-ng-wasi::convert::wasi_err`, раскладывающие 39 вариантов
//! `ErrorCode` на восемь `ErrorKind`, выбрасывались одним слоем выше:
//! каждый предикат `is_*` фасада возвращал `false` для любой ошибки,
//! пришедшей от транспорта, а `kind()` был `Other` одинаково для DNS-сбоя,
//! TLS-сбоя, connect-таймаута и отказа хоста. 165 тестов этого не видели,
//! потому что мок умел провалить `execute` только исчерпанием очереди — а у
//! той категория `Other` и так верная.

// `http_ng::mock` живёт за фичей `test-util` (см. `mock.rs`).
#![cfg(feature = "test-util")]

use http_ng::mock::MockTransport;
use http_ng::{Client, Error, ErrorKind, Phase};

#[derive(Debug)]
struct Backend(&'static str);
impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Backend {}

fn client_failing_with(kind: ErrorKind, msg: &'static str) -> Client<MockTransport> {
    let m = MockTransport::new();
    m.push_transport_error(Error::new(kind, Backend(msg)));
    Client::builder(m).build().unwrap()
}

/// Ядро находки: `err.kind()` обязан быть тем, чем его назвал бэкенд.
/// `Timeout(Connect)` выбран потому, что это ровно то, что `wasi_err`
/// производит для `ErrorCode::ConnectionTimeout`, и ровно то, ради чего
/// существует `Phase`.
#[test]
fn transport_error_kind_reaches_the_caller_instead_of_being_flattened() {
    let c = client_failing_with(ErrorKind::Timeout(Phase::Connect), "connect timed out");
    let err = futures_executor::block_on(c.get("https://a/x").send()).unwrap_err();

    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::Connect),
        "категория, которую назвал транспорт, обязана дожить до вызывающей стороны, а не \
         расплющиться в Other: {err}"
    );
    assert!(
        err.is_timeout(),
        "и предикаты фасада обязаны с ней согласиться"
    );
}

/// Тот же дефект с другой стороны таксономии: `Unsupported` — категория, про
/// которую `http-ng-wasi::convert` прямо пишет, что вызывающая сторона
/// должна отличать «бэкенд так не умеет» от прочих отказов «через
/// `is_unsupported()`». Через фасад она этого не могла.
#[test]
fn unsupported_from_the_transport_is_still_unsupported_at_the_facade() {
    let c = client_failing_with(
        ErrorKind::Unsupported,
        "wasi:http host rejected setting 'scheme'",
    );
    let err = futures_executor::block_on(c.get("https://a/x").send()).unwrap_err();

    assert!(err.is_unsupported(), "{err}");
    assert!(!err.is_timeout());
    assert!(!err.is_connect());
}

/// Отложенный минор Task 6 («`Display` дублирует текст источника») умирает
/// здесь же: он был симптомом того самого вложения. Раньше потребитель
/// видел `Other: Unsupported: wasi:http host rejected …` — категория,
/// которой у ошибки нет, впереди категории, которая у неё есть.
#[test]
fn display_does_not_nest_a_second_kind_in_front_of_the_real_one() {
    let c = client_failing_with(ErrorKind::Unsupported, "host rejected setting 'scheme'");
    let err = futures_executor::block_on(c.get("https://a/x").send()).unwrap_err();

    let msg = err.to_string();
    assert_eq!(
        msg, "Unsupported: host rejected setting 'scheme'",
        "категория печатается один раз, и это настоящая категория"
    );
    assert!(!msg.contains("Other"), "{msg}");
}

/// Обратная сторона: ошибка, у которой категория ДЕЙСТВИТЕЛЬНО `Other`,
/// такой и остаётся — тождество в `MockTransport::to_error` не значит
/// «всё стало не-`Other`». Исчерпание очереди — единственный собственный
/// отказ мока, и он честно `Other`.
#[test]
fn a_genuinely_other_transport_error_stays_other() {
    let m = MockTransport::new();
    let c = Client::builder(m).build().unwrap();
    let err = futures_executor::block_on(c.get("https://a/x").send()).unwrap_err();

    assert_eq!(*err.kind(), ErrorKind::Other);
    let src = std::error::Error::source(&err).expect("Error::new всегда кладёт source");
    assert!(
        src.downcast_ref::<http_ng::mock::QueueEmpty>().is_some(),
        "и источник по-прежнему сам QueueEmpty, а не ещё одна обёртка над ним"
    );
}
