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

/// Прокачать всё, что rustls хочет записать, в нижележащий транспорт.
pub(crate) fn flush_outgoing<S: Write + Unpin>(
    io: &mut S,
    conn: &mut rustls::ClientConnection,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<()>> {
    while conn.wants_write() {
        let mut buf = Vec::new();
        conn.write_tls(&mut buf).map_err(tls_err)?;
        let mut written = 0;
        while written < buf.len() {
            let n = ready!(Pin::new(&mut *io).poll_write(cx, &buf[written..]))?;
            if n == 0 {
                return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()));
            }
            written += n;
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
                Ok(0) => {}
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
            } // EOF
        }
    }
}

impl<S: Read + Write + Unpin> Write for TlsStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = &mut *self;
        let n = this.conn.writer().write(data)?;
        ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;
        Poll::Ready(Ok(n))
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
