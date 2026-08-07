//! Реализация способностей `http-ng-rt` поверх smol.
//!
//! **Никакого `async-compat`.** Он поднимает второй рантайм в процессе, если
//! tokio-контекст не найден, — то есть скрывает ровно ту проблему, которую эта
//! вертикаль должна выявить.
#![forbid(unsafe_code)]

use http_ng_rt::{Blocking, Cancelled, FuturesIo, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, Timer};
use std::future::Future;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default)]
pub struct Smol;

impl Timer for Smol {
    type Instant = Instant;
    // `async fn`, не `fn sleep(...) -> impl Future<Output = ()> { async move
    // { ... } }`: та форма триггерит `clippy::manual_async_fn`, включённый
    // по умолчанию под `-D warnings` этого workspace.
    async fn sleep(&self, d: Duration) {
        async_io::Timer::after(d).await;
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn elapsed_since(&self, earlier: Instant) -> Duration {
        Instant::now().saturating_duration_since(earlier)
    }
}

impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for Smol {
    fn spawn(&self, f: F) {
        // `detach` намеренно: время жизни задачи привязано к соединению,
        // а не к вызывающему.
        smol_spawn(f);
    }
}

fn smol_spawn<F: Future<Output = ()> + Send + 'static>(f: F) {
    static EXEC: std::sync::OnceLock<async_executor::Executor<'static>> =
        std::sync::OnceLock::new();
    let ex = EXEC.get_or_init(|| {
        let ex = async_executor::Executor::new();
        std::thread::Builder::new()
            .name("http-ng-smol".into())
            .spawn(|| {
                futures_lite::future::block_on(
                    EXEC.get()
                        .expect("initialised")
                        .run(std::future::pending::<()>()),
                )
            })
            .expect("spawn executor thread");
        ex
    });
    ex.spawn(f).detach();
}

impl Blocking for Smol {
    /// `blocking::unblock`'s `Task<T>` — это `Future<Output = T>`: ни
    /// `Result`, ни аналога `JoinError` вообще нет, потому что пул фоновых
    /// потоков `blocking` (`blocking::unblock`) — лениво инициализируемый
    /// процесс-глобальный `static`, без жизненного цикла остановки,
    /// привязанного к какому-либо конкретному экзекьютору или к конкретному
    /// значению `Smol`. У этого пула нет события "ушёл, пока задача ещё
    /// стояла в очереди" — в отличие от `tokio::task::spawn_blocking`,
    /// который может гоняться с `Runtime::shutdown_timeout` целого рантайма.
    /// `Cancelled` для этого бэкенда структурно недостижим, а не просто
    /// непроверен: тут действительно неоткуда взяться отказу такой формы.
    /// Всегда возвращать `Ok(..)` здесь — честное отражение этого факта, а
    /// не подлог: трейт обещает "ЕСЛИ происходит отказ именно такой формы —
    /// он типизирован", а не "каждый бэкенд обязан уметь его произвести".
    ///
    /// Панику `f` реализации тоже не нужно как-то специально обрабатывать:
    /// `blocking::unblock` строит свою задачу через
    /// `async_task::Builder::new().propagate_panic(true)`, а `Task::poll`
    /// самого `async-task` перевызывает распространённую панику через
    /// `std::panic::resume_unwind` с оригинальным payload — тем же самым
    /// механизмом и с той же гарантией "оригинальный payload, без
    /// stringify", которую `http-ng-rt-tokio`'s `classify()` собирает
    /// вручную. Простой `.await` над `Task<T>` уже делает то, что нужно.
    async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
        &self,
        f: F,
    ) -> Result<T, Cancelled> {
        Ok(blocking::unblock(f).await)
    }
}

impl TcpConnect for Smol {
    type Stream = FuturesIo<async_net::TcpStream>;

    async fn connect(&self, addr: SocketAddr, opts: &TcpOpts) -> std::io::Result<Self::Stream> {
        // Опции применяются здесь, на `socket2::Socket`, ДО того как
        // дескриптор вообще передаётся рантайму — тот же шов, что и в
        // `http-ng-rt-tokio::build_socket`, и намеренно тот же порядок
        // операций: рантайм только усыновляет уже настроенный сокет.
        let sock = build_socket(addr, opts)?;
        sock.set_nonblocking(true)?;
        begin_connect(&sock, addr)?;

        let std_stream: std::net::TcpStream = sock.into();
        // `async_io::Async::new` регистрирует дескриптор в реакторе smol.
        // Сокет уже неблокирующий и уже начал коннект (см. `begin_connect`
        // выше) — `Async::new`, а не `new_nonblocking`, потому что публичное
        // API `async-io` не экспортирует последний; повторная установка
        // неблокирующего режима внутри него безвредна (идемпотентна).
        let async_stream = async_io::Async::new(std_stream)?;
        // Сокет становится writable, когда неблокирующий `connect()`
        // завершается — успехом или ошибкой. Тот же приём, что и в
        // `async_io::Async::<TcpStream>::connect` (единственная причина,
        // почему это МОЖНО не изобретать заново, а списать: `async-io` сам
        // не даёт способа передать туда уже настроенный сокет — у него нет
        // перегрузки `connect`, принимающей `socket2::Socket`).
        async_stream.writable().await?;
        // `connect()` неблокирующего сокета не гарантирует, что "стал
        // writable" значит "подключился успешно" — ошибка тоже делает сокет
        // writable. `take_error` — единственный надёжный способ различить.
        if let Some(err) = async_stream.get_ref().take_error()? {
            return Err(err);
        }

        Ok(FuturesIo::new(async_net::TcpStream::from(async_stream)))
    }
}

