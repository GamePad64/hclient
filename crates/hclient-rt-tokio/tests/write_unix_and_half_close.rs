//! The write side of `TokioIo`, its half-close, and the Unix-domain arm.
//!
//! The sibling of `hclient-rt-smol`'s `smol_unix_and_write.rs`, and it did
//! not exist: a crate-scoped mutation sweep found every one of
//! `poll_write`, `poll_write_vectored`, `poll_close` and `poll_shutdown`
//! replaceable by a success that moves nothing, with this crate's suite
//! green. `adversarial_tokio_io.rs` is a **read**-side suite — it writes
//! from the plain `std` end and reads through `TokioIo` — and the one
//! half-close pin in the workspace was smol's.
//!
//! Two things are asserted here that the smol file did not assert, both
//! seam obligations rather than behaviours of one runtime:
//!
//! - **`Shutdown` is a half-close**: after it the peer sees EOF *and this
//!   side can still read the peer's answer*. A `shutdown(Both)` would pass
//!   every EOF test and break every HTTP/1 exchange that half-closes.
//! - **`poll_close` forwards to it**, so a caller holding the stream as
//!   plain `futures-io` gets the same half-close — `hclient_rt::Shutdown`'s
//!   own doc asks for exactly that.

use futures_io::AsyncWrite as _;
#[cfg(unix)]
use hclient_rt::IpcConnect;
use hclient_rt::Shutdown as _;
use hclient_rt::{TcpConnect, TcpOpts};
use hclient_rt_tokio::{Tokio, TokioIo};
use std::future::poll_fn;
use std::io::{Read as _, Write as _};
use std::pin::Pin;
use std::time::Duration;

/// Every wait is bounded: a regression that stalls must fail with a name
/// rather than wedge the binary.
const BOUND: Duration = Duration::from_secs(10);

async fn bounded<T>(fut: impl Future<Output = std::io::Result<T>>) -> std::io::Result<T> {
    match tokio::time::timeout(BOUND, fut).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("did not resolve within {BOUND:?} — treating a stall as a regression"),
        )),
    }
}

/// `poll_write` may take any prefix, so this loops; an `Ok(0)` for a
/// non-empty buffer is the mutant and fails here rather than spinning.
async fn write_all(s: &mut TokioIo, mut buf: &[u8]) -> std::io::Result<()> {
    let mut guard = 0;
    while !buf.is_empty() {
        let n = bounded(poll_fn(|cx| Pin::new(&mut *s).poll_write(cx, buf))).await?;
        assert!(n > 0, "poll_write returned 0 for a non-empty buffer");
        buf = &buf[n..];
        guard += 1;
        assert!(guard < 10_000, "poll_write made no progress");
    }
    Ok(())
}

async fn read_to_end(s: &mut TokioIo) -> Vec<u8> {
    use futures_io::AsyncRead as _;
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = bounded(poll_fn(|cx| Pin::new(&mut *s).poll_read(cx, &mut buf)))
            .await
            .expect("read");
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&buf[..n]);
    }
}

/// Which spelling of the half-close a test asks for.
#[derive(Clone, Copy, Debug)]
enum Close {
    /// `hclient_rt::Shutdown::poll_shutdown` — what the seam's consumers call.
    Shutdown,
    /// `futures_io::AsyncWrite::poll_close` — what a plain `futures-io`
    /// caller calls, and which must be the same half-close.
    Close,
}

async fn half_close(s: &mut TokioIo, how: Close) {
    bounded(poll_fn(|cx| match how {
        Close::Shutdown => Pin::new(&mut *s).poll_shutdown(cx),
        Close::Close => Pin::new(&mut *s).poll_close(cx),
    }))
    .await
    .unwrap_or_else(|e| panic!("{how:?}: {e}"));
}

/// The peer reads to EOF — which arrives only from a real FIN — and then
/// answers. What it answers must reach this side **after** this side's
/// half-close: that is the difference between `Shutdown::Write` and a full
/// close, and the whole reason `hclient_rt::Shutdown` exists.
///
/// The socket is held across the observation, so an EOF produced by a
/// drop rather than by the half-close cannot make it pass — the lesson
/// `smol_unix_and_write.rs` records paying for.
async fn the_peer_sees_eof_and_can_still_answer(how: Close) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("local_addr");
    let peer = std::thread::spawn(move || {
        let (mut s, _) = l.accept().expect("accept");
        s.set_read_timeout(Some(BOUND)).expect("read timeout");
        let mut got = Vec::new();
        s.read_to_end(&mut got)
            .expect("the peer must see EOF from the half-close, with this side still open");
        s.write_all(b"the-answer").expect("answer");
        got
    });

    let mut s = bounded(Tokio.connect(addr, &TcpOpts::default()))
        .await
        .expect("connect");
    write_all(&mut s, b"the-request").await.expect("write");
    half_close(&mut s, how).await;
    assert_eq!(
        read_to_end(&mut s).await,
        b"the-answer",
        "{how:?} must close only the writing half: the peer's answer has to \
         arrive after it"
    );
    assert_eq!(peer.join().expect("peer thread"), b"the-request");
}

