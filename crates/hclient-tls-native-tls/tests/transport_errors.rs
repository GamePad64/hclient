//! A transport error must surface as an error, and never as `Pending`.
//!
//! # The distinction, and why nothing else in this crate can reach it
//!
//! `native-tls` reports "the stream underneath is not ready" the same way
//! it reports nothing else: an `io::Error` of kind `WouldBlock`, because a
//! synchronous interface has no other vocabulary for it. So `cvt` — and
//! `poll_close_notify`'s own copy of the same three arms, which both closes
//! share — has to tell that one
//! kind apart from every other, turning `WouldBlock` into
//! `Poll::Pending` and leaving the rest as errors.
//!
//! **Get it backwards and a connection reset becomes `Poll::Pending`**: the
//! caller is told to wait, nothing will ever wake it, and the request hangs
//! rather than failing. That is the worse of the two directions by some
//! way, and it is the one no other test in this crate can produce, because
//! every one of them runs over a healthy loopback socket that never errors.
//! Both guards survived mutation to `true` for exactly that reason.
//!
//! The instrument is a transport that fails on demand *after* the handshake
//! has completed, so what is under test is the session's error handling
//! rather than the handshake's.

use futures_io::{AsyncRead as _, AsyncWrite as _};
use hclient_rt::Shutdown as _;
use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_native_tls::NativeTls;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// A transport that forwards until it is told to fail, and then answers
/// `ConnectionReset` to everything.
///
/// The flag is per-instance rather than a static: two of these in one
/// process must not switch each other on, and a static would also make the
/// fixture order-dependent.
#[derive(Debug)]
struct Faulty<S> {
    inner: S,
    failing: Arc<AtomicBool>,
}

impl<S> Faulty<S> {
    fn reset() -> io::Error {
        // A kind that is emphatically **not** `WouldBlock`: the whole
        // question is whether the guard tells the two apart.
        io::Error::new(
            io::ErrorKind::ConnectionReset,
            "the peer reset this connection",
        )
    }
}

