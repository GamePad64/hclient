//! Реализация способностей `http-ng-rt` поверх tokio.
#![forbid(unsafe_code)]

mod io;

pub use io::TokioIo;

use http_ng_rt::{Blocking, Cancelled, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, Timer};
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

/// ZST: tokio-хендл берётся из окружающего рантайма, как это делает reqwest.
/// Вне рантайма `spawn`/`sleep` паникуют — задокументировано.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tokio;

impl Timer for Tokio {
    type Instant = tokio::time::Instant;
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> {
        tokio::time::sleep(d)
    }
    fn now(&self) -> Self::Instant {
        tokio::time::Instant::now()
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        tokio::time::Instant::now().saturating_duration_since(earlier)
    }
}

impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for Tokio {
    fn spawn(&self, f: F) {
        tokio::spawn(f);
    }
}

impl Blocking for Tokio {
    /// `tokio::task::spawn_blocking` возвращает `JoinError` в двух разных
    /// случаях, и `caps::Blocking`'s контракт (fix round 1, координатор)
    /// обязывает не путать их: замыкание паникнуло, ИЛИ пул фоновых потоков
    /// исчез раньше, чем задача успела начать выполняться (не гипотетически
    /// — происходит, когда `spawn_blocking`-задача всё ещё стоит в очереди
    /// пула, а рантайм начинает shutdown). Первое перевызывается как паника
    /// (`resume_unwind`, оригинальный payload доходит до вызывающего кода
    /// как есть); второе — типизированная `Cancelled`, не паника: это не
    /// баг вызывающего кода, а обычное событие жизненного цикла рантайма.
    ///
    /// `classify` вынесена отдельной функцией и покрыта модульным тестом на
    /// РЕАЛЬНОМ `JoinError` — см. `tests::classify_reports_cancelled_for_a_join_error_that_is_not_a_panic`
    /// и его комментарий про то, почему `JoinError` там получен через
    /// `AbortHandle::abort()`, а не через гонку с shutdown целого рантайма.
    async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
        &self,
        f: F,
    ) -> Result<T, Cancelled> {
        classify(tokio::task::spawn_blocking(f).await)
    }
}

fn classify<T>(r: Result<T, tokio::task::JoinError>) -> Result<T, Cancelled> {
    match r {
        Ok(v) => Ok(v),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(_) => Err(Cancelled),
    }
}

impl TcpConnect for Tokio {
    type Stream = TokioIo;

    async fn connect(&self, addr: SocketAddr, opts: &TcpOpts) -> std::io::Result<TokioIo> {
        // Опции применяются на `socket2::Socket` **один раз**, а рантайм
        // усыновляет готовый дескриптор. Это и есть шов `TcpAdoptStd`:
        // без него каждый рантайм-крейт переписывал бы эту простыню заново.
        let sock = build_socket(addr, opts)?;
        sock.set_nonblocking(true)?;
        let std_stream: std::net::TcpStream = sock.into();
        let tcp = tokio::net::TcpSocket::from_std_stream(std_stream)
            .connect(addr)
            .await?;
        Ok(TokioIo::new(tcp))
    }
}

impl TcpAdoptStd for Tokio {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<TokioIo> {
        std.set_nonblocking(true)?;
        Ok(TokioIo::new(tokio::net::TcpStream::from_std(std)?))
    }
}

