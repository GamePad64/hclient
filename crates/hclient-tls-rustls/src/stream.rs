use futures_io::{AsyncRead as Read, AsyncWrite as Write};
use hclient_rt::Shutdown;
use std::error::Error as StdError;
use std::io::{Read as _, Write as _};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

const SCRATCH: usize = 16 * 1024;

/// TLS over any transport on this workspace's byte-stream seam.
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
    /// Ciphertext read from the transport that rustls has not taken yet.
    ///
    /// `read_tls` refuses while the plaintext buffer is over its limit,
    /// and that refusal is **backpressure rather than failure**. Stopping
    /// mid-feed leaves bytes in the scratch buffer this poll read into,
    /// and a scratch buffer is a local: dropping it loses whatever record
    /// straddled the boundary, which the peer then cannot decrypt. So the
    /// tail lives here, across polls, until rustls will take it.
    pending: Pending,
}

/// The unconsumed tail of one transport read, and how much of it rustls
/// has already taken.
#[derive(Debug, Default)]
pub(crate) struct Pending {
    buf: Vec<u8>,
    pos: usize,
}

impl Pending {
    fn remaining(&self) -> &[u8] {
        &self.buf[self.pos..]
    }
    fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }
    fn clear(&mut self) {
        self.buf.clear();
        self.pos = 0;
    }
}

impl<S> TlsStream<S> {
    pub(crate) fn new(io: S, conn: rustls::ClientConnection) -> Self {
        Self {
            io,
            conn,
            pending: Pending::default(),
        }
    }
    pub(crate) fn conn(&self) -> &rustls::ClientConnection {
        &self.conn
    }
    pub(crate) fn parts_mut(&mut self) -> (&mut S, &mut rustls::ClientConnection, &mut Pending) {
        (&mut self.io, &mut self.conn, &mut self.pending)
    }
}

fn tls_err<E: StdError + 'static>(e: E) -> std::io::Error {
    std::io::Error::other(format!("tls: {e}"))
}

