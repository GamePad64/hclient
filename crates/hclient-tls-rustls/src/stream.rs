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
///
/// **This one will be rewritten for 0.24 too, only less.** Read in
/// `0.24.0-dev.1` rather than assumed: `unbuffered` is gone as predicted,
/// and so is `read_tls` — `process_new_packets` takes a caller-owned
/// `&mut dyn TlsInputBuffer` instead. That moves the ciphertext buffer from
/// rustls into this type, which is roughly what `Pending` already is, so
/// the change is to the read half's plumbing rather than its shape.
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
    /// A caller that got `Pending` repeats `poll_write` with the SAME
    /// `data` — `futures_io::AsyncWrite`'s contract, as it was
    /// `hyper::rt`'s — and `rustls::Writer::write` is not idempotent — every call unconditionally buffers and
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
            // the `futures_io::AsyncWrite::poll_write` contract. The only way
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
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        this.conn.send_close_notify();
        ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;
        Pin::new(&mut this.io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod in_memory {
    //! `TlsStream` over an in-memory transport, driven by hand against a
    //! rustls server — so each property below happens on purpose rather than
    //! when a kernel happens to cut a read in the right place.
    //!
    //! This module exists because a mutation sweep found the stream's
    //! backpressure, write, flush, close and half-close paths all
    //! replaceable with the suite green. The loopback tests in `tests/` go
    //! through real sockets and a real runtime, which is what makes them
    //! worth having and also what keeps them from choosing *which* path is
    //! taken.

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// One direction of an in-memory pipe.
    type Pipe = Arc<Mutex<Vec<u8>>>;

    /// What the transport beneath the stream was asked to do, and how it
    /// answers writes.
    #[derive(Default)]
    struct Transport {
        /// Writes answered `Pending` before the transport accepts any.
        refuse_writes: AtomicUsize,
        /// Every write answered `Ready(Ok(0))` — a transport that will
        /// never take another byte.
        write_zero: std::sync::atomic::AtomicBool,
        shutdowns: AtomicUsize,
        closes: AtomicUsize,
    }

    /// The client's end: reads from `inbound`, writes to `outbound`, and is
    /// `Pending` rather than at end-of-stream when there is nothing to read,
    /// because the test drives the peer by hand between polls.
    struct Mem {
        inbound: Pipe,
        outbound: Pipe,
        t: Arc<Transport>,
    }

    impl Read for Mem {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            let mut inbound = self.inbound.lock().unwrap();
            if inbound.is_empty() {
                return Poll::Pending;
            }
            let n = buf.len().min(inbound.len());
            buf[..n].copy_from_slice(&inbound[..n]);
            inbound.drain(..n);
            Poll::Ready(Ok(n))
        }
    }

    impl Write for Mem {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.t.write_zero.load(Ordering::SeqCst) {
                return Poll::Ready(Ok(0));
            }
            if self
                .t
                .refuse_writes
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return Poll::Pending;
            }
            self.outbound.lock().unwrap().extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            self.t.closes.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }
    }

    impl Shutdown for Mem {
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            self.t.shutdowns.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }
    }

    /// A handshaken client stream and the server at the other end.
    struct Pair {
        stream: TlsStream<Mem>,
        server: rustls::ServerConnection,
        to_client: Pipe,
        to_server: Pipe,
        t: Arc<Transport>,
    }

    impl Pair {
        fn new() -> Self {
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let server_cfg = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.cert.der().clone()],
                    rustls_pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into()),
                )
                .unwrap();
            let mut server = rustls::ServerConnection::new(Arc::new(server_cfg)).unwrap();
            let mut roots = rustls::RootCertStore::empty();
            roots.add(cert.cert.der().clone()).unwrap();
            let client = crate::Rustls::from_config(Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            ));

            let to_client: Pipe = Arc::default();
            let to_server: Pipe = Arc::default();
            let t = Arc::new(Transport::default());
            let io = Mem {
                inbound: Arc::clone(&to_client),
                outbound: Arc::clone(&to_server),
                t: Arc::clone(&t),
            };
            let mut cx = Context::from_waker(std::task::Waker::noop());
            let mut handshake = std::pin::pin!(hclient_tls::TlsConnect::connect(
                &client,
                io,
                hclient_tls::TlsRequest::new("localhost", &[]),
            ));
            let stream = loop {
                match handshake.as_mut().poll(&mut cx) {
                    Poll::Ready(r) => break r.expect("handshake").0,
                    Poll::Pending => exchange(&mut server, &to_server, &to_client),
                }
            };
            let mut pair = Self {
                stream,
                server,
                to_client,
                to_server,
                t,
            };
            // The client's `Finished`, so the server may send data.
            pair.exchange();
            pair
        }

        /// Everything the client wrote goes to the server, and everything
        /// the server has to say goes back.
        fn exchange(&mut self) {
            exchange(&mut self.server, &self.to_server, &self.to_client);
        }

        fn server_flush(&mut self) {
            server_flush(&mut self.server, &self.to_client);
        }

        /// What the server has received as plaintext: `Some(bytes)` while
        /// the stream is open, `None` once `close_notify` has arrived and
        /// nothing precedes it.
        fn server_reads(&mut self) -> Option<Vec<u8>> {
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut self.server.reader(), &mut buf) {
                    Ok(0) if got.is_empty() => return None,
                    Ok(0) => return Some(got),
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Some(got),
                    Err(e) => panic!("server read: {e}"),
                }
            }
        }

        fn client_read_all(&mut self, cx: &mut Context<'_>) -> Vec<u8> {
            let mut got = Vec::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match Pin::new(&mut self.stream).poll_read(cx, &mut buf) {
                    Poll::Ready(Ok(0)) | Poll::Pending => return got,
                    Poll::Ready(Ok(n)) => got.extend_from_slice(&buf[..n]),
                    Poll::Ready(Err(e)) => panic!("after {} bytes: {e}", got.len()),
                }
            }
        }
    }

    fn exchange(server: &mut rustls::ServerConnection, to_server: &Pipe, to_client: &Pipe) {
        let bytes = std::mem::take(&mut *to_server.lock().unwrap());
        let mut cursor = std::io::Cursor::new(bytes);
        while cursor.position() < cursor.get_ref().len() as u64 {
            server.read_tls(&mut cursor).unwrap();
            server.process_new_packets().unwrap();
        }
        server_flush(server, to_client);
    }

    fn server_flush(server: &mut rustls::ServerConnection, to_client: &Pipe) {
        while server.wants_write() {
            server.write_tls(&mut *to_client.lock().unwrap()).unwrap();
        }
    }

    fn cx() -> Context<'static> {
        Context::from_waker(std::task::Waker::noop())
    }

    /// Counts `trace!` events whose message mentions backpressure.
    struct Signals(Arc<AtomicUsize>);

    struct Message(bool);
    impl tracing::field::Visit for Message {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" && format!("{value:?}").contains("backpressure") {
                self.0 = true;
            }
        }
    }

    impl tracing::subscriber::Subscriber for Signals {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::Id {
            tracing::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::Id, _: &tracing::Id) {}
        fn event(&self, e: &tracing::Event<'_>) {
            let mut m = Message(false);
            e.record(&mut m);
            if m.0 {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        fn enter(&self, _: &tracing::Id) {}
        fn exit(&self, _: &tracing::Id) {}
    }

    /// **rustls' backpressure, reached on purpose and not by luck.**
    ///
    /// The regression test for the `act` defect —
    /// `a_pushed_body_larger_than_the_plaintext_buffer_survives_a_slow_reader`
    /// in `tests/adversarial_tls_stream.rs` — never reached this path: a
    /// mutation sweep turned the backpressure arm of `feed` into an error,
    /// and dropped the carried-over tail, and that test stayed green both
    /// times. Over loopback TCP whether a feed crosses rustls' 16 KiB
    /// received-plaintext limit depends on how the kernel cuts the reads.
    ///
    /// Here it does not. The transport hands over exactly as much as
    /// `pump_incoming` asks for, and the server writes 3000-byte records —
    /// about 3022 bytes on the wire — so a 16 KiB read holds five or six
    /// whole records. Six are 18 000 bytes of plaintext against a limit
    /// `is_full` compares with `>`, so the feed stops part-way and the
    /// remainder must survive to the next poll. The phase drifts by 16 384
    /// mod 3022 per read, so over a mebibyte that happens many times.
    ///
    /// And the claim that it happened is not inferred: `feed` emits a
    /// `trace!` line at exactly that moment, and a subscriber counts it.
    /// This replaces a test that emitted its own `trace!` and counted that,
    /// which could not fail.
    #[test]
    fn a_feed_stopped_by_backpressure_loses_nothing_and_says_so() {
        const BODY: usize = 1024 * 1024;
        const RECORD: usize = 3000;
        let mut p = Pair::new();

        let body: Vec<u8> = (0..BODY).map(|i| u8::try_from(i % 251).unwrap()).collect();
        for chunk in body.chunks(RECORD) {
            std::io::Write::write_all(&mut p.server.writer(), chunk).unwrap();
            p.server_flush();
        }
        p.server.send_close_notify();
        p.server_flush();

        let signals = Arc::new(AtomicUsize::new(0));
        let got = tracing::subscriber::with_default(Signals(Arc::clone(&signals)), || {
            p.client_read_all(&mut cx())
        });

        assert!(
            signals.load(Ordering::SeqCst) > 0,
            "rustls never signalled backpressure, so this test measured nothing about it"
        );
        assert_eq!(got.len(), BODY, "the whole body must come back");
        assert!(got == body, "and byte for byte");
    }

    /// **The half-close is `close_notify` and then the transport's own
    /// `poll_shutdown` — never its `poll_close` — and the read half stays
    /// open.** That is `hclient_rt::Shutdown`'s whole promise, and what an
    /// HTTP/1 client relies on to send FIN and still read the response.
    #[test]
    fn shutdown_sends_close_notify_half_closes_the_transport_and_keeps_reading() {
        let mut p = Pair::new();
        let Poll::Ready(Ok(())) = Pin::new(&mut p.stream).poll_shutdown(&mut cx()) else {
            panic!("an in-memory shutdown completes at once");
        };
        assert_eq!(
            p.t.shutdowns.load(Ordering::SeqCst),
            1,
            "the transport half-closed"
        );
        assert_eq!(p.t.closes.load(Ordering::SeqCst), 0, "and was not closed");

        p.exchange();
        assert_eq!(p.server_reads(), None, "the server received close_notify");

        // The other direction is still open: the server answers after the
        // client's close_notify, and the client reads it.
        std::io::Write::write_all(&mut p.server.writer(), b"after").unwrap();
        p.server_flush();
        assert_eq!(p.client_read_all(&mut cx()), b"after");
    }

    /// `futures-io`'s `poll_close` is the same half-close here, forwarded
    /// rather than implemented twice — see its doc.
    #[test]
    fn close_is_the_same_half_close() {
        let mut p = Pair::new();
        let Poll::Ready(Ok(())) = Pin::new(&mut p.stream).poll_close(&mut cx()) else {
            panic!("an in-memory close completes at once");
        };
        assert_eq!(p.t.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(p.t.closes.load(Ordering::SeqCst), 0);
        p.exchange();
        assert_eq!(p.server_reads(), None, "the server received close_notify");
    }

    /// A write the transport could not take yet is **accepted** — rustls
    /// has encrypted it and will not lose it — and `poll_flush` is what
    /// puts it on the wire.
    #[test]
    fn flush_delivers_what_a_refused_write_left_queued() {
        let mut p = Pair::new();
        p.t.refuse_writes.store(1, Ordering::SeqCst);
        let Poll::Ready(Ok(5)) = Pin::new(&mut p.stream).poll_write(&mut cx(), b"hello") else {
            panic!("the write is accepted even though the transport was not ready");
        };
        assert!(
            p.to_server.lock().unwrap().is_empty(),
            "nothing reached the transport yet"
        );

        let Poll::Ready(Ok(())) = Pin::new(&mut p.stream).poll_flush(&mut cx()) else {
            panic!("the transport is ready now, so the flush completes");
        };
        p.exchange();
        assert_eq!(p.server_reads().as_deref(), Some(&b"hello"[..]));
    }

    /// An empty write is an empty answer, not a `Pending` nobody will wake.
    #[test]
    fn an_empty_write_is_ready_at_once() {
        let mut p = Pair::new();
        let Poll::Ready(Ok(0)) = Pin::new(&mut p.stream).poll_write(&mut cx(), b"") else {
            panic!("an empty write must complete immediately");
        };
    }

    /// A transport answering `Ok(0)` to a non-empty write will never take
    /// another byte, per `futures_io::AsyncWrite`'s own contract — so that
    /// is `WriteZero`, not a `wants_write` loop that spins forever.
    #[test]
    ///
    /// On a thread with a bound, because the defect this guards against
    /// is a spin inside one poll — no `Pending`, nothing a timeout around
    /// a future could interrupt.
    fn a_transport_that_takes_nothing_is_write_zero() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut p = Pair::new();
            p.t.write_zero.store(true, Ordering::SeqCst);
            let answer = match Pin::new(&mut p.stream).poll_write(&mut cx(), b"hello") {
                Poll::Ready(Err(e)) => Ok(e.kind()),
                other => Err(format!("{other:?}")),
            };
            let _ = tx.send(answer);
        });
        let answer = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("poll_write spun on a transport answering Ok(0) instead of failing");
        assert_eq!(answer, Ok(std::io::ErrorKind::WriteZero));
    }
}