/// Весь `TcpOpts`-лист применяется здесь, на `socket2::Socket`, ДО того как
/// дескриптор вообще передаётся tokio — ровно то, что обещает doc-комментарий
/// на `TcpConnect::connect`: рантайм только усыновляет готовый сокет.
///
/// Fix round 1 (координатор): было — `nodelay`/`keepalive` применялись
/// отдельным шагом ПОСЛЕ `connect()`, на уже tokio-обёрнутом `TcpStream`
/// (`apply_post_connect`), тогда как этот комментарий уже тогда обещал "один
/// раз, на `socket2::Socket`". Не баг (`TCP_NODELAY`/`SO_KEEPALIVE`
/// одинаково действуют что до, что после `connect()`), но расхождение между
/// текстом и кодом — а Task 4 (`http-ng-rt-smol`) собирался бы копировать
/// именно этот файл. И `nodelay`, и `keepalive` у `socket2::Socket` можно
/// выставить до `connect()`; исключений, которые пришлось бы оставить
/// пост-коннекту, не нашлось — весь лист теперь в одном месте.
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
    use http_ng_rt::{Blocking, Cancelled, TcpConnect, TcpOpts, Timer};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[tokio::test]
    async fn timer_sleeps_and_measures() {
        let t = Tokio;
        let start = t.now();
        t.sleep(Duration::from_millis(20)).await;
        assert!(t.elapsed_since(start) >= Duration::from_millis(20));
    }

    #[tokio::test]
    async fn blocking_runs_off_the_reactor() {
        let out = Tokio.run(|| 6 * 7).await;
        assert_eq!(out, Ok(42));
    }

    #[tokio::test]
    #[should_panic(expected = "boom")]
    async fn blocking_propagates_the_original_panic_payload() {
        // Проверяет `resume_unwind`, а не текст `.expect(...)`: если бы
        // `run` просто делал `.expect("blocking task panicked")`, сообщение
        // паники здесь было бы этой строкой, а не `"boom"` — оригинальный
        // payload замыкания терялся бы.
        let _ = Tokio.run(|| panic!("boom")).await;
    }

    #[test]
    fn classify_reports_cancelled_for_a_join_error_that_is_not_a_panic() {
        // `Tokio::run`'s собственный `spawn_blocking`-хендл приватный — извне
        // нет крючка, чтобы вызвать `.abort()` именно на нём. Вместо этого
        // тест строит РЕАЛЬНЫЙ `tokio::task::JoinError` с `is_cancelled() ==
        // true, is_panic() == false` — ровно ту форму, которую `classify`
        // обязана превращать в `Cancelled` — тем же способом, которым его
        // производит сам tokio: `spawn_blocking`-задача, всё ещё стоящая в
        // очереди пула, отменяется до того, как рабочий поток успевает её
        // забрать.
        //
        // Гонка с НАСТОЯЩИМ shutdown рантайма была опробована первой и
        // отвергнута: с `max_blocking_threads(1)`, занятым единственным
        // потоком, второй `spawn_blocking` ставится в очередь, наблюдающая
        // задача (`tokio::spawn(async { Tokio.run(f).await })`) ждёт его, и
        // `Runtime::shutdown_timeout` вызывается сразу же — но саму
        // наблюдающую задачу shutdown стабильно убивал раньше, чем она
        // успевала отчитаться о результате, ДАЖЕ в прогонах, где блокирующее
        // замыкание уже успело выполниться (`BLOCKING_RAN=true`, но канал
        // всё равно `Disconnected`). Подтверждено эмпирически (5/5 прогонов)
        // одноразовым пробником на tokio 1.53.1 вне этого крейта — гонка
        // с shutdown целого рантайма ненадёжна как основа для теста.
        // `AbortHandle::abort()` на ещё не забранной из очереди задаче —
        // детерминированный, документированный способ получить ту же форму
        // `JoinError` без этой гонки (тоже проверено эмпирически, 5/5).
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            // Занимаем единственный блокирующий поток, чтобы второй
            // `spawn_blocking` гарантированно встал в очередь, а не начал
            // выполняться немедленно.
            let (started_tx, started_rx) = mpsc::channel::<()>();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let occupier = tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            started_rx.recv().unwrap();

            let ran = std::sync::Arc::new(AtomicBool::new(false));
            let ran_inner = ran.clone();
            let handle = tokio::task::spawn_blocking(move || {
                ran_inner.store(true, Ordering::SeqCst);
            });
            // Дать планировщику время реально протолкнуть задачу в очередь
            // пула (в этой версии это происходит синхронно внутри
            // `spawn_blocking`, но щедрый запас не вредит).
            tokio::time::sleep(Duration::from_millis(20)).await;

            handle.abort();
            // Освобождаем "оккупанта", чтобы рабочий поток вообще дошёл до
            // очереди и обработал отменённую задачу.
            let _ = release_tx.send(());
            let join_err = handle
                .await
                .expect_err("aborted-до-запуска задача обязана вернуть JoinError");

            assert!(
                !ran.load(Ordering::SeqCst),
                "замыкание не должно было выполниться"
            );
            assert!(join_err.is_cancelled());
            assert!(!join_err.is_panic());

            assert_eq!(classify::<()>(Err(join_err)), Err(Cancelled));

            let _ = occupier.await;
        });
    }

    #[tokio::test]
    async fn connects_to_a_local_listener_with_options() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });

        let opts = TcpOpts {
            nodelay: true,
            ..Default::default()
        };
        let s = Tokio.connect(addr, &opts).await.expect("connect");
        // Не только "соединение состоялось" (это прошло бы даже если
        // `build_socket` тихо игнорировал `opts`), а что `nodelay: true`
        // реально долетело до сокета: читаем опцию обратно с самого
        // `TcpStream`, а не полагаемся на факт вызова.
        assert!(
            s.get_ref().nodelay().expect("nodelay query"),
            "TcpOpts::nodelay не применилась к соединённому сокету"
        );
    }

    #[tokio::test]
    async fn connects_with_keepalive_enabled() {
        // Тот же принцип, что и у nodelay-теста выше, для второй опции,
        // переехавшей в `build_socket` в этом раунде правок: `keepalive`
        // читается обратно, а не только проверяется, что `connect()`
        // вернул `Ok`. `tokio::net::TcpStream` не даёт геттер для
        // `SO_KEEPALIVE` напрямую — используем `socket2::SockRef`, как и
        // `apply_post_connect` (в прежней версии этого файла) использовал
        // его же, только для записи, а не для чтения.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });

        let opts = TcpOpts {
            keepalive: Some(Duration::from_secs(30)),
            ..Default::default()
        };
        let s = Tokio.connect(addr, &opts).await.expect("connect");
        let enabled = socket2::SockRef::from(s.get_ref())
            .keepalive()
            .expect("keepalive query");
        assert!(
            enabled,
            "TcpOpts::keepalive не применилась к соединённому сокету"
        );
    }
}