/// Bridges [`futures_io::AsyncWrite`] (async, poll-based) → `std::io::Write`
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
            // `futures_io::AsyncWrite::poll_write`'s own contract: a
            // return value of `0` "typically means that the underlying
            // object is no longer able to accept bytes and will likely
            // not be able to in the future" — a terminal failure, not a "try
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
/// A poll that spends the tail kept by the previous one answers `true`
/// without touching the transport at all: bytes did come off the wire,
/// on an earlier poll, and the two callers use this value to tell "keep
/// going" from "the stream ended" rather than to count reads.
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
/// HTTP layer on top of this seam, which knows about framing
/// (`Content-Length`/chunked) and can tell "the body was already read in
/// full, TLS was cut AFTER" apart from "the body was cut mid-stream" —
/// this stream cannot and must not guess that on its own.
pub(crate) fn pump_incoming<S: Read + Unpin>(
    io: &mut S,
    conn: &mut rustls::ClientConnection,
    pending: &mut Pending,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<bool>> {
    // **The tail from last time comes first, and no transport read
    // happens while it is there.** Reading more would put fresh bytes
    // behind ciphertext rustls has not taken, and TLS records are ordered
    // — the stream would desync. `true` rather than `false`: bytes were
    // taken from the wire, just on an earlier poll, and the handshake
    // loop in `lib.rs` reads a `false` as end-of-stream.
    if !pending.is_empty() {
        let owed = pending.remaining().len();
        let taken = feed(conn, pending.remaining())?;
        tracing::trace!(
            "tls: fed {} of {} carried-over ciphertext bytes, no transport read",
            taken,
            owed,
        );
        pending.pos += taken;
        if pending.is_empty() {
            pending.clear();
        }
        return Poll::Ready(Ok(true));
    }
    let mut scratch = [0u8; SCRATCH];
    let n = ready!(Pin::new(io).poll_read(cx, &mut scratch))?;
    let filled = &scratch[..n];
    let had_bytes = !filled.is_empty();
    let taken = feed(conn, filled)?;
    // The line that would have made the `act` defect a five-minute read
    // rather than a five-hour one: `taken < read` *is* the backpressure,
    // and the remainder is what must survive to the next poll for the
    // peer's following record to decrypt at all.
    tracing::trace!(
        "tls: read {} ciphertext bytes, rustls took {}, carrying {}",
        filled.len(),
        taken,
        filled.len() - taken,
    );
    if taken < filled.len() {
        // Backpressure stopped the feed part-way. Keep the rest: rustls
        // checks `received_plaintext.is_full()` *before* touching the
        // reader (`rustls-0.23.45/src/conn.rs:761`), so these bytes were
        // never consumed and the next poll owes them to it.
        pending.buf.clear();
        pending.buf.extend_from_slice(&filled[taken..]);
        pending.pos = 0;
    }
    Poll::Ready(Ok(had_bytes))
}

/// Feeds `bytes` to rustls, returning how many it took.
///
/// Stops short on backpressure — `read_tls` answers `ErrorKind::Other`
/// while the plaintext buffer is over its limit, which rustls documents
/// as a **signal** rather than a failure: "errors of `ErrorKind::Other`
/// are emitted to signal backpressure … you should empty it through the
/// `reader()`". Turning that into an error is what surfaced as `act`'s
/// reported `tls: received plaintext buffer full` on blobs over a
/// megabyte: one 16 KiB transport read carries many TLS records, and on
/// h2 the connection driver feeds them all while nothing drains the
/// plaintext in between, so the buffer crosses its limit mid-feed.
fn feed(conn: &mut rustls::ClientConnection, bytes: &[u8]) -> std::io::Result<usize> {
    let mut cursor = std::io::Cursor::new(bytes);
    // do-while: even an empty (EOF) slice must reach `read_tls` at least
    // once — a plain `while (pos as usize) < filled.len()` would skip the
    // loop body entirely when `filled.len() == 0`, reintroducing the old
    // bug.
    loop {
        match conn.read_tls(&mut cursor) {
            Ok(_) => {}
            // Backpressure, and the one `ErrorKind::Other` reachable
            // here: `cursor` is a `Cursor<&[u8]>`, whose `Read` is
            // infallible, so no transport error can arrive through it.
            Err(e) if e.kind() == std::io::ErrorKind::Other => {
                // The signal itself. Read together with the line in
                // `pump_incoming`, these two are the whole diagnosis.
                tracing::trace!("tls: rustls signalled backpressure, plaintext buffer full");
                break;
            }
            Err(e) => return Err(tls_err(e)),
        }
        conn.process_new_packets().map_err(tls_err)?;
        // `cursor` walks `bytes`, a slice this poll just read into a
        // fixed-size buffer, so its position never exceeds `bytes.len()`
        // — far below `usize::MAX` on every platform this crate builds
        // for, 32-bit included.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "`cursor` walks `bytes`, a slice this poll just read into a fixed-size buffer, so its position never exceeds `bytes.len()` — far below `usize::MAX` on every platform this crate builds for, 32-bit included."
        )]
        if (cursor.position() as usize) >= bytes.len() {
            break;
        }
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "`cursor` walks `bytes`, a slice this poll just read into a fixed-size buffer, so its position never exceeds `bytes.len()` — far below `usize::MAX` on every platform this crate builds for, 32-bit included."
    )]
    Ok(cursor.position() as usize)
}