impl TcpAdoptStd for Smol {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream> {
        std.set_nonblocking(true)?;
        Ok(FuturesIo::new(async_net::TcpStream::try_from(std)?))
    }
}

/// Инициирует неблокирующий `connect(2)` на уже настроенном сокете и
/// классифицирует немедленный результат: `Ok(())` (редко, но случается для
/// localhost) — коннект уже завершился; `WouldBlock`/`EINPROGRESS` — коннект
/// в процессе, ожидается снаружи через `writable()`; что угодно ещё —
/// настоящая ошибка.
///
/// `EINPROGRESS` не наблюдаем через `std::io::ErrorKind`
/// (`ErrorKind::InProgress` до сих пор `#[unstable]`), поэтому сверяем
/// `raw_os_error()` напрямую — тем же способом, каким `socket2`'s
/// собственный `Socket::connect_timeout` решает ровно эту же проблему.
fn begin_connect(sock: &socket2::Socket, addr: SocketAddr) -> std::io::Result<()> {
    match sock.connect(&addr.into()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(libc::EINPROGRESS) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Идентична `http_ng_rt_tokio::build_socket` по списку и порядку опций —
/// намеренно: обе функции реализуют один и тот же контракт `TcpOpts`, и
/// расхождение между ними означало бы, что один из двух рантаймов лжёт о
/// какой-то опции. Единственное отличие — тип, в который в итоге
/// заворачивается сокет.
fn build_socket(addr: SocketAddr, opts: &TcpOpts) -> std::io::Result<socket2::Socket> {
    let domain = socket2::Domain::for_address(addr);
    let sock = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
    if opts.reuse_address {
        sock.set_reuse_address(true)?;
    }
    if let Some(size) = opts.send_buffer_size {
        sock.set_send_buffer_size(size)?;
    }
    if let Some(size) = opts.recv_buffer_size {
        sock.set_recv_buffer_size(size)?;
    }
    if let Some(ip) = opts.local_address {
        sock.bind(&SocketAddr::new(ip, 0).into())?;
    }
    if opts.nodelay {
        sock.set_tcp_nodelay(true)?;
    }
    if let Some(d) = opts.keepalive {
        sock.set_tcp_keepalive(&socket2::TcpKeepalive::new().with_time(d))?;
    }
    Ok(sock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_rt::{Blocking, TcpConnect, TcpOpts, Timer};

    #[test]
    fn timer_sleeps_and_measures() {
        futures_executor::block_on(async {
            let t = Smol;
            let start = t.now();
            t.sleep(Duration::from_millis(20)).await;
            assert!(t.elapsed_since(start) >= Duration::from_millis(20));
        });
    }

    #[test]
    fn blocking_runs_off_the_reactor_and_returns_ok() {
        let out = futures_executor::block_on(Smol.run(|| 6 * 7));
        assert_eq!(out, Ok(42));
    }

    #[test]
    #[should_panic(expected = "boom-from-smol")]
    fn blocking_propagates_the_original_panic_payload() {
        // `blocking::unblock`'s `propagate_panic(true)` даёт это бесплатно,
        // без какого-либо аналога `classify()` в этой реализации.
        let _ = futures_executor::block_on(Smol.run(|| panic!("boom-from-smol")));
    }

    #[test]
    fn connects_to_a_local_listener_with_nodelay() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });
        futures_executor::block_on(async {
            let s = Smol
                .connect(
                    addr,
                    &TcpOpts {
                        nodelay: true,
                        ..Default::default()
                    },
                )
                .await
                .expect("connect");
            // Не только "соединение состоялось" (это прошло бы даже если
            // `build_socket` тихо игнорировал `opts`), а что `nodelay: true`
            // реально долетело до сокета: читаем опцию обратно, а не
            // полагаемся на факт вызова.
            assert!(
                s.get_ref().nodelay().expect("nodelay query"),
                "TcpOpts::nodelay не применилась к соединённому сокету"
            );
        });
    }

    #[test]
    fn connects_with_keepalive_enabled() {
        // Тот же принцип, что и у nodelay-теста выше, для второй опции,
        // применяемой в `build_socket` до `connect()`.
        // `async_net::TcpStream` не даёт геттер для `SO_KEEPALIVE` напрямую —
        // используем `socket2::SockRef`, как и `http-ng-rt-tokio`'s
        // одноимённый тест.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });
        futures_executor::block_on(async {
            let s = Smol
                .connect(
                    addr,
                    &TcpOpts {
                        keepalive: Some(Duration::from_secs(30)),
                        ..Default::default()
                    },
                )
                .await
                .expect("connect");
            let enabled = socket2::SockRef::from(s.get_ref())
                .keepalive()
                .expect("keepalive query");
            assert!(
                enabled,
                "TcpOpts::keepalive не применилась к соединённому сокету"
            );
        });
    }
}
