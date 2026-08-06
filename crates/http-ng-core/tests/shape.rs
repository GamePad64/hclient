//! Утверждения о форме публичного API, вынесенные за пределы `src`.
//!
//! Проверка `no-declared-send` в CI сканирует только `crates/*/src`, поэтому
//! обычная генерик-форма здесь не конфликтует с ней, а список исключений
//! сохраняет смысл «обоснованное исключение в продакшн-коде».

use bytes::Bytes;
use http_ng_core::unversioned::{Timer, Transport};
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, Timeouts, UnsupportedCapability};

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn capability_types_are_send_and_sync() {
    assert_send_sync::<Capabilities>();
    assert_send_sync::<Timeouts>();
    assert_send_sync::<UnsupportedCapability>();
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
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
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
