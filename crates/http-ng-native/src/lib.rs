//! Native-транспорт http-ng: TCP + TLS + HTTP/1.1 поверх hyper.
//!
//! Этот крейт собирает воедино рантайм ([`http_ng_rt`]), DNS ([`http_ng_dns`])
//! и TLS ([`http_ng_tls`]) поверх `hyper`. Task 10 заложила адаптер тела
//! запроса ([`body`], `pub(crate)`); Task 11 добавила коннектор
//! ([`connect`], тоже `pub(crate)`); Task 12 добавляет HTTP/1-драйвер
//! ([`h1`], `pub(crate)`) — первый настоящий потребитель обоих:
//! `h1::exchange` доводит запрос до ответа, поллируя `hyper::client::conn::
//! http1::Connection` вручную, рядом с чтением тела, без единого `spawn`
//! (сам `Transport` появится в Task 13). Крейт по-прежнему не экспортирует
//! ничего публично, кроме тестового хелпера [`testing`].
#![forbid(unsafe_code)]

mod body;
mod connect;
mod h1;

/// Только для интеграционных тестов этого крейта: `pub`, а не `pub(crate)`,
/// потому что `tests/*.rs` компилируются как отдельный внешний крейт и не
/// видят `pub(crate)`-элементы вроде `connect::race_connect`/`h1::exchange`
/// напрямую. `#[doc(hidden)]` — это не часть публичного API крейта, а щель,
/// специально проделанная для интеграционных тестов задачи (см.
/// `tests/connect.rs`, `tests/dual_runtime.rs`, `tests/h1.rs`); Task 13 не
/// обязана и не должна на неё полагаться.
#[doc(hidden)]
pub mod testing {
    /// Гоняет Happy Eyeballs по готовому списку адресов, минуя DNS —
    /// обёртка над `connect::race_connect` с дефолтным `HeConfig` и
    /// `TcpOpts`, ровно то, что нужно тесту, который контролирует только
    /// список адресов и порт.
    pub async fn connect_for_test<R>(
        rt: &R,
        addrs: &[std::net::IpAddr],
        port: u16,
    ) -> Result<R::Stream, http_ng_core::Error>
    where
        R: http_ng_rt::TcpConnect + http_ng_rt::Timer,
    {
        let (v6, v4): (Vec<_>, Vec<_>) = addrs.iter().copied().partition(|a| a.is_ipv6());
        crate::connect::race_connect(
            rt,
            v6,
            v4,
            port,
            &http_ng_rt::TcpOpts::default(),
            http_ng_proto::happy_eyeballs::HeConfig::default(),
        )
        .await
    }

    pub use crate::body::OutgoingBody;
    pub use crate::h1::NativeBody;

    /// Тело пустого запроса — то, что нужно любому тесту `h1::exchange`,
    /// которому нечего слать (GET без тела).
    pub fn empty_body() -> crate::body::OutgoingBody {
        crate::body::OutgoingBody::from_request_body(http_ng_core::RequestBody::Empty)
    }

