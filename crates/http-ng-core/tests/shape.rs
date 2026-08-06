//! Утверждения о форме публичного API, вынесенные за пределы `src`.
//!
//! Проверка `no-declared-send` в CI сканирует только `crates/*/src`, поэтому
//! обычная генерик-форма здесь не конфликтует с ней, а список исключений
//! сохраняет смысл «обоснованное исключение в продакшн-коде».

use bytes::Bytes;
use http_ng_core::unversioned::{Timer, Transport};
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, Timeouts, UnsupportedCapability};

fn assert_send_sync<T: Send + Sync>() {}
fn assert_send<T: Send>() {}

#[test]
fn capability_types_are_send_and_sync() {
    assert_send_sync::<Capabilities>();
    assert_send_sync::<Timeouts>();
    assert_send_sync::<UnsupportedCapability>();
}

/// `Error: Send + Sync` — spec amendment C1, the single documented exception
/// from "ядро не объявляет Send/Sync": `Error::source` обязан быть
/// `Send + Sync`, иначе future, который возвращает `Client::execute`, не мог
/// бы попасть в `tokio::spawn` ни для одного бэкенда. Was a compile-time-only
/// assertion inside `error.rs`'s own `#[cfg(test)] mod tests` until Task 12's
/// fix round 1 moved it here (amendment C3: such assertions belong in
/// `tests/`, not `src`) — the runtime construction below keeps it from being
/// a vacuous no-op, same as the original.
#[test]
fn error_is_send_sync_and_constructs_a_real_error_not_just_compiles() {
    assert_send_sync::<Error>();
    let e = Error::new(ErrorKind::Other, Never);
    assert_eq!(e.kind(), &ErrorKind::Other);
}

/// `RequestBody: Send` and `http::Request<RequestBody>: Send` — spec
/// amendment C2: without `+ Send` on both of `RequestBody`'s trait objects,
/// `RequestBody` and therefore `http::Request<RequestBody>` would be
/// `!Send`, and `Transport::execute`'s future with it. Relocated from
/// `body.rs`'s own test module for the same C3 reason as the `Error` test
/// above.
#[test]
fn request_body_and_its_request_are_send() {
    assert_send::<RequestBody>();
    assert_send::<http::Request<RequestBody>>();
}

struct Echo {
    caps: Capabilities,
}

#[derive(Debug)]
struct Never;
impl std::fmt::Display for Never {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "never")
    }
}
impl std::error::Error for Never {}

impl Transport for Echo {
    type Body = http_body_util::Full<Bytes>;
    type Error = Error;
    async fn execute(
        &self,
        _req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        Ok(http::Response::new(http_body_util::Full::new(
            Bytes::from_static(b"ok"),
        )))
    }
    fn into_error(&self, e: Self::Error) -> Error {
        e
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// Бэкенд со своим типом ошибки, который `into_error` не переопределяет —
/// то, ради чего у хука вообще есть дефолт (B2 финального ревью ветки).
struct Bare {
    caps: Capabilities,
}

#[derive(Debug, PartialEq)]
struct Custom;
impl std::fmt::Display for Custom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "backend said no")
    }
}
impl std::error::Error for Custom {}

