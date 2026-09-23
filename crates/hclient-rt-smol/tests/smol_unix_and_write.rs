//! The two halves of `SmolSocket` that nothing in this crate reached: the
//! Unix-domain arm, and the write side of `AsyncWrite`.
//!
//! Both were whole-method mutation survivors. `connect_ipc` — `connect_unix`
//! then — had no test at all: `IPC_SUPPORT.unix` is `cfg!(unix)` and this crate says so, but
//! the claim and the connect were each unasserted — and every one of
//! `poll_write`, `poll_flush`, `poll_close` and `poll_write_vectored`
//! could be replaced by a success that moves no bytes with the suite
//! staying green, because `adversarial_smol_io.rs` is a **read**-side
//! suite: it writes to the socket from the plain `std` end and reads
//! through `FuturesIo`, so the wrapper's own write path is never driven.
//!
//! The two live in one file because the Unix arm is what makes the write
//! tests discriminate the `either!` macro as well: a `poll_write` that
//! only ever saw a TCP stream would pass for a macro that routed both
//! variants to `SmolSocket::Tcp`, and `tcp()` panics on a Unix stream, so
//! the arms cannot be confused silently.
use futures_lite::io::AsyncWrite as _;
use hclient_rt::Shutdown as _;
use hclient_rt::{IpcConnect, TcpConnect, TcpOpts};
use hclient_rt_smol::{Smol, SmolSocket};
use std::future::poll_fn;
use std::io::Read as _;
use std::pin::Pin;
use std::time::Duration;

/// Every wait here is bounded, for `adversarial_smol_connect.rs`'s stated
/// reason: an unbounded await on a regression wedges the binary with no
/// diagnosis instead of failing.
const BOUND: Duration = Duration::from_secs(10);

async fn bounded<T>(
    fut: impl std::future::Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    futures_lite::future::or(fut, async {
        async_io::Timer::after(BOUND).await;
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("did not resolve within {BOUND:?} — treating a stall as a regression"),
        ))
    })
    .await
}

/// Write through the wrapper until the whole buffer is gone.
///
/// `poll_write` is allowed to write less than it was given, so a test that
/// called it once and asserted the byte count would be pinning this
/// kernel's socket buffer rather than the wrapper. Looping is what makes
/// the assertion *the bytes arrive* — which is the thing a `Poll::Ready(Ok(0))`
/// or `Ok(1)` mutant breaks.
async fn write_all(s: &mut SmolSocket, mut buf: &[u8]) -> std::io::Result<()> {
    let mut guard = 0;
    while !buf.is_empty() {
        let n = poll_fn(|cx| Pin::new(&mut *s).poll_write(cx, buf)).await?;
        // An `Ok(0)` for a non-empty buffer is the mutant, and it would
        // otherwise spin here for ever rather than fail.
        assert!(n > 0, "poll_write returned 0 for a non-empty buffer");
        buf = &buf[n..];
        guard += 1;
        assert!(guard < 10_000, "poll_write made no progress");
    }
    Ok(())
}

/// The bytes a caller writes through `SmolSocket` are the bytes
/// the peer reads.
///
/// This is the floor under `poll_write` and `poll_flush`. Measured with
/// each mutation applied by hand: `poll_write -> Ok(0)` and `-> Ok(1)`
/// both fail here, and so does routing the write nowhere.
#[test]
fn what_is_written_through_the_wrapper_is_what_the_peer_reads() {
    // The peer end, read from a thread so the assertion is what *arrived*
    // rather than what this side believed it sent.
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let peer_addr = l.local_addr().expect("local_addr");
    let joiner = std::thread::spawn(move || {
        let (mut s, _) = l.accept().expect("accept");
        let mut got = Vec::new();
        // Read until EOF, which only arrives once the client's half-close
        // has actually been sent — see the `poll_close` test below.
        let _ = s.read_to_end(&mut got);
        got
    });

    futures_executor::block_on(async {
        let mut s = bounded(Smol.connect(peer_addr, &TcpOpts::default()))
            .await
            .expect("connect");
        write_all(&mut s, b"the-bytes-that-were-written")
            .await
            .expect("write");
        // `poll_flush` is called here because a caller writing bytes does,
        // and **not** as an assertion: see the note at the foot of this
        // file for why that mutant cannot be killed.
        bounded(poll_fn(|cx| Pin::new(&mut s).poll_flush(cx)))
            .await
            .expect("flush");
        // Half-close, so the peer's `read_to_end` terminates. That this
        // works at all is the `poll_close` assertion.
        bounded(poll_fn(|cx| Pin::new(&mut s).poll_shutdown(cx)))
            .await
            .expect("shutdown");
    });

    let got = joiner.join().expect("reader thread");
    assert_eq!(
        got, b"the-bytes-that-were-written",
        "the peer must read exactly what was written through the wrapper"
    );
}

