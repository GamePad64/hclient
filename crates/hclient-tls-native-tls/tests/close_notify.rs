//! Closing the session sends `close_notify`, and the peer is what says so.
//!
//! # The distinction, and why only the peer can see it
//!
//! *A peer cannot tell a bare FIN from a truncation attack*, and the layer
//! where the alert is produced is this crate's: both closes —
//! `hclient_rt::Shutdown::poll_shutdown`, which is what this family calls,
//! and `futures_io::AsyncWrite::poll_close`, which a caller holding the
//! stream as plain `futures-io` calls — go through
//! `native_tls::TlsStream::shutdown`. Each was a live mutation survivor in
//! its turn: replaced by `Poll::from(Ok(()))` it skips the alert, returns
//! success, and every other test in this crate passes, because a client
//! that has already read its bytes cannot tell whether it said goodbye.
//! `poll_close` survived a second time after the seam's half-close became
//! `poll_shutdown` and every test here moved with it.
//!
//! So the assertion is made from the other end. A TLS peer reading to the
//! end of a session distinguishes the two outcomes itself: a `close_notify`
//! surfaces as a clean end of stream, a bare FIN as
//! `UnexpectedEof` — which is precisely the signal that exists so a
//! truncated response cannot be passed off as a complete one. The fixture
//! reports which it saw and the test asserts the first.
//!
//! Measured while writing: with the close intact the peer reports a
//! clean end; with the mutation applied it does not, and this file fails
//! while the rest of the suite stays green.

use hclient_rt::Shutdown as _;
use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_native_tls::NativeTls;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// What the server made of the end of the session.
///
/// A `String` rather than an enum so that an unexpected outcome names
/// itself in the failure message instead of arriving as a wildcard — the
/// interesting failures here are the ones nobody predicted.
type Outcome = Arc<Mutex<Option<String>>>;

/// A TLS server that accepts one connection, reads to the end of the
/// session, and records how it ended.
fn spawn_tls_server(outcome: Outcome) -> (std::net::SocketAddr, Vec<u8>) {
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
            if let Ok((tcp, _)) = listener.accept().await
                && let Ok(mut tls) = acceptor.accept(tcp).await
            {
                use tokio::io::AsyncReadExt as _;
                let mut sink = [0u8; 64];
                // The whole of the measurement. `rustls` maps a
                // `close_notify` to an ordinary end of stream and a
                // FIN without one to `UnexpectedEof`, which is the
                // distinction TLS defines the alert for.
                let seen = match tls.read(&mut sink).await {
                    Ok(0) => "clean end of stream".to_owned(),
                    Ok(n) => format!("{n} bytes of data, which this test sends none of"),
                    Err(e) => format!("error: {:?}", e.kind()),
                };
                *outcome.lock().expect("outcome") = Some(seen);
            }
        });
    });
    (addr, der)
}

