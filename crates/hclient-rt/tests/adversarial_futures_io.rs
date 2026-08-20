//! Independent adversarial-review test suite for `FuturesIo` (Task 2 of
//! vertical 2). NOT part of the implementer's work — written from scratch by
//! the reviewer against the public API only (`FuturesIo::new`, and the
//! `hyper::rt::{Read, Write}` impls), to probe behaviour the brief's
//! cooperative-source tests (which always return `Ready` immediately) cannot
//! exercise: real `Pending`, spurious `Ok(0)`, mid-stream errors, and the
//! exact hyper EOF contract at the raw `ReadBufCursor` level.
//!
//! Contract facts used below, read directly from source rather than assumed:
//! - `hyper::rt::io::Read::poll_read` doc (hyper 1.11.0, src/rt/io.rs):
//!   "If no data was read (`buf.remaining()` is unchanged), it implies that
//!   EOF has been reached."
//! - `ReadBufCursor::put_slice` panics if `src.len() > self.remaining()`.
//! - `hyper::proto::h1::io::Buffered::poll_shutdown` (src/proto/h1/io.rs):
//!   `ready!(self.poll_flush(cx))?; Pin::new(&mut self.io).poll_shutdown(cx)`
//!   — hyper itself always flushes its internal write buffer through to the
//!   `hyper::rt::Write` impl before calling `poll_shutdown` on it. So the
//!   shim's own `poll_shutdown` does not need to flush again; it only needs
//!   to forward to `poll_close` without swallowing pending or error results.

use futures_io::{AsyncRead, AsyncWrite};
use hclient_rt::FuturesIo;
use hyper::rt::{Read as HyperRead, ReadBuf, Write as HyperWrite};
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

/// A waker that just records whether it was woken, so `Pending` tests don't
/// need a real executor.
struct RecordingWaker(Mutex<bool>);
impl Wake for RecordingWaker {
    fn wake(self: Arc<Self>) {
        *self.0.lock().unwrap() = true;
    }
    fn wake_by_ref(self: &Arc<Self>) {
        *self.0.lock().unwrap() = true;
    }
}
fn test_waker() -> (Waker, Arc<RecordingWaker>) {
    let rec = Arc::new(RecordingWaker(Mutex::new(false)));
    (Waker::from(rec.clone()), rec)
}

// ---------------------------------------------------------------------
// A. Correctness under adversarial I/O
// ---------------------------------------------------------------------

