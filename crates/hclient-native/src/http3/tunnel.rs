//! An extended CONNECT (RFC 9220) to a proxy spoken to over HTTP/3, the
//! stream it opens, and the stream's HTTP datagrams (RFC 9297).
//!
//! The connection is [`H3::tunnel_connection`](crate::http3::H3)'s: one
//! per tunnel, with its own SETTINGS, and its h3 driver spawned the way
//! every connection of the QUIC arm's is. So unlike the HTTP/2 half,
//! nothing here drives a connection: the stream's halves and the datagram
//! path each register the waker of whoever polls them, directly with quinn
//! or through [`Waiters`], and whichever task reads, writes, sends or
//! receives hears what it waits for.

use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker, ready};

use bytes::{Buf as _, Bytes};

/// The client handle a tunnel keeps: `h3` closes the connection when the
/// last one is dropped.
pub(crate) type SendRequest = ::h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;
/// The CONNECT stream, before it is split.
pub(crate) type Stream = ::h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;
type Writer = ::h3::client::RequestStream<h3_quinn::SendStream<Bytes>, Bytes>;
type Reader = ::h3::client::RequestStream<h3_quinn::RecvStream, Bytes>;

/// One write to the request stream, owning the half it writes to and
/// handing it back.
type Op = Pin<
    Box<dyn Future<Output = (Box<Writer>, Result<(), ::h3::error::StreamError>)> + Send>, // send-bound-exception: amendment-C15
>;

/// One `read_datagram`, owning a handle on the connection.
type Read = Pin<
    Box<dyn Future<Output = Result<Bytes, quinn::ConnectionError>> + Send>, // send-bound-exception: amendment-C15
>;

fn stream_io_error(e: &::h3::error::StreamError) -> io::Error {
    io::Error::other(e.to_string())
}

/// Where the write half is.
enum Write {
    /// Nothing in flight. Boxed, so the states in flight are not the
    /// size of the one that is not.
    Idle(Box<Writer>),
    /// A DATA frame being written; the bytes were already accepted.
    Busy(Op),
    /// The end of the request stream being written.
    Finishing(Op),
    /// Ended.
    Shut,
}

/// The CONNECT stream as a byte stream: the request body is what is
/// written, the response body what is read, and a half-close ends the
/// request stream.
///
/// A write is accepted once its DATA frame is handed to `h3`, and finished
/// by the next write, flush or shutdown — which is where a failure of it is
/// reported. The halves are `h3`'s own, so a reader and a writer on
/// different tasks each register with quinn for what they wait on.
pub(crate) struct H3Stream {
    reader: Reader,
    /// What is left of the last DATA frame read, not yet handed out.
    chunk: Bytes,
    write: Write,
    /// Held so the connection stays open for as long as the stream does.
    _send: Mutex<SendRequest>,
}

impl H3Stream {
    pub(crate) fn new(stream: Stream, send: SendRequest) -> Self {
        let (writer, reader) = stream.split();
        Self {
            reader,
            chunk: Bytes::new(),
            write: Write::Idle(Box::new(writer)),
            _send: Mutex::new(send),
        }
    }

    /// Finish whatever write is in flight.
    fn settle(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let (op, finishing) = match &mut self.write {
            Write::Busy(op) => (op, false),
            Write::Finishing(op) => (op, true),
            Write::Idle(_) | Write::Shut => return Poll::Ready(Ok(())),
        };
        let (writer, r) = ready!(op.as_mut().poll(cx));
        self.write = if finishing {
            drop(writer);
            Write::Shut
        } else {
            Write::Idle(writer)
        };
        Poll::Ready(r.map_err(|e| stream_io_error(&e)))
    }

    fn end(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.settle(cx))?;
        if let Write::Idle(_) = self.write {
            let Write::Idle(mut w) = std::mem::replace(&mut self.write, Write::Shut) else {
                unreachable!("matched just above")
            };
            self.write = Write::Finishing(Box::pin(async move {
                let r = w.finish().await;
                (w, r)
            }));
            ready!(self.settle(cx))?;
        }
        Poll::Ready(Ok(()))
    }
}

impl fmt::Debug for H3Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("H3Stream")
    }
}

