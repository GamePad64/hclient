use hyper::rt::ReadBufCursor;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Мост `futures_io::{AsyncRead, AsyncWrite}` → `hyper::rt::{Read, Write}`.
///
/// В hyper-util есть только `TokioIo`; `smol-hyper` 0.1.1 мёртв с 2023-12-29 и
/// мостит в противоположную сторону. Без этого моста smol-бэкенда (Task 4)
/// не существует.
///
/// Реализация **без `unsafe`**: читаем во временный буфер и копируем через
/// безопасный `ReadBufCursor::put_slice` — именно этот приём рекомендует
/// документация `hyper::rt::Read`. Цена — одно копирование на чтение;
/// zero-copy потребовал бы `unsafe` (`ReadBufCursor::as_mut`/`advance`), а
/// крейт объявляет `#![deny(unsafe_code)]` — отложено намеренно.
pub struct FuturesIo<S> {
    inner: S,
    /// Буфер выделяется и обнуляется ОДИН раз, в [`FuturesIo::new`] — не на
    /// каждый `poll_read`.
    ///
    /// Черновик задачи держал его как `[0u8; SCRATCH]` на стеке внутри
    /// `poll_read`. Измерено (`rustc -O --emit=asm` на изолированном
    /// воспроизведении обеих версий вне остального крейта): стековый
    /// вариант зовёт `memset` на КАЖДЫЙ вызов (`subq $4096,%rsp` дважды —
    /// это и есть 8192 байта — затем `callq *memset@GOTPCREL`), тогда как
    /// версия с полем структуры этого вызова не делает вовсе — читающая
    /// функция вызывается прямо на уже готовом буфере. Аллокация и
    /// единственное обнуление происходят в `new()`
    /// (`__rust_alloc_zeroed`), один раз на соединение, а не на каждое
    /// чтение. `poll_read` — горячий путь каждого запроса на smol; лишний
    /// `memset` там не гипотетическая цена.
    scratch: Box<[u8]>,
}

/// Размер буфера. 8 KiB — типичный размер чтения у hyper, поэтому лишних
/// итераций не возникает.
const SCRATCH: usize = 8 * 1024;

impl<S> FuturesIo<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            scratch: vec![0u8; SCRATCH].into_boxed_slice(),
        }
    }

    pub fn into_inner(self) -> S {
        self.inner
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }
}

// Ручной `Debug`, а не `#[derive]`: `derive` дампил бы все 8 KiB `scratch`
// как список чисел при каждом форматировании — бесполезно и шумно в логах.
// Тот же приём уже применён в `http_ng_core::RequestBody` (длина вместо
// содержимого).
impl<S: std::fmt::Debug> std::fmt::Debug for FuturesIo<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuturesIo")
            .field("inner", &self.inner)
            .field("scratch_len", &self.scratch.len())
            .finish()
    }
}

impl<S: futures_io::AsyncRead + Unpin> hyper::rt::Read for FuturesIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        let want = buf.remaining().min(self.scratch.len());
        if want == 0 {
            return Poll::Ready(Ok(()));
        }
        // Разбираем на непересекающиеся поля явно: `inner` и `scratch`
        // заимствуются одновременно, но независимо друг от друга.
        let Self { inner, scratch } = &mut *self;
        let n = std::task::ready!(Pin::new(inner).poll_read(cx, &mut scratch[..want]))?;
        buf.put_slice(&scratch[..n]);
        Poll::Ready(Ok(()))
    }
}

