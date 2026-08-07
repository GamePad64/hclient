use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Форма скопирована у `hyper::rt::Executor` намеренно: генерик по future,
/// ноль бондов в объявлении. `Send` добавляет `impl`, а не трейт, поэтому
/// однопоточные рантаймы реализуют его честно.
pub trait Spawn<F: Future<Output = ()>> {
    fn spawn(&self, f: F);
}

/// Опции сокета применяются в http-ng **один раз**, на `socket2::Socket`, и
/// рантайм только усыновляет дескриптор (`TcpAdoptStd`). Иначе каждый
/// рантайм-крейт переписывал бы эту простыню заново.
#[derive(Debug, Clone, Default)]
pub struct TcpOpts {
    pub nodelay: bool,
    pub keepalive: Option<Duration>,
    pub local_address: Option<IpAddr>,
    pub send_buffer_size: Option<usize>,
    pub recv_buffer_size: Option<usize>,
    pub reuse_address: bool,
}

pub trait TcpConnect {
    type Stream: hyper::rt::Read + hyper::rt::Write + Unpin;

    fn connect(
        &self,
        addr: SocketAddr,
        opts: &TcpOpts,
    ) -> impl Future<Output = std::io::Result<Self::Stream>>;
}

/// На платформах с файловыми дескрипторами весь набор socket-опций
/// применяется вне рантайма, а рантайм только усыновляет готовый сокет.
pub trait TcpAdoptStd: TcpConnect {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream>;
}

/// Отдельный трейт, а не метод: `getaddrinfo` блокирующий, а на wasm и
/// embedded блокирующего пула нет вовсе. Отсутствие способности должно быть
/// ошибкой компиляции, а не `unimplemented!()` в рантайме.
///
/// **Единственное место во всём проекте, где `Send` объявляем мы сами**, и он
/// здесь честен: и `tokio::task::spawn_blocking`, и `blocking::unblock`
/// требуют `Send + 'static`, а способности `Blocking` на wasm нет вовсе —
/// заражать ей нечего. Обоснование — `amendment-C5`
/// (`docs/superpowers/specs/2026-08-05-http-ng-design.md`), отдельная от C1/C2
/// поправка: те две — про стирание auto-traits в `dyn Trait` на пути
/// `Client -> Transport`, а здесь бонд объявлен прямо в сигнатуре трейта
/// способности, которого на wasm просто нет.
///
/// Бонды — в `where`, а не в списке дженериков `fn run<T: Send + …>`, чтобы
/// каждый нёс собственный маркер `send-bound-exception` на своей строке: CI
/// `no-declared-send` матчит объявление бонда построчно, и один общий
/// комментарий после списка дженериков его бы не покрыл.
///
/// Два разных отказа `f` не смешиваются в один канал:
///
/// - Паника `f` — баг вызывающего кода. Она обязана перевызываться как
///   паника (`std::panic::resume_unwind`, с оригинальным payload), а не
///   тихо превращаться в значение, которое можно `?`-пропустить — иначе
///   реализация прячет дефект вызывающего кода за `Result`.
/// - Уход пула фоновых потоков (например, рантайм завершает работу, пока
///   задача ещё стоит в очереди и не начала выполняться) — это не баг
///   вызывающего кода, а обычное событие жизненного цикла рантайма.
///   Реализация обязана вернуть [`Cancelled`], а не паниковать: паника
///   библиотечного кода на штатном (пусть и редком) сценарии остановки
///   рантайма противоречила бы остальному проекту ("no silent no-ops...
///   typed error, never a discarded value" — то же самое применено и
///   здесь, только к отказу, а не к успеху).
pub trait Blocking {
    fn run<T, F>(&self, f: F) -> impl Future<Output = Result<T, Cancelled>>
    where
        T: Send + 'static, // send-bound-exception: amendment-C5
        F: FnOnce() -> T + Send + 'static; // send-bound-exception: amendment-C5
}

/// Пул фоновых потоков, на котором должна была выполниться `Blocking::run`,
/// исчез раньше, чем задача успела начать выполняться — например, рантайм
/// завершает работу, пока задача ещё стоит в очереди. Без payload: это не
/// ошибка `f` (`f` не запускалась вовсе), а сигнал среды выполнения, что
/// результата не будет.
///
/// Паника `f`, для контраста, НЕ становится `Cancelled` — она перевызывается
/// как паника у реализации `Blocking`, см. doc-комментарий трейта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("blocking task pool went away before the work started")
    }
}

impl std::error::Error for Cancelled {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_opts_default_is_conservative() {
        // Все ШЕСТЬ полей, не четыре: ручной `Default`, выставляющий
        // `send_buffer_size`/`recv_buffer_size` в `Some(1 << 20)`, проходил
        // бы этот тест незамеченным, пока проверялись только остальные
        // четыре (carried finding, review Task 1). Сегодня `#[derive(Default)]`
        // даёт `None` по построению, но имя теста обещает весь struct — тест
        // должен проверять весь struct.
        let o = TcpOpts::default();
        assert!(!o.nodelay, "nodelay включает пользователь, не мы");
        assert!(o.keepalive.is_none());
        assert!(o.local_address.is_none());
        assert!(o.send_buffer_size.is_none());
        assert!(o.recv_buffer_size.is_none());
        assert!(!o.reuse_address);
    }

    #[test]
    fn spawn_is_generic_over_the_future_not_boxed() {
        // Форма скопирована у hyper::rt::Executor: генерик по F, ноль бондов
        // в объявлении. Send добавляет impl, а не трейт.
        struct Immediate;
        impl<F: std::future::Future<Output = ()>> Spawn<F> for Immediate {
            fn spawn(&self, f: F) {
                futures_executor::block_on(f)
            }
        }
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let d = done.clone();
        // !Send future — трейт это допускает.
        Immediate.spawn(async move { d.set(true) });
        assert!(done.get());
    }
}
