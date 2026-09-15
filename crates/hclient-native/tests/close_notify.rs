//! The TLS connection is shut down cleanly, and the server sees
//! `close_notify` rather than a bare FIN.
//!
//! **The defect this pins is a shutdown this client believes it performed
//! and did not.** `Conn` is the IO every request travels over, one enum
//! forwarding to whichever half it holds, and its `poll_shutdown` is what
//! reaches `TlsStream::poll_shutdown` — which is where rustls' outgoing
//! `close_notify` alert is written. Replacing that delegation with
//! `Poll::Ready(Ok(()))` left **all 579 tests of this crate green**
//! before this file existed.
//!
//! Measured rather than assumed: across this crate's whole suite
//! `poll_shutdown` is reached **74 times on the TLS arm** against 8 on
//! the plaintext one, so the TLS path is where the method earns its
//! keep — and it is also the only one with a wire-visible consequence.
//!
//! # Why this is asserted on TLS and not on TCP
//!
//! A plaintext half-close is observable only as EOF, and **a full close
//! produces EOF too** — a fixture that reads to `Ok(0)` passes whether
//! the client shut its write half or dropped the socket. One was written
//! against the plaintext arm and deleted for exactly that reason.
//!
//! TLS has no such ambiguity, and this workspace's own `stream.rs` says
//! why at length: rustls distinguishes a genuine `close_notify` from a
//! raw TCP close, because collapsing them is a truncation-attack hole.
//! That distinction surfaces here as the server's own read —
//! `Ok(0)` for a clean shutdown against `UnexpectedEof` for a bare FIN —
//! so the assertion is about which of the two the peer saw, and nothing
//! about it is timing-dependent.
#![cfg(not(target_family = "wasm"))]

use hclient_core::body::RequestBody;
use hclient_core::transport::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use http_body_util::BodyExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(20);

/// How the client's end of the TLS session finished, from the server.
#[derive(Debug, PartialEq, Eq)]
enum Ending {
    /// A `close_notify` alert arrived: `tokio_rustls` reports the clean
    /// end of the session as an ordinary end of stream.
    CloseNotify,
    /// The TCP connection ended with no alert. rustls reports this as
    /// `UnexpectedEof` precisely so it cannot be mistaken for the above.
    BareFin,
    /// Neither, within the fixture's own patience.
    Nothing,
}

fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])
        .expect("a self-signed certificate");
    let key = PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    (cert.cert.der().clone(), key)
}

/// A TLS server that answers one request and then reads its socket to the
/// end, reporting how that end arrived.
fn server() -> (SocketAddr, CertificateDer<'static>, mpsc::Receiver<Ending>) {
    let (cert_der, key_der) = identity();
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    listener.set_nonblocking(true).expect("nonblocking");
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while tls.read_exact(&mut byte).await.is_ok() {
                        head.push(byte[0]);
                        if head.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let _ = tls
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi",
                        )
                        .await;
                    let _ = tls.flush().await;

                    // How does this session end? `read` answers `Ok(0)`
                    // for a `close_notify` and `UnexpectedEof` for a TCP
                    // close without one — rustls draws that line on
                    // purpose, and it is the whole assertion.
                    let mut sink = [0u8; 64];
                    let ending =
                        match tokio::time::timeout(Duration::from_secs(5), tls.read(&mut sink))
                            .await
                        {
                            Ok(Ok(0)) => Ending::CloseNotify,
                            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                                Ending::BareFin
                            }
                            // Any other read is the response's own trailing
                            // bytes being drained; keep looking.
                            Ok(_) => match tokio::time::timeout(
                                Duration::from_secs(5),
                                tls.read(&mut sink),
                            )
                            .await
                            {
                                Ok(Ok(0)) => Ending::CloseNotify,
                                Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                                    Ending::BareFin
                                }
                                _ => Ending::Nothing,
                            },
                            Err(_) => Ending::Nothing,
                        };
                    let _ = tx.send(ending);
                });
            }
        });
    });
    (addr, cert_der, rx)
}

/// **A finished TLS exchange ends with `close_notify`**, which is the one
/// outcome a no-op `poll_shutdown` cannot produce.
///
/// `Connection: close` on the response is what makes the client tear the
/// session down as soon as the body is drained, rather than returning it
/// to a pool — so the shutdown is part of this exchange rather than
/// something a later test might trigger. The pool is off as well, so
/// neither half of that depends on the other.
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_tls_exchange_sends_close_notify() {
    let (addr, cert, endings) = server();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).expect("the server's own certificate");
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let t = Native::new(Tokio, Rustls::from_config(Arc::new(cfg)), IpLiteralOnly).without_pool();
    let req = http::Request::get(format!("https://127.0.0.1:{}/x", addr.port()))
        .body(RequestBody::Empty)
        .expect("a well-formed request");

    let resp = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("must not hang")
        .expect("the exchange completes");
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(&body[..], b"hi");

    assert_eq!(
        endings.recv_timeout(BOUND).expect("the server reported"),
        Ending::CloseNotify,
        "the client must send `close_notify` when it finishes with a TLS \
         connection — a bare FIN is what rustls refuses to treat as a clean \
         end, because a truncation attack looks exactly like one"
    );
}
