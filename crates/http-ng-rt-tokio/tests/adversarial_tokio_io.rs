//! Reviewer-written adversarial test suite for `TokioIo` (Task 3 of
//! vertical 2), independent of the implementer's work. Adapted from the
//! sibling suite for `FuturesIo` (Task 2 review,
//! `.superpowers/sdd/2026-08-05-v01-native-vertical/adversarial_futures_io.rs`)
//! to the fact that `TokioIo` is concrete over `tokio::net::TcpStream`
//! rather than generic over an injectable `AsyncRead`/`AsyncWrite` - so
//! these drive a real loopback TCP pair instead of hand-written mock
//! sources, and `tokio::io::AsyncRead` fills a `ReadBuf` rather than
//! returning a byte count.
//!
//! Ran as an integration test (`crates/http-ng-rt-tokio/tests/*.rs`) in a
//! throwaway clone during the Task 3 review: all 8 tests passed against
//! `http-ng-rt-tokio` at `2948375`. Confirmed non-vacuous by re-mutating
//! `poll_read`'s `.min(self.scratch.len())` away and watching
//! `cursor_one_byte_larger_than_scratch_buffer` go red with the exact
//! panic the doc comment predicts (`range end index 8193 out of range for
//! slice of length 8192`), then restoring and reconfirming green. To
//! re-run: drop this file into `crates/http-ng-rt-tokio/tests/` in a
//! scratch clone and `cargo test -p http-ng-rt-tokio --test
//! adversarial_tokio_io --all-features`.
//!
//! Landed as 7 executable tests, not 8: the reviewer's 8th,
//! `no_sticky_eof_state_field_exists_by_construction`, was an empty
//! `#[test]` fn whose own doc comment already calls it "a documentation
//! test, not a behavioural one" — it can never turn red under any code
//! change, which is the vacuous-test pattern this project removes rather
//! than accumulates. Its reasoning is real and worth keeping, so it
//! survives below as a plain comment (section E) instead of a test item
//! that always reports "ok".
use http_ng_rt::{TcpAdoptStd, TcpConnect, TcpOpts};
use http_ng_rt_tokio::{Tokio, TokioIo};
use hyper::rt::Read as HyperRead;
use std::io::Write as _;
use std::pin::Pin;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

const SCRATCH: usize = 8 * 1024; // must match the private const in io.rs

/// Fix round 3 (coordinator): every direct wait on `poll_read` in this file
/// goes through this helper instead of a bare `.await` on `poll_fn(...)`, so
/// a regression that makes `poll_read` never resolve (return `Pending`
/// forever) reports `FAILED` with a named test and a clear message, instead
/// of hanging the test binary - and, in CI, the whole job - with nothing to
/// investigate. Found the hard way: mutating `poll_read` to drop the last
/// byte of every read made `adopted_stream_reads_correctly_too` (via
/// `read_exactly` below) wait forever instead of failing, because its
/// read-until-length loop had no bound on wall-clock time, only on bytes
/// read.
///
/// Deliberately generous - it must never fire against correct code. Every
/// read in this file is over loopback and completes in well under a
/// millisecond normally; ten seconds is orders of magnitude more slack than
/// that, while still failing inside a single test run rather than eating a
/// CI job's entire time budget.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

async fn read_ready<F: std::future::Future<Output = std::io::Result<()>>>(
    fut: F,
) -> std::io::Result<()> {
    tokio::time::timeout(READ_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "poll_read did not resolve within {READ_TIMEOUT:?} - treating a stalled read as a \
             regression (FAILED), not letting it hang the job with no diagnosis"
            )
        })
}

struct RecordingWaker(std::sync::Mutex<bool>);
impl Wake for RecordingWaker {
    fn wake(self: std::sync::Arc<Self>) {
        *self.0.lock().unwrap() = true;
    }
    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        *self.0.lock().unwrap() = true;
    }
}
fn test_waker() -> Waker {
    Waker::from(std::sync::Arc::new(RecordingWaker(std::sync::Mutex::new(
        false,
    ))))
}

async fn connected_pair() -> (TokioIo, std::net::TcpStream) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || listener.accept().unwrap().0);
    let client = Tokio
        .connect(addr, &TcpOpts::default())
        .await
        .expect("connect");
    (client, server.join().unwrap())
}

// ---------------------------------------------------------------------
// A. Pending before data must not be confused with EOF or with data.
// ---------------------------------------------------------------------

