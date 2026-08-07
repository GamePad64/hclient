use hyper::rt::{Read, ReadBufCursor, Write};
use std::io::{Read as _, Write as _};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

const SCRATCH: usize = 16 * 1024;

/// TLS поверх любого `hyper::rt`-транспорта.
///
/// Построен на поверхности rustls, стабильной с 0.20: `read_tls` /
/// `process_new_packets` / `wants_write` / `write_tls`. **Не** на `unbuffered`
/// — тот удалён в main rustls (PR #2905, 2026-02-06), и адаптер на нём пришлось
/// бы переписывать целиком под 0.24.
#[derive(Debug)]
pub struct TlsStream<S> {
    io: S,
    conn: rustls::ClientConnection,
}

impl<S> TlsStream<S> {
    pub(crate) fn new(io: S, conn: rustls::ClientConnection) -> Self {
        Self { io, conn }
    }
    pub(crate) fn conn(&self) -> &rustls::ClientConnection {
        &self.conn
    }
    pub(crate) fn parts_mut(&mut self) -> (&mut S, &mut rustls::ClientConnection) {
        (&mut self.io, &mut self.conn)
    }
}

fn tls_err<E: std::error::Error + 'static>(e: E) -> std::io::Error {
    std::io::Error::other(format!("tls: {e}"))
}

/// Мост `hyper::rt::Write` (асинхронный, poll) → `std::io::Write`
/// (синхронный, блокирующий) — тот интерфейс, на который написан
/// `ClientConnection::write_tls`. `Poll::Pending` становится
/// `Err(WouldBlock)`; вызывающий, получивший `WouldBlock`, обязан отличить
/// его от настоящей ошибки и вернуть `Poll::Pending` сам, а не ошибку
/// наверх.
///
/// Тот же приём, каким `tokio-rustls` закрывает ровно эту же задачу
/// (`common/mod.rs::SyncWriteAdapter`, `tokio-rustls-0.26.4`) — не
/// изобретение этого фикса, а перенос уже проверенного паттерна. Критично
/// здесь то, что `write_tls` реализован через `ChunkVecBuffer::write_to`
/// (`rustls-0.23.43 src/vecbuf.rs`), который делает
/// `wr.write_vectored(bufs)?` и продвигает внутреннюю очередь
/// (`self.consume(used)`) СТРОГО на то, что вернул `wr` — если `wr` вернул
/// `Err` (в том числе наш `WouldBlock`), `?` прерывает `write_tls` РАНЬШЕ
/// `consume`, и внутренняя очередь rustls остаётся нетронутой, сколько бы
/// раз это ни повторилось. Раньше между rustls и транспортом стоял голый
/// `Vec<u8>` — `impl Write for Vec<u8>` никогда не отказывает и не умеет
/// сказать "нет, не всё принял", поэтому `write_tls` безусловно решал, что
/// записал всё (и продвигал sequence/nonce соответственно), а сам транспорт
/// эти байты мог ни разу не увидеть, если следующий шаг (перекачка `Vec` в
/// транспорт) возвращал `Pending` — тогда `Vec` просто терялся вместе с
/// байтами при досрочном возврате (review finding 1, fix round 1).
struct PollWriter<'a, 'cx, S> {
    io: &'a mut S,
    cx: &'a mut Context<'cx>,
}

impl<S: Write + Unpin> std::io::Write for PollWriter<'_, '_, S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match Pin::new(&mut *self.io).poll_write(self.cx, buf) {
            // `hyper::rt::Write::poll_write`'s собственный контракт: "A
            // return value of `0` means that the underlying object is no
            // longer able to accept bytes" — терминальный отказ, не сигнал
            // "попробуй ещё раз попозже" (для этого есть `Pending`). Та же
            // трактовка, что и раньше была на уровне `flush_outgoing`
            // (`WriteZero`), просто теперь она нужна здесь, а не снаружи.
            Poll::Ready(Ok(0)) if !buf.is_empty() => Err(std::io::ErrorKind::WriteZero.into()),
            Poll::Ready(r) => r,
            Poll::Pending => Err(std::io::ErrorKind::WouldBlock.into()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match Pin::new(&mut *self.io).poll_flush(self.cx) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(std::io::ErrorKind::WouldBlock.into()),
        }
    }
}

/// Прокачать всё, что rustls хочет записать, в нижележащий транспорт.
///
/// Байты, уже извлечённые из rustls через `write_tls`, никогда не могут
/// потеряться на `Pending`: `write_tls` вызывается напрямую против моста
/// `PollWriter`, а не против промежуточного буфера — если транспорт не готов
/// принять ни байта, `write_tls` возвращает ошибку (пойманную ниже как
/// `WouldBlock`) ДО того, как продвинет свою очередь, так что терять
/// нечего — очередь rustls (`wants_write()`) остаётся ровно такой же, какой
/// была до вызова.
pub(crate) fn flush_outgoing<S: Write + Unpin>(
    io: &mut S,
    conn: &mut rustls::ClientConnection,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<()>> {
    while conn.wants_write() {
        let mut writer = PollWriter { io, cx };
        match conn.write_tls(&mut writer) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Poll::Pending,
            Err(e) => return Poll::Ready(Err(e)),
        }
    }
    Pin::new(io).poll_flush(cx)
}