/// `poll_close` really sends FIN **while the socket is still open**,
/// rather than answering `Ok(())` and leaving the peer waiting.
///
/// The discriminator is the *liveness* of this side, and getting that
/// wrong is what made the first version of this test worthless. It
/// shut down, slept 200 ms and then let the socket drop — and it passed
/// with `poll_close` replaced by `Poll::Ready(Ok(()))`, because dropping a
/// `TcpStream` closes the descriptor and the peer sees EOF from the drop
/// rather than from the half-close. Measured, 5 of 5 green with the
/// mutation applied; the assertion was true for the wrong reason.
///
/// So the socket is **held** across the observation: the reader reports
/// EOF on a channel, and this side does not drop its stream until that
/// report has arrived. A `poll_close` that sends nothing leaves the reader
/// blocked, the channel silent, and this test failing on its bound instead
/// of passing on a drop. `shutdown_is_done`'s own unit test pins the
/// `ENOTCONN` rule over a `Result`; this pins that the call producing that
/// `Result` is made at all.
#[test]
fn poll_close_half_closes_so_the_peer_sees_eof() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("local_addr");
    let (tx, rx) = std::sync::mpsc::channel();
    let joiner = std::thread::spawn(move || {
        let (mut s, _) = l.accept().expect("accept");
        let mut one = [0u8; 1];
        let n = s.read(&mut one).expect("read");
        // Report before this thread's own socket is dropped, so the
        // channel carries the half-close and nothing else.
        let _ = tx.send(n);
        n
    });

    futures_executor::block_on(async {
        let mut s = bounded(Smol.connect(addr, &TcpOpts::default()))
            .await
            .expect("connect");
        bounded(poll_fn(|cx| Pin::new(&mut s).poll_shutdown(cx)))
            .await
            .expect("shutdown");
        // The socket is still alive here, and stays alive until the peer
        // has answered. `recv_timeout` rather than `recv`, so a
        // `poll_close` that sends no FIN fails on a bound instead of
        // hanging the binary with nothing to investigate.
        let seen = rx
            .recv_timeout(BOUND)
            .expect("the peer must observe EOF from the half-close, with this socket still open");
        assert_eq!(
            seen, 0,
            "poll_close must send FIN: the peer's read has to report EOF"
        );
        drop(s);
    });

    assert_eq!(joiner.join().expect("reader thread"), 0);
}

/// The vectored write path carries every buffer, in order.
///
/// `poll_write_vectored` has its own mutants (`Ok(0)`, `Ok(1)`) and its own
/// `either!` arm, and nothing reached it: hyper writes through it for a
/// head-plus-body, so it is a real path rather than a courtesy
/// implementation. Three slices rather than one, because a single slice
/// cannot tell a correct implementation from one that writes only the
/// first.
#[test]
fn a_vectored_write_carries_every_buffer_in_order() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("local_addr");
    let joiner = std::thread::spawn(move || {
        let (mut s, _) = l.accept().expect("accept");
        let mut got = Vec::new();
        let _ = s.read_to_end(&mut got);
        got
    });

    futures_executor::block_on(async {
        let mut s = bounded(Smol.connect(addr, &TcpOpts::default()))
            .await
            .expect("connect");
        let parts: [&[u8]; 3] = [b"head--", b"middle--", b"tail"];
        let mut written = 0usize;
        let total: usize = parts.iter().map(|p| p.len()).sum();
        let mut guard = 0;
        while written < total {
            // Rebuild the slice list each turn from what is left, because
            // a short vectored write is legal and the wrapper is allowed
            // to take any prefix.
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
        bounded(poll_fn(|cx| Pin::new(&mut s).poll_shutdown(cx)))
            .await
            .expect("shutdown");
    });

    assert_eq!(
        joiner.join().expect("reader thread"),
        b"head--middle--tail",
        "every buffer of a vectored write must arrive, in order"
    );
}