#[tokio::test]
async fn pending_before_data_is_not_confused_with_eof_or_data() {
    let (mut client, mut server) = connected_pair().await;

    // Nothing has been written yet: a manual, non-executor-driven poll must
    // return Pending, and must NOT fill anything (which would be an
    // EOF-shaped Ready(Ok(())) with nothing filled - a live connection
    // wrongly reported as closed).
    let waker = test_waker();
    let mut cx = Context::from_waker(&waker);
    let mut store = [0u8; 64];
    let mut rb = hyper::rt::ReadBuf::new(&mut store);
    match Pin::new(&mut client).poll_read(&mut cx, rb.unfilled()) {
        Poll::Pending => {}
        other => panic!("expected Pending before any data was written, got {other:?}"),
    }
    assert_eq!(rb.filled().len(), 0, "must not fill anything while Pending");

    // Now real data arrives; poll again (via .await this time, so the
    // waker registered above is superseded by a real one tied to the
    // tokio reactor) and confirm it surfaces as real data, not EOF. A
    // single `.await` already blocks until `poll_read` returns Ready (the
    // executor re-invokes the closure whenever the reactor wakes it), so
    // no retry loop is needed here - the reviewer's original wrapped this
    // in `loop { ...; break; } ... panic!(...)`, which `clippy::never_loop`
    // (on by default, not just under `-D warnings`) rightly rejects: every
    // path through the body either breaks or diverges on the very first
    // iteration, so it was never actually a loop. Adapted, not weakened:
    // same two outcomes, same assertions, just without the dead loop
    // wrapper that couldn't compile against the current tree.
    server.write_all(b"after pending").unwrap();
    let mut store2 = [0u8; 64];
    let mut rb2 = hyper::rt::ReadBuf::new(&mut store2);
    read_ready(std::future::poll_fn(|cx| {
        Pin::new(&mut client).poll_read(cx, rb2.unfilled())
    }))
    .await
    .unwrap();
    if rb2.filled().is_empty() {
        // Ready(Ok(())) with nothing filled before data has arrived would
        // be a false EOF - fail loudly rather than looping forever.
        panic!("got EOF-shaped Ready before any data was ever read");
    }
    assert_eq!(rb2.filled(), b"after pending");
}

// ---------------------------------------------------------------------
// B. Byte-by-byte ordering, no drop, no duplicate.
// ---------------------------------------------------------------------

#[tokio::test]
async fn one_byte_at_a_time_preserves_order_no_drop_no_duplicate() {
    let (mut client, mut server) = connected_pair().await;
    let msg: Vec<u8> = (0..=255u8).collect(); // every byte value once, order matters

    let writer = std::thread::spawn(move || {
        for b in &msg {
            server.write_all(std::slice::from_ref(b)).unwrap();
            server.flush().unwrap();
        }
        msg
    });

    let mut out = Vec::new();
    // deliberately not a divisor of 256: exercises the boundary too
    let mut store = [0u8; 32];
    while out.len() < 256 {
        let mut rb = hyper::rt::ReadBuf::new(&mut store);
        read_ready(std::future::poll_fn(|cx| {
            Pin::new(&mut client).poll_read(cx, rb.unfilled())
        }))
        .await
        .unwrap();
        assert!(
            !rb.filled().is_empty(),
            "unexpected EOF before all 256 bytes arrived"
        );
        out.extend_from_slice(rb.filled());
    }
    let original = writer.join().unwrap();
    assert_eq!(
        out, original,
        "did not preserve byte order / dropped or duplicated bytes"
    );
}

// ---------------------------------------------------------------------
// C. An error after partial data must be propagated, not swallowed or
//    confused with EOF. Forced via SO_LINGER=0 + drop, which makes the
//    kernel send RST instead of a clean FIN, so the client's next read
//    observes ECONNRESET rather than Ok(0).
// ---------------------------------------------------------------------

#[tokio::test]
async fn error_after_partial_data_is_propagated_not_swallowed_or_confused_with_eof() {
    let (mut client, mut server) = connected_pair().await;

    server.write_all(b"partial").unwrap();
    server.flush().unwrap();
    // SO_LINGER(0) + drop => kernel sends RST instead of FIN on close.
    // `TcpStream::set_linger` is still unstable (tcp_linger, #88494) on
    // this toolchain, so go through socket2 instead.
    socket2::Socket::from(server)
        .set_linger(Some(std::time::Duration::ZERO))
        .unwrap();
    // dropping the socket2::Socket above closes the fd and sends the RST.

    let mut out = Vec::new();
    let mut store = [0u8; 4];
    loop {
        let mut rb = hyper::rt::ReadBuf::new(&mut store);
        let res = read_ready(std::future::poll_fn(|cx| {
            Pin::new(&mut client).poll_read(cx, rb.unfilled())
        }))
        .await;
        match res {
            Ok(()) if !rb.filled().is_empty() => out.extend_from_slice(rb.filled()),
            Ok(()) => panic!(
                "reported EOF (Ready(Ok(())) with nothing filled) instead of the RST error; \
                 got {out:?} of expected b\"partial\" so far"
            ),
            Err(e) => {
                // Two separate claims, because the platforms differ on
                // exactly one of them.
                //
                // Everywhere: what did arrive must be a PREFIX of what
                // was written — never reordered, never corrupted, never
                // more than the 7 bytes sent. That is the part this
                // wrapper could plausibly get wrong, and it is checked
                // on every platform.
                assert!(
                    b"partial".starts_with(&out),
                    "bytes delivered before the error were reordered or corrupted: {out:?}"
                );
                // POSIX only: data already sitting in the receive queue
                // survives an incoming RST and is delivered ahead of the
                // error. Winsock discards it — measured on
                // `windows-latest`, where this same test read back `[]`
                // where Linux and macOS read back b"partial". That is a
                // property of the OS, not of the IO wrapper, so
                // demanding it on Windows would be asserting something
                // the platform does not offer.
                #[cfg(not(windows))]
                assert_eq!(
                    out, b"partial",
                    "error surfaced but partial data before it was lost or corrupted"
                );
                // ECONNRESET is the expected kind for an RST; assert on the
                // io::ErrorKind rather than the message, which is
                // platform-dependent.
                assert_eq!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset,
                    "expected ConnectionReset from the RST, got {e:?}"
                );
                return;
            }
        }
        if out.len() > 7 {
            panic!("read more than the 7 bytes written before the reset: {out:?}");
        }
    }
}

