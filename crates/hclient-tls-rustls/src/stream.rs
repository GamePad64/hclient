use hyper::rt::{Read, ReadBufCursor, Write};
use std::io::{Read as _, Write as _};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

const SCRATCH: usize = 16 * 1024;

/// TLS over any `hyper::rt` transport.
///
/// Built on the rustls surface that's been stable since 0.20: `read_tls` /
/// `process_new_packets` / `wants_write` / `write_tls`. **Not**
/// `unbuffered` — that was removed on rustls main (PR #2905, 2026-02-06),
/// and an adapter built on it would have to be rewritten wholesale for
/// 0.24.
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

/// Bridges `hyper::rt::Write` (async, poll-based) → `std::io::Write`
/// (synchronous, blocking) — the interface `ClientConnection::write_tls`
/// is written against. `Poll::Pending` becomes `Err(WouldBlock)`; a
/// caller that gets `WouldBlock` must tell it apart from a real error and
/// return `Poll::Pending` itself, rather than propagating an error
/// upward.
///
/// The same technique `tokio-rustls` uses to close exactly this same gap
/// (`common/mod.rs::SyncWriteAdapter`, `tokio-rustls-0.26.4`) — not
/// invented for this fix, but a proven pattern carried over. What's
/// critical here is that `write_tls` is implemented via
/// `ChunkVecBuffer::write_to` (`rustls-0.23.43 src/vecbuf.rs`), which does
/// `wr.write_vectored(bufs)?` and advances the internal queue
/// (`self.consume(used)`) STRICTLY by what `wr` returned — if `wr`
/// returns `Err` (including our `WouldBlock`), the `?` aborts `write_tls`
/// BEFORE `consume`, and rustls's internal queue is left untouched no
/// matter how many times this repeats. Previously, a bare `Vec<u8>` sat
/// between rustls and the transport — `impl Write for Vec<u8>` never
/// fails and has no way to say "no, I didn't take it all," so
/// `write_tls` unconditionally decided it had written everything (and
/// advanced the sequence/nonce accordingly), while the transport itself
/// might never see those bytes at all if the next step (draining the
/// `Vec` into the transport) returned `Pending` — the `Vec` would simply
/// be lost along with the bytes on an early return.
struct PollWriter<'a, 'cx, S> {
    io: &'a mut S,
    cx: &'a mut Context<'cx>,
}

