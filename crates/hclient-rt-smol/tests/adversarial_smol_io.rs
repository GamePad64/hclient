//! Reviewer-written adversarial test suite for `hclient_rt_smol::SmolIo`
//! driven through the real `Smol` runtime. The sibling suite for `TokioIo`
//! is `crates/hclient-rt-tokio/tests/adversarial_tokio_io.rs`. This drives
//! `SmolIo` against a real loopback
//! TCP pair through `Smol::connect`/`Smol::adopt`, so a bug that only shows
//! up against a genuine socket (partial reads, real EOF, a real RST) is
//! covered on the smol side the same way it already is on the tokio side.
//!
//! Every wait is bounded the same way the tokio suite bounds every
//! `poll_read` wait: a regression that makes `poll_read` never resolve must
//! report FAILED with a clear message, not hang the test binary (and the CI
//! job) with nothing to investigate.
//!
//! **One claim here has outlived its subject.** This header recorded the
//! suite as confirmed non-vacuous by dropping a `.min(self.scratch.len())`
//! from `hclient-rt`'s `FuturesIo::poll_read`. That adapter was deleted when
//! the byte-stream seam moved to `futures-io`, and `SmolIo` reads
//! straight into the caller's buffer with no scratch at all — so
//! `cursor_one_byte_larger_than_scratch_buffer` now asserts that a read
//! larger than a buffer nobody has still succeeds, which is true and no
//! longer guards a boundary.
//! Also mirrors the tokio suite's other design note: only 7 tests here where
//! the tokio sibling settled on 7 too (its own former 8th was dropped as
//! vacuous) - no equivalent gap found on the smol side.
//!
//! To run: `cargo nextest run -p hclient-rt-smol --test adversarial_smol_io`.
use futures_lite::io::AsyncRead as _;
use hclient_rt::{TcpAdoptStd, TcpConnect, TcpOpts};
use hclient_rt_smol::Smol;
use std::future::poll_fn;
use std::io::Write as _;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

const SCRATCH: usize = 8 * 1024; // must match the private const in hclient-rt's futures_io.rs

const READ_TIMEOUT: Duration = Duration::from_secs(10);

enum ReadOutcome<T> {
    Done(std::io::Result<T>),
    TimedOut,
}

// Generic in the result, because `futures-io` answers a count where the
// cursor answered `()` — the helper is about the timeout, not the shape.
async fn read_ready<T, F: std::future::Future<Output = std::io::Result<T>>>(
    fut: F,
) -> std::io::Result<T> {
    let outcome = futures_lite::future::or(async { ReadOutcome::Done(fut.await) }, async {
        async_io::Timer::after(READ_TIMEOUT).await;
        ReadOutcome::TimedOut
    })
    .await;
    match outcome {
        ReadOutcome::Done(r) => r,
        ReadOutcome::TimedOut => panic!(
            "poll_read did not resolve within {READ_TIMEOUT:?} - treating a stalled read as a \
             regression (FAILED), not letting it hang the job with no diagnosis"
        ),
    }
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

async fn connected_pair() -> (hclient_rt_smol::SmolIo, std::net::TcpStream) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || listener.accept().unwrap().0);
    let client = Smol
        .connect(addr, &TcpOpts::default())
        .await
        .expect("connect");
    (client, server.join().unwrap())
}

// ---------------------------------------------------------------------
// A. Pending before data must not be confused with EOF or with data.
// ---------------------------------------------------------------------

#[test]
fn pending_before_data_is_not_confused_with_eof_or_data() {
    futures_executor::block_on(async {
        let (mut client, mut server) = connected_pair().await;

        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);
        let mut store = [0u8; 64];
        match Pin::new(&mut client).poll_read(&mut cx, &mut store) {
            Poll::Pending => {}
            other @ Poll::Ready(_) => {
                panic!("expected Pending before any data was written, got {other:?}")
            }
        }
        // No count to assert on: `Pending` carries none, where the cursor
        // version could look at the buffer it had handed over. The claim
        // that matters — that this is `Pending` rather than an EOF-shaped
        // `Ready` — is the match above.

        server.write_all(b"after pending").unwrap();
        let mut store2 = [0u8; 64];
        let n2 = read_ready(poll_fn(|cx| {
            Pin::new(&mut client).poll_read(cx, &mut store2)
        }))
        .await
        .unwrap();
        assert!(
            n2 != 0,
            "got EOF-shaped Ready before any data was ever read"
        );
        assert_eq!(&store2[..n2], b"after pending");
    });
}