/// Returns `Pending` on the first `n` polls, then serves `data` in one shot.
struct PendingThenReady {
    pending_polls_left: usize,
    data: Vec<u8>,
    served: bool,
}
impl AsyncRead for PendingThenReady {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.pending_polls_left > 0 {
            self.pending_polls_left -= 1;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        if self.served {
            return Poll::Ready(Ok(0));
        }
        let n = self.data.len().min(buf.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.served = true;
        Poll::Ready(Ok(n))
    }
}

#[test]
fn pending_before_data_is_not_confused_with_eof_or_data() {
    let mut io = FuturesIo::new(PendingThenReady {
        pending_polls_left: 3,
        data: b"after pending".to_vec(),
        served: false,
    });
    let (waker, _rec) = test_waker();
    let mut cx = Context::from_waker(&waker);

    // Three Pending polls: must be genuinely Pending, not Ready(Ok(())) with
    // an empty fill (which would be misread by hyper as EOF and drop a live
    // connection).
    for _ in 0..3 {
        let mut store = [0u8; 64];
        let mut rb = ReadBuf::new(&mut store);
        match HyperRead::poll_read(Pin::new(&mut io), &mut cx, rb.unfilled()) {
            Poll::Pending => {}
            Poll::Ready(r) => panic!("expected Pending, got Ready({r:?})"),
        }
        assert_eq!(rb.filled().len(), 0, "must not fill anything while Pending");
    }

    // Fourth poll: real data arrives.
    let mut store = [0u8; 64];
    let mut rb = ReadBuf::new(&mut store);
    match HyperRead::poll_read(Pin::new(&mut io), &mut cx, rb.unfilled()) {
        Poll::Ready(Ok(())) => {}
        other => panic!("expected Ready(Ok(())), got {other:?}"),
    }
    assert_eq!(rb.filled(), b"after pending");
}

/// Returns `Ok(0)` exactly once (a spurious/misbehaving zero-length read that
/// is not the final EOF), then serves real data on the next poll.
struct SpuriousZeroThenData {
    calls: usize,
    data: Vec<u8>,
}
impl AsyncRead for SpuriousZeroThenData {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.calls += 1;
        if self.calls == 1 {
            return Poll::Ready(Ok(0));
        }
        let n = self.data.len().min(buf.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data.drain(..n);
        Poll::Ready(Ok(n))
    }
}

#[test]
fn ok_zero_mid_stream_is_reported_as_eof_for_that_call_and_shim_keeps_no_stale_state() {
    // futures_io::AsyncRead gives `Ok(0)` the same meaning std::io::Read does
    // (EOF), so a source that does this "mid-stream" is itself misbehaving —
    // but the shim must still (a) report it faithfully as an EOF-shaped
    // Ready(Ok(())) with nothing filled for *that* call, matching hyper's
    // contract exactly, and (b) not cache "we saw EOF" anywhere: it has no
    // field for that, so a later call must re-poll `inner` and can still
    // recover real data, rather than wedging into permanent-EOF.
    let mut io = FuturesIo::new(SpuriousZeroThenData {
        calls: 0,
        data: b"recovered".to_vec(),
    });
    let (waker, _rec) = test_waker();
    let mut cx = Context::from_waker(&waker);

    let mut store = [0u8; 64];
    let mut rb = ReadBuf::new(&mut store);
    let r = HyperRead::poll_read(Pin::new(&mut io), &mut cx, rb.unfilled());
    assert!(matches!(r, Poll::Ready(Ok(()))));
    assert_eq!(
        rb.filled().len(),
        0,
        "Ok(0) must surface as an unchanged buffer (EOF shape)"
    );

    // Shim must not remember "EOF" as sticky internal state: a subsequent
    // call re-polls `inner` and gets real bytes.
    let mut store2 = [0u8; 64];
    let mut rb2 = ReadBuf::new(&mut store2);
    let r2 = HyperRead::poll_read(Pin::new(&mut io), &mut cx, rb2.unfilled());
    assert!(matches!(r2, Poll::Ready(Ok(()))));
    assert_eq!(rb2.filled(), b"recovered");
}

/// One byte per `poll_read` call, driven manually (not through the brief's
/// `read_all` helper) to double check ordering byte-by-byte.
struct OneByteAtATime {
    data: Vec<u8>,
    at: usize,
}
impl AsyncRead for OneByteAtATime {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.at >= self.data.len() {
            return Poll::Ready(Ok(0));
        }
        buf[0] = self.data[self.at];
        self.at += 1;
        Poll::Ready(Ok(1))
    }
}

#[test]
fn one_byte_at_a_time_preserves_order_no_drop_no_duplicate() {
    let msg: Vec<u8> = (0..=255u8).collect(); // every byte value once, order matters
    let mut io = FuturesIo::new(OneByteAtATime {
        data: msg.clone(),
        at: 0,
    });
    let (waker, _rec) = test_waker();
    let mut cx = Context::from_waker(&waker);
    let mut out = Vec::new();
    for _ in 0..(msg.len() * 4 + 10) {
        let mut store = [0u8; 32]; // deliberately not a divisor tricks: exercises boundary too
        let mut rb = ReadBuf::new(&mut store);
        let r = HyperRead::poll_read(Pin::new(&mut io), &mut cx, rb.unfilled());
        assert!(matches!(r, Poll::Ready(Ok(()))));
        if rb.filled().is_empty() {
            break;
        }
        out.extend_from_slice(rb.filled());
        if out.len() >= msg.len() {
            break;
        }
    }
    assert_eq!(
        out, msg,
        "did not terminate/complete within a bounded number of polls"
    );
}

