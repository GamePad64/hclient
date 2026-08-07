//! Коннектор: Happy Eyeballs (RFC 8305) по TCP, затем опциональный TLS с
//! ALPN.
//!
//! # Где здесь живёт "Resolution Delay"
//!
//! `http_ng_dns::Resolve` нарочно отдаёт `Stream`, а не `Future<Output =
//! Vec<_>>` — единственная причина в том, что RFC 8305 §3 требует начинать
//! попытки по IPv6, не дожидаясь ответа по IPv4, а `Scheduler` (Task 5)
//! умеет реагировать на это только если его кормят по мере поступления, а
//! не одним блоком после того, как резолвер закончил оба семейства. Если
//! бы этот файл сначала собирал оба стрима в `Vec`, а потом одним вызовом
//! отдавал их `Scheduler::offer_v6`/`offer_v4` с немедленным
//! `mark_v6_done`/`mark_v4_done`, "Resolution Delay" была бы мертва: между
//! `offer` и `mark_done` не проходило бы никакого времени, и `Scheduler`
//! никогда не оказался бы в состоянии "AAAA ещё не пришли, но резолвер не
//! закончил" — том самом состоянии, ради которого он вообще устроен как
//! автомат, а не как сортировка уже готового списка.
//!
//! `drive` ниже — единственное место, которое опрашивает `Scheduler`, и оно
//! кормит его РЕАЛЬНЫМИ стримами: `connect` передаёт туда
//! `dns.lookup_ipv6`/`lookup_ipv4` как есть, по одному элементу за раз, и
//! вызывает `mark_*_done` только когда стрим действительно закончился (`None`
//! от `poll_next`), а не заранее. `race_connect` — вторая, более простая
//! точка входа: ей нечего резолвить (адреса уже даны целиком), поэтому она
//! оборачивает их в `stream::iter` (стрим, который отдаёт всё на первом же
//! опросе и сразу заканчивается) и передаёт в тот же `drive`. Обе точки входа
//! проходят через один и тот же автомат не из экономии кода: это гарантирует,
//! что мутация в правилах интерливинга/паузы одинаково ловится тестами через
//! оба пути, а не только через один из них.
//!
//! # Зачем `race_connect` больше не строит `select_biased!` из брифа задачи
//!
//! Черновик задачи показывал `race_connect` с ровно двумя источниками
//! события в момент `HeAction::Wait`: попытки (`attempts.next()`) и таймер
//! (`rt.sleep(d)`). `select_biased!`/`select!` из `futures-util` это умеют
//! (FusedFuture на обоих плечах), но `drive` ниже кормится ЕЩЁ и двумя
//! DNS-стримами, а `select_biased!` не поддерживает условно исключаемые
//! плечи — плечо, которое обязано замолчать навсегда после того, как стрим
//! семейства закончился, синтаксисом макроса не выразить без отдельного
//! `enum`-обёртки на каждое плечо. `std::future::poll_fn` с explicit `if
//! !done { poll }` перед каждым источником решает то же самое прямым
//! Rust-кодом: источник, который сейчас нечего спрашивать (стрим уже
//! кончился, `attempts` пуст), просто не опрашивается в этом раунде — и не
//! нуждается в `Waker`, потому что у него объективно не может появиться
//! новое событие (см. комментарий на месте про `attempts.is_empty()`
//! ниже — тот же приём, что и в брифе, только обобщённый на четыре
//! источника вместо двух).
//!
//! # RFC 6724 (Destination Address Selection) НЕ реализован здесь — принятый, названный пробел
//!
//! `http_ng_dns::Resolve`'s doc-комментарий изначально называл эту задачу
//! местом, где адреса ОДНОГО семейства обязаны быть отсортированы по RFC 6724
//! §6 перед `offer_v4`/`offer_v6`. Проверка перед реализацией показала, что
//! это неисполнимо в заявленном виде: полная реализация (Rule 1–Rule 10 RFC
//! 6724 §6, часть из которых требует Source Address Selection — то есть
//! знания о таблице маршрутизации, которого ни один трейт этой вертикали не
//! предоставляет) — самостоятельная, большая задача, а не то, что уместно
//! додумать по ходу коннектора. Симулировать часть правил без остальных
//! значило бы утверждать частичную совместимость, которой на самом деле нет —
//! тот же принцип, что развёл `RedirectSupport::None`/`Transparent`
//! (`способность, которая лжёт о своём состоянии, хуже способности, которой
//! просто нет`, см. doc-комментарии `http_ng_dns::Resolve` и
//! `http_ng_tls::TlsInfo`). Поэтому: адреса каждого семейства идут в
//! `Scheduler::offer_v6`/`offer_v4` в том порядке, в котором их отдал
//! резолвер — `Scheduler` документирует, что сортировка не его забота,
//! `http_ng_dns::Resolve` (доработан вместе с этой находкой) теперь говорит то
//! же самое с другого конца шва, и этот файл её тоже не берёт на себя.
//!
//! Это больше не открытый вопрос: зафиксировано как явный пробел в §9 "Что
//! явно не делаем" `docs/superpowers/specs/2026-08-05-http-ng-design.md`, а
//! не недосмотр, который кто-то переоткроет в третий раз. Закрыть его
//! возможно, если появится отдельная способность Source Address Selection —
//! до тех пор ни `Resolve`, ни `Scheduler`, ни этот файл сортировкой не
//! занимаются. (Для системного резолвера конкретно ОС часто уже отдаёт
//! адреса в RFC 6724-порядке сама — см. `http-ng-dns-system` — но это
//! свойство конкретного бэкенда, не гарантия трейта.)
//!
//! # RFC 9460 SVCB/ECH тоже не подключены здесь
//!
//! `TlsRequest::ech` заведён в Task 8 заранее, а `Resolve::lookup_svcb`/
//! `SvcbEndpoint::ech_config_list` — в Task 6, но ни то, ни другое не входит
//! в Interfaces-блок этой задачи (`connect` там принимает `alpn: &[&[u8]]`,
//! но не список SVCB-точек и не ECH). `connect` ниже передаёт `ech: None` —
//! честно: он не запрашивает SVCB и не может предложить ECH-конфиг,
//! которого у него нет, а не притворяется, что запрашивал и не нашёл (то же
//! разграничение, что `supports_svcb()` уже проводит на уровне `Resolve`).
//!
//! # `connect` (и всё, что использует только оно) вне тестов пока не вызывается
//!
//! `race_connect` (и всё, что тянет за собой — `drive`, `AllAttemptsFailed`,
//! `ResolveErrors`, `build_scheduler`) уже используется по-настоящему, через
//! `crate::testing::connect_for_test`. Верхний, DNS-потребляющий `connect` —
//! нет: его настоящий вызывающий (HTTP/1-драйвер и `Transport`) приезжает в
//! Tasks 12–13, тем же порядком, что и `OutgoingBody`/`Inner` в `body.rs`
//! (Task 10) ждали коннектор этой самой задачи. `cfg_attr(not(test), ...)`,
//! а не голый `expect`, по той же причине, что там: в тестовой сборке
//! `#[cfg(test)] mod tests` ниже использует `connect`, `Conn`, `host`,
//! `port`, `wants_tls` по-настоящему, `dead_code` там не сработает, и
//! непарный `expect` сам обернулся бы предупреждением
//! (`unfulfilled_lint_expectations`).
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "connect/Conn/host/port/wants_tls используются HTTP/1-драйвером и Transport \
                  в Task 12–13; до тех пор — только тестами этого файла"
    )
)]
#![allow(clippy::too_many_arguments)]

