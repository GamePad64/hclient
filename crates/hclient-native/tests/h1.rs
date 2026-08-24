//! The server is a bare `std::net::TcpListener` speaking HTTP/1.1 by hand.
//! No server frameworks: the test is checking our client, not someone
//! else's server.
//!
//! IO is `hclient_native::testing::blocking_io`, a non-blocking
//! `TcpStream` with a busy-spin instead of a reactor (see its doc comment
//! for a detailed breakdown of why a literally blocking `poll_read` hangs
//! the exchange before the request is even sent — `hyper::proto::h1::
//! dispatch::Dispatcher::poll_loop` tries reading before writing, on
//! every iteration).
//!
//! # Why `block_on` here is wrapped in `bounded_block_on` rather than bare
//!
//! This is exactly the spot where the mutation "the inline drive of
//! `Connection` was removed" must produce a red test, not a hung process
//! — `body_keeps_driving_the_connection_after_headers` is built so that
//! the response's second chunk only arrives if someone keeps polling
//! `Connection` AFTER the headers; without that, `NativeBody::poll_frame`
//! would return `Pending` forever (the `Incoming` channel is empty, and
//! nothing is left to fill it, including `blocking_io`'s own busy-spin
//! `wake_by_ref` — it only wakes on SOCKET readiness, and the socket has
//! nothing to do with it here: nobody else is reading from it). A test
//! that hangs under mutation instead of failing wedges CI with no test
//! name and no diagnosis. The same technique as
//! `hclient-native::connect::tests::bounded_block_on` and
//! `tests/dual_runtime.rs`'s watchdog for `smol`: a separate watchdog
//! thread + `process::exit(101)`, no `Send` bound on `fut` itself.
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

fn spawn_h1_server(response: &'static str) -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = s.write_all(response.as_bytes());
            let _ = s.flush();
        }
    });
    addr
}

/// `futures_executor::block_on`, but with a `BOUND` ceiling. Not `F: Send`
/// — only an `Arc<AtomicBool>` crosses the thread boundary, `fut` itself
/// runs on the current thread as usual, so this doesn't undermine the
/// "runtime seam with no `Send`" that this file's tests are proving.
fn bounded_block_on<F: std::future::Future>(fut: F) -> F::Output {
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog_done = done.clone();
    std::thread::spawn(move || {
        std::thread::sleep(BOUND);
        if !watchdog_done.load(Ordering::SeqCst) {
            eprintln!(
                "bounded_block_on: did not finish within {BOUND:?} - looks like the \
                 connection stopped being polled (a regression of the inline drive); \
                 failing instead of wedging CI with no test name and no diagnosis"
            );
            std::process::exit(101);
        }
    });
    let result = futures_executor::block_on(fut);
    done.store(true, Ordering::SeqCst);
    result
}

#[test]
fn works_on_a_bare_futures_executor_with_no_spawn() {
    // The vertical's key test: no tokio, no smol — just futures::block_on.
    let addr = spawn_h1_server("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello");

    bounded_block_on(async move {
        let std_tcp = std::net::TcpStream::connect(addr).unwrap();
        // `blocking_io` reconfigures the socket itself (see its doc
        // comment); what's being checked here is that hyper needs neither
        // spawn nor a timer — not what mode the socket is in.
        let io = hclient_native::testing::blocking_io(std_tcp);
        let req = http::Request::builder()
            .uri("/")
            .body(hclient_native::testing::empty_body())
            .unwrap();
        let resp = hclient_native::testing::exchange_for_test(io, req)
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = hclient_native::testing::collect(resp.into_body())
            .await
            .unwrap();
        assert_eq!(&body[..], b"hello");
    });
}

#[test]
fn body_keeps_driving_the_connection_after_headers() {
    // The body arrives as a separate chunk after the headers: if the
    // connection stopped being polled, the read would hang.
    let addr = spawn_h1_server(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
    );
    bounded_block_on(async move {
        let std_tcp = std::net::TcpStream::connect(addr).unwrap();
        let io = hclient_native::testing::blocking_io(std_tcp);
        let req = http::Request::builder()
            .uri("/")
            .body(hclient_native::testing::empty_body())
            .unwrap();
        let resp = hclient_native::testing::exchange_for_test(io, req)
            .await
            .unwrap();
        let body = hclient_native::testing::collect(resp.into_body())
            .await
            .unwrap();
        assert_eq!(&body[..], b"hello");
    });
}
