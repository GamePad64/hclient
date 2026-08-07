//! Native-транспорт http-ng: TCP + TLS + HTTP/1.1 поверх hyper.
//!
//! Этот крейт собирает воедино рантайм ([`http_ng_rt`]), DNS ([`http_ng_dns`])
//! и TLS ([`http_ng_tls`]) поверх `hyper`. Task 10 заложила адаптер тела
//! запроса ([`body`], `pub(crate)`); Task 11 добавила коннектор
//! ([`connect`], тоже `pub(crate)`); Task 12 добавила HTTP/1-драйвер
//! ([`h1`], `pub(crate)`); Task 13 собирает всё это в [`Native`] —
//! единственный публичный тип крейта, реализующий `http_ng_core::
//! unversioned::Transport`.
//!
//! # `Native::execute` не резолвит DNS сам
//!
//! Черновик этой задачи (`task-13-brief.md`) резолвил адреса вручную —
//! `.filter_map(|r| async { r.ok() })` над `Resolve::lookup_ipv4`/
//! `lookup_ipv6`, отбрасывая ЛЮБУЮ ошибку резолвера (в том числе
//! `ErrorKind::Cancelled` — обычное завершение фонового пула, Task 7) и
//! синтезируя единый `ErrorKind::Resolve`, если оба стрима оказались пусты.
//! Review Task 7 уже нашла ровно этот дефект на этом же месте: смешение
//! «резолвер отказал» и «рантайм завершает работу» ломает circuit breaker,
//! ключующийся на `Resolve` — он ошибочно занёс бы живой хост в чёрный
//! список во время обычного shutdown.
//!
//! Task 11 решила это один раз и структурно в `connect::drive`/
//! `ResolveErrors::distinguishing_error` (см. doc-комментарий `connect.rs`):
//! отличающийся от синтетического `Resolve` `kind()` проверяется ДО обеих
//! веток отказа, так что отбрасывание становится структурно недостижимым, а
//! не просто обработанным для одного найденного случая. `execute` ниже
//! поэтому не резолвит и не гоняет Happy Eyeballs сам — он вызывает
//! `connect::connect`, ту же точку входа, что уже покрыта юнит-тестами
//! `connect.rs`; `crates/http-ng-native/tests/transport.rs`'s
//! `resolver_cancelled_error_reaches_the_caller_through_execute_not_flattened`
//! проверяет, что это свойство доживает до всего пути `Client::execute`, не
//! только до `connect::drive` самого по себе.
#![forbid(unsafe_code)]

mod body;
mod connect;
mod h1;

use http_ng_core::unversioned::Transport;
use http_ng_core::{
    Capabilities, Error, RedirectSupport, RequestBody, TimeoutSupport, TlsSupport, UpgradeSupport,
};
use http_ng_dns::Resolve;
use http_ng_rt::{TcpConnect, TcpOpts, Timer};
use http_ng_tls::TlsConnect;

/// Транспорт http-ng поверх реального TCP/TLS/HTTP1: подключает вместе
/// рантайм `R` ([`http_ng_rt::TcpConnect`] + [`http_ng_rt::Timer`]), TLS `T`
/// ([`http_ng_tls::TlsConnect`]) и резолвер `D` ([`http_ng_dns::Resolve`]).
///
/// v0.1: одно соединение на запрос (пула нет), тело запроса буферизуется
/// целиком (`streaming_request_body: false`), HTTP/1.1 only, без upgrade —
/// см. [`Native::new`] и [`Capabilities`], которые эти ограничения
/// объявляют честно, а не молчат о них.
#[derive(Debug)]
pub struct Native<R, T, D> {
    rt: R,
    tls: T,
    dns: D,
    opts: TcpOpts,
    caps: Capabilities,
}

impl<R, T, D> Native<R, T, D> {
    pub fn new(rt: R, tls: T, dns: D) -> Self {
        let mut caps = Capabilities::none();
        // Честно про v0.1: пула соединений нет, стриминга тела запроса нет,
        // upgrade нет — остальные поля остаются на консервативной базе
        // `Capabilities::none()` (см. `tests/transport.rs`'s
        // `undeclared_capability_fields_match_their_conservative_defaults_today`).
        caps.streaming_request_body = false;
        caps.redirects = RedirectSupport::Configurable;
        caps.tls_config = TlsSupport::Full;
        caps.version_reported = true;
        caps.timeouts = TimeoutSupport {
            connect: true,
            // Ни пула, ни таймера ответа в v0.1 нет — заявлять эти фазы
            // значило бы способность, которая лжёт о своём состоянии.
            first_byte: false,
            between_bytes: false,
        };
        caps.upgrade = UpgradeSupport::None;
        Self {
            rt,
            tls,
            dns,
            opts: TcpOpts::default(),
            caps,
        }
    }