    /// `std::net::TcpStream` как `hyper::rt` IO для тестов на голом
    /// executor'е, где реактора нет вовсе — `works_on_a_bare_futures_
    /// executor_with_no_spawn` в `tests/h1.rs` нарочно не тянет ни `tokio`,
    /// ни `smol`.
    ///
    /// # Почему НЕ буквально блокирующий сокет, вопреки названию
    ///
    /// Первая версия этого хелпера честно блокировала внутри `poll_read`
    /// (`std::io::Read::read` на сокете в блокирующем режиме,
    /// `Poll::Ready` без исключений) — и вешала ЛЮБОЙ обмен, а не только
    /// граничные случаи: `hyper::proto::h1::dispatch::Dispatcher::
    /// poll_loop` (hyper 1.11.0) на КАЖДОЙ итерации сперва вызывает
    /// `let _ = self.poll_read(cx)?;` и только потом `self.poll_write(cx)`
    /// — результат чтения отбрасывается через `let _ =`, что при
    /// неблокирующем IO означает "чтения ещё нет — не страшно, попробуем
    /// писать в этой же итерации". Но если `poll_read` сам блокирует
    /// поток до появления байт, до `poll_write` дело никогда не доходит:
    /// клиент ждёт ответ, которого не будет, пока сервер ждёт запрос,
    /// который не отправлен, потому что клиент застрял в чтении. Поймано
    /// не чтением кода, а тем, что `works_on_a_bare_futures_executor_with_
    /// no_spawn` реально зависал (см. отчёт Task 12) — до первого байта
    /// запроса дело не доходило вообще.
    ///
    /// Значит сокет обязан быть неблокирующим, а `poll_read`/`poll_write`
    /// — при `WouldBlock` возвращать `Pending`, а не ждать. Но реактора,
    /// который разбудит нас, когда сокет станет готов, тоже нет — так что
    /// `Pending` здесь сопровождается немедленным `cx.waker().wake_by_ref()`:
    /// это busy-spin (`futures_executor::block_on` тут же поллит снова,
    /// вместо настоящего ожидания готовности через ОС), а не притворство,
    /// что реактора не нужно. Годится это только для теста на локальном
    /// сокете с ответом за доли миллисекунды — не паттерн для
    /// переиспользования где-либо за пределами этого хелпера.
    pub fn blocking_io(s: std::net::TcpStream) -> BlockingIo {
        s.set_nonblocking(true)
            .expect("свежий, только что подключённый TcpStream должен принять set_nonblocking");
        BlockingIo(s)
    }

    /// См. [`blocking_io`].
    #[derive(Debug)]
    pub struct BlockingIo(std::net::TcpStream);

    /// `Pending` при `WouldBlock`, но с немедленным `wake_by_ref` — см.
    /// doc-комментарий [`blocking_io`] про то, почему без реактора это
    /// единственный способ не потерять пробуждение.
    fn poll_would_block<T>(
        cx: &std::task::Context<'_>,
        r: std::io::Result<T>,
    ) -> std::task::Poll<std::io::Result<T>> {
        match r {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
            other => std::task::Poll::Ready(other),
        }
    }

    impl hyper::rt::Read for BlockingIo {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            // Без `unsafe`: читаем в стековый буфер, затем копируем через
            // `put_slice` — тот же путь, что и безопасный пример в
            // doc-комментарии `hyper::rt::Read`.
            let mut scratch = [0u8; 8192];
            let want = buf.remaining().min(scratch.len());
            match poll_would_block(
                cx,
                std::io::Read::read(&mut self.get_mut().0, &mut scratch[..want]),
            ) {
                std::task::Poll::Ready(Ok(n)) => {
                    buf.put_slice(&scratch[..n]);
                    std::task::Poll::Ready(Ok(()))
                }
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        }
    }

    impl hyper::rt::Write for BlockingIo {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            poll_would_block(cx, std::io::Write::write(&mut self.get_mut().0, buf))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            // `TcpStream::flush` — не-op (нет буферизации в userspace),
            // никогда не блокирует и не возвращает `WouldBlock`.
            std::task::Poll::Ready(std::io::Write::flush(&mut self.get_mut().0))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(self.get_mut().0.shutdown(std::net::Shutdown::Both))
        }
    }

    pub async fn exchange_for_test<I>(
        io: I,
        req: http::Request<crate::body::OutgoingBody>,
    ) -> Result<http::Response<crate::h1::NativeBody>, http_ng_core::Error>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        crate::h1::exchange(io, req).await
    }

    pub async fn collect(b: crate::h1::NativeBody) -> Result<bytes::Bytes, http_ng_core::Error> {
        use http_body_util::BodyExt;
        Ok(b.collect().await?.to_bytes())
    }
}