// ---------------------------------------------------------------------
// D. Scratch-buffer boundary: caller cursor sizes smaller than, exactly
//    equal to, and one byte larger than SCRATCH (8 KiB).
// ---------------------------------------------------------------------

async fn read_exactly(client: &mut TokioIo, dest_len: usize, expected_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut store = vec![0u8; dest_len];
    while out.len() < expected_len {
        let mut rb = hyper::rt::ReadBuf::new(&mut store);
        read_ready(std::future::poll_fn(|cx| {
            Pin::new(&mut *client).poll_read(cx, rb.unfilled())
        }))
        .await
        .unwrap();
        if rb.filled().is_empty() {
            break;
        }
        out.extend_from_slice(rb.filled());
    }
    out
}

#[tokio::test]
async fn cursor_smaller_than_scratch_buffer() {
    let (mut client, mut server) = connected_pair().await;
    let data = vec![0xABu8; SCRATCH / 2];
    let writer = {
        let data = data.clone();
        std::thread::spawn(move || server.write_all(&data).unwrap())
    };
    let out = read_exactly(&mut client, 3, data.len()).await;
    writer.join().unwrap();
    assert_eq!(out, data);
}

#[tokio::test]
async fn cursor_exactly_equal_to_scratch_buffer() {
    let (mut client, mut server) = connected_pair().await;
    let data: Vec<u8> = (0..SCRATCH).map(|i| (i % 256) as u8).collect();
    let writer = {
        let data = data.clone();
        std::thread::spawn(move || server.write_all(&data).unwrap())
    };
    let out = read_exactly(&mut client, SCRATCH, SCRATCH).await;
    writer.join().unwrap();
    assert_eq!(out, data);
}

#[tokio::test]
async fn cursor_one_byte_larger_than_scratch_buffer() {
    let (mut client, mut server) = connected_pair().await;
    let data: Vec<u8> = (0..SCRATCH + 1).map(|i| (i % 251) as u8).collect();
    let writer = {
        let data = data.clone();
        std::thread::spawn(move || server.write_all(&data).unwrap())
    };
    let out = read_exactly(&mut client, SCRATCH + 1, SCRATCH + 1).await;
    writer.join().unwrap();
    assert_eq!(out, data);
}

// ---------------------------------------------------------------------
// E. Structural note on "spurious Ok(0) mid-stream, shim keeps no sticky
//    EOF state" (one of the 13 FuturesIo adversarial tests): unlike
//    FuturesIo<S>, TokioIo is concrete over tokio::net::TcpStream, so a
//    real socket cannot be driven to emit a "spurious" non-final Ok(0) -
//    tokio's AsyncRead for TcpStream only ever returns a 0-fill at genuine,
//    permanent EOF. There is no way to turn this into a behavioural test
//    against a real socket: the property in question is that `poll_read`
//    holds no "have we seen EOF" flag, so it cannot wedge into a stale
//    state - confirmed by reading io.rs (the only per-instance state is
//    `inner` and `scratch`, and `poll_read` always re-polls `inner` fresh).
//    That is a claim about private struct layout, invisible to an
//    integration test, so it stays a comment rather than a `#[test]` fn
//    that would report "ok" unconditionally and never be able to catch a
//    regression (landing note, http-ng-rt-tokio fix round 2: the
//    reviewer's original version was exactly such a fn, an empty body
//    under a name implying a real check - dropped as vacuous, see the
//    module doc comment above).

// Also confirmed: adopt() goes through the same TokioIo, so it inherits
// the same read behaviour as connect() - spot check with TcpAdoptStd.
#[tokio::test]
async fn adopted_stream_reads_correctly_too() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server_thread = std::thread::spawn(move || listener.accept().unwrap().0);
    let std_client = std::net::TcpStream::connect(addr).unwrap();
    let mut server = server_thread.join().unwrap();
    let mut client = Tokio.adopt(std_client).expect("adopt");

    server.write_all(b"adopted-ok").unwrap();
    let out = read_exactly(&mut client, 64, 10).await;
    assert_eq!(out, b"adopted-ok");
}