impl futures_io::AsyncRead for H3Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        loop {
            if !this.chunk.is_empty() {
                let n = buf.len().min(this.chunk.len());
                buf[..n].copy_from_slice(&this.chunk[..n]);
                this.chunk.advance(n);
                return Poll::Ready(Ok(n));
            }
            match ready!(this.reader.poll_recv_data(cx)) {
                Ok(Some(mut data)) => this.chunk = data.copy_to_bytes(data.remaining()),
                Ok(None) => return Poll::Ready(Ok(0)),
                Err(e) => return Poll::Ready(Err(stream_io_error(&e))),
            }
        }
    }
}

impl futures_io::AsyncWrite for H3Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        ready!(this.settle(cx))?;
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let Write::Idle(_) = this.write else {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        };
        let Write::Idle(mut w) = std::mem::replace(&mut this.write, Write::Shut) else {
            unreachable!("matched just above")
        };
        let data = Bytes::copy_from_slice(buf);
        this.write = Write::Busy(Box::pin(async move {
            let r = w.send_data(data).await;
            (w, r)
        }));
        // Polled once so the frame is on its way now; a write still in
        // flight is finished by the next call, which is where its failure
        // would be reported. `Ok(n)` here is "accepted", not "on the
        // wire" — the contract's own shape for a buffered writer, whose
        // `poll_flush`/`poll_close` settle what is staged; both route
        // through [`Self::settle`], so a staged failure reaches the
        // driver's close as well as its next write. What no driver can
        // answer is a stream dropped without a close, which is the
        // contract's abandonment, not this impl's silence.
        match this.settle(cx) {
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) | Poll::Pending => Poll::Ready(Ok(buf.len())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().settle(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().end(cx)
    }
}

impl hclient_rt::Shutdown for H3Stream {
    /// The end of the request stream: the proxy goes on sending, and the
    /// stream goes on being read.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().end(cx)
    }
}

/// Everyone parked on [`H3Datagrams::poll_recv`], woken together.
///
/// The one `read_datagram` in flight registers a single waker with quinn,
/// and more than one task may be waiting on it; polling it with this one
/// wakes them all, so nobody depends on having been the last to poll.
#[derive(Default)]
struct Waiters(Mutex<Vec<Waker>>);

impl Waiters {
    fn register(&self, waker: &Waker) {
        let mut v = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !v.iter().any(|w| w.will_wake(waker)) {
            v.push(waker.clone());
        }
    }
}

impl Wake for Waiters {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let taken = std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        for w in taken {
            w.wake();
        }
    }
}

/// The CONNECT stream's HTTP datagrams, as a path: each carries the
/// stream's quarter stream id as a prefix, written here on the way out and
/// checked and stripped here on the way in.
pub(crate) struct H3Datagrams {
    conn: quinn::Connection,
    quarter: u64,
    prefix: Vec<u8>,
    read: Mutex<Option<Read>>,
    waiters: Arc<Waiters>,
    waker: Waker,
    /// Held so the connection stays open for as long as the path does.
    _send: Mutex<SendRequest>,
}

impl H3Datagrams {
    /// The datagrams of the stream `stream_id` on `conn` — a
    /// client-initiated bidirectional stream, whose id is a multiple of
    /// four, so the quarter is exact.
    pub(crate) fn new(conn: quinn::Connection, stream_id: u64, send: SendRequest) -> Self {
        let quarter = stream_id / 4;
        let mut prefix = Vec::with_capacity(8);
        put_varint(&mut prefix, quarter);
        let waiters = Arc::new(Waiters::default());
        Self {
            conn,
            quarter,
            prefix,
            read: Mutex::new(None),
            waker: Waker::from(Arc::clone(&waiters)),
            waiters,
            _send: Mutex::new(send),
        }
    }

    fn start_read(&self) -> Read {
        let conn = self.conn.clone();
        Box::pin(async move { conn.read_datagram().await })
    }
}

impl fmt::Debug for H3Datagrams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("H3Datagrams")
            .field("quarter", &self.quarter)
            .finish_non_exhaustive()
    }
}