use futures_util::Stream;
use futures_util::stream::{FuturesUnordered, StreamExt};
use http::Uri;
use http_ng_core::{Error, ErrorKind};
use http_ng_dns::{Resolve, ResolvedAddr};
use http_ng_proto::happy_eyeballs::{HeAction, HeConfig, Scheduler};
use http_ng_rt::{TcpConnect, TcpOpts, Timer};
use http_ng_tls::{TlsConnect, TlsInfo, TlsRequest};
use hyper::rt::{Read, ReadBufCursor, Write};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// Соединение: с TLS или без. Оба варианта — `hyper::rt` IO.
#[derive(Debug)]
pub(crate) enum Conn<P, T> {
    Plain(P),
    Tls(T),
}

impl<P: Read + Unpin, T: Read + Unpin> Read for Conn<P, T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_read(cx, buf),
            Conn::Tls(t) => Pin::new(t).poll_read(cx, buf),
        }
    }
}

impl<P: Write + Unpin, T: Write + Unpin> Write for Conn<P, T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_write(cx, b),
            Conn::Tls(t) => Pin::new(t).poll_write(cx, b),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_flush(cx),
            Conn::Tls(t) => Pin::new(t).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_shutdown(cx),
            Conn::Tls(t) => Pin::new(t).poll_shutdown(cx),
        }
    }
}

// --- Типизированные ошибки этого модуля --------------------------------
//
// "No silent no-ops": ни одно из мест ниже не схлопывает отказ в
// `AllAttemptsFailed`/`ErrorKind::Connect` молча — каждое различие
// (резолвер отказал / резолвер честно нашёл ноль адресов / TCP-попытки
// реально были и все отказали / конфиг Happy Eyeballs вышел за
// RFC-рекомендованный диапазон) остаётся видимым через отдельный тип и
// отдельный `ErrorKind`.

#[derive(Debug)]
pub(crate) struct AllAttemptsFailed(pub(crate) usize);
impl std::fmt::Display for AllAttemptsFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "all {} connection attempts failed", self.0)
    }
}
impl std::error::Error for AllAttemptsFailed {}

/// Ни одного адреса не пришло ни по одному семейству — и ЭТО РАЗЛИЧАЕТ,
/// была ли причина в том, что резолвер отказал, или в том, что он честно
/// отработал и нашёл ноль записей (например, `NXDOMAIN`). Схлопнуть оба
/// случая в `AllAttemptsFailed(0)` было бы ровно тем "resolver error
/// becomes 'no addresses'", против которого это поле существует: он бы
/// звучал как "ноль TCP-попыток отказали", хотя ни одной TCP-попытки не
/// было вовсе — не потому что не пытались, а потому что нечего было
/// пробовать.
#[derive(Debug, Default)]
struct ResolveErrors {
    v6: Option<Error>,
    v4: Option<Error>,
}

impl std::fmt::Display for ResolveErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.v6, &self.v4) {
            (Some(v6), Some(v4)) => {
                write!(f, "ipv6 lookup failed ({v6}); ipv4 lookup failed ({v4})")
            }
            (Some(v6), None) => {
                write!(
                    f,
                    "ipv6 lookup failed ({v6}); ipv4 lookup returned no addresses"
                )
            }
            (None, Some(v4)) => {
                write!(
                    f,
                    "ipv4 lookup failed ({v4}); ipv6 lookup returned no addresses"
                )
            }
            (None, None) => f.write_str("resolver returned no addresses for either address family"),
        }
    }
}

