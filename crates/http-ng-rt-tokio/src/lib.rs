//! Реализация способностей `http-ng-rt` поверх tokio.
#![forbid(unsafe_code)]

mod io;

pub use io::TokioIo;

use http_ng_rt::{Blocking, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, Timer};
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
    /// случаях: замыкание паникнуло, ИЛИ рантайм завершает работу и задача
    /// была отменена, так и не выполнившись (не гипотетически — это
    /// происходит, когда `spawn_blocking`-задача всё ещё стоит в очереди
    /// пула, когда рантайм начинает shutdown). Трейт `Blocking::run`
    /// возвращает `impl Future<Output = T>` без канала для ошибки (форма
    /// зафиксирована Task 1 и разделяется Task 7, который использует
    /// `Blocking` для `getaddrinfo`), так что оба случая здесь становятся
    /// паникой в библиотечном коде — задокументированное, а не случайное
    /// поведение.
    ///
    /// Внутри эти два случая всё же не смешиваются: настоящая паника
    /// замыкания перевызывается через `resume_unwind`, чтобы сообщение и
    /// payload паники дошли до вызывающего кода как есть, а не потерялись
    /// за текстом `.expect(...)`; отмена из-за shutdown паникует отдельным,
    /// честным сообщением. См. также `task-3-report.md` — вопрос, стоит ли
    /// вообще паниковать здесь, а не менять форму трейта, передан
    /// координатору вертикали, а не решён в одиночку в этом крейте.
    async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(&self, f: F) -> T {
        match tokio::task::spawn_blocking(f).await {
            Ok(v) => v,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => {
                panic!("blocking task cancelled: tokio runtime is shutting down before it ran")
            }
        }
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
        apply_post_connect(&tcp, opts)?;
        Ok(TokioIo::new(tcp))
    }
}

impl TcpAdoptStd for Tokio {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<TokioIo> {
        std.set_nonblocking(true)?;
        Ok(TokioIo::new(tokio::net::TcpStream::from_std(std)?))
    }
}

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
    Ok(sock)
}

fn apply_post_connect(tcp: &tokio::net::TcpStream, opts: &TcpOpts) -> std::io::Result<()> {
    if opts.nodelay {
        tcp.set_nodelay(true)?;
    }
    if let Some(d) = opts.keepalive {
        let sock = socket2::SockRef::from(tcp);
        sock.set_tcp_keepalive(&socket2::TcpKeepalive::new().with_time(d))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_rt::{Blocking, TcpConnect, TcpOpts, Timer};
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
        assert_eq!(out, 42);
    }

    #[tokio::test]
    #[should_panic(expected = "boom")]
    async fn blocking_propagates_the_original_panic_payload() {
        // Проверяет `resume_unwind`, а не текст `.expect(...)`: если бы
        // `run` просто делал `.expect("blocking task panicked")`, сообщение
        // паники здесь было бы этой строкой, а не `"boom"` — оригинальный
        // payload замыкания терялся бы.
        Tokio.run(|| panic!("boom")).await;
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
        // `build_socket`/`apply_post_connect` тихо игнорировали `opts`), а
        // что `nodelay: true` реально долетело до сокета: читаем опцию
        // обратно с самого `TcpStream`, а не полагаемся на факт вызова.
        assert!(
            s.get_ref().nodelay().expect("nodelay query"),
            "TcpOpts::nodelay не применилась к соединённому сокету"
        );
    }
}