#[tokio::test]
async fn poll_shutdown_is_a_half_close() {
    the_peer_sees_eof_and_can_still_answer(Close::Shutdown).await;
}

#[tokio::test]
async fn poll_close_forwards_to_the_same_half_close() {
    the_peer_sees_eof_and_can_still_answer(Close::Close).await;
}

/// The vectored path carries every buffer, in order. Three slices, because
/// one cannot tell a correct forwarding from one that writes only the first.
#[tokio::test]
async fn a_vectored_write_carries_every_buffer_in_order() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("local_addr");
    let peer = std::thread::spawn(move || {
        let (mut s, _) = l.accept().expect("accept");
        // Bounded, so a half-close that sends nothing fails this test
        // rather than hanging it.
        s.set_read_timeout(Some(BOUND)).expect("read timeout");
        let mut got = Vec::new();
        s.read_to_end(&mut got).expect("EOF from the half-close");
        got
    });

    let mut s = bounded(Tokio.connect(addr, &TcpOpts::default()))
        .await
        .expect("connect");
    let parts: [&[u8]; 3] = [b"head--", b"middle--", b"tail"];
    let total: usize = parts.iter().map(|p| p.len()).sum();
    let mut written = 0usize;
    let mut guard = 0;
    while written < total {
        // Rebuilt from what is left each turn: a short vectored write is
        // legal and may take any prefix.
        let mut remaining = Vec::new();
        let mut skip = written;
        for p in &parts {
            if skip >= p.len() {
                skip -= p.len();
            } else {
                remaining.push(std::io::IoSlice::new(&p[skip..]));
                skip = 0;
            }
        }
        let n = bounded(poll_fn(|cx| {
            Pin::new(&mut s).poll_write_vectored(cx, &remaining)
        }))
        .await
        .expect("vectored write");
        assert!(n > 0, "poll_write_vectored returned 0 with bytes remaining");
        written += n;
        guard += 1;
        assert!(guard < 10_000, "vectored write made no progress");
    }
    half_close(&mut s, Close::Shutdown).await;
    assert_eq!(peer.join().expect("peer thread"), b"head--middle--tail");
}

// **`poll_flush` is deliberately not asserted**: tokio's `TcpStream` and
// `UnixStream` answer `Poll::Ready(Ok(()))` from `poll_flush` unconditionally
// — they buffer nothing — so a mutant replacing the forwarding with that
// value computes the same function. The smol twin reaches the same verdict
// by measurement; here it is read off tokio's source (`net/tcp/stream.rs`,
// `net/unix/stream.rs`: `poll_flush` is `Poll::Ready(Ok(()))`).

/// `IPC_SUPPORT.unix` is a claim with a connect behind it: where it says
/// `true`, a Unix-domain connect works and the stream carries bytes both
/// ways across a half-close. `TokioIo`'s `Unix` arm is also the second arm
/// of its `either!` macro, so this is what says the macro routes it.
#[cfg(unix)]
#[tokio::test]
async fn a_unix_socket_connects_and_half_closes_when_ipc_says_so() {
    const { assert!(<Tokio as IpcConnect>::IPC_SUPPORT.unix) };

    let dir = std::env::temp_dir().join(format!(
        "hclient-tokio-unix-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind unix socket");
    let peer = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        s.set_read_timeout(Some(BOUND)).expect("read timeout");
        let mut got = Vec::new();
        s.read_to_end(&mut got).expect("EOF from the half-close");
        s.write_all(b"unix-answer").expect("answer");
        got
    });

    let mut s = bounded(Tokio.connect_ipc(&hclient_rt::IpcAddr::unix(&path)))
        .await
        .expect("connect_ipc to a bound unix socket");
    write_all(&mut s, b"over-a-unix-socket")
        .await
        .expect("write");
    half_close(&mut s, Close::Shutdown).await;
    assert_eq!(read_to_end(&mut s).await, b"unix-answer");
    assert_eq!(peer.join().expect("peer thread"), b"over-a-unix-socket");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The control for the test above: a path nobody listens on is an error,
/// not a stream that fails on first use.
#[cfg(unix)]
#[tokio::test]
async fn connect_ipc_to_a_path_with_no_listener_is_an_error() {
    let missing = std::env::temp_dir().join(format!(
        "hclient-tokio-unix-absent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_nanos()
    ));
    let r = bounded(Tokio.connect_ipc(&hclient_rt::IpcAddr::unix(&missing))).await;
    assert!(
        r.is_err(),
        "connecting to a path with no listener must fail"
    );
}