/// Yields a few bytes successfully, then an error — must not lose the
/// already-yielded bytes' correctness, and must propagate the error (not
/// silently reinterpret it as EOF or swallow it).
struct GoodThenError {
    good: Vec<u8>,
    at: usize,
    err_fired: bool,
}
impl AsyncRead for GoodThenError {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.at < self.good.len() {
            let n = (self.good.len() - self.at).min(buf.len()).min(4);
            buf[..n].copy_from_slice(&self.good[self.at..self.at + n]);
            self.at += n;
            return Poll::Ready(Ok(n));
        }
        if !self.err_fired {
            self.err_fired = true;
            return Poll::Ready(Err(io::Error::other("simulated mid-stream failure")));
        }
        Poll::Ready(Ok(0))
    }
}

#[test]
fn error_after_partial_data_is_propagated_not_swallowed_or_confused_with_eof() {
    let mut io = FuturesIo::new(GoodThenError {
        good: b"partial".to_vec(),
        at: 0,
        err_fired: false,
    });
    let (waker, _rec) = test_waker();
    let mut cx = Context::from_waker(&waker);

    let mut out = Vec::new();
    loop {
        let mut store = [0u8; 4];
        let mut rb = ReadBuf::new(&mut store);
        match HyperRead::poll_read(Pin::new(&mut io), &mut cx, rb.unfilled()) {
            Poll::Ready(Ok(())) => {
                if rb.filled().is_empty() {
                    panic!("must not report EOF before the error surfaces");
                }
                out.extend_from_slice(rb.filled());
            }
            Poll::Ready(Err(e)) => {
                assert_eq!(e.to_string(), "simulated mid-stream failure");
                break;
            }
            Poll::Pending => panic!("source never returns Pending here"),
        }
    }
    assert_eq!(out, b"partial");
}

/// A source that always has exactly `len` bytes ready, for probing the
/// `remaining()` vs `SCRATCH` boundary precisely (smaller / equal / larger).
struct Exact {
    data: Vec<u8>,
    at: usize,
}
impl AsyncRead for Exact {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let n = (self.data.len() - self.at).min(buf.len());
        buf[..n].copy_from_slice(&self.data[self.at..self.at + n]);
        self.at += n;
        Poll::Ready(Ok(n))
    }
}

/// `expected_len` bounds the loop so a mutation that fabricates a phantom
/// EOF byte (defeating `rb.filled().is_empty()` as a terminator) fails as a
/// clean assertion instead of hanging the suite.
fn read_all_with_dest_buf(
    mut io: FuturesIo<Exact>,
    dest_len: usize,
    expected_len: usize,
) -> Vec<u8> {
    let (waker, _rec) = test_waker();
    let mut cx = Context::from_waker(&waker);
    let mut out = Vec::new();
    let mut store = vec![0u8; dest_len];
    while out.len() < expected_len {
        let mut rb = ReadBuf::new(&mut store);
        let r = HyperRead::poll_read(Pin::new(&mut io), &mut cx, rb.unfilled());
        assert!(matches!(r, Poll::Ready(Ok(()))));
        if rb.filled().is_empty() {
            break;
        }
        out.extend_from_slice(rb.filled());
    }
    out
}

const SCRATCH: usize = 8 * 1024; // must match the private const in futures_io.rs

#[test]
fn caller_buffer_smaller_than_scratch() {
    let data = vec![0xABu8; SCRATCH / 2];
    let io = FuturesIo::new(Exact {
        data: data.clone(),
        at: 0,
    });
    let len = data.len();
    assert_eq!(read_all_with_dest_buf(io, 3, len), data);
}

#[test]
fn caller_buffer_exactly_equal_to_scratch() {
    // Boundary: want = remaining().min(scratch.len()) == SCRATCH exactly.
    let data: Vec<u8> = (0..SCRATCH).map(|i| (i % 256) as u8).collect();
    let io = FuturesIo::new(Exact {
        data: data.clone(),
        at: 0,
    });
    let out = read_all_with_dest_buf(io, SCRATCH, SCRATCH);
    assert_eq!(out, data);
}

