use hyper::rt::ReadBufCursor;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

/// Мост `tokio::net::TcpStream` → `hyper::rt`. Без `unsafe`: читаем во
/// временный буфер и копируем безопасным `put_slice` — тот же приём, что и
/// в `http_ng_rt::FuturesIo` (Task 2), но здесь напрямую поверх
/// `tokio::io::{AsyncRead, AsyncWrite}`, а не `futures_io`: у tokio свои
/// IO-трейты, и лишний слой через `FuturesIo` только добавил бы копирование.
pub struct TokioIo {
    inner: tokio::net::TcpStream,
    /// Буфер выделяется и обнуляется ОДИН раз, в [`TokioIo::new`] — не на
    /// каждый `poll_read`.
    ///
    /// Черновик задачи держал его как `[0u8; SCRATCH]` на стеке внутри
    /// `poll_read`. Task 2 уже измерил цену этого паттерна
    /// (`rustc -O --emit=asm`, см. комментарий на `FuturesIo::scratch`):
    /// стековый вариант зовёт `memset` на КАЖДЫЙ вызов, тогда как поле
    /// структуры, выделенное один раз в конструкторе, этого не делает —
    /// `poll_read` вызывается прямо на уже готовом буфере. `poll_read` —
    /// горячий путь каждого запроса; лишний `memset` там не гипотетическая
    /// цена, а измеренная.
    scratch: Box<[u8]>,
}

/// Размер буфера. 8 KiB — типичный размер чтения у hyper, поэтому лишних
/// итераций не возникает. Совпадает с `http_ng_rt::FuturesIo::SCRATCH`.
const SCRATCH: usize = 8 * 1024;

impl TokioIo {
    pub(crate) fn new(inner: tokio::net::TcpStream) -> Self {
        Self {
            inner,
            scratch: vec![0u8; SCRATCH].into_boxed_slice(),
        }
    }

    /// Ссылка на нижележащий `tokio::net::TcpStream` — например, чтобы
    /// прочитать применённые `TcpOpts` обратно (`nodelay()`, …) в тестах или
    /// диагностике.
    pub fn get_ref(&self) -> &tokio::net::TcpStream {
        &self.inner
    }

    pub fn into_inner(self) -> tokio::net::TcpStream {
        self.inner
    }
}

// Ручной `Debug`, а не `#[derive]`: `derive` дампил бы все 8 KiB `scratch`
// как список чисел при каждом форматировании — бесполезно и шумно в логах.
// Тот же приём уже применён в `http_ng_rt::FuturesIo`.
impl std::fmt::Debug for TokioIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioIo")
            .field("inner", &self.inner)
            .field("scratch_len", &self.scratch.len())
            .finish()
    }
}

impl hyper::rt::Read for TokioIo {
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
        let mut rb = tokio::io::ReadBuf::new(&mut scratch[..want]);
        std::task::ready!(Pin::new(inner).poll_read(cx, &mut rb))?;
        let filled = rb.filled().len();
        buf.put_slice(&scratch[..filled]);
        Poll::Ready(Ok(()))
    }
}

impl hyper::rt::Write for TokioIo {
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
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    /// В отличие от `FuturesIo` (где `futures_io::AsyncWrite` не даёт способа
    /// спросить это у `S` вовсе), `tokio::io::AsyncWrite` несёт
    /// `is_write_vectored` как метод трейта — здесь это честная делегация
    /// нижележащему `TcpStream`, а не решение за него.
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::rt::Read as _;
    use std::io::Write as _;

    fn connected_pair() -> (TokioIo, std::net::TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || listener.accept().unwrap().0);
        let std_client = std::net::TcpStream::connect(addr).unwrap();
        std_client.set_nonblocking(true).unwrap();
        let client = TokioIo::new(tokio::net::TcpStream::from_std(std_client).unwrap());
        (client, server.join().unwrap())
    }

    #[tokio::test]
    async fn reads_bytes_larger_than_the_scratch_buffer() {
        // Как и `read_request_larger_than_scratch_buffer_does_not_panic` в
        // `http_ng_rt::FuturesIo`: `buf.remaining()` управляется вызывающей
        // стороной и может быть больше `scratch.len()` — без `.min(..)`
        // `&mut scratch[..want]` индексирует за пределы и паникует.
        let (mut client, mut server) = connected_pair();
        let len = SCRATCH + 137;
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let writer = {
            let data = data.clone();
            std::thread::spawn(move || server.write_all(&data).unwrap())
        };

        let mut out = Vec::new();
        let mut store = vec![0u8; len];
        loop {
            let mut rb = hyper::rt::ReadBuf::new(&mut store);
            std::future::poll_fn(|cx| Pin::new(&mut client).poll_read(cx, rb.unfilled()))
                .await
                .unwrap();
            let filled = rb.filled().to_vec();
            if filled.is_empty() {
                break;
            }
            out.extend_from_slice(&filled);
            if out.len() >= len {
                break;
            }
        }
        writer.join().unwrap();
        assert_eq!(out, data);
    }

    #[tokio::test]
    async fn is_write_vectored_delegates_to_the_inner_stream() {
        let (client, _server) = connected_pair();
        // `TcpStream::is_write_vectored` — честная делегация, не
        // консервативная заглушка: сверяем с самим `TcpStream`, а не
        // жёстко закодированным значением, чтобы тест не разошёлся с
        // платформой независимо от `TokioIo`.
        assert_eq!(
            hyper::rt::Write::is_write_vectored(&client),
            client.get_ref().is_write_vectored()
        );
    }
}
