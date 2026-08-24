//! Reviewer-written adversarial test suite for `FuturesIo<hclient_rt_smol::SmolSocket>`
//! driven through the real `Smol` runtime. The sibling suite for `TokioIo`
//! is `crates/hclient-rt-tokio/tests/adversarial_tokio_io.rs`, and the
//! mock-source one for `FuturesIo` is
//! `crates/hclient-rt/tests/adversarial_futures_io.rs`. Those exercise
//! `FuturesIo` against hand-written mock `AsyncRead`/`AsyncWrite` sources;
//! this drives it against a real loopback
//! TCP pair through `Smol::connect`/`Smol::adopt`, so a bug that only shows
//! up against a genuine socket (partial reads, real EOF, a real RST) is
//! covered on the smol side the same way it already is on the tokio side.
//!
//! Every wait is bounded the same way the tokio suite bounds every
//! `poll_read` wait: a regression that makes `poll_read` never resolve must
//! report FAILED with a clear message, not hang the test binary (and the CI
//! job) with nothing to investigate.
//!
//! Confirmed non-vacuous the same way the tokio suite is: temporarily
//! dropping
//! `hclient-rt`'s `FuturesIo::poll_read`'s `.min(self.scratch.len())` made
//! `cursor_one_byte_larger_than_scratch_buffer` go red with the exact panic
//! the doc comment predicts (`range end index 8193 out of range for slice of
//! length 8192`), then restored (`cp` from a backup) and reconfirmed green.
//! Also mirrors the tokio suite's other design note: only 7 tests here where
//! the tokio sibling settled on 7 too (its own former 8th was dropped as
//! vacuous) - no equivalent gap found on the smol side.
//!
//! To re-run: this file needs `hyper = { version = "1.11", default-features
//! = false }` as a **dev-dependency** of `hclient-rt-smol` (it is only a
//! normal, non-dev dependency of `hclient-rt`, so it is not visible to an
//! integration test under `crates/hclient-rt-smol/tests/` without this) -
//! drop this file into `crates/hclient-rt-smol/tests/` in a scratch clone,
//! add that dev-dependency, and `cargo test -p hclient-rt-smol --test
//! adversarial_smol_io --all-features`.
use hclient_rt::{FuturesIo, TcpAdoptStd, TcpConnect, TcpOpts};
use hclient_rt_smol::Smol;
use hyper::rt::Read as HyperRead;
use std::future::poll_fn;
use std::io::Write as _;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

const SCRATCH: usize = 8 * 1024; // must match the private const in hclient-rt's futures_io.rs

const READ_TIMEOUT: Duration = Duration::from_secs(10);

enum ReadOutcome {
    Done(std::io::Result<()>),
    TimedOut,
}

async fn read_ready<F: std::future::Future<Output = std::io::Result<()>>>(
    fut: F,
) -> std::io::Result<()> {
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

async fn connected_pair() -> (FuturesIo<hclient_rt_smol::SmolSocket>, std::net::TcpStream) {
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
        let mut rb = hyper::rt::ReadBuf::new(&mut store);
        match Pin::new(&mut client).poll_read(&mut cx, rb.unfilled()) {
            Poll::Pending => {}
            other => panic!("expected Pending before any data was written, got {other:?}"),
        }
        assert_eq!(rb.filled().len(), 0, "must not fill anything while Pending");

        server.write_all(b"after pending").unwrap();
        let mut store2 = [0u8; 64];
        let mut rb2 = hyper::rt::ReadBuf::new(&mut store2);
        read_ready(poll_fn(|cx| {
            Pin::new(&mut client).poll_read(cx, rb2.unfilled())
        }))
        .await
        .unwrap();
        if rb2.filled().is_empty() {
            panic!("got EOF-shaped Ready before any data was ever read");
        }
        assert_eq!(rb2.filled(), b"after pending");
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
            let mut rb = hyper::rt::ReadBuf::new(&mut store);
            read_ready(poll_fn(|cx| {
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
            let mut rb = hyper::rt::ReadBuf::new(&mut store);
            let res = read_ready(poll_fn(|cx| {
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
    });
}

// ---------------------------------------------------------------------
// D. Scratch-buffer boundary: caller cursor sizes smaller than, exactly
//    equal to, and one byte larger than SCRATCH (8 KiB).
// ---------------------------------------------------------------------

async fn read_exactly(
    client: &mut FuturesIo<hclient_rt_smol::SmolSocket>,
    dest_len: usize,
    expected_len: usize,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut store = vec![0u8; dest_len];
    while out.len() < expected_len {
        let mut rb = hyper::rt::ReadBuf::new(&mut store);
        read_ready(poll_fn(|cx| {
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
        let data: Vec<u8> = (0..SCRATCH + 1).map(|i| (i % 251) as u8).collect();
        let writer = {
            let data = data.clone();
            std::thread::spawn(move || server.write_all(&data).unwrap())
        };
        let out = read_exactly(&mut client, SCRATCH + 1, SCRATCH + 1).await;
        writer.join().unwrap();
        assert_eq!(out, data);
    });
}

// Also confirmed: adopt() goes through the same FuturesIo, so it inherits
// the same read behaviour as connect() - spot check with TcpAdoptStd.
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