/// **A closed session ends with `close_notify`, and the peer sees a clean
/// end of stream rather than a truncation.**
///
/// The client writes nothing: the only thing crossing the wire after the
/// handshake is the alert, so the peer's answer is about that and nothing
/// else. With `TlsStream::poll_shutdown` replaced by `Ok(())` the peer reports
/// an `UnexpectedEof` instead and this line fails.
#[tokio::test]
async fn closing_the_session_sends_close_notify_and_the_peer_sees_a_clean_end() {
    let outcome: Outcome = Arc::new(Mutex::new(None));
    let (addr, der) = spawn_tls_server(Arc::clone(&outcome));
    let root = native_tls::Certificate::from_der(&der).expect("the fixture's own DER is a root");

    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .expect("tcp");
    let (mut stream, _) = tokio::time::timeout(
        OP_TIMEOUT,
        NativeTls::new()
            .with_root_certificate(root)
            .connect(tcp, TlsRequest::new("localhost", &[])),
    )
    .await
    .expect("handshake within the bound")
    .expect("a root the client was given must verify");

    tokio::time::timeout(
        OP_TIMEOUT,
        std::future::poll_fn(|cx| Pin::new(&mut stream).poll_shutdown(cx)),
    )
    .await
    .expect("close within the bound")
    .expect("close");
    // The TCP FIN follows the alert, and the peer needs both to answer.
    drop(stream);

    // Polling rather than one sleep: the fixture is on its own runtime in
    // its own thread, so the answer arrives when it arrives, and a fixed
    // wait is either flaky or slow.
    let deadline = std::time::Instant::now() + OP_TIMEOUT;
    let seen = loop {
        if let Some(seen) = outcome.lock().expect("outcome").clone() {
            break seen;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the server never reached the end of the session within {OP_TIMEOUT:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    assert_eq!(
        seen, "clean end of stream",
        "the peer must see a close_notify. Anything else means the alert was \
         not sent, and a peer cannot tell a bare FIN from a truncation attack \
         — which is the entire reason TLS has the alert"
    );
}

/// A transport that records which close reached it, and can answer the
/// first `poll_shutdown` with `Pending`.
#[derive(Debug)]
struct Recording<S> {
    inner: S,
    closed: Arc<AtomicBool>,
    shut: Arc<AtomicBool>,
    /// Answer the next `poll_shutdown` with `Pending` (waking at once), as
    /// a transport with its own buffered layer would.
    pend_next_shutdown: bool,
}

impl<S: futures_io::AsyncRead + Unpin> futures_io::AsyncRead for Recording<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: futures_io::AsyncWrite + Unpin> futures_io::AsyncWrite for Recording<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.closed.store(true, Ordering::SeqCst);
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

impl<S: hclient_rt::Shutdown + Unpin> hclient_rt::Shutdown for Recording<S> {
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if std::mem::take(&mut self.pend_next_shutdown) {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.shut.store(true, Ordering::SeqCst);
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

type Session = <NativeTls as TlsConnect>::Stream<Recording<hclient_rt_tokio::TokioIo>>;

struct Fixture {
    stream: Session,
    outcome: Outcome,
    closed: Arc<AtomicBool>,
    shut: Arc<AtomicBool>,
}

async fn session(pend_next_shutdown: bool) -> Fixture {
    let outcome: Outcome = Arc::new(Mutex::new(None));
    let (addr, der) = spawn_tls_server(Arc::clone(&outcome));
    let root = native_tls::Certificate::from_der(&der).expect("the fixture's own DER is a root");
    let closed = Arc::new(AtomicBool::new(false));
    let shut = Arc::new(AtomicBool::new(false));
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .expect("tcp");
    let io = Recording {
        inner: tcp,
        closed: Arc::clone(&closed),
        shut: Arc::clone(&shut),
        pend_next_shutdown,
    };
    let (stream, _) = tokio::time::timeout(
        OP_TIMEOUT,
        NativeTls::new()
            .with_root_certificate(root)
            .connect(io, TlsRequest::new("localhost", &[])),
    )
    .await
    .expect("handshake within the bound")
    .expect("a root the client was given must verify");
    Fixture {
        stream,
        outcome,
        closed,
        shut,
    }
}

async fn peer_saw(outcome: &Outcome) -> String {
    let deadline = std::time::Instant::now() + OP_TIMEOUT;
    loop {
        if let Some(seen) = outcome.lock().expect("outcome").clone() {
            return seen;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the server never reached the end of the session within {OP_TIMEOUT:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// **`poll_close` sends `close_notify` and then closes the transport** —
/// the same half-close `poll_shutdown` is, reached through `futures-io`.
///
/// Two observations, because there were two defects to rule out: a
/// `poll_close` that skipped the alert (the peer sees a truncation), and
/// one that sent the alert and never asked the transport for its FIN,
/// which is what this crate shipped until the Shutdown split made nobody
/// call it. The stream is held, not dropped, so the FIN cannot come from
/// the drop instead.
#[tokio::test]
async fn poll_close_sends_close_notify_and_closes_the_transport() {
    let mut f = session(false).await;
    tokio::time::timeout(
        OP_TIMEOUT,
        std::future::poll_fn(|cx| futures_io::AsyncWrite::poll_close(Pin::new(&mut f.stream), cx)),
    )
    .await
    .expect("close within the bound")
    .expect("close");

    assert!(
        f.closed.load(Ordering::SeqCst),
        "the transport's own poll_close must be reached, or no FIN is sent"
    );
    assert!(
        !f.shut.load(Ordering::SeqCst),
        "and through poll_close, which is the trait the caller used"
    );
    assert_eq!(
        peer_saw(&f.outcome).await,
        "clean end of stream",
        "the peer must see a close_notify before the transport closes"
    );
    drop(f.stream);
}

/// **A half-close whose transport answers `Pending` once still completes.**
///
/// OpenSSL's `SSL_shutdown` sends the alert on its first call and tries to
/// *receive* the peer's on every call after. So a close re-polled after the
/// transport said `Pending` must not call it again: the peer here reads to
/// the end and hangs up without an alert of its own, as an HTTP peer does,
/// and a second call turns a finished half-close into an error. Measured
/// by making `poll_close_notify` forget that the alert went out.
#[tokio::test]
async fn a_half_close_re_polled_after_the_transport_waited_does_not_resend() {
    let mut f = session(true).await;
    tokio::time::timeout(
        OP_TIMEOUT,
        std::future::poll_fn(|cx| Pin::new(&mut f.stream).poll_shutdown(cx)),
    )
    .await
    .expect("close within the bound")
    .expect("the half-close must complete once the transport stops waiting");

    assert!(f.shut.load(Ordering::SeqCst), "the FIN was asked for");
    assert_eq!(peer_saw(&f.outcome).await, "clean end of stream");
}