impl<S: Write + Unpin> std::io::Write for PollWriter<'_, '_, S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match Pin::new(&mut *self.io).poll_write(self.cx, buf) {
            // `hyper::rt::Write::poll_write`'s own contract: "A return
            // value of `0` means that the underlying object is no longer
            // able to accept bytes" — a terminal failure, not a "try
            // again later" signal (that's what `Pending` is for). The
            // same interpretation `flush_outgoing` used to apply
            // (`WriteZero`), just needed here now instead of outside.
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

/// Drains everything rustls wants to write into the underlying transport.
///
/// Bytes already pulled out of rustls via `write_tls` can never be lost
/// to `Pending`: `write_tls` is called directly against the `PollWriter`
/// bridge, not against an intermediate buffer — if the transport isn't
/// ready to accept a single byte, `write_tls` returns an error (caught
/// below as `WouldBlock`) BEFORE advancing its queue, so there's nothing
/// to lose — rustls's queue (`wants_write()`) is left exactly as it was
/// before the call.
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

/// Reads from the transport and feeds rustls. `Ok(false)` — the raw
/// transport hit EOF on this read (0 bytes from `poll_read`); `Ok(true)`
/// — something was actually read.
///
/// EOF is handed to rustls EXPLICITLY, through the same `read_tls` call
/// that carries ordinary bytes — it is not intercepted beforehand, before
/// rustls gets to see it. `read_tls` on a 0-byte read sets the internal
/// `has_seen_eof` flag (`rustls-0.23.43 src/conn.rs:776`), and it is
/// exactly this flag that tells a raw TCP close without `close_notify`
/// apart from a genuine `close_notify`: `Reader::check_no_bytes_state`
/// (`src/conn.rs:183`), on the NEXT call to `conn.reader().read()`,
/// returns `Err(UnexpectedEof)` for the first case and `Ok(0)` for the
/// second — but only if `read_tls` ever saw that 0-byte outcome at all.
/// Intercepting `filled.is_empty()` IMMEDIATELY and returning `Ok(false)`,
/// never once passing an empty read through to rustls, is a
/// truncation-attack hole: a bare TCP FIN with no `close_notify` then
/// resolves identically to a genuine `close_notify` at the
/// `TlsStream::poll_read` level, because rustls's own built-in distinction
/// never runs. The fix does not belong
/// here: this function's job is to honestly hand rustls what it knows how
/// to tell apart, not to decide on its behalf that "nothing to read" and
/// "the connection was cut without warning" are the same thing.
/// Tolerating servers that close without `close_notify` (there are plenty
/// of those in practice), if that's ever needed, is a decision for the
/// HTTP layer on top of `hyper::rt::Read`, which knows about framing
/// (`Content-Length`/chunked) and can tell "the body was already read in
/// full, TLS was cut AFTER" apart from "the body was cut mid-stream" —
/// this stream cannot and must not guess that on its own.
pub(crate) fn pump_incoming<S: Read + Unpin>(
    io: &mut S,
    conn: &mut rustls::ClientConnection,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<bool>> {
    let mut scratch = [0u8; SCRATCH];
    let mut rb = hyper::rt::ReadBuf::new(&mut scratch);
    ready!(Pin::new(io).poll_read(cx, rb.unfilled()))?;
    let filled = rb.filled();
    let had_bytes = !filled.is_empty();
    let mut cursor = std::io::Cursor::new(filled);
    // do-while: even an empty (EOF) slice must reach `read_tls` at least
    // once — a plain `while (pos as usize) < filled.len()` would skip the
    // loop body entirely when `filled.len() == 0`, reintroducing the old
    // bug.
    loop {
        conn.read_tls(&mut cursor).map_err(tls_err)?;
        conn.process_new_packets().map_err(tls_err)?;
        if (cursor.position() as usize) >= filled.len() {
            break;
        }
    }
    Poll::Ready(Ok(had_bytes))
}

impl<S: Read + Write + Unpin> Read for TlsStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        loop {
            // 1. Hand back whatever's already decrypted.
            let mut scratch = [0u8; SCRATCH];
            let want = buf.remaining().min(SCRATCH);
            if want == 0 {
                return Poll::Ready(Ok(()));
            }
            match this.conn.reader().read(&mut scratch[..want]) {
                // rustls guarantees `Ok(0)` STRICTLY on a clean
                // `close_notify` (`has_received_close_notify`) — a
                // terminal "there will be no more data" signal, not "no
                // data yet" (that's what the `WouldBlock` arm below is
                // for). These two used to collapse into the same "read
                // the transport again" — if the peer sent `close_notify`
                // but didn't close the TCP socket itself (TLS doesn't
                // require that), `poll_read` waited on a transport read
                // that would never come: a permanent hang.
                Ok(0) => return Poll::Ready(Ok(())),
                Ok(n) => {
                    buf.put_slice(&scratch[..n]);
                    return Poll::Ready(Ok(()));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Poll::Ready(Err(e)),
            }
            // 2. Flush everything outgoing (renegotiation, close_notify, etc).
            ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;
            // 3. Read more from the transport. No short-circuit on "the
            // raw transport returned 0 bytes" — `pump_incoming` has
            // already fed that outcome to rustls (see its doc comment),
            // and the next loop iteration goes back to step 1, where
            // `conn.reader().read()` now honestly tells `close_notify`
            // (`Ok(0)`) apart from a raw cut without one
            // (`Err(UnexpectedEof)`) — that distinction is neither needed
            // nor allowed to be made here.
            ready!(pump_incoming(&mut this.io, &mut this.conn, cx))?;
        }
    }
}

impl<S: Read + Write + Unpin> Write for TlsStream<S> {
    /// The order here is critical: flush
    /// the leftover from the previous call before touching this call's
    /// `data` — otherwise `conn.writer().write(data)` below would queue
    /// the same bytes AGAIN on top of ones not yet sent from last time.
    /// The `hyper::rt::Write` contract requires repeating `poll_write`
    /// with the SAME `data` after a `Pending`, and `rustls::Writer::write`
    /// is not idempotent — every call unconditionally buffers and
    /// encrypts new bytes, with no deduplication (`Writer::write`'s doc:
    /// "buffers plaintext sent... and sends it as soon as it can" — not a
    /// word there about repeated calls with already-seen bytes, because
    /// from rustls's point of view that's simply NEW data). The same
    /// class of desync as the original `flush_outgoing` bug, just at the
    /// plaintext level instead of ciphertext — the original shape of the
    /// code was vulnerable to this independently of the `flush_outgoing`
    /// fix above: `ready!(flush_outgoing(...))` placed RIGHT AFTER
    /// `conn.writer().write(data)` would propagate a `Pending` upward
    /// AFTER `data` had already been queued — meaning the
    /// `flush_outgoing` fix on its own (see `PollWriter` above) doesn't
    /// lose bytes, but doesn't stop them from being queued a second time
    /// on a retry with the same `data`, should the same function land in
    /// this call order again.
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = &mut *self;
        ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;

        let n = this.conn.writer().write(data)?;
        if n == 0 && !data.is_empty() {
            // rustls's internal outgoing-plaintext buffer
            // (`set_buffer_limit`, 64 KiB by default,
            // `common_state::DEFAULT_BUFFER_LIMIT`) is full — temporary
            // backpressure at the rustls level, NOT "the transport will
            // never accept another byte," which is what `Ok(0)` means in
            // the `hyper::rt::Write::poll_write` contract. The only way
            // to free up room is to flush what's already queued to the
            // transport.
            return match flush_outgoing(&mut this.io, &mut this.conn, cx) {
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) | Poll::Pending => Poll::Pending,
            };
        }

        // Best-effort: `n` bytes have already been accepted AND encrypted
        // by rustls, which is guaranteed to send them itself once it can
        // (the drain step at the start of the next call will confirm
        // this) — it can no longer lose them (see `PollWriter`/
        // `flush_outgoing` above). So we report `Ready(Ok(n))` regardless
        // of whether the flush reached the transport right now;
        // propagating `Pending` here would force the caller to repeat
        // `poll_write` with the SAME `data` bytes — see the method's doc
        // comment.
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