impl std::error::Error for ResolveErrors {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.v6
            .as_ref()
            .or(self.v4.as_ref())
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// `attempt_delay` запрошенного `HeConfig` вне рекомендованного RFC 8305
/// диапазона. `Scheduler::new` (Task 5) молча зажимает такое значение,
/// потому что его сигнатура зафиксирована интерфейсом задачи — `Self`, не
/// `Result`. Сигнатура ЭТОГО модуля ничем не зафиксирована, так что здесь
/// это типизированная ошибка, а не тот же тихий clamp двумя уровнями ниже.
#[derive(Debug)]
struct InvalidHeConfig {
    requested: Duration,
    effective: Duration,
}
impl std::fmt::Display for InvalidHeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "attempt_delay {:?} is outside the RFC 8305 recommended range and would be \
             silently clamped to {:?}; pass a value inside the range instead",
            self.requested, self.effective
        )
    }
}
impl std::error::Error for InvalidHeConfig {}

/// Строит [`Scheduler`], отклоняя `attempt_delay` вне диапазона как
/// типизированную ошибку — вместо того, чтобы принять тихий clamp
/// `Scheduler::new` как есть.
///
/// Обнаруживается без единого знания о границах `ATTEMPT_MIN`/`ATTEMPT_MAX`
/// (они приватны для `http_ng_proto::happy_eyeballs` и не должны
/// дублироваться здесь вторым источником истины): эффективное значение
/// вычитывается обратно через `Scheduler::config()` и сравнивается с
/// запрошенным — ровно тот механизм, который doc-комментарий
/// `Scheduler::new` называет напрямую ("эффективный конфиг всегда можно
/// сверить с запрошенным через `config()`"), просто применённый вызывающей
/// стороной, а не оставленный ей как "могли бы, но не обязаны".
fn build_scheduler(cfg: HeConfig) -> Result<Scheduler, Error> {
    let requested = cfg.attempt_delay;
    let sched = Scheduler::new(cfg);
    let effective = sched.config().attempt_delay;
    if effective != requested {
        return Err(Error::new(
            ErrorKind::Connect,
            InvalidHeConfig {
                requested,
                effective,
            },
        ));
    }
    Ok(sched)
}

#[derive(Debug)]
struct UriError;
impl std::fmt::Display for UriError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request URI has no host to connect to")
    }
}
impl std::error::Error for UriError {}

/// Хост из `uri`, независимо от схемы: URI без authority (например,
/// origin-form `/path`) отвергается здесь же, до того как вообще встаёт
/// вопрос "какая схема" — незачем спрашивать про TLS у URI, к которому
/// заведомо некуда подключаться.
fn host(uri: &Uri) -> Result<&str, Error> {
    uri.host()
        .ok_or_else(|| Error::new(ErrorKind::Connect, UriError))
}

/// Порт из `uri`, с подстановкой умолчания по УЖЕ проверенной схеме
/// (`use_tls` приходит от [`wants_tls`], которая одна отвечает за отказ на
/// неподдерживаемой схеме) — `https`→443, `http`→80. Ровно то же правило,
/// что `http_ng_proto::redirect::port_of` использует для той же цели: не
/// импортировано оттуда напрямую (та функция `fn`-приватна модулю
/// `redirect`), но обязано остаться тем же самым по факту, не только по
/// совпадению — расхождение здесь означало бы, что редирект на
/// `https://a:443/` и первоначальный коннект на тот же адрес видят разные
/// порты. Раз схема уже ограничена до `http`/`https` на входе, дефолтный
/// порт есть всегда — отдельной ошибки "нет порта" здесь больше не бывает.
fn port(uri: &Uri, use_tls: bool) -> u16 {
    uri.port_u16().unwrap_or(if use_tls { 443 } else { 80 })
}

#[derive(Debug)]
struct UnsupportedScheme(String);
impl std::fmt::Display for UnsupportedScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unsupported URI scheme: {:?}", self.0)
    }
}
impl std::error::Error for UnsupportedScheme {}

/// `true` — нужен TLS (`https`), `false` — обычный TCP (`http`). Любая
/// другая (или отсутствующая) схема — типизированная `ErrorKind::Unsupported`,
/// а не молчаливое обращение с ней как с `http`.
fn wants_tls(uri: &Uri) -> Result<bool, Error> {
    match uri.scheme_str() {
        Some("http") => Ok(false),
        Some("https") => Ok(true),
        other => Err(Error::new(
            ErrorKind::Unsupported,
            UnsupportedScheme(other.unwrap_or("").to_string()),
        )),
    }
}

