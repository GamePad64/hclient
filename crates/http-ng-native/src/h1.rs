//! HTTP/1-обмен на hyper, доведённый до ответа без единого `spawn`.
//!
//! # Почему это технический центр вертикали
//!
//! `hyper::client::conn::http1::handshake` не требует ни executor'а, ни
//! таймера — сама по себе она просто пишет запрос и читает ответ через
//! `hyper::rt::{Read, Write}`. Но у HTTP/1 есть `Connection` — future,
//! который обязан кто-то поллить, иначе байты не поедут ни в одну сторону
//! (см. doc-комментарий `hyper::client::conn::http1::Connection`: "в
//! большинстве случаев его нужно заспавнить в executor"). Мы принципиально
//! не спавним (см. doc-комментарий крейта и `http-ng-rt-pair-check`), так
//! что этот файл поллит `Connection` **вручную, рядом** с чтением ответа —
//! сначала внутри [`exchange`] (пока не пришли заголовки), затем внутри
//! [`NativeBody::poll_frame`] (пока не дочитано тело). `tests/h1.rs`
//! проверяет это не как утверждение, а как факт: `works_on_a_bare_futures_
//! executor_with_no_spawn` гоняет весь путь на голом
//! `futures_executor::block_on` — рантайме без единой возможности
//! заспавнить задачу. Если бы это требовало `spawn`, тест не
//! скомпилировался или завис бы.
//!
//! # Ручной поллинг рядом — не busy-spin на настоящем рантайме
//!
//! "Поллит вручную, рядом" звучит как то же самое, что busy-spin — крутить
//! цикл `poll` в надежде поймать прогресс. Это не так, и вопрос был не
//! обсуждён, а измерен (Task 12, review round 1): на голом executor'е
//! (`futures_executor::block_on`, без реактора) CPU-время действительно
//! равно wall-time — там спин настоящий, потому что просто нет ничего, что
//! умеет ждать готовность сокета иначе, чем перепрашивать. Но тот же самый
//! код [`exchange`]/[`NativeBody`], прогнанный через настоящий реактор
//! (tokio, smol), стоит ~0 CPU за то же wall-time: ни `TokioIo`, ни
//! `FuturesIo` не вызывают `wake_by_ref` сами по себе — они возвращают
//! `Pending` и полагаются на реактор, который разбудит задачу, когда сокет
//! реально готов, а `poll_fn` в [`exchange`] и `poll_frame` в
//! [`NativeBody`] просто НЕ ВЫЗЫВАЮТСЯ, пока их не разбудили. Спин целиком
//! живёт в тестовом хелпере `testing::BlockingIo` (его `poll_would_block`
//! честно зовёт `cx.waker().wake_by_ref()` из-за отсутствия реактора на
//! голом `futures_executor::block_on` — см. его doc-комментарий) и не
//! существует ни на одном продакшн-пути. Task 14 иначе переспросила бы это
//! заново на два рантайма сразу — записано здесь один раз, там где его
//! увидит следующий потребитель этого файла.
//!
//! # Что происходит, когда `Connection` заканчивается первым или падает
//!
//! `Connection` может завершиться (`Ready(Ok(()))`) или упасть
//! (`Ready(Err(_))`) в двух разных местах, и оба обработаны по-разному:
//!
//! - **Внутри [`exchange`]**, пока ответ ещё не пришёл: если `Connection`
//!   падает, обмен обязан упасть тоже — без него `send_request` никогда не
//!   получит ответ. Если `Connection` успешно ЗАВЕРШАЕТСЯ раньше, чем
//!   `send_request`, это не считается успехом сразу: `exchange` перестаёт
//!   поллить уже готовый `Connection` (переполлить завершённый `Future` —
//!   не гарантированно безопасно ни для одной реализации) и продолжает
//!   поллить `send_request`. По инварианту диспетчера hyper (`SendRequest::
//!   send_request` паникует в самом hyper, если её канал уронили, не
//!   ответив ни успехом, ни ошибкой — `dispatch dropped without returning
//!   error`) `Connection` не может дойти до `Dispatched::Shutdown`, не
//!   закрыв этот канал: так что после того как `conn` вернул `Ready(Ok(()))`,
//!   `send` обязана либо уже быть готова, либо стать готовой на следующий
//!   же опрос — зависания здесь нет, а если бы инвариант hyper был нарушен,
//!   `send.await` сам упал бы паникой hyper'а, а не тихо завис.
//! - **Внутри [`NativeBody::poll_frame`]**, после того как заголовки уже
//!   отданы вызывающей стороне: `Connection` — то же самое, что и раньше,
//!   только теперь его задача — дописать оставшиеся байты тела в канал
//!   `hyper::body::Incoming`. Диспетчер hyper доходит до
//!   `Dispatched::Shutdown` не раньше, чем канал тела закрыт (наполнен до
//!   конца или закрыт из-за ошибки) — так что как только `conn.poll()`
//!   единожды вернул `Ready`, дальше можно просто не поллить его
//!   (`self.conn = None`) и продолжать читать из `incoming`: он либо отдаст
//!   оставшиеся уже доставленные кадры, либо `None`, либо (при обрыве)
//!   ошибку — никогда не "тихо замолчит". Именно ЭТО и делает возможной
//!   опасность, о которой предупреждает бриф — "тело, которое молча
//!   перестаёт отдавать байты, потому что никто больше не крутит его
//!   `Connection`" — тест `body_keeps_driving_the_connection_after_headers`
//!   ловит регресс именно этого: без строк, которые поллят `conn` внутри
//!   `poll_frame`, второй чанк тела никогда не будет прочитан из сокета, и
//!   `incoming.poll_frame()` будет возвращать `Pending` вечно (в канал
//!   никто больше не пишет), что и превращает мутацию в зависший тест —
//!   поэтому тест обёрнут в watchdog с потолком (см. `tests/h1.rs`), а не
//!   оставлен голым `block_on`.
//!
//! `exchange` возвращает `Response<NativeBody>`: `SendRequest`, локальная
//! переменная `exchange`, роняется, когда функция возвращает, но
//! `Connection` — нет, он переезжает В ТЕЛО (`NativeBody::conn`) и живёт
//! ровно до тех пор, пока живёт тело или пока тело не дочитано целиком.
//! Дропнуть `SendRequest` раньше времени безопасно и даже правильно для
//! v0.1: пула соединений тут нет ("один запрос — одно соединение"), так что
//! отсутствие возможности отправить ВТОРОЙ запрос через тот же `SendRequest`
//! не помеха, а сигнал диспетчеру hyper, что новых запросов не будет — он
//! доведёт текущий обмен до конца и закроется, а не будет ждать переиспользования
//! (keep-alive реюз — забота будущего пула, не этого файла). Если вызывающая
//! сторона роняет `NativeBody`, не дочитав его, `Connection` внутри роняется
//! вместе с ним — это осознанный выбор вызывающей стороны прервать чтение,
//! а не тихая потеря байт: hyper документирует именно такое поведение при
//! дропе `Connection`.
//!
//! # `ErrorKind` через `hyper::Error`
//!
//! Task 10 (`body.rs`, doc-комментарий модуля) доказало настоящим
//! рукопожатием, что ошибка исходящего тела (`http_ng_core::Error`)
//! переживает путь через `hyper::Error` без потери `ErrorKind` — она
//! достаётся обратно через `hyper::Error::source().downcast_ref()`, а не
//! теряется в `Display`-строке. Наивная версия этого файла (см. черновик
//! задачи) заворачивала КАЖДУЮ ошибку `hyper::Error` в `Error::new(fixed_kind,
//! e)` напрямую — а значит ошибка исходящего тела (`ErrorKind::Body`),
//! долетевшая до `conn.poll()` или `send.poll()` как `hyper::Error`, была бы
//! молча расплющена в `ErrorKind::Connect`, хотя `http_ng_core::Error` с
//! верным `ErrorKind` лежит прямо в `source()` этой же ошибки. [`from_hyper_error`]
//! — общая точка конвертации для всех мест этого файла: сначала пробует
//! восстановить нашу `Error` из `source()`, и только если её там нет —
//! заворачивает `hyper::Error` заново под переданным `fallback`. Тест
//! `exchange_recovers_error_kind_through_hyper_error_not_flattening_it`
//! ниже доказывает это настоящим (хоть и синтетическим IO) рукопожатием —
//! не только чтением кода.
use crate::body::OutgoingBody;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_ng_core::{Error, ErrorKind};
use hyper::client::conn::http1;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

