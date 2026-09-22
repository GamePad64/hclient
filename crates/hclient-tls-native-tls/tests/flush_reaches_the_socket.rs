//! `flush` is forwarded the whole way down, and nothing but a counter can
//! see it.
//!
//! # Why this needs a file of its own
//!
//! A flush crosses four layers here — `TlsStream::poll_flush` ->
//! `StdAdapter::flush` -> `HyperIo::poll_flush` -> the transport — and
//! **every one of them survived mutation to `Ok(())`**. Not because the
//! call does nothing: measured on a real session, a `poll_flush` moves the
//! transport's flush count from 2 to 3, so each layer genuinely forwards.
//! It survived because the bottom of the chain on every test in this crate
//! is a TCP socket, whose flush has no observable effect — so a layer that
//! quietly stopped forwarding would be invisible to any assertion about
//! bytes.
//!
//! What makes it worth pinning rather than accepting is the layer this
//! chain exists to serve. `native-tls` fronts `SChannel` and
//! Security.framework as well as OpenSSL, and a stack that buffers a
//! record until it is flushed is a stack this crate would silently stall
//! against. The counting transport below is the only instrument that can
//! tell "forwarded" from "returned `Ok(())`", so it is what the three
//! layers are held to.
//!
//! The session is real — a handshake against the `rustls` fixture, trusted
//! by adding its root — because `StdAdapter` only ever runs underneath
//! `native-tls`, and only a completed handshake puts it there.

use futures_io::AsyncWrite as _;
use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_native_tls::NativeTls;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// Counts what reached the socket, per connection rather than globally —
/// nextest runs each test in its own process, but two connections inside
/// one test would still share a static and this file makes two.
#[derive(Debug, Default)]
struct Counts {
    flushes: AtomicUsize,
    writes: AtomicUsize,
}

/// A transparent transport that records the calls this file is about.
///
/// It forwards everything, so the session on top of it is an ordinary one:
/// what is under test is the *forwarding*, and a fixture that answered for
/// itself would be testing the fixture.
#[derive(Debug)]
struct Counting<S> {
    inner: S,
    counts: Arc<Counts>,
}

impl<S: futures_io::AsyncRead + Unpin> futures_io::AsyncRead for Counting<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: futures_io::AsyncWrite + Unpin> futures_io::AsyncWrite for Counting<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.counts.writes.fetch_add(1, Ordering::SeqCst);
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.counts.flushes.fetch_add(1, Ordering::SeqCst);
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

/// Forwarded: this fixture counts writes and flushes and has no opinion
/// about the half-close.
impl<S: hclient_rt::Shutdown + Unpin> hclient_rt::Shutdown for Counting<S> {
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// A TLS server that accepts a handshake and then reads. It never answers,
/// which is all this file needs: nothing here reads from the session.
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
                    }
                });
            }
        });
    });
    (addr, der)
}

async fn bounded<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(OP_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| panic!("did not resolve within {OP_TIMEOUT:?}"))
}

/// **A flush on the session reaches the socket**, through all three
/// forwarding layers.
///
/// The write before it is what makes the assertion about `flush` rather
/// than about the handshake: the handshake itself flushes, so a count taken
/// from zero would be satisfied by a session that never forwarded another
/// one. The delta is taken across the `poll_flush` call alone.
///
/// Any of the three layers replaced by `Ok(())` fails this line —
/// `TlsStream::poll_flush`, `StdAdapter::flush` and `HyperIo::poll_flush`
/// were all live survivors, and each was applied by hand to confirm it.
#[tokio::test]
async fn a_flush_on_the_session_reaches_the_transport() {
    let (addr, der) = spawn_tls_server();
    let root = native_tls::Certificate::from_der(&der).expect("the fixture's own DER is a root");
    let counts = Arc::new(Counts::default());

    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .expect("tcp");
    let (mut stream, _) = bounded(NativeTls::new().add_root_certificate(root).connect(
        Counting {
            inner: tcp,
            counts: Arc::clone(&counts),
        },
        TlsRequest::new("localhost", &[]),
    ))
    .await
    .expect("a root the client was given must verify");

    // A record to have something to flush. `native-tls` may or may not push
    // it to the socket on its own, which is exactly why the assertion below
    // is a delta rather than a total.
    let n = bounded(std::future::poll_fn(|cx| {
        Pin::new(&mut stream).poll_write(cx, b"ping")
    }))
    .await
    .expect("write");
    assert_ne!(n, 0, "the record must have gone into the session");

    let before = counts.flushes.load(Ordering::SeqCst);
    bounded(std::future::poll_fn(|cx| {
        Pin::new(&mut stream).poll_flush(cx)
    }))
    .await
    .expect("flush");
    let after = counts.flushes.load(Ordering::SeqCst);

    assert!(
        after > before,
        "a flush must be forwarded to the transport, not answered with Ok(()) \
         somewhere in the middle: the count stayed at {before}. A TCP socket's \
         flush does nothing observable, so nothing but this counter can tell \
         a forwarded flush from a swallowed one — and a stack that buffers a \
         record until it is flushed would stall against a layer that swallows."
    );
}

/// **A write reaches the socket too**, which is the control.
///
/// Without it the test above would pass for a transport that was never
/// reached at all — the counter would be zero on both sides of a `flush`
/// that never happened, and `after > before` would simply fail rather than
/// telling anyone why. This says the counting transport really is underneath
/// the session.
#[tokio::test]
async fn the_counting_transport_is_genuinely_under_the_session() {
    let (addr, der) = spawn_tls_server();
    let root = native_tls::Certificate::from_der(&der).expect("the fixture's own DER is a root");
    let counts = Arc::new(Counts::default());

    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .expect("tcp");
    let _stream = bounded(NativeTls::new().add_root_certificate(root).connect(
        Counting {
            inner: tcp,
            counts: Arc::clone(&counts),
        },
        TlsRequest::new("localhost", &[]),
    ))
    .await
    .expect("a root the client was given must verify");

    assert!(
        counts.writes.load(Ordering::SeqCst) > 0,
        "the handshake alone must have written through this transport, or the \
         flush test above is counting a layer nothing runs"
    );
}