/// Вычитать из транспорта и скормить rustls. `Ok(false)` — EOF.
pub(crate) fn pump_incoming<S: Read + Unpin>(
    io: &mut S,
    conn: &mut rustls::ClientConnection,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<bool>> {
    let mut scratch = [0u8; SCRATCH];
    let mut rb = hyper::rt::ReadBuf::new(&mut scratch);
    ready!(Pin::new(io).poll_read(cx, rb.unfilled()))?;
    let filled = rb.filled();
    if filled.is_empty() {
        return Poll::Ready(Ok(false));
    }
    let mut cursor = std::io::Cursor::new(filled);
    while (cursor.position() as usize) < filled.len() {
        conn.read_tls(&mut cursor).map_err(tls_err)?;
        conn.process_new_packets().map_err(tls_err)?;
    }
    Poll::Ready(Ok(true))
}

impl<S: Read + Write + Unpin> Read for TlsStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        loop {
            // 1. Отдать уже расшифрованное.
            let mut scratch = [0u8; SCRATCH];
            let want = buf.remaining().min(SCRATCH);
            if want == 0 {
                return Poll::Ready(Ok(()));
            }
            match this.conn.reader().read(&mut scratch[..want]) {
                // rustls гарантирует `Ok(0)` СТРОГО при чистом
                // `close_notify` (`has_received_close_notify`) —
                // терминальный сигнал "данных больше не будет", а не
                // "данных пока нет" (для этого есть `WouldBlock` веткой
                // ниже). Раньше обе ветки схлопывались в одно и то же
                // "читать транспорт ещё раз" — если пир прислал
                // `close_notify`, но не закрыл сам TCP-сокет (TLS этого не
                // требует), `poll_read` ждал транспортного чтения, которое
                // никогда не придёт: зависание навсегда (review finding 2,
                // fix round 1).
                Ok(0) => return Poll::Ready(Ok(())),
                Ok(n) => {
                    buf.put_slice(&scratch[..n]);
                    return Poll::Ready(Ok(()));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Poll::Ready(Err(e)),
            }
            // 2. Отдать всё исходящее (renegotiation, close_notify и т.п.).
            ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;
            // 3. Дочитать из транспорта.
            let more = ready!(pump_incoming(&mut this.io, &mut this.conn, cx))?;
            if !more {
                return Poll::Ready(Ok(()));
            } // EOF (закрытие сырого транспорта, не TLS-уровня — отдельный
            // случай от close_notify выше)
        }
    }
}

impl<S: Read + Write + Unpin> Write for TlsStream<S> {
    /// Порядок здесь принципиален (review finding 1, fix round 1): дослать
    /// хвост с прошлого вызова, прежде чем трогать `data` этого — иначе
    /// `conn.writer().write(data)` ниже заквьюит те же самые байты ЕЩЁ РАЗ
    /// поверх ещё не отправленных с прошлого раза. Контракт
    /// `hyper::rt::Write` требует повторять `poll_write` с ТЕМ ЖЕ `data`
    /// после `Pending`, а `rustls::Writer::write` не идемпотентен — каждый
    /// вызов безусловно буферизует и шифрует новые байты, без дедупликации
    /// (`Writer::write`'s doc: "buffers plaintext sent... and sends it as
    /// soon as it can" — про повторные вызовы с уже виденными байтами там
    /// ни слова, потому что с точки зрения rustls это просто НОВЫЕ данные).
    /// Тот же класс рассинхронизации, что и в исходном баге
    /// `flush_outgoing`, только на уровне plaintext, а не ciphertext —
    /// исходная форма кода была уязвима для него независимо от починки
    /// `flush_outgoing` выше: `ready!(flush_outgoing(...))` СРАЗУ ПОСЛЕ
    /// `conn.writer().write(data)` пробрасывал `Pending` наверх уже ПОСЛЕ
    /// того, как `data` была поставлена в очередь — то есть само по себе
    /// исправление `flush_outgoing` (см. `PollWriter` выше) не теряет
    /// байты, но не мешает повторной постановке в очередь ПРИ retry с тем
    /// же `data`, если та же функция ещё раз попадёт в этот порядок вызовов.
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = &mut *self;
        ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;

        let n = this.conn.writer().write(data)?;
        if n == 0 && !data.is_empty() {
            // Внутренний буфер исходящего plaintext у rustls
            // (`set_buffer_limit`, по умолчанию 64 КиБ,
            // `common_state::DEFAULT_BUFFER_LIMIT`) переполнен —
            // временный бэкпрешер на уровне rustls, а НЕ "транспорт больше
            // никогда не примет байт", который означает `Ok(0)` в
            // контракте `hyper::rt::Write::poll_write`. Освободить место
            // можно, только дослав уже поставленное в очередь транспорту.
            return match flush_outgoing(&mut this.io, &mut this.conn, cx) {
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) | Poll::Pending => Poll::Pending,
            };
        }

        // Best-effort: `n` байт уже приняты И зашифрованы rustls'ом, который
        // гарантированно отправит их сам, когда сможет (это подтвердит шаг
        // дренажа в начале следующего вызова) — потерять их он больше не
        // может (см. `PollWriter`/`flush_outgoing` выше). Поэтому
        // отчитываемся `Ready(Ok(n))` независимо от того, добрался ли флаш
        // до транспорта прямо сейчас; пробросить здесь `Pending` значило бы
        // заставить вызывающий код повторить `poll_write` с ТЕМИ ЖЕ байтами
        // `data` — см. doc-комментарий метода.
        match flush_outgoing(&mut this.io, &mut this.conn, cx) {
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) | Poll::Pending => Poll::Ready(Ok(n)),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        this.conn.writer().flush()?;
        flush_outgoing(&mut this.io, &mut this.conn, cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        this.conn.send_close_notify();
        ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;
        Pin::new(&mut this.io).poll_shutdown(cx)
    }
}