// ---------------------------------------------------------------------
// B. Byte-by-byte ordering, no drop, no duplicate.
// ---------------------------------------------------------------------

#[test]
fn one_byte_at_a_time_preserves_order_no_drop_no_duplicate() {
    futures_executor::block_on(async {
        let (mut client, mut server) = connected_pair().await;
        let msg: Vec<u8> = (0..=255u8).collect();

        let writer = std::thread::spawn(move || {
            for b in &msg {
                server.write_all(std::slice::from_ref(b)).unwrap();
                server.flush().unwrap();
            }
            msg
        });

        let mut out = Vec::new();
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
    });
}

// ---------------------------------------------------------------------
// C. An error after partial data must be propagated, not swallowed or
//    confused with EOF. Forced via SO_LINGER=0 + drop, same as the tokio
//    suite: the kernel then sends RST instead of a clean FIN.
// ---------------------------------------------------------------------

#[test]
fn error_after_partial_data_is_propagated_not_swallowed_or_confused_with_eof() {
    futures_executor::block_on(async {
        let (mut client, mut server) = connected_pair().await;

        server.write_all(b"partial").unwrap();
        server.flush().unwrap();
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
    });
}

// ---------------------------------------------------------------------
// D. Scratch-buffer boundary: caller cursor sizes smaller than, exactly
//    equal to, and one byte larger than SCRATCH (8 KiB).
// ---------------------------------------------------------------------

async fn read_exactly(
    client: &mut hclient_rt_smol::SmolIo,
    dest_len: usize,
    expected_len: usize,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut store = vec![0u8; dest_len];
    while out.len() < expected_len {
        // `futures-io` hands back the count rather than filling a cursor,
        // so the `ReadBuf`/`unfilled()`/`filled()` trio this used to need
        // is one `usize`.
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

#[test]
fn cursor_smaller_than_scratch_buffer() {
    futures_executor::block_on(async {
        let (mut client, mut server) = connected_pair().await;
        let data = vec![0xABu8; SCRATCH / 2];
        let writer = {
            let data = data.clone();
            std::thread::spawn(move || server.write_all(&data).unwrap())
        };
        let out = read_exactly(&mut client, 3, data.len()).await;
        writer.join().unwrap();
        assert_eq!(out, data);
    });
}

#[test]
fn cursor_exactly_equal_to_scratch_buffer() {
    futures_executor::block_on(async {
        let (mut client, mut server) = connected_pair().await;
        // `i % 256` is always in 0..256, which fits `u8` — bounded by the
        // modulus, not by `SCRATCH`.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "`i % 256` is always in 0..256, which fits `u8` — bounded by the modulus, not by `SCRATCH`."
        )]
        let data: Vec<u8> = (0..SCRATCH).map(|i| (i % 256) as u8).collect();
        let writer = {
            let data = data.clone();
            std::thread::spawn(move || server.write_all(&data).unwrap())
        };
        let out = read_exactly(&mut client, SCRATCH, SCRATCH).await;
        writer.join().unwrap();
        assert_eq!(out, data);
    });
}

#[test]
fn cursor_one_byte_larger_than_scratch_buffer() {
    futures_executor::block_on(async {
        let (mut client, mut server) = connected_pair().await;
        // `i % 251` is always in 0..251, which fits `u8` — bounded by the
        // modulus, not by `SCRATCH`.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "`i % 251` is always in 0..251, which fits `u8` — bounded by the modulus, not by `SCRATCH`."
        )]
        let data: Vec<u8> = (0..=SCRATCH).map(|i| (i % 251) as u8).collect();
        let writer = {
            let data = data.clone();
            std::thread::spawn(move || server.write_all(&data).unwrap())
        };
        let out = read_exactly(&mut client, SCRATCH + 1, SCRATCH + 1).await;
        writer.join().unwrap();
        assert_eq!(out, data);
    });
}

// adopt() hands back the same `SmolIo` connect() does, so it inherits
// the same read behaviour - spot check with TcpAdoptStd.
#[test]
fn adopted_stream_reads_correctly_too() {
    futures_executor::block_on(async {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_thread = std::thread::spawn(move || listener.accept().unwrap().0);
        let std_client = std::net::TcpStream::connect(addr).unwrap();
        let mut server = server_thread.join().unwrap();
        let mut client = Smol.adopt(std_client).expect("adopt");

        server.write_all(b"adopted-ok").unwrap();
        let out = read_exactly(&mut client, 64, 10).await;
        assert_eq!(out, b"adopted-ok");
    });
}