impl<S: futures_io::AsyncWrite + Unpin> hyper::rt::Write for FuturesIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    /// `futures_io::AsyncWrite` не даёт способа спросить `S`, эффективна ли у
    /// него векторная запись: в трейте нет метода `is_write_vectored` вовсе
    /// — только `poll_write`, `poll_write_vectored`, `poll_flush`,
    /// `poll_close`. Дефолтная реализация `poll_write_vectored` в
    /// `futures-io` 0.3.33 пишет только первый непустой буфер
    /// (`futures_io::AsyncWrite::poll_write_vectored`, проверено по
    /// исходнику зависимости); для любого `S`, не переопределившего её,
    /// векторная запись молча деградирует в одну обычную — больше
    /// сисколов, не меньше.
    ///
    /// hyper документирует этот метод как обещание "efficient"
    /// `poll_write_vectored`-реализации и ветвится по нему, решая, стоит ли
    /// объединять буферы перед записью. Вернуть здесь `true` значило бы
    /// утверждать то, что нечем подтвердить — а способность, которая лжёт,
    /// хуже отсутствующей, потому что вызывающий код (здесь — сам hyper)
    /// ветвится по ней. Честный консервативный ответ — `false`.
    ///
    /// Если появится конкретный `S`, для которого векторная запись заведомо
    /// эффективна, правильный путь — отдельный конструктор с explicit
    /// opt-in, а не оптимистичный дефолт здесь. Сегодня такого потребителя
    /// нет — Task 4 (`http-ng-rt-smol`) ещё не написан — так что добавлять
    /// его сейчас было бы спекулятивным API без вызывающей стороны.
    fn is_write_vectored(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_executor::block_on;
    use std::pin::Pin;

    /// Источник, отдающий данные порциями, чтобы поймать частичные чтения.
    struct Chunked {
        data: Vec<u8>,
        at: usize,
        step: usize,
    }
    impl futures_io::AsyncRead for Chunked {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let n = self.step.min(buf.len()).min(self.data.len() - self.at);
            buf[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            std::task::Poll::Ready(Ok(n))
        }
    }

    fn read_all(mut io: FuturesIo<Chunked>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut store = [0u8; 8];
        loop {
            let mut rb = hyper::rt::ReadBuf::new(&mut store);
            let poll = block_on(std::future::poll_fn(|cx| {
                hyper::rt::Read::poll_read(Pin::new(&mut io), cx, rb.unfilled())
            }));
            poll.unwrap();
            let filled = rb.filled().to_vec();
            if filled.is_empty() {
                return out;
            }
            out.extend_from_slice(&filled);
        }
    }

    #[test]
    fn forwards_bytes_through_partial_reads() {
        let io = FuturesIo::new(Chunked {
            data: b"hello world".to_vec(),
            at: 0,
            step: 3,
        });
        assert_eq!(read_all(io), b"hello world");
    }

    #[test]
    fn never_writes_more_than_remaining() {
        // step больше, чем ёмкость буфера: put_slice не должен паниковать.
        let io = FuturesIo::new(Chunked {
            data: vec![7u8; 64],
            at: 0,
            step: 64,
        });
        assert_eq!(read_all(io).len(), 64);
    }

    #[test]
    fn into_inner_round_trips() {
        let io = FuturesIo::new(Chunked {
            data: vec![],
            at: 0,
            step: 1,
        });
        let c = io.into_inner();
        assert_eq!(c.step, 1);
    }

    #[test]
    fn read_request_larger_than_scratch_buffer_does_not_panic() {
        // `buf.remaining()` управляется вызывающей стороной (hyper) и может
        // быть больше `scratch.len()` — `want` обязан ограничиваться сверху
        // размером буфера, иначе `&mut scratch[..want]` индексирует за
        // пределы и паникует. Раньше это не проверялось ни одним тестом:
        // `never_writes_more_than_remaining` использует буфер вызывающей
        // стороны на 8 байт, что всегда меньше `SCRATCH`, и снятие `.min(..)`
        // там не ловилось.
        let len = super::SCRATCH + 137;
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let mut io = FuturesIo::new(Chunked {
            data: data.clone(),
            at: 0,
            step: len,
        });
        let mut out = Vec::new();
        let mut store = vec![0u8; len];
        loop {
            let mut rb = hyper::rt::ReadBuf::new(&mut store);
            let poll = block_on(std::future::poll_fn(|cx| {
                hyper::rt::Read::poll_read(Pin::new(&mut io), cx, rb.unfilled())
            }));
            poll.unwrap();
            let filled = rb.filled().to_vec();
            if filled.is_empty() {
                break;
            }
            out.extend_from_slice(&filled);
        }
        assert_eq!(out, data);
    }

    /// Заглушка `AsyncWrite`, которая всегда "успешна" — нужна только чтобы
    /// проверить `is_write_vectored()`, а не поведение записи.
    struct NullWrite;
    impl futures_io::AsyncWrite for NullWrite {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn is_write_vectored_reports_false_not_an_unverifiable_claim() {
        // `futures_io::AsyncWrite` не даёт способа спросить `S`, эффективна ли
        // у него векторная запись — значит `true` здесь была бы утверждением,
        // которое нечем подтвердить. См. doc-комментарий на impl.
        let io = FuturesIo::new(NullWrite);
        assert!(!hyper::rt::Write::is_write_vectored(&io));
    }
}