/// `IPC_SUPPORT.unix` is not a free-floating claim: where it says `true`, a
/// Unix-domain connect really works, and the stream really carries bytes.
///
/// `connect_ipc`'s wildcard arm is a refusal, so the whole of the Unix arm is a thing that can be removed with
/// nothing local noticing. `SmolSocket::Unix` is also the second `either!`
/// arm, so this is what says the macro routes it rather than falling
/// through to the TCP one.
///
/// It pins the `#[cfg(unix)]` arm, which is the one Linux compiles. The
/// refusal every other target takes is `RefuseIpc`, pinned in
/// `hclient-rt` itself.
#[cfg(unix)]
#[test]
fn a_unix_socket_connects_and_carries_bytes_when_ipc_says_so() {
    // A `const` block, because the claim is a compile-time one: this
    // function is `#[cfg(unix)]` and `IPC_SUPPORT.unix` is `cfg!(unix)`, so
    // the two must agree before anything runs. Written as a runtime
    // `assert!` first, which clippy correctly refused as an assertion on a
    // constant.
    const { assert!(<Smol as IpcConnect>::IPC_SUPPORT.unix) };

    let dir = std::env::temp_dir().join(format!(
        "hclient-smol-unix-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("sock");

    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind unix socket");
    let joiner = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let mut got = Vec::new();
        let _ = s.read_to_end(&mut got);
        got
    });

    futures_executor::block_on(async {
        let mut s = bounded(Smol.connect_ipc(&hclient_rt::IpcAddr::unix(&path)))
            .await
            .expect("connect_ipc to a bound unix socket");
        write_all(&mut s, b"over-a-unix-socket")
            .await
            .expect("write");
        bounded(poll_fn(|cx| Pin::new(&mut s).poll_shutdown(cx)))
            .await
            .expect("shutdown");
    });

    assert_eq!(
        joiner.join().expect("reader thread"),
        b"over-a-unix-socket",
        "a Unix-domain stream must carry the bytes written through it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The negative half: a path nobody is listening on is refused, rather
/// than answering a stream that fails on first use.
///
/// The control for the test above — without it, a `connect_ipc` that
/// returned a connected-looking stream for every path would pass the
/// positive case only by luck of the ordering.
#[cfg(unix)]
#[test]
fn connect_ipc_to_a_path_with_no_listener_is_an_error() {
    let missing = std::env::temp_dir().join(format!(
        "hclient-smol-unix-absent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_nanos()
    ));
    futures_executor::block_on(async {
        let r = bounded(Smol.connect_ipc(&hclient_rt::IpcAddr::unix(&missing))).await;
        let err = r
            .map(|_| ())
            .expect_err("connecting to a path with no listener must fail");
        println!("connect_ipc to an absent path: {:?}", err.kind());
    });
}

// **`poll_flush` is deliberately not asserted, and the verdict is a
// measurement rather than an omission.** Replacing its body with
// `Poll::Ready(Ok(()))` computes the same function, because
// `async_net::TcpStream::poll_flush` loops over
// `std::io::Write::flush` on the underlying `TcpStream`, and that is
// documented as a no-op — measured in four states, including the two that
// could plausibly fail: a healthy socket, after a write, after the peer
// went away, and after our own `shutdown(Write)`. All four answer
// `Ok(())`.
//
// So there is no input that separates the mutant from the original, and a
// test for one is impossible by construction rather than merely missing —
// the same verdict `.notes/mutation-survivors-classified.md` reaches about
// `hclient-idn`'s `|`→`^` mutants on disjoint bit words, and for the same
// reason: an equivalent mutant is not a gap.
//
// The call stays in `what_is_written_through_the_wrapper_is_what_the_peer_reads`
// because a caller writing bytes does call it, so the path is exercised;
// what it is not is evidence.