type ConnFuture = Pin<Box<dyn Future<Output = hyper::Result<()>>>>;

/// Общая точка конвертации `hyper::Error` → `http_ng_core::Error` для этого
/// файла — см. doc-комментарий модуля про то, почему расплющивание всего в
/// `fallback` было бы регрессом свойства, доказанного Task 10.
fn from_hyper_error(e: hyper::Error, fallback: ErrorKind) -> Error {
    match std::error::Error::source(&e).and_then(|s| s.downcast_ref::<Error>()) {
        Some(inner) => inner.clone(),
        None => Error::new(fallback, e),
    }
}

/// Тело ответа, которое **само поллит соединение**.
///
/// Без этого после прихода заголовков соединение перестало бы двигаться:
/// hyper требует, чтобы кто-то драйвил `Connection`, а мы принципиально не
/// спавним — иначе понадобился бы `Send + 'static`, и однопоточные рантаймы
/// оказались бы закрыты (см. doc-комментарий крейта и `http-ng-rt-pair-check`).
pub struct NativeBody {
    incoming: hyper::body::Incoming,
    /// `None` — `Connection` уже завершился (успешно или с ошибкой,
    /// зафиксированной ниже); дальше можно просто дочитывать `incoming` без
    /// него, см. doc-комментарий модуля про то, почему это не тихое
    /// зависание. `Box<dyn Future>`, не конкретный тип — единственное место
    /// в вертикали, где мы боксим, и не ради стирания `Send` (бонд `Send`
    /// нигде не объявлен), а чтобы вообще смочь положить `Connection` полем
    /// структуры: у него самого именованного типа нет снаружи `http1`.
    conn: Option<ConnFuture>,
}