#[test]
fn caller_buffer_larger_than_scratch_by_one_byte() {
    // The exact off-by-one the brief's tests never exercised (all use an
    // 8-byte destination). remaining() == SCRATCH + 1 forces two internal
    // reads through the scratch buffer to fill one hyper poll_read call's
    // worth of *capacity*, and the shim must not panic indexing `scratch`.
    let data: Vec<u8> = (0..SCRATCH + 1).map(|i| (i % 251) as u8).collect();
    let io = FuturesIo::new(Exact {
        data: data.clone(),
        at: 0,
    });
    let out = read_all_with_dest_buf(io, SCRATCH + 1, SCRATCH + 1);
    assert_eq!(out, data);
}

// ---------------------------------------------------------------------
// B. EOF contract at the raw ReadBufCursor level (not through a helper that
//    treats "filled.is_empty()" as loop termination, which could mask a
//    Pending being misreported as Ready(Ok(())) with nothing filled).
// ---------------------------------------------------------------------

struct ImmediateEof;
impl AsyncRead for ImmediateEof {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(0))
    }
}

#[test]
fn eof_is_ready_ok_with_untouched_buffer_never_pending_never_error() {
    let mut io = FuturesIo::new(ImmediateEof);
    let (waker, _rec) = test_waker();
    let mut cx = Context::from_waker(&waker);
    let mut store = [0xFFu8; 16]; // poison the store to catch accidental writes
    let mut rb = ReadBuf::new(&mut store);
    let r = HyperRead::poll_read(Pin::new(&mut io), &mut cx, rb.unfilled());
    assert!(
        matches!(r, Poll::Ready(Ok(()))),
        "EOF must be Ready(Ok(()))"
    );
    assert_eq!(
        rb.filled().len(),
        0,
        "EOF must leave buf.remaining() unchanged (hyper's EOF signal)"
    );
}

struct NeverEof(u8);
impl AsyncRead for NeverEof {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        // Alternates Pending/data forever — never signals EOF. Confirms the
        // shim never *manufactures* an EOF signal on its own when the source
        // is Pending or has data (which would make hyper drop a live conn).
        if self.0.is_multiple_of(2) {
            self.0 += 1;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.0 += 1;
        buf[0] = 0x42;
        Poll::Ready(Ok(1))
    }
}

#[test]
fn shim_never_manufactures_eof_when_source_is_pending_or_has_data() {
    let mut io = FuturesIo::new(NeverEof(0));
    let (waker, _rec) = test_waker();
    let mut cx = Context::from_waker(&waker);
    for i in 0..20 {
        let mut store = [0u8; 4];
        let mut rb = ReadBuf::new(&mut store);
        let r = HyperRead::poll_read(Pin::new(&mut io), &mut cx, rb.unfilled());
        if i % 2 == 0 {
            assert!(matches!(r, Poll::Pending), "iter {i}: expected Pending");
        } else {
            assert!(
                matches!(r, Poll::Ready(Ok(()))),
                "iter {i}: expected Ready(Ok(()))"
            );
            assert_eq!(
                rb.filled(),
                &[0x42],
                "iter {i}: expected real data, not an EOF shape"
            );
        }
    }
}

// ---------------------------------------------------------------------
// C. poll_shutdown -> poll_close mapping and flush ordering
// ---------------------------------------------------------------------

#[derive(Default)]
struct WriteRecorder {
    calls: Vec<&'static str>,
    written: Vec<u8>,
    flushed_len: usize, // length of `written` as of the last poll_flush that returned Ready
    close_before_flush_of_pending: bool,
}

/// Wrapped in Arc<Mutex<_>> so the test can inspect it after handing
/// ownership of a clone-like handle into FuturesIo. Since futures_io::AsyncWrite
/// needs a concrete Unpin type owned by FuturesIo, we use a small handle type.
struct RecorderHandle(Arc<Mutex<WriteRecorder>>);