impl hclient_proxy::DatagramPath for H3Datagrams {
    fn try_send(&self, datagram: &[u8]) -> io::Result<()> {
        if datagram.len() > self.max_datagram_size() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a datagram longer than the tunnel carries",
            ));
        }
        let mut frame = Vec::with_capacity(self.prefix.len() + datagram.len());
        frame.extend_from_slice(&self.prefix);
        frame.extend_from_slice(datagram);
        self.conn
            .send_datagram(Bytes::from(frame))
            .map_err(|e| match e {
                quinn::SendDatagramError::TooLarge => {
                    io::Error::new(io::ErrorKind::InvalidInput, e)
                }
                quinn::SendDatagramError::UnsupportedByPeer
                | quinn::SendDatagramError::Disabled => {
                    io::Error::new(io::ErrorKind::Unsupported, e)
                }
                quinn::SendDatagramError::ConnectionLost(_) => {
                    io::Error::new(io::ErrorKind::ConnectionAborted, e)
                }
            })
    }

    // quinn queues a datagram rather than refusing it, dropping the oldest
    // where its buffer is full — so a send never waits, and the only thing
    // this can report is the connection's end.
    fn poll_writable(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.conn.close_reason() {
            Some(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionAborted, e))),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        self.waiters.register(cx.waker());
        let mut slot = self.read.lock().unwrap_or_else(PoisonError::into_inner);
        let mut own = Context::from_waker(&self.waker);
        loop {
            let read = slot.get_or_insert_with(|| self.start_read());
            let frame = match ready!(read.as_mut().poll(&mut own)) {
                Ok(frame) => frame,
                Err(e) => {
                    *slot = None;
                    return Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionAborted, e)));
                }
            };
            *slot = None;
            // Another stream's, or too short to carry a quarter stream id
            // at all: not this path's, and dropped rather than failed.
            let Some((quarter, header)) = get_varint(&frame) else {
                continue;
            };
            if quarter != self.quarter {
                continue;
            }
            let payload = &frame[header..];
            let n = payload.len().min(buf.len());
            buf[..n].copy_from_slice(&payload[..n]);
            return Poll::Ready(Ok(n));
        }
    }

    fn max_datagram_size(&self) -> usize {
        self.conn
            .max_datagram_size()
            .unwrap_or(0)
            .saturating_sub(self.prefix.len())
    }
}

/// QUIC's variable-length integer, RFC 9000 §16: 1, 2, 4 or 8 bytes, the
/// top two bits of the first saying which. A value of `2^62` or more has no
/// encoding, and a quarter stream id never is one.
fn put_varint(buf: &mut Vec<u8>, v: u64) {
    let b = v.min((1 << 62) - 1).to_be_bytes();
    if v < 1 << 6 {
        buf.push(b[7]);
    } else if v < 1 << 14 {
        buf.push(0x40 | b[6]);
        buf.push(b[7]);
    } else if v < 1 << 30 {
        buf.push(0x80 | b[4]);
        buf.extend_from_slice(&b[5..]);
    } else {
        buf.push(0xc0 | b[0]);
        buf.extend_from_slice(&b[1..]);
    }
}

/// One variable-length integer and how many bytes it took, or `None` for a
/// buffer that ends inside one.
fn get_varint(buf: &[u8]) -> Option<(u64, usize)> {
    let first = *buf.first()?;
    let len = 1usize << (first >> 6);
    let rest = buf.get(1..len)?;
    Some((
        rest.iter()
            .fold(u64::from(first & 0x3f), |v, b| (v << 8) | u64::from(*b)),
        len,
    ))
}

#[cfg(test)]
mod tests {
    use super::{get_varint, put_varint};

    /// Each boundary where the encoding grows, RFC 9000 §16's own table.
    #[test]
    fn varints_grow_at_the_rfcs_boundaries() {
        for (v, want) in [
            (0u64, &[0x00][..]),
            (63, &[0x3f]),
            (64, &[0x40, 0x40]),
            (16383, &[0x7f, 0xff]),
            (16384, &[0x80, 0x00, 0x40, 0x00]),
            (1 << 30, &[0xc0, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00]),
        ] {
            let mut buf = Vec::new();
            put_varint(&mut buf, v);
            assert_eq!(buf, want, "{v}");
            assert_eq!(get_varint(&buf), Some((v, want.len())), "{v}");
        }
    }

    /// RFC 9000 §A.1's worked examples, read back.
    #[test]
    fn varints_read_the_rfcs_examples() {
        assert_eq!(
            get_varint(&[0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c]),
            Some((151_288_809_941_952_652, 8))
        );
        assert_eq!(
            get_varint(&[0x9d, 0x7f, 0x3e, 0x7d]),
            Some((494_878_333, 4))
        );
        assert_eq!(get_varint(&[0x7b, 0xbd]), Some((15293, 2)));
        assert_eq!(get_varint(&[0x25]), Some((37, 1)));
    }

    #[test]
    fn a_buffer_ending_inside_a_varint_is_none() {
        assert_eq!(get_varint(&[]), None);
        assert_eq!(get_varint(&[0x40]), None);
        assert_eq!(get_varint(&[0x80, 0, 0]), None);
    }
}
