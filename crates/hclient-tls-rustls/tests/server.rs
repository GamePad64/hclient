//! A minimal TLS echo server on a self-signed certificate.
//! Lives in dev-dependencies and never reaches the public dependency graph.

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// # Panics
///
/// Panics if generating the self-signed certificate, building the server
/// config, or binding the listener fails — any of which means the test
/// fixture itself is broken, not the code under test.
#[allow(
    dead_code,
    reason = "this module is shared by several test binaries and each uses its own subset; `dead_code` is per-binary. `truncation_detection` is the first consumer that takes only `HyperIo`."
)]
pub fn spawn_tls_echo() -> (SocketAddr, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.signing_key.serialize_der();

    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der.clone().into()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )
        .unwrap();
    let mut cfg = cfg;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = tls.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if tls.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
    });

    (addr, cert_der)
}

/// A TLS server that **pushes** `n` bytes as fast as the socket takes
/// them, without waiting to be asked — a blob download rather than an
/// echo. `spawn_tls_echo`'s read-then-answer loop delivers in the
/// reader's own rhythm, so nothing can accumulate on the client side;
/// this one is what puts a slow reader behind.
///
/// # Panics
///
/// On any failure to build the certificate, the config, the listener or
/// the runtime — this is a fixture, and a fixture that cannot start has
/// nothing to say about the code under test.
#[allow(
    dead_code,
    reason = "this module is shared by several test binaries and each uses its own subset; `dead_code` is per-binary."
)]
pub fn spawn_tls_pusher(n: usize) -> (SocketAddr, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.signing_key.serialize_der();

    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der.clone().into()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )
        .unwrap();
    let mut cfg = cfg;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let body: Vec<u8> =
                        (0..n).map(|i| u8::try_from(i % 251).unwrap_or(0)).collect();
                    let _ = tls.write_all(&body).await;
                    let _ = tls.flush().await;
                });
            }
        });
    });
    (addr, cert_der)
}

/// The seam → `hyper::rt`, as a test double.
///
/// **A deliberate duplicate of `hclient_native::hyperio::HyperIo`, and the
/// duplication is the dependency graph rather than an oversight.**
/// `TlsStream` is written against `futures_io::{AsyncRead, AsyncWrite}`
/// plus `hclient_rt::Shutdown`; `hyper::client::conn::http1::handshake`
/// accepts `hyper::rt::Read + Write` and nothing else. The crate that owns
/// that conversion in the shipped stack is `hclient-native` — which
/// **dev-depends on this crate**, so depending on it from here would be a
/// cycle cargo tolerates in a workspace and refuses at package time. That
/// is not hypothetical: `just package-build` caught exactly this shape
/// once, between `hclient` and its two backends, and it would have blocked
/// the whole publication.
///
/// So this is twenty lines of test scaffolding, not a second
/// implementation anything ships. What it must stay faithful to is the one
/// thing a wrong copy would hide: `hyper::rt::Write::poll_shutdown` is the
/// **half-close**, so it forwards to `hclient_rt::Shutdown` and never to
/// `futures_io::AsyncWrite::poll_close`.
#[allow(
    dead_code,
    reason = "this module is shared by several test binaries and each uses its own subset; `dead_code` is per-binary."
)]
#[derive(Debug)]
pub struct HyperIo<S> {
    inner: S,
    scratch: Box<[u8]>,
}

#[allow(
    dead_code,
    reason = "this module is shared by several test binaries and each uses its own subset; `dead_code` is per-binary."
)]
impl<S> HyperIo<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            scratch: vec![0u8; 16 * 1024].into_boxed_slice(),
        }
    }
}

impl<S: futures_io::AsyncRead + Unpin> hyper::rt::Read for HyperIo<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let want = buf.remaining().min(self.scratch.len());
        if want == 0 {
            return std::task::Poll::Ready(Ok(()));
        }
        let this = &mut *self;
        let n = std::task::ready!(
            std::pin::Pin::new(&mut this.inner).poll_read(cx, &mut this.scratch[..want])
        )?;
        buf.put_slice(&this.scratch[..n]);
        std::task::Poll::Ready(Ok(()))
    }
}

impl<S: futures_io::AsyncWrite + hclient_rt::Shutdown + Unpin> hyper::rt::Write for HyperIo<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    /// The half-close, not `poll_close` — hyper shuts the writing half and
    /// goes on reading the response.
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }
}