impl AsyncWrite for RecorderHandle {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut r = self.0.lock().unwrap();
        r.calls.push("write");
        r.written.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut r = self.0.lock().unwrap();
        r.calls.push("flush");
        r.flushed_len = r.written.len();
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut r = self.0.lock().unwrap();
        r.calls.push("close");
        if r.flushed_len < r.written.len() {
            r.close_before_flush_of_pending = true;
        }
        Poll::Ready(Ok(()))
    }
}

#[test]
fn poll_shutdown_maps_to_poll_close_not_to_flush_or_a_noop() {
    let rec = Arc::new(Mutex::new(WriteRecorder::default()));
    let mut io = FuturesIo::new(RecorderHandle(rec.clone()));
    let (waker, _r) = test_waker();
    let mut cx = Context::from_waker(&waker);

    let r = HyperWrite::poll_shutdown(Pin::new(&mut io), &mut cx);
    assert!(matches!(r, Poll::Ready(Ok(()))));
    assert_eq!(
        rec.lock().unwrap().calls,
        vec!["close"],
        "poll_shutdown must map 1:1 to poll_close"
    );
}

#[test]
fn write_then_hypers_own_flush_then_shutdown_loses_nothing() {
    // This reproduces exactly what hyper's own h1 dispatcher does before it
    // ever calls the shim's poll_shutdown (verified in
    // hyper-1.11.0/src/proto/h1/io.rs: `Buffered::poll_shutdown` does
    // `ready!(self.poll_flush(cx))?;` before calling the raw io's
    // `poll_shutdown`). The shim's own poll_shutdown does not flush again —
    // that's correct only because the caller (hyper) already guarantees the
    // flush happened first.
    let rec = Arc::new(Mutex::new(WriteRecorder::default()));
    let mut io = FuturesIo::new(RecorderHandle(rec.clone()));
    let (waker, _r) = test_waker();
    let mut cx = Context::from_waker(&waker);

    let w = HyperWrite::poll_write(Pin::new(&mut io), &mut cx, b"last write");
    assert!(matches!(w, Poll::Ready(Ok(10))));
    let f = HyperWrite::poll_flush(Pin::new(&mut io), &mut cx);
    assert!(matches!(f, Poll::Ready(Ok(()))));
    let s = HyperWrite::poll_shutdown(Pin::new(&mut io), &mut cx);
    assert!(matches!(s, Poll::Ready(Ok(()))));

    let r = rec.lock().unwrap();
    assert_eq!(r.calls, vec!["write", "flush", "close"]);
    assert_eq!(r.written, b"last write");
    assert!(
        !r.close_before_flush_of_pending,
        "close observed after flush had already covered all written bytes"
    );
}

struct PendingClose;
impl AsyncWrite for PendingClose {
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
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

#[test]
fn poll_shutdown_propagates_pending_from_poll_close_without_swallowing_it() {
    let mut io = FuturesIo::new(PendingClose);
    let (waker, _r) = test_waker();
    let mut cx = Context::from_waker(&waker);
    let r = HyperWrite::poll_shutdown(Pin::new(&mut io), &mut cx);
    assert!(
        matches!(r, Poll::Pending),
        "a Pending poll_close must surface as Pending, not Ready(Ok(()))"
    );
}

struct ErroringClose;
impl AsyncWrite for ErroringClose {
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
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("close failed")))
    }
}

#[test]
fn poll_shutdown_propagates_error_from_poll_close() {
    let mut io = FuturesIo::new(ErroringClose);
    let (waker, _r) = test_waker();
    let mut cx = Context::from_waker(&waker);
    let r = HyperWrite::poll_shutdown(Pin::new(&mut io), &mut cx);
    match r {
        Poll::Ready(Err(e)) => assert_eq!(e.to_string(), "close failed"),
        other => panic!("expected Ready(Err(_)), got {other:?}"),
    }
}
