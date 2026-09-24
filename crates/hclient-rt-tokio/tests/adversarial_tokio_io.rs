//! Adversarial read-side suite for `TokioIo`, over a real loopback TCP
//! pair rather than mock sources — `TokioIo` is concrete over tokio's
//! sockets, so there is nothing to inject.
//!
//! **Its header outlived its subject once, and this is the rewrite.** It
//! described a sibling suite for `hclient-rt`'s `FuturesIo` and a
//! per-connection 8 KiB scratch buffer whose `.min(..)` a mutation had
//! removed to prove the suite non-vacuous. Both are gone: `FuturesIo` was
//! deleted when the byte-stream seam moved to `futures-io`, and `TokioIo`
//! reads straight into the caller's buffer. Section D below still runs
//! reads either side of 8 KiB, which now asserts a property — any read
//! size delivers every byte in order — rather than guarding a boundary.
//!
//! The write side, the half-close and the Unix arm are in
//! `write_unix_and_half_close.rs`.
//!
//! A former eighth test, an empty `#[test]` whose own doc called it "a
//! documentation test, not a behavioural one", was dropped as vacuous; its
//! reasoning survives as section E.
use futures_io::AsyncRead as SeamRead;
use hclient_rt::{TcpAdoptStd, TcpConnect, TcpOpts};
use hclient_rt_tokio::{Tokio, TokioIo};
use std::future::poll_fn;
use std::io::Write as _;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

/// A read size either side of which section D probes. It used to have to
/// match a private scratch buffer in `io.rs`; there is no such buffer now.
const SIZE: usize = 8 * 1024;

/// Every direct wait on `poll_read` in this file goes through this helper
/// instead of a bare `.await` on `poll_fn(...)`, so
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

// Generic in the result, because `futures-io` answers a count where
// the cursor answered `()` — the helper is about the timeout.
async fn read_ready<T, F: std::future::Future<Output = std::io::Result<T>>>(
    fut: F,
) -> std::io::Result<T> {
    tokio::time::timeout(READ_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "poll_read did not resolve within {READ_TIMEOUT:?} - treating a stalled read as a \
             regression (FAILED), not letting it hang the job with no diagnosis"
            )
        })
}

struct RecordingWaker(Mutex<bool>);
impl Wake for RecordingWaker {
    fn wake(self: Arc<Self>) {
        *self.0.lock().unwrap() = true;
    }
    fn wake_by_ref(self: &Arc<Self>) {
        *self.0.lock().unwrap() = true;
    }
}
fn test_waker() -> Waker {
    Waker::from(Arc::new(RecordingWaker(Mutex::new(false))))
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
    match Pin::new(&mut client).poll_read(&mut cx, &mut store) {
        Poll::Pending => {}
        other @ Poll::Ready(_) => {
            panic!("expected Pending before any data was written, got {other:?}")
        }
    }
    // No count to assert on: `Pending` carries none. The claim that
    // matters — `Pending` rather than an EOF-shaped `Ready` — is the match
    // above.

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
    let n2 = read_ready(poll_fn(|cx| {
        Pin::new(&mut client).poll_read(cx, &mut store2)
    }))
    .await
    .unwrap();
    // Ready(Ok(())) with nothing filled before data has arrived would
    // be a false EOF - fail loudly rather than looping forever.
    assert!(
        n2 != 0,
        "got EOF-shaped Ready before any data was ever read"
    );
    assert_eq!(&store2[..n2], b"after pending");
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
        let n = read_ready(poll_fn(|cx| {
            Pin::new(&mut client).poll_read(cx, &mut store)
        }))
        .await
        .unwrap();
        assert!(n != 0, "unexpected EOF before all 256 bytes arrived");
        out.extend_from_slice(&store[..n]);
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
        .set_linger(Some(Duration::ZERO))
        .unwrap();
    // dropping the socket2::Socket above closes the fd and sends the RST.

    let mut out = Vec::new();
    let mut store = [0u8; 4];
    loop {
        let res = read_ready(poll_fn(|cx| {
            Pin::new(&mut client).poll_read(cx, &mut store)
        }))
        .await;
        match res {
            Ok(n) if n != 0 => out.extend_from_slice(&store[..n]),
            Ok(_) => panic!(
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
        assert!(
            out.len() <= 7,
            "read more than the 7 bytes written before the reset: {out:?}"
        );
    }
}

// ---------------------------------------------------------------------
// D. Read sizes smaller than, equal to and one byte larger than 8 KiB —
//    once a scratch-buffer boundary, now a check that any size delivers
//    every byte in order.
// ---------------------------------------------------------------------

async fn read_exactly(client: &mut TokioIo, dest_len: usize, expected_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut store = vec![0u8; dest_len];
    while out.len() < expected_len {
        let n = read_ready(poll_fn(|cx| {
            Pin::new(&mut *client).poll_read(cx, &mut store)
        }))
        .await
        .unwrap();
        if n == 0 {
            break;
        }
        out.extend_from_slice(&store[..n]);
    }
    out
}

#[tokio::test]
async fn a_read_smaller_than_8_kib_delivers_every_byte() {
    let (mut client, mut server) = connected_pair().await;
    let data = vec![0xABu8; SIZE / 2];
    let writer = {
        let data = data.clone();
        std::thread::spawn(move || server.write_all(&data).unwrap())
    };
    let out = read_exactly(&mut client, 3, data.len()).await;
    writer.join().unwrap();
    assert_eq!(out, data);
}

#[tokio::test]
async fn a_read_of_exactly_8_kib_delivers_every_byte() {
    let (mut client, mut server) = connected_pair().await;
    // `i % 256` is always in 0..256, which fits `u8` — bounded by the
    // modulus, not by `SIZE`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "`i % 256` is always in 0..256, which fits `u8` — bounded by the modulus, not by `SIZE`."
    )]
    let data: Vec<u8> = (0..SIZE).map(|i| (i % 256) as u8).collect();
    let writer = {
        let data = data.clone();
        std::thread::spawn(move || server.write_all(&data).unwrap())
    };
    let out = read_exactly(&mut client, SIZE, SIZE).await;
    writer.join().unwrap();
    assert_eq!(out, data);
}

#[tokio::test]
async fn a_read_one_byte_over_8_kib_delivers_every_byte() {
    let (mut client, mut server) = connected_pair().await;
    // `i % 251` is always in 0..251, which fits `u8` — bounded by the
    // modulus, not by `SIZE`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "`i % 251` is always in 0..251, which fits `u8` — bounded by the modulus, not by `SIZE`."
    )]
    let data: Vec<u8> = (0..=SIZE).map(|i| (i % 251) as u8).collect();
    let writer = {
        let data = data.clone();
        std::thread::spawn(move || server.write_all(&data).unwrap())
    };
    let out = read_exactly(&mut client, SIZE + 1, SIZE + 1).await;
    writer.join().unwrap();
    assert_eq!(out, data);
}

// ---------------------------------------------------------------------
// E. Structural note on "a spurious Ok(0) mid-stream, and no sticky EOF
//    state": a real tokio socket only ever reads 0 at genuine, permanent
//    EOF, so the property cannot be driven from outside. What it rests on
//    is that `TokioIo` holds no "have we seen EOF" flag — its one field is
//    the socket, and `poll_read` re-polls it every time — which is a claim
//    about private layout, invisible to an integration test. So it stays a
//    comment rather than a `#[test]` that could never fail.

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