    /// Параметры сокета для КАЖДОЙ TCP-попытки этого транспорта (см.
    /// [`http_ng_rt::TcpOpts`]).
    pub fn tcp_opts(mut self, opts: TcpOpts) -> Self {
        self.opts = opts;
        self
    }
}

impl<R, T, D> Transport for Native<R, T, D>
where
    R: TcpConnect + Timer,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
{
    type Body = h1::NativeBody;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Error> {
        let (parts, body) = req.into_parts();

        // См. doc-комментарий модуля: `connect::connect` — не переиспользование
        // ради экономии кода, а единственный путь, на котором отличающийся
        // `ErrorKind` резолвера (в частности, `Cancelled`) структурно не может
        // быть отброшен. Она же резолвит схему (`http`/`https`, любая другая —
        // типизированный `ErrorKind::Unsupported`) и опционально проводит
        // TLS-хендшейк с ALPN `http/1.1` — единственным протоколом, который
        // умеет `h1::exchange`.
        let (conn, _tls_info) = connect::connect(
            &self.rt,
            &self.dns,
            &self.tls,
            &parts.uri,
            &self.opts,
            &[b"http/1.1"],
        )
        .await?;

        let outgoing = body::OutgoingBody::from_request_body(body);
        let mut req = http::Request::from_parts(parts, outgoing);
        // hyper h1 требует origin-form и заголовок `Host:` — `connect::connect`
        // уже успел проверить и хост, и схему, так что здесь их не перепроверяем.
        origin_form(&mut req);

        h1::exchange(conn, req).await
    }

    /// Тождество: `Self::Error` — уже `http_ng_core::Error`, и категория в
    /// ней проставлена там, где отказ произошёл (`Resolve`/`Connect`/
    /// `Unsupported` в `connect::connect`, `Tls` в `TlsConnect::connect`,
    /// `Body`/`Connect` в `h1::exchange`). Дефолт хука сделал бы ровно то же
    /// самое (он узнаёт нашу `Error` и пропускает её насквозь) — строка
    /// избыточна по поведению и нужна по смыслу: называет намерение там, где
    /// его читают, и переживёт возможное изменение дефолта. См.
    /// doc-комментарий `Transport::to_error` в `http-ng-core`.
    fn to_error(&self, e: Self::Error) -> Error {
        e
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// Переписывает URI запроса в origin-form (`hyper`'s h1-клиент требует
/// именно её, не absolute-form) и проставляет `Host:`, если вызывающая
/// сторона его не задала сама.
///
/// К моменту вызова `connect::connect` уже успешно отработал — а значит его
/// собственные проверки (`host()`, `wants_tls()`) уже прошли, так что
/// `req.uri()` гарантированно несёт хост и поддерживаемую (`http`/`https`)
/// схему; эта функция их не перепроверяет.
fn origin_form(req: &mut http::Request<body::OutgoingBody>) {
    let uri = req.uri().clone();
    let https = uri.scheme_str() == Some("https");
    let default_port = if https { 443 } else { 80 };
    let port = uri.port_u16().unwrap_or(default_port);
    let host = uri.host().unwrap_or_default();

    if !req.headers().contains_key(http::header::HOST) {
        let authority = if port == default_port {
            host.to_owned()
        } else {
            format!("{host}:{port}")
        };
        // Хост, доживший до `connect::connect` (прошедший DNS-резолюцию, а
        // для `https` — ещё и построение TLS SNI), на практике всегда
        // валиден как значение заголовка. Если всё же нет — запрос уйдёт без
        // `Host:`, и это не тихая потеря: ни один сервер, с которым говорит
        // этот крейт, не примет HTTP/1.1-запрос без `Host:`, так что отказ
        // будет немедленным и явным протокольным отказом, а не тихим no-op.
        if let Ok(v) = http::HeaderValue::from_str(&authority) {
            req.headers_mut().insert(http::header::HOST, v);
        }
    }
    let pq = uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_owned();
    if let Ok(u) = pq.parse::<http::Uri>() {
        *req.uri_mut() = u;
    }
}

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