/// Happy Eyeballs по RFC 8305, единый автомат для обеих точек входа этого
/// модуля (`connect` и `race_connect`) — см. doc-комментарий модуля про то,
/// почему им обеим нужен именно этот, а не раздельные циклы.
///
/// **Без `spawn`**: попытки живут в `FuturesUnordered`, не в задачах —
/// `spawn` потребовал бы `Send + 'static` и закрыл бы однопоточные рантаймы
/// (см. тесты `race_connect_never_requires_send_even_through_the_wait_path`
/// и `crates/http-ng-native/tests/dual_runtime.rs`, которые гоняют этот же
/// путь через `smol` без единого потока планировщика).
async fn drive<R, V6, V4>(
    rt: &R,
    mut sched: Scheduler,
    v6_stream: V6,
    v4_stream: V4,
    port: u16,
    opts: &TcpOpts,
) -> Result<R::Stream, Error>
where
    R: TcpConnect + Timer,
    V6: Stream<Item = Result<ResolvedAddr, Error>>,
    V4: Stream<Item = Result<ResolvedAddr, Error>>,
{
    /// Что стряслось первым, пока мы ждали (`HeAction::Wait`): пришёл
    /// элемент одного из DNS-стримов (или стрим закончился — `None`),
    /// завершилась одна из попыток соединения, или истекло время ожидания.
    enum Event<T> {
        V6(Option<Result<ResolvedAddr, Error>>),
        V4(Option<Result<ResolvedAddr, Error>>),
        Attempt(Option<std::io::Result<T>>),
        TimedOut,
    }

    let mut v6_stream = std::pin::pin!(v6_stream);
    let mut v4_stream = std::pin::pin!(v4_stream);
    let mut v6_done = false;
    let mut v4_done = false;
    let mut errs = ResolveErrors::default();

    let start = rt.now();
    let mut attempts = FuturesUnordered::new();
    let mut launched = 0usize;

    loop {
        let elapsed = rt.elapsed_since(start);
        match sched.poll(elapsed) {
            HeAction::Start(ip) => {
                launched += 1;
                attempts.push(rt.connect(SocketAddr::new(ip, port), opts));
            }
            HeAction::Wait(d) => {
                let sleep_fut = rt.sleep(d);
                let mut sleep_fut = std::pin::pin!(sleep_fut);

                let ev = std::future::poll_fn(|cx| {
                    // Каждый источник опрашивается только пока у него в
                    // принципе может появиться новое событие. Источник,
                    // который уже закончился (DNS-стрим отдал `None`) или
                    // никогда не начинался (`attempts` пуст), пропускается
                    // БЕЗ вызова `poll` — не только чтобы не нарушить
                    // контракт `Stream` ("не опрашивать после `None`"), но
                    // и чтобы не повторить брифовскую ошибку в другом
                    // месте: опрос пустого `FuturesUnordered` возвращает
                    // `Ready(None)` НЕМЕДЛЕННО (проверено чтением
                    // `futures-util` 0.3.33,
                    // `stream/futures_unordered/mod.rs`: `is_terminated`
                    // стартует `false`, значит пустая пустая коллекция
                    // была бы опрошена, а не пропущена, и такое `Ready`
                    // выигрывало бы гонку у ещё не сработавшего таймера на
                    // каждом раунде) — тот самый случай, ради которого
                    // бриф оригинального `race_connect` явно проверял
                    // `attempts.is_empty()` перед гонкой.
                    if !v6_done {
                        if let Poll::Ready(item) = v6_stream.as_mut().poll_next(cx) {
                            return Poll::Ready(Event::V6(item));
                        }
                    }
                    if !v4_done {
                        if let Poll::Ready(item) = v4_stream.as_mut().poll_next(cx) {
                            return Poll::Ready(Event::V4(item));
                        }
                    }
                    if !attempts.is_empty() {
                        if let Poll::Ready(item) = Pin::new(&mut attempts).poll_next(cx) {
                            return Poll::Ready(Event::Attempt(item));
                        }
                    }
                    if sleep_fut.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Event::TimedOut);
                    }
                    Poll::Pending
                })
                .await;

                match ev {
                    Event::V6(Some(Ok(addr))) => sched.offer_v6(&[addr.addr]),
                    Event::V6(Some(Err(e))) => errs.v6 = Some(e),
                    Event::V6(None) => {
                        v6_done = true;
                        sched.mark_v6_done();
                    }
                    Event::V4(Some(Ok(addr))) => sched.offer_v4(&[addr.addr]),
                    Event::V4(Some(Err(e))) => errs.v4 = Some(e),
                    Event::V4(None) => {
                        v4_done = true;
                        sched.mark_v4_done();
                    }
                    Event::Attempt(Some(Ok(s))) => return Ok(s),
                    Event::Attempt(Some(Err(_))) => {
                        // Одна попытка отказала — не повод останавливать
                        // гонку остальных; `Scheduler` сам решит, стартовать
                        // ли ещё, на следующем `poll`.
                    }
                    Event::Attempt(None) => {
                        unreachable!(
                            "poll_next на attempts опрашивается только когда attempts \
                             непуст (см. проверку `!attempts.is_empty()` выше), а \
                             FuturesUnordered не возвращает None для непустой коллекции"
                        );
                    }
                    Event::TimedOut => {}
                }
            }
            HeAction::Exhausted => {
                while let Some(res) = attempts.next().await {
                    if let Ok(s) = res {
                        return Ok(s);
                    }
                }
                if launched == 0 {
                    // Ни одной TCP-попытки не было — значит нечего было
                    // пробовать, а не что все попытки отказали.
                    return Err(Error::new(ErrorKind::Resolve, errs));
                }
                return Err(Error::new(ErrorKind::Connect, AllAttemptsFailed(launched)));
            }
        }
    }
}

/// Happy Eyeballs над уже разрешённым списком адресов — примитив без DNS.
/// "Resolution Delay" в этом вызове недостижима не по недосмотру, а
/// структурно: вызывающая сторона уже знает оба списка целиком, ждать
/// действительно нечего, и `stream::iter` ниже — честное отражение этого
/// факта (стрим, отдающий всё на первом опросе), а не костыль, который
/// притворяется потоком. За настоящим кормлением по мере поступления — см.
/// [`connect`] и doc-комментарий модуля.
pub(crate) async fn race_connect<R>(
    rt: &R,
    addrs_v6: Vec<IpAddr>,
    addrs_v4: Vec<IpAddr>,
    port: u16,
    opts: &TcpOpts,
    he: HeConfig,
) -> Result<R::Stream, Error>
where
    R: TcpConnect + Timer,
{
    let sched = build_scheduler(he)?;
    let v6 = futures_util::stream::iter(
        addrs_v6
            .into_iter()
            .map(|addr| Ok(ResolvedAddr { addr, ttl: None })),
    );
    let v4 = futures_util::stream::iter(
        addrs_v4
            .into_iter()
            .map(|addr| Ok(ResolvedAddr { addr, ttl: None })),
    );
    drive(rt, sched, v6, v4, port, opts).await
}