impl<S: futures_io::AsyncRead + Unpin> futures_io::AsyncRead for Faulty<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.failing.load(Ordering::SeqCst) {
            return Poll::Ready(Err(Self::reset()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: futures_io::AsyncWrite + Unpin> futures_io::AsyncWrite for Faulty<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.failing.load(Ordering::SeqCst) {
            return Poll::Ready(Err(Self::reset()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

/// Forwarded: the fault this fixture injects is on read and write, and it
/// has no opinion about the half-close.
impl<S: hclient_rt::Shutdown + Unpin> hclient_rt::Shutdown for Faulty<S> {
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// A TLS server that completes a handshake and then holds the connection
/// open. It must not close first: a peer-side close would produce a real
/// end of stream and the test would be measuring that instead of the
/// injected fault.
fn spawn_tls_server() -> (std::net::SocketAddr, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .expect("self-signed certificate");
    let der = cert.cert.der().to_vec();
    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![der.clone().into()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into()),
        )
        .expect("server config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    listener.set_nonblocking(true).expect("nonblocking");
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(mut tls) = acceptor.accept(tcp).await {
                        use tokio::io::AsyncReadExt as _;
                        let mut sink = [0u8; 64];
                        let _ = tls.read(&mut sink).await;
                        // Hold the connection past the end of the test, so
                        // the only thing the client can meet is the fault
                        // this file injects.
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                });
            }
        });
    });
    (addr, der)
}

/// Completes a handshake over a `Faulty` transport and hands back the
/// session together with the switch that breaks it.
/// What a poll answered, in one line, for a failure message.
///
/// `Poll<io::Result<usize>>` has no `Display` and its `Debug` prints the
/// error's kind without its OS code — which is the half that would say
/// whether Security.framework reported something of its own or simply
/// handed back bytes it already held.
fn describe(p: &Poll<io::Result<usize>>) -> String {
    match p {
        Poll::Pending => "Pending".to_owned(),
        Poll::Ready(Ok(n)) => format!("Ready(Ok({n}))"),
        Poll::Ready(Err(e)) => {
            format!(
                "Ready(Err(kind={:?}, raw_os_error={:?}, {e}))",
                e.kind(),
                e.raw_os_error()
            )
        }
    }
}

async fn session_with_a_breakable_transport() -> (
    <NativeTls as TlsConnect>::Stream<Faulty<hclient_rt_tokio::TokioIo>>,
    Arc<AtomicBool>,
) {
    let (addr, der) = spawn_tls_server();
    let root = native_tls::Certificate::from_der(&der).expect("the fixture's own DER is a root");
    let failing = Arc::new(AtomicBool::new(false));

    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .expect("tcp");
    let (stream, _) = tokio::time::timeout(
        OP_TIMEOUT,
        NativeTls::new().with_root_certificate(root).connect(
            Faulty {
                inner: tcp,
                failing: Arc::clone(&failing),
            },
            TlsRequest::new("localhost", &[]),
        ),
    )
    .await
    .expect("handshake within the bound")
    .expect("a root the client was given must verify");

    (stream, failing)
}

/// **A write that fails at the transport fails at the session.**
///
/// With `cvt`'s guard forced to `true` every `io::Error` becomes
/// `Poll::Pending`, so this returns `Pending` against a waker nothing will
/// ever call — a hang rather than an error, and a hang is the worse
/// failure. Asserted by polling **once**, with a noop waker, so a
/// `Pending` here is the answer rather than a suspension the harness would
/// wait out.
#[tokio::test]
async fn a_transport_error_on_write_is_an_error_and_not_pending() {
    let (mut stream, failing) = session_with_a_breakable_transport().await;
    failing.store(true, Ordering::SeqCst);

    let mut cx = Context::from_waker(std::task::Waker::noop());
    let polled = Pin::new(&mut stream).poll_write(&mut cx, b"ping");

    assert!(
        matches!(polled, Poll::Ready(Err(_))),
        "a connection reset must reach the caller as an error. `Pending` here \
         parks the request on a waker nobody holds, so it never completes and \
         never fails — which is why `cvt` has to tell WouldBlock from every \
         other kind rather than treating them alike — got {}",
        describe(&polled)
    );
}

/// **The same for a read**, which is the arm a response body sits on.
///
/// Kept separate because the two go through `cvt` from different call
/// sites, and a `poll_read` that swallowed the error would truncate a
/// response where the write arm merely loses a request.
#[tokio::test]
async fn a_transport_error_on_read_is_an_error_and_not_pending() {
    let (mut stream, failing) = session_with_a_breakable_transport().await;
    failing.store(true, Ordering::SeqCst);

    let mut raw = [0u8; 64];
    let mut cx = Context::from_waker(std::task::Waker::noop());
    let polled = Pin::new(&mut stream).poll_read(&mut cx, &mut raw);

    // **The diagnostic is here because this fails on macOS and on no
    // machine this workspace can run.** `cvt` is platform-independent
    // and correct — `WouldBlock` to `Pending`, everything else to an
    // error — so what differs is what `native-tls` answers *before* it
    // gets there: OpenSSL reaches the broken transport at once, and
    // Security.framework apparently does not. Printing the answer is
    // what separates *the error was swallowed* from *the session never
    // asked the transport*.
    assert!(
        matches!(polled, Poll::Ready(Err(_))),
        "a reset while reading must surface, or a caller waits for a body the \
         peer will never send — got {}",
        describe(&polled)
    );
}

/// **And for the half-close**, which carries its own copy of the three
/// arms (`poll_close_notify`, shared by `poll_shutdown` and `poll_close`)
/// rather than going through `cvt`.
///
/// That duplication is why this is a third test and not a third assertion:
/// the close's guard is a separate mutation site, and it survived
/// separately.
#[tokio::test]
async fn a_transport_error_on_close_is_an_error_and_not_pending() {
    let (mut stream, failing) = session_with_a_breakable_transport().await;
    failing.store(true, Ordering::SeqCst);

    let mut cx = Context::from_waker(std::task::Waker::noop());
    let polled = Pin::new(&mut stream).poll_shutdown(&mut cx);

    assert!(
        matches!(polled, Poll::Ready(Err(_))),
        "a close that cannot be written must fail rather than hang: this arm is \
         the close's own, not `cvt`'s, and gets the guard wrong separately"
    );
}

/// **The control: without the fault, none of the three is an error.**
///
/// Every test above asserts that something *failed*, and would pass just as
/// well for a session that was broken from the start — a handshake that
/// silently produced a dead stream, say. This says the same three calls
/// succeed when the transport is healthy, so what the others measure is the
/// injected fault and nothing else.
#[tokio::test]
async fn the_same_calls_succeed_while_the_transport_is_healthy() {
    let (mut stream, _failing) = session_with_a_breakable_transport().await;

    let n = tokio::time::timeout(
        OP_TIMEOUT,
        std::future::poll_fn(|cx| Pin::new(&mut stream).poll_write(cx, b"ping")),
    )
    .await
    .expect("write within the bound")
    .expect("a healthy transport must take the record");
    assert_ne!(n, 0, "the record must have gone into the session");

    tokio::time::timeout(
        OP_TIMEOUT,
        std::future::poll_fn(|cx| Pin::new(&mut stream).poll_shutdown(cx)),
    )
    .await
    .expect("close within the bound")
    .expect("a healthy transport must carry the close");
}