impl std::fmt::Debug for NativeBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBody")
            .field("still_driving_connection", &self.conn.is_some())
            .finish()
    }
}

impl Body for NativeBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let this = &mut *self;
        // Сначала подвинуть соединение — иначе новые данные не приедут в
        // канал `incoming` (см. doc-комментарий модуля).
        if let Some(conn) = this.conn.as_mut() {
            match conn.as_mut().poll(cx) {
                Poll::Ready(Ok(())) => {
                    this.conn = None;
                }
                Poll::Ready(Err(e)) => {
                    this.conn = None;
                    return Poll::Ready(Some(Err(from_hyper_error(e, ErrorKind::Body))));
                }
                Poll::Pending => {}
            }
        }
        match Pin::new(&mut this.incoming).poll_frame(cx) {
            Poll::Ready(Some(Ok(f))) => Poll::Ready(Some(Ok(f))),
            Poll::Ready(Some(Err(e))) => {
                Poll::Ready(Some(Err(from_hyper_error(e, ErrorKind::Body))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.incoming.is_end_stream()
    }

    /// Пробрасывает настоящую подсказку `hyper::body::Incoming` (например,
    /// из `Content-Length`), а не дефолтную "неизвестно": она у нас уже
    /// есть бесплатно, отбрасывать её не за чем.
    fn size_hint(&self) -> SizeHint {
        self.incoming.size_hint()
    }
}

/// Один запрос по одному соединению. Пула в v0.1 нет.
pub(crate) async fn exchange<I>(
    io: I,
    req: http::Request<OutgoingBody>,
) -> Result<http::Response<NativeBody>, Error>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    let (mut sender, conn) = http1::handshake::<I, OutgoingBody>(io)
        .await
        .map_err(|e| from_hyper_error(e, ErrorKind::Connect))?;

    // Драйвим соединение и запрос **вместе**, без spawn.
    let mut conn = Box::pin(conn);
    let mut send = Box::pin(sender.send_request(req));
    // `conn` не поллится повторно после того, как один раз вернул `Ready`:
    // per `Future`'s contract переполлить уже завершившийся future не
    // гарантированно безопасно ни для одной конкретной реализации. См.
    // doc-комментарий модуля про то, что происходит с `send`, если
    // `Connection` завершается раньше него.
    let mut conn_done = false;

    let resp = std::future::poll_fn(|cx| {
        if !conn_done {
            match conn.as_mut().poll(cx) {
                Poll::Ready(Ok(())) => conn_done = true,
                Poll::Ready(Err(e)) => {
                    conn_done = true;
                    return Poll::Ready(Err(from_hyper_error(e, ErrorKind::Connect)));
                }
                Poll::Pending => {}
            }
        }
        match send.as_mut().poll(cx) {
            Poll::Ready(Ok(r)) => Poll::Ready(Ok(r)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(from_hyper_error(e, ErrorKind::Connect))),
            Poll::Pending => Poll::Pending,
        }
    })
    .await?;

    let (parts, incoming) = resp.into_parts();
    Ok(http::Response::from_parts(
        parts,
        NativeBody {
            incoming,
            // `conn_done` тут не переиспользуется: если `Connection` уже
            // завершился, `conn_done` было бы `true`, но `send` только что
            // вернула `Ready(Ok(_))` — то есть по анализу в
            // doc-комментарии модуля обмен всё равно корректен, дальше
            // просто нечего поллить, и `NativeBody` обязана узнать об этом
            // сама, а не унаследовать состояние из `exchange`.
            conn: if conn_done {
                None
            } else {
                Some(conn as ConnFuture)
            },
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_core::RequestBody;
    use std::io;

    /// Опрашивает future РОВНО один раз и паникует на `Pending` — то же
    /// решение и то же обоснование, что `http-ng-native::body::tests::
    /// poll_once`: весь путь ниже (рукопожатие, отправка заголовков без
    /// тела, отказ исходящего тела, доставка ошибки в `send_request`)
    /// проходит синхронно на `SinkIo`, так что `Pending` здесь означает
    /// сломанное предположение теста, а не "подождать ещё" — и ловится
    /// немедленно, а не зависанием.
    fn poll_once<F: Future>(fut: Pin<&mut F>) -> F::Output {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        match fut.poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("SinkIo не должен возвращать Pending нигде на этом пути"),
        }
    }

    /// Раковина: пишет в никуда, на чтение всегда отвечает `Pending` — см.
    /// `http-ng-native::body::tests::SinkIo` за подробным разбором, почему
    /// `Pending`, а не мгновенный EOF (свежее соединение стартует в
    /// `KA::Busy`, и немедленный EOF там читается как "неожиданный конец
    /// занятого соединения", а не как отказ ТЕЛА запроса, который здесь и
    /// проверяется).
    #[derive(Default)]
    struct SinkIo;

    impl hyper::rt::Read for SinkIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    impl hyper::rt::Write for SinkIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Однопроходный поток из одной ошибки — тот же приём, что
    /// `body::tests::OneShotStream::error`, только копия здесь: у файлов
    /// этого крейта нет общего тестового модуля, и дублировать маленький
    /// фикстур дешевле, чем городить его наружу ради одного потребителя.
    struct OneShotErr(Option<Error>);
    impl Body for OneShotErr {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
            Poll::Ready(self.0.take().map(Err))
        }
        fn is_end_stream(&self) -> bool {
            self.0.is_none()
        }
    }

    /// Доказывает свойство из doc-комментария модуля, а не только
    /// декларирует его: отказавшее исходящее тело несёт `ErrorKind::Body`,
    /// и этот `ErrorKind` обязан пережить путь `OutgoingBody::poll_frame`
    /// → hyper (`new_user_body`/`.with()`) → `hyper::Error`, вернувшуюся из
    /// `conn`/`send` → `from_hyper_error`. Mutation-проверка: если
    /// `from_hyper_error` вернуть к брифовскому `Error::new(fallback, e)`
    /// без попытки `downcast`, этот тест красится немедленно —
    /// `err.kind()` стал бы `ErrorKind::Connect`, не `ErrorKind::Body`.
    #[test]
    fn exchange_recovers_error_kind_through_hyper_error_not_flattening_it() {
        let original = Error::new(ErrorKind::Body, io::Error::other("stream broke"));

        let body = OutgoingBody::from_request_body(RequestBody::Streaming(Box::new(OneShotErr(
            Some(original.clone()),
        ))));
        let req = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "example.invalid")
            .body(body)
            .unwrap();

        let fut = exchange(SinkIo, req);
        let mut fut = std::pin::pin!(fut);
        let err = match poll_once(fut.as_mut()) {
            Ok(_) => panic!("отказавшее исходящее тело обязано провалить exchange"),
            Err(e) => e,
        };

        assert_eq!(
            err.kind(),
            &ErrorKind::Body,
            "ErrorKind обязан пережить путь через hyper::Error, а не расплющиться \
             в ErrorKind::Connect: {err}"
        );
    }
}