impl Transport for Bare {
    type Body = http_body_util::Full<Bytes>;
    type Error = Custom;
    async fn execute(
        &self,
        _req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        Err(Custom)
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// Дефолт `Transport::into_error` обёртывает с `ErrorKind::Other`,
/// **сохраняя источник целиком**: бэкенду, которому нечего сказать о
/// категории, не нужно ничего писать, а вызывающая сторона всё равно
/// получает типизированный источник, а не строку.
#[test]
fn into_error_defaults_to_other_and_keeps_the_source_intact() {
    let t = Bare {
        caps: Capabilities::none(),
    };
    let e = t.into_error(Custom);
    assert_eq!(e.kind(), &ErrorKind::Other);
    let src = std::error::Error::source(&e).expect("Error::new всегда кладёт source");
    assert_eq!(
        src.downcast_ref::<Custom>(),
        Some(&Custom),
        "источник обязан остаться собой, а не стать строкой"
    );
}

/// А бэкенд, чья ошибка уже `Error`, переопределяет хук тождеством — иначе
/// его категория теряется, а `Display` печатает источник дважды. `Echo`
/// здесь стоит за `http-ng-wasi`, чей `type Error = http_ng_core::Error` и
/// который делает ровно это.
#[test]
fn a_backend_whose_error_is_already_ours_can_pass_it_through_unchanged() {
    let t = Echo {
        caps: Capabilities::none(),
    };
    let e = t.into_error(Error::new(ErrorKind::Tls, Never));
    assert_eq!(
        e.kind(),
        &ErrorKind::Tls,
        "тождество обязано сохранять категорию, а не пересобирать ошибку"
    );
    assert_eq!(
        e.to_string(),
        "Tls: never",
        "и не вкладывать вторую категорию перед настоящей"
    );
}

/// Утверждает главное архитектурное свойство ядра: `Send` нигде не
/// объявлен, но выводится auto-traits, когда транспорт действительно Send.
#[test]
fn send_propagates_without_being_declared() {
    fn assert_send<T: Send>(_: T) {}
    let t = Echo {
        caps: Capabilities::none(),
    };
    let fut = t.execute(http::Request::new(RequestBody::Empty));
    assert_send(fut);
}

#[test]
fn non_send_transport_still_satisfies_the_trait() {
    struct Local {
        caps: Capabilities,
        _rc: std::rc::Rc<()>,
    }
    impl Transport for Local {
        type Body = http_body_util::Full<Bytes>;
        type Error = Error;
        async fn execute(
            &self,
            _req: http::Request<RequestBody>,
        ) -> Result<http::Response<Self::Body>, Self::Error> {
            Err(Error::new(ErrorKind::Other, Never))
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
    }
    let _ = Local {
        caps: Capabilities::none(),
        _rc: std::rc::Rc::new(()),
    };
}

/// Тот же инвариант, но по оси, которую `into_error` могла бы сломать
/// (B2 финального ревью ветки): транспорт, чья ОШИБКА честно `!Send`.
///
/// Это единственная причина, по которой `into_error` — дефолтный метод с
/// where-клаузой, а не `Transport::Error: Into<Error>` на трейте и не
/// `Error` в качестве типа ошибки шва: любая из тех двух форм потребовала
/// бы `Send + Sync` от ошибки каждого бэкенда и выбросила бы этот тип из
/// `Transport` вовсе. Поправка C1 сохраняет его представимым — он не может
/// пользоваться `Client` (и не может вызвать `into_error`), но `Transport`
/// реализует. Тест ничего не «проверяет» в рантайме; он не компилируется,
/// если инвариант нарушен, и `Rc` внутри ошибки — не украшение, а то, что
/// делает её `!Send` по-настоящему.
#[test]
fn a_transport_whose_error_is_not_send_still_implements_the_trait() {
    #[derive(Debug)]
    struct NotSend(#[allow(dead_code)] std::rc::Rc<()>);
    impl std::fmt::Display for NotSend {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "not send")
        }
    }
    impl std::error::Error for NotSend {}

    struct LocalErr {
        caps: Capabilities,
    }
    impl Transport for LocalErr {
        type Body = http_body_util::Full<Bytes>;
        type Error = NotSend;
        async fn execute(
            &self,
            _req: http::Request<RequestBody>,
        ) -> Result<http::Response<Self::Body>, Self::Error> {
            Err(NotSend(std::rc::Rc::new(())))
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
    }
    let t = LocalErr {
        caps: Capabilities::none(),
    };
    assert!(!t.capabilities().streaming_request_body);
}

/// Тривиальный `Timer`, чей `Instant` — просто счётчик. Достаточно, чтобы
/// проверить свойство трейта, а не поведение реального таймера.
struct Fake(std::cell::Cell<u64>);

impl Timer for Fake {
    type Instant = u64;

    async fn sleep(&self, _d: core::time::Duration) {}

    fn now(&self) -> Self::Instant {
        let v = self.0.get();
        self.0.set(v + 1);
        v
    }

    fn elapsed_since(&self, earlier: Self::Instant) -> core::time::Duration {
        core::time::Duration::from_secs(self.now().saturating_sub(earlier))
    }
}

/// Сравнивает два уже захваченных `Instant`, зная о них только то, что даёт
/// сам трейт `Timer` — то есть **обобщённо** по `T: Timer`, без
/// монопморфизации до конкретного `Fake::Instant = u64`. Это важно: если бы
/// тест сравнивал `a < b` на конкретном `u64`, компилятор нашёл бы
/// `Ord`/`PartialOrd` на `u64` напрямую и пропустил бы отсутствие бонда на
/// самом трейте незамеченным. Здесь же `a` и `b` имеют абстрактный тип
/// `T::Instant`, и без `PartialOrd` в объявлении `Timer::Instant` строка
/// `a < b` не компилируется — `E0369: binary operation '<' cannot be
/// applied to type '<T as Timer>::Instant'`.
fn are_ordered<T: Timer>(a: T::Instant, b: T::Instant) -> bool {
    a < b
}

/// Потребитель, держащий на руках два уже захваченных `Instant`, должен
/// иметь возможность сравнить их напрямую — без третьего вызова `now()`
/// (третий вызов не эквивалентен: он меряет момент сравнения, а не момент
/// второго `now()`).
#[test]
fn captured_instants_are_orderable_without_a_third_now_call() {
    let t = Fake(std::cell::Cell::new(0));
    let a = t.now();
    let b = t.now();
    assert!(
        are_ordered::<Fake>(a, b),
        "second capture must order after the first"
    );
}