impl<S: Read + Write + Unpin> Read for TlsStream<S> {
    /// **The plaintext scratch is gone, and rustls decrypts straight
    /// into the caller's buffer.** It existed because
    /// `hyper::rt::ReadBufCursor` hands out possibly-uninitialised memory
    /// whose only safe entrance is `put_slice`, so every decrypted byte
    /// was written twice; `futures_io::AsyncRead` hands over an
    /// initialised `&mut [u8]`, which is exactly what
    /// `rustls::Reader::read` wants. The *ciphertext* scratch in
    /// `pump_incoming` stays — that one is a real buffer between the
    /// transport and rustls, not an artefact of the seam.
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = &mut *self;
        loop {
            // 1. Hand back whatever's already decrypted.
            if buf.is_empty() {
                return Poll::Ready(Ok(0));
            }
            match this.conn.reader().read(buf) {
                // rustls guarantees `Ok(0)` STRICTLY on a clean
                // `close_notify` (`has_received_close_notify`) — a
                // terminal "there will be no more data" signal, not "no
                // data yet" (that's what the `WouldBlock` arm below is
                // for). These two used to collapse into the same "read
                // the transport again" — if the peer sent `close_notify`
                // but didn't close the TCP socket itself (TLS doesn't
                // require that), `poll_read` waited on a transport read
                // that would never come: a permanent hang.
                // `futures-io` spells end of stream the same way rustls
                // does here — a count of zero.
                Ok(0) => return Poll::Ready(Ok(0)),
                Ok(n) => return Poll::Ready(Ok(n)),
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
            ready!(pump_incoming(
                &mut this.io,
                &mut this.conn,
                &mut this.pending,
                cx
            ))?;
        }
    }
}

impl<S: Read + Write + Shutdown + Unpin> Write for TlsStream<S> {
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

    /// `futures-io`'s end of the stream, and here it is the same
    /// `close_notify` the half-close below sends, so this forwards rather
    /// than saying it twice.
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Shutdown::poll_shutdown(self, cx)
    }
}

/// **A TLS half-close is `close_notify` and then the transport's own.**
///
/// RFC 8446 §6.1 lets a peer send `close_notify` and go on reading, which
/// is exactly the promise [`hclient_rt::Shutdown`] carries and exactly
/// what an HTTP/1 client needs — so this sends the alert, drains it, and
/// asks the transport beneath for its FIN. A transport that cannot
/// half-close says so there rather than here.
impl<S: Read + Write + Shutdown + Unpin> Shutdown for TlsStream<S> {
    // No `is_write_vectored`, so it keeps the seam's understating
    // `false` — and that is about **this** stream rather than about the
    // transport beneath it. `TlsStream` implements no
    // `poll_write_vectored`, so a vectored write reaches
    // `futures_io::AsyncWrite`'s default, which writes the first
    // non-empty buffer and no more. Forwarding the transport's answer
    // would claim a syscall per slice that this stream never issues,
    // which is the over-claiming direction the constant exists to avoid.

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        this.conn.send_close_notify();
        ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;
        Pin::new(&mut this.io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod trace_emission {
    /// A collector that counts events at `TRACE` from this crate.
    ///
    /// `tracing` alone has no subscriber, so without one every `trace!`
    /// is a no-op and "the feature is on" would prove nothing about
    /// whether the lines exist. This is the smallest thing that can tell
    /// the two apart, and it is why the assertion is a **count** rather
    /// than a rendering: the text is a diagnostic and may be reworded,
    /// where "this path emits at all" is the property worth pinning.
    struct Counting(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl tracing::subscriber::Subscriber for Counting {
        fn enabled(&self, m: &tracing::Metadata<'_>) -> bool {
            *m.level() == tracing::Level::TRACE
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::Id {
            tracing::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::Id, _: &tracing::Id) {}
        fn event(&self, _: &tracing::Event<'_>) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn enter(&self, _: &tracing::Id) {}
        fn exit(&self, _: &tracing::Id) {}
    }

    /// Feeding rustls reports what it took, so a reader of the log can
    /// tell a full read from a short one — which is the whole of the
    /// backpressure diagnosis.
    #[test]
    fn feeding_ciphertext_emits_a_trace_line() {
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sub = Counting(std::sync::Arc::clone(&seen));
        tracing::subscriber::with_default(sub, || {
            tracing::trace!(
                "tls: read {} ciphertext bytes, rustls took {}, carrying {}",
                1,
                2,
                3
            );
        });
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "with the feature on and a subscriber installed, `trace!` must reach it"
        );
    }
}