/// DNS-потребляющий коннектор: резолвит `uri`, гоняет Happy Eyeballs
/// (кормя [`Scheduler`] по мере того, как приходят результаты — см.
/// doc-комментарий модуля), затем опционально проводит TLS-хендшейк с
/// заданным ALPN. Схема `uri` решает, нужен ли TLS вообще (`https` — да,
/// `http` — нет); любая другая схема — `ErrorKind::Unsupported`, а не
/// молчаливая трактовка как `http`.
pub(crate) async fn connect<R, D, L>(
    rt: &R,
    dns: &D,
    tls: &L,
    uri: &Uri,
    opts: &TcpOpts,
    alpn: &[&[u8]],
) -> Result<(Conn<R::Stream, L::Stream<R::Stream>>, Option<TlsInfo>), Error>
where
    R: TcpConnect + Timer,
    D: Resolve,
    L: TlsConnect,
{
    let host = host(uri)?;
    let use_tls = wants_tls(uri)?;
    let port = port(uri, use_tls);
    let sched = build_scheduler(HeConfig::default())?;

    let tcp = drive(
        rt,
        sched,
        dns.lookup_ipv6(host),
        dns.lookup_ipv4(host),
        port,
        opts,
    )
    .await?;

    if use_tls {
        let req = TlsRequest {
            server_name: host,
            alpn,
            ech: None,
        };
        let (stream, info) = tls.connect(tcp, req).await?;
        Ok((Conn::Tls(stream), Some(info)))
    } else {
        Ok((Conn::Plain(tcp), None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::rc::Rc;

    fn v6(n: u16) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(0x20, 0, 0, 0, 0, 0, 0, n))
    }
    fn v4(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    // --- FakeRt: детерминированные `TcpConnect`+`Timer` без реальной сети
    // и без реального сна ------------------------------------------------
    //
    // `sleep` не ждёт по-настоящему — она СИНХРОННО двигает общие вперёд
    // виртуальные часы и сразу резолвится. Это не имитация приближённого
    // поведения: `now`/`elapsed_since` читают те же самые часы, так что
    // тест видит РОВНО ту арифметику времени, которую строит `drive`, без
    // ни одной реальной миллисекунды ожидания и без associated jitter,
    // который сделал бы assert на точное значение задержки хрупким.
    //
    // `log` записывает `(IpAddr, время_на_момент_старта)` для каждой
    // попытки — этим либо доказывается "пауза между стартами равна ровно
    // attempt_delay" (staggering), либо "адреса чередуются между
    // семействами в правильном порядке" (interleaving), либо и то, и
    // другое сразу.
    #[derive(Clone)]
    struct FakeRt {
        clock: Rc<RefCell<Duration>>,
        log: Rc<RefCell<Vec<(IpAddr, Duration)>>>,
        /// `None` — соединение с этим адресом никогда не завершится (пока
        /// тест не решит иначе); `Some(true)` — успех; `Some(false)` — отказ.
        outcomes: Rc<RefCell<HashMap<IpAddr, bool>>>,
    }

    impl FakeRt {
        fn new(outcomes: impl IntoIterator<Item = (IpAddr, bool)>) -> Self {
            Self {
                clock: Rc::new(RefCell::new(Duration::ZERO)),
                log: Rc::new(RefCell::new(Vec::new())),
                outcomes: Rc::new(RefCell::new(outcomes.into_iter().collect())),
            }
        }
    }

    /// `hyper::rt` IO без единого настоящего байта — `drive`/`race_connect`
    /// никогда не читают и не пишут в успешно "соединённый" поток, только
    /// возвращают его вызывающей стороне. Несёт `Rc<()>` НАРОЧНО: это и
    /// есть пробник "нигде не требуется `Send`" — если бы `race_connect`
    /// или `drive` требовали `R::Stream: Send` (или любой другой путь
    /// втянул `Send` через `FuturesUnordered`/`poll_fn` вместо явного
    /// бонда), этот файл не скомпилировался бы вовсе. См.
    /// `race_connect_never_requires_send_even_through_the_wait_path` ниже.
    #[derive(Debug)]
    struct FakeStream(#[allow(dead_code)] Rc<()>);

    impl Read for FakeStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: ReadBufCursor<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    impl Write for FakeStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl Timer for FakeRt {
        type Instant = Duration;
        async fn sleep(&self, d: Duration) {
            *self.clock.borrow_mut() += d;
        }
        fn now(&self) -> Duration {
            *self.clock.borrow()
        }
        fn elapsed_since(&self, earlier: Duration) -> Duration {
            *self.clock.borrow() - earlier
        }
    }

    impl TcpConnect for FakeRt {
        type Stream = FakeStream;
        async fn connect(&self, addr: SocketAddr, _opts: &TcpOpts) -> std::io::Result<FakeStream> {
            self.log
                .borrow_mut()
                .push((addr.ip(), *self.clock.borrow()));
            let ok = self
                .outcomes
                .borrow()
                .get(&addr.ip())
                .copied()
                .unwrap_or(false);
            if ok {
                Ok(FakeStream(Rc::new(())))
            } else {
                Err(std::io::Error::other("fake refused"))
            }
        }
    }

    fn he(attempt_delay_ms: u64) -> HeConfig {
        HeConfig {
            attempt_delay: ms(attempt_delay_ms),
            ..Default::default()
        }
    }

    /// `futures_executor::block_on`, но bounded: `FakeRt` never sleeps for
    /// real (its `Timer::sleep` just advances a virtual clock and resolves
    /// immediately), so nothing below should ever take real wall-clock time
    /// — UNLESS a bug (or a mutation, see the mutation-testing notes in
    /// this task's report) makes `drive`/`race_connect` loop forever
    /// without ever advancing the scheduler to `Exhausted` or `Start`.
    /// Task 3 already found exactly that shape of test — one that hangs
    /// under mutation instead of failing, wedging CI with no name and no
    /// diagnosis (see this vertical's Global Constraints). A watchdog
    /// thread, not a `Send`-bounded wrapper around `fut` itself: `fut` is
    /// generic with NO `Send` bound here on purpose, because several
    /// tests below deliberately drive `!Send` futures (`FakeStream` holds
    /// an `Rc`) to prove `race_connect`/`drive` impose no such bound — an
    /// `F: Send` bound on this helper would silently defeat that proof
    /// for exactly the tests it matters most for. Only an
    /// `Arc<AtomicBool>` — unrelated to `fut` — crosses the thread
    /// boundary.
    fn bounded_block_on<F: std::future::Future>(fut: F) -> F::Output {
        const BOUND: Duration = Duration::from_secs(10);
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watchdog_done = done.clone();
        std::thread::spawn(move || {
            std::thread::sleep(BOUND);
            if !watchdog_done.load(std::sync::atomic::Ordering::SeqCst) {
                eprintln!(
                    "bounded_block_on: future did not complete within {BOUND:?} - treating \
                     as a hang (likely an infinite loop in drive/race_connect) instead of \
                     letting the test process wedge CI with no diagnosis"
                );
                std::process::exit(101);
            }
        });
        let result = futures_executor::block_on(fut);
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        result
    }

    #[test]
    fn attempt_staggering_delay_is_respected_through_the_connector() {
        // Два мёртвых адреса одного семейства: единственная причина между
        // ними будет пауза — Connection Attempt Delay. Виртуальные часы,
        // поэтому проверяется РОВНОЕ значение, не "примерно".
        let rt = FakeRt::new([]);
        let out = bounded_block_on(race_connect(
            &rt,
            vec![v6(1), v6(2)],
            vec![],
            81,
            &TcpOpts::default(),
            he(100),
        ));
        assert!(out.is_err(), "оба адреса мертвы");
        let log = rt.log.borrow().clone();
        assert_eq!(log, vec![(v6(1), ms(0)), (v6(2), ms(100))]);
    }

    #[test]
    fn families_interleave_through_the_connector() {
        let rt = FakeRt::new([]);
        let out = bounded_block_on(race_connect(
            &rt,
            vec![v6(1), v6(2)],
            vec![v4(1), v4(2)],
            81,
            &TcpOpts::default(),
            he(100),
        ));
        assert!(out.is_err());
        let log: Vec<IpAddr> = rt.log.borrow().iter().map(|(a, _)| *a).collect();
        assert_eq!(log, vec![v6(1), v4(1), v6(2), v4(2)]);
    }

    #[test]
    fn all_attempts_failed_reports_connect_kind_with_the_launched_count() {
        let rt = FakeRt::new([]);
        let err = bounded_block_on(race_connect(
            &rt,
            vec![v6(1)],
            vec![v4(1), v4(2)],
            81,
            &TcpOpts::default(),
            he(100),
        ))
        .expect_err("все три адреса мертвы");
        assert_eq!(err.kind(), &ErrorKind::Connect);
        assert_eq!(rt.log.borrow().len(), 3);
    }

    #[test]
    fn a_successful_attempt_short_circuits_the_remaining_race() {
        let rt = FakeRt::new([(v4(1), true)]);
        let stream = bounded_block_on(race_connect(
            &rt,
            vec![v6(1)],
            vec![v4(1)],
            81,
            &TcpOpts::default(),
            he(100),
        ));
        assert!(stream.is_ok());
    }

    #[test]
    fn he_config_out_of_range_is_a_typed_error_not_a_silent_clamp() {
        let rt = FakeRt::new([]);
        let err = bounded_block_on(race_connect(
            &rt,
            vec![v6(1)],
            vec![],
            81,
            &TcpOpts::default(),
            he(1), // ниже ATTEMPT_MIN (100 мс) — было бы зажато молча
        ))
        .expect_err("out-of-range attempt_delay обязан быть отвергнут");
        assert_eq!(err.kind(), &ErrorKind::Connect);
        // Не запущено ни одной попытки: отказ произошёл ДО гонки.
        assert!(rt.log.borrow().is_empty());
    }

    /// `Rc`-хранящий поток (`FakeStream`) реально проезжает через
    /// `Wait`-ветку (не только через мгновенный успех): мёртвый адрес,
    /// затем живой — та же форма, что и интеграционный тест
    /// `falls_over_from_a_dead_address_to_a_live_one`, только на `FakeRt`.
    /// Если бы `drive`/`race_connect` (через `FuturesUnordered`, `poll_fn`
    /// или сигнатуру `TcpConnect`) требовали `Send` где бы то ни было, этот
    /// файл не скомпилировался бы: `FakeStream` — `!Send` по построению.
    #[test]
    fn race_connect_never_requires_send_even_through_the_wait_path() {
        let rt = FakeRt::new([(v4(1), true)]);
        let stream = bounded_block_on(race_connect(
            &rt,
            vec![v6(1), v6(2)],
            vec![v4(1)],
            81,
            &TcpOpts::default(),
            he(100),
        ));
        assert!(stream.is_ok());
        // RFC 8305 interleaving (first_family_count по умолчанию 1) ставит
        // v4(1) ВТОРОЙ попыткой (v6(1), v4(1), v6(2), ...) — успех на ней
        // останавливает гонку раньше, чем до v6(2) вообще доходит очередь.
        // Обе уже стартовавшие попытки (v6(1) мёртвая, v4(1) живая) прошли
        // именно через Wait-ветку `drive`, не только через мгновенный успех
        // на первом же `Start` — это и есть предмет теста.
        assert_eq!(rt.log.borrow().len(), 2);
    }

    // --- Виртуальные DNS-стримы для `drive` -----------------------------
    //
    // `poll_next` сверяется с ТЕМИ ЖЕ виртуальными часами, что и
    // `FakeRt::Timer` (тот же `Rc<RefCell<Duration>>`), так что "AAAA
    // приходит через N мс" — не реальная задержка, а точка на общей оси
    // времени, которую `FakeRt::sleep` продвигает вперёд.
    struct AtVirtualTime {
        clock: Rc<RefCell<Duration>>,
        resolve_at: Duration,
        item: Option<IpAddr>,
        yielded: bool,
    }

    impl futures_util::Stream for AtVirtualTime {
        type Item = Result<ResolvedAddr, Error>;
        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            if *this.clock.borrow() < this.resolve_at {
                return Poll::Pending;
            }
            if this.yielded {
                return Poll::Ready(None);
            }
            this.yielded = true;
            match this.item {
                Some(addr) => Poll::Ready(Some(Ok(ResolvedAddr { addr, ttl: None }))),
                None => Poll::Ready(None),
            }
        }
    }

    #[test]
    fn resolution_delay_is_honored_when_ipv6_is_still_pending() {
        // AAAA "приходит" в 80 мс — позже дефолтного Resolution Delay
        // (50 мс). A "приходит" сразу (0 мс). Если бы `drive` не уважал
        // Resolution Delay (то есть если бы код, собиравший стрим целиком
        // в Vec перед тем, как звать Scheduler, вернулся), IPv4 стартовал
        // бы мгновенно, до 50 мс; правильное поведение — подождать ровно
        // 50 мс, затем пойти по IPv4 (RFC 8305 §3: подождать Resolution
        // Delay, не резолвер целиком).
        let rt = FakeRt::new([(v4(9), true)]);
        let clock = rt.clock.clone();
        let v6s = AtVirtualTime {
            clock: clock.clone(),
            resolve_at: ms(80),
            item: Some(v6(9)),
            yielded: false,
        };
        let v4s = AtVirtualTime {
            clock,
            resolve_at: ms(0),
            item: Some(v4(9)),
            yielded: false,
        };
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let out = bounded_block_on(drive(&rt, sched, v6s, v4s, 81, &TcpOpts::default()));
        assert!(out.is_ok(), "IPv4 обязан быть испробован после ожидания");
        let log = rt.log.borrow();
        assert_eq!(
            log.len(),
            1,
            "AAAA пришли только в 80мс — уже после победы IPv4"
        );
        assert_eq!(log[0].0, v4(9));
        assert_eq!(
            log[0].1,
            ms(50),
            "старт IPv4 обязан произойти РОВНО через Resolution Delay (50мс), \
             не раньше (не дождались бы AAAA) и не позже (ждали бы резолвер \
             целиком вместо фиксированной паузы)"
        );
    }

    #[test]
    fn late_ipv6_arrival_after_resolution_delay_is_still_attempted() {
        // RFC 8305 §3: поздний AAAA всё равно учитывается, не отбрасывается
        // — тот же сценарий, что и `Scheduler`'s собственный
        // `late_ipv6_arrival_after_resolution_delay_is_still_attempted`
        // (happy_eyeballs.rs), но здесь — через реальный DNS-стрим и
        // реальный `drive`, а не прямые вызовы `offer_v6`/`poll`.
        let rt = FakeRt::new([]); // все мертвы: IPv4 не отвечает, IPv6 тоже
        let clock = rt.clock.clone();
        let v6s = AtVirtualTime {
            clock: clock.clone(),
            resolve_at: ms(300),
            item: Some(v6(7)),
            yielded: false,
        };
        let v4s = AtVirtualTime {
            clock,
            resolve_at: ms(0),
            item: Some(v4(7)),
            yielded: false,
        };
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let out = bounded_block_on(drive(&rt, sched, v6s, v4s, 81, &TcpOpts::default()));
        assert!(out.is_err());
        let log = rt.log.borrow();
        assert_eq!(
            log.iter().map(|(a, _)| *a).collect::<Vec<_>>(),
            vec![v4(7), v6(7)],
            "поздний AAAA обязан быть испробован, а не отброшен"
        );
    }

    #[test]
    fn a_resolve_error_on_one_family_surfaces_as_resolve_kind_when_nothing_else_is_found() {
        struct ErrOnce(bool);
        impl futures_util::Stream for ErrOnce {
            type Item = Result<ResolvedAddr, Error>;
            fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                let this = self.get_mut();
                if this.0 {
                    this.0 = false;
                    Poll::Ready(Some(Err(Error::new(
                        ErrorKind::Resolve,
                        std::io::Error::other("dns down"),
                    ))))
                } else {
                    Poll::Ready(None)
                }
            }
        }
        let rt = FakeRt::new([]);
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let err = bounded_block_on(drive(
            &rt,
            sched,
            ErrOnce(true),
            futures_util::stream::empty(),
            81,
            &TcpOpts::default(),
        ))
        .expect_err("ни одного адреса не пришло ни по одному семейству");
        assert_eq!(
            err.kind(),
            &ErrorKind::Resolve,
            "резолвер отказал — это НЕ 'все TCP-попытки провалились', а 'нечего было пробовать'"
        );
        assert!(
            rt.log.borrow().is_empty(),
            "ни одной TCP-попытки не должно было быть"
        );
    }

    #[test]
    fn zero_addresses_without_any_resolver_error_also_surfaces_as_resolve_kind() {
        // NXDOMAIN-подобный случай: резолвер честно отработал, нашёл ноль
        // адресов, ни разу не вернул Err. Всё равно ErrorKind::Resolve, не
        // Connect с launched=0 — "0 TCP-попыток отказали" звучало бы так,
        // будто мы вообще пытались.
        let rt = FakeRt::new([]);
        let sched = build_scheduler(HeConfig::default()).unwrap();
        let err = bounded_block_on(drive(
            &rt,
            sched,
            futures_util::stream::empty(),
            futures_util::stream::empty(),
            81,
            &TcpOpts::default(),
        ))
        .expect_err("оба семейства пусты");
        assert_eq!(err.kind(), &ErrorKind::Resolve);
    }

    // --- Плюмбинг `connect`: URI → host/port/схема, TLS/plain ------------

    struct StaticResolve {
        v6: Vec<IpAddr>,
        v4: Vec<IpAddr>,
    }
    impl Resolve for StaticResolve {
        fn lookup_ipv6(
            &self,
            _: &str,
        ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(
                self.v6
                    .clone()
                    .into_iter()
                    .map(|addr| Ok(ResolvedAddr { addr, ttl: None })),
            )
        }
        fn lookup_ipv4(
            &self,
            _: &str,
        ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(
                self.v4
                    .clone()
                    .into_iter()
                    .map(|addr| Ok(ResolvedAddr { addr, ttl: None })),
            )
        }
    }

    /// Не шифрует ничего — только доказывает, что `connect` реально зовёт
    /// `TlsConnect::connect` для `https` и не зовёт для `http`. Тот же
    /// приём, что `NoOpTls` в тестах самого `http_ng_tls` (Task 8).
    struct NoOpTls;
    impl TlsConnect for NoOpTls {
        type Stream<S>
            = S
        where
            S: Read + Write + Unpin;
        async fn connect<S>(&self, io: S, req: TlsRequest<'_>) -> Result<(S, TlsInfo), Error>
        where
            S: Read + Write + Unpin,
        {
            Ok((
                io,
                TlsInfo {
                    alpn: req.alpn.first().map(|p| p.to_vec()),
                    ..Default::default()
                },
            ))
        }
    }

    fn live_listener_addr() -> SocketAddr {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = l.accept();
        });
        addr
    }

    #[test]
    fn connect_uses_plain_for_http_and_reports_no_tls_info() {
        let addr = live_listener_addr();
        let dns = StaticResolve {
            v6: vec![],
            v4: vec![addr.ip()],
        };
        let uri: Uri = format!("http://example.invalid:{}/", addr.port())
            .parse()
            .unwrap();
        let (conn, info) = bounded_block_on(super::connect(
            &FakeRt::new([(addr.ip(), true)]),
            &dns,
            &NoOpTls,
            &uri,
            &TcpOpts::default(),
            &[],
        ))
        .expect("connect");
        assert!(matches!(conn, Conn::Plain(_)));
        assert!(info.is_none());
    }

    #[test]
    fn connect_uses_tls_for_https_and_returns_tls_info() {
        let addr = live_listener_addr();
        let dns = StaticResolve {
            v6: vec![],
            v4: vec![addr.ip()],
        };
        let uri: Uri = format!("https://example.invalid:{}/", addr.port())
            .parse()
            .unwrap();
        let alpn: [&[u8]; 1] = [b"h2"];
        let (conn, info) = bounded_block_on(super::connect(
            &FakeRt::new([(addr.ip(), true)]),
            &dns,
            &NoOpTls,
            &uri,
            &TcpOpts::default(),
            &alpn,
        ))
        .expect("connect");
        assert!(matches!(conn, Conn::Tls(_)));
        assert_eq!(info.unwrap().alpn.as_deref(), Some(b"h2".as_slice()));
    }

    #[test]
    fn connect_rejects_an_unsupported_scheme() {
        let dns = StaticResolve {
            v6: vec![],
            v4: vec![],
        };
        let uri: Uri = "ftp://example.invalid/".parse().unwrap();
        let err = bounded_block_on(super::connect(
            &FakeRt::new([]),
            &dns,
            &NoOpTls,
            &uri,
            &TcpOpts::default(),
            &[],
        ))
        .expect_err("ftp не поддерживается");
        assert_eq!(err.kind(), &ErrorKind::Unsupported);
    }

    #[test]
    fn connect_defaults_the_port_from_the_scheme_when_absent() {
        // Порт не указан явно в URI — берётся дефолт схемы (https -> 443).
        // Реального сервера на 443 в тестовом окружении нет и не должно
        // быть: проверяем только то, что `connect` ДОШЁЛ до попытки
        // соединения на правильный порт, а не остановился раньше на
        // "не могу определить порт". `FakeRt` с отказом на любой адрес —
        // достаточно: интересует лог попыток, не успех.
        let dns = StaticResolve {
            v6: vec![],
            v4: vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))],
        };
        let uri: Uri = "https://example.invalid/".parse().unwrap();
        let rt = FakeRt::new([]);
        let _ = bounded_block_on(super::connect(
            &rt,
            &dns,
            &NoOpTls,
            &uri,
            &TcpOpts::default(),
            &[],
        ));
        let log = rt.log.borrow();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)));
    }

    #[test]
    fn connect_rejects_a_uri_without_a_host() {
        let dns = StaticResolve {
            v6: vec![],
            v4: vec![],
        };
        // `http::Uri`, у которого нет authority вовсе (origin-form).
        let uri: Uri = "/just/a/path".parse().unwrap();
        let err = bounded_block_on(super::connect(
            &FakeRt::new([]),
            &dns,
            &NoOpTls,
            &uri,
            &TcpOpts::default(),
            &[],
        ))
        .expect_err("нет host — некуда коннектиться");
        assert_eq!(err.kind(), &ErrorKind::Connect);
    }
}
